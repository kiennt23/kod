use kod_audit::{AuditLog, GENESIS, RETENTION, RUN_AUDIT, entry_hash};
use kod_policy::Decision;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{cell::Cell, path::PathBuf, thread};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    log: AuditLog,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(&dir.path().join("audit.db")).unwrap();
    Fixture { dir, log }
}

impl Fixture {
    fn db(&self) -> PathBuf {
        self.dir.path().join("audit.db")
    }

    /// Recomputes every hash from GENESIS and returns the number of entries.
    fn chain_len(&self) -> u64 {
        let c = Connection::open(self.db()).unwrap();
        let rows: Vec<(i64, i64, String, String, String)> = c
            .prepare("SELECT seq, at, record_type, payload_sha256, hash FROM audit ORDER BY seq")
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut prev = GENESIS.to_string();
        for (i, (seq, at, rt, ph, hash)) in rows.iter().enumerate() {
            assert_eq!(*seq, i as i64 + 1);
            assert_eq!(entry_hash(&prev, *seq as u64, *at, rt, ph).unwrap(), *hash);
            prev = hash.clone();
        }
        rows.len() as u64
    }
}

#[test]
fn entries_form_a_chain_from_genesis() {
    let f = fixture();
    assert_eq!(f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap(), 1);
    assert_eq!(f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap(), 2);
    assert_eq!(f.chain_len(), 2);
}

#[test]
fn secrets_are_redacted_before_storage() {
    let f = fixture();
    let secret = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");
    f.log
        .append("prompts", &json!({"prompt": format!("push with {secret}")}))
        .unwrap();
    drop(f.log);
    let mut raw = Vec::new();
    for name in ["audit.db", "audit.db-wal"] {
        if let Ok(b) = std::fs::read(f.dir.path().join(name)) {
            raw.extend(b);
        }
    }
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("[REDACTED]"));
    assert!(!raw.contains(secret));
}

#[test]
fn a_bad_record_type_is_refused() {
    let f = fixture();
    for rt in ["", "Run_Audit", "run audit", "run-audit"] {
        assert!(f.log.append(rt, &json!({})).is_err(), "{rt:?}");
    }
}

#[test]
fn decisions_are_recorded_with_their_policy_version() {
    let f = fixture();
    let decision = Decision {
        allow: false,
        reasons: vec!["publish: the run is canceled".into()],
        policy_version: "sha256:abc;regorus=0.13.0".into(),
    };
    f.log
        .record_decision(&json!({"action": "publish"}), &decision)
        .unwrap();
    let c = Connection::open(f.db()).unwrap();
    let payload: String = c
        .query_row("SELECT payload FROM audit WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    let v: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["kind"], "policy_decision");
    assert_eq!(v["input"]["action"], "publish");
    assert_eq!(v["decision"]["allow"], false);
    assert_eq!(v["decision"]["policy_version"], "sha256:abc;regorus=0.13.0");
}

#[test]
fn rows_cannot_be_changed_or_deleted_through_sql() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let c = Connection::open(f.db()).unwrap();
    for sql in [
        "DELETE FROM audit",
        "UPDATE audit SET payload = '{}'",
        "UPDATE audit SET at = 0",
        "UPDATE audit SET hash = 'x'",
    ] {
        let err = c.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
}

#[test]
fn payloads_cannot_be_cleared_or_rows_replaced_through_sql() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    let c = Connection::open(f.db()).unwrap();
    for sql in [
        // No retention record covers seq 1.
        "UPDATE audit SET payload = NULL WHERE seq = 1",
        // REPLACE deletes without firing the delete trigger.
        "INSERT OR REPLACE INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (1, 0, 'run_audit', '{}', 'x', 'x')",
        "INSERT INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (9, 0, 'run_audit', '{}', 'x', 'x')",
    ] {
        let err = c.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
}

#[test]
fn retention_records_cannot_be_appended_directly() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let forged = json!({"kind": "retention_delete", "record_type": RUN_AUDIT, "seqs": [1]});
    assert!(f.log.append(RETENTION, &forged).is_err());
    assert!(f.log.before(RETENTION, &forged, || ()).is_err());
    assert_eq!(f.chain_len(), 1);
}

#[cfg(unix)]
#[test]
fn the_store_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    f.log
        .append(RUN_AUDIT, &json!({"prompt": "private"}))
        .unwrap();
    for name in ["audit.db", "audit.db-wal", "audit.db-shm"] {
        let mode = std::fs::metadata(f.dir.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

#[test]
fn concurrent_writers_keep_one_linear_chain() {
    let f = fixture();
    let db = f.db();
    thread::scope(|s| {
        for w in 0..4 {
            let log = AuditLog::open(&db).unwrap();
            s.spawn(move || {
                for n in 0..25 {
                    log.append(RUN_AUDIT, &json!({"w": w, "n": n})).unwrap();
                }
            });
        }
    });
    assert_eq!(f.chain_len(), 100);
}

#[test]
fn when_the_store_is_down_the_external_write_is_refused() {
    let f = fixture();
    let called = Cell::new(false);
    assert!(
        f.log
            .before(RUN_AUDIT, &json!({"call": "publish"}), || called.set(true))
            .is_ok()
    );
    assert!(called.replace(false));

    // Another holder takes the write lock and keeps it: the store is unavailable.
    let blocker = Connection::open(f.db()).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let err = f
        .log
        .before(RUN_AUDIT, &json!({"call": "publish"}), || called.set(true))
        .unwrap_err();
    assert!(err.to_string().contains("locked"), "{err:#}");
    assert!(!called.get());
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.chain_len(), 1);
}

#[test]
fn a_store_that_cannot_be_opened_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(AuditLog::open(&dir.path().join("missing/audit.db")).is_err());
    std::fs::write(dir.path().join("junk.db"), "not a database, just junk text").unwrap();
    assert!(AuditLog::open(&dir.path().join("junk.db")).is_err());
}

#[test]
fn oversized_payloads_are_refused_before_waiting_for_the_write_lock() {
    let f = fixture();
    let blocker = Connection::open(f.db()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let called = Cell::new(false);
    let payload = json!({"output": "_".repeat(kod_audit::MAX_PAYLOAD_BYTES)});
    let err = f
        .log
        .before(RUN_AUDIT, &payload, || called.set(true))
        .unwrap_err();
    assert!(err.to_string().contains("payload exceeds"), "{err:#}");
    assert!(!called.get());
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.chain_len(), 0);
}

#[test]
fn entry_hash_rejects_malformed_hex_without_panicking() {
    let good = "0".repeat(64);
    let multibyte = format!("0\u{e9}{}", "0".repeat(61)); // 64 bytes, not 64 chars
    assert_eq!(multibyte.len(), 64);
    let upper = format!("A{}", "0".repeat(63));
    let plus = format!("+a{}", "0".repeat(62));
    for bad in [multibyte, upper, plus, "0".repeat(63)] {
        assert!(entry_hash(&bad, 1, 0, "x", &good).is_err(), "{bad:?}");
        assert!(entry_hash(&good, 1, 0, "x", &bad).is_err(), "{bad:?}");
    }
}
