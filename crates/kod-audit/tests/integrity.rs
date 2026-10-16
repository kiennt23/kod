mod common;

use anyhow::Result;
use common::fixture;
use kod_audit::{
    AuditLog, RETENTION, RUN_AUDIT,
    checkpoint::{Checkpoint, CheckpointTarget, DirTarget},
    entry_hash,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::cell::Cell;

fn filled(n: u64) -> common::Fixture {
    let f = fixture();
    for i in 0..n {
        f.log.append(RUN_AUDIT, &json!({"n": i})).unwrap();
    }
    f
}

fn err(f: &common::Fixture) -> String {
    format!("{:#}", f.log.verify(&f.target).unwrap_err())
}

#[test]
fn checkpoints_are_write_once() {
    let f = filled(3);
    let cp = f.log.checkpoint(&f.target).unwrap().unwrap();
    assert_eq!(cp.seq, 3);
    // Nothing new: no second checkpoint, and no error.
    assert_eq!(f.log.checkpoint(&f.target).unwrap(), None);
    let forged = Checkpoint {
        hash: "f".repeat(64),
        ..cp.clone()
    };
    assert!(f.target.put(&forged).is_err());
    assert_eq!(f.target.list().unwrap(), vec![cp]);
    f.log.append(RUN_AUDIT, &json!({"n": 3})).unwrap();
    assert_eq!(f.log.checkpoint(&f.target).unwrap().unwrap().seq, 4);
    assert_eq!(f.log.verify(&f.target).unwrap().last_checkpoint, Some(4));
}

#[test]
fn an_edited_payload_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET payload = '{\"n\":99}' WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 2: payload does not match"));
}

#[test]
fn an_edited_entry_with_a_matching_payload_hash_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET at = at + 1 WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 2: entry hash"));
}

#[test]
fn a_removed_payload_without_a_retention_record_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 1: payload removed without a retention record"));
}

#[test]
fn a_deleted_middle_entry_breaks_the_chain() {
    let f = filled(3);
    f.attacker()
        .execute("DELETE FROM audit WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("chain broken: expected seq 2, found seq 3"));
}

/// Rewrites entry 2 and recomputes every hash after it, so the chain itself
/// is consistent again: only a checkpoint can show the change.
fn rewrite_entry_2(f: &common::Fixture) {
    let c = f.attacker();
    c.execute(
        "UPDATE audit SET payload = '{\"n\":99}', payload_sha256 = ?1 WHERE seq = 2",
        [sha256_hex("{\"n\":99}")],
    )
    .unwrap();
    let mut prev = c
        .query_row("SELECT hash FROM audit WHERE seq = 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    let last: i64 = c
        .query_row("SELECT MAX(seq) FROM audit", [], |r| r.get(0))
        .unwrap();
    for seq in 2..=last {
        let (at, rt, ph): (i64, String, String) = c
            .query_row(
                "SELECT at, record_type, payload_sha256 FROM audit WHERE seq = ?1",
                [seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let h = entry_hash(&prev, seq as u64, at, &rt, &ph).unwrap();
        c.execute("UPDATE audit SET hash = ?1 WHERE seq = ?2", (&h, seq))
            .unwrap();
        prev = h;
    }
}

#[test]
fn a_rewritten_chain_is_caught_by_the_checkpoint() {
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    rewrite_entry_2(&f);
    assert!(err(&f).contains("tampered: checkpoint at seq 3 does not match the log"));
    // And the tampered log is never checkpointed.
    assert!(f.log.checkpoint(&f.target).is_err());
}

#[test]
fn the_off_host_verifier_catches_a_rewrite_behind_a_forged_mount() {
    // f.target is the real write-once store under the checkpoint account.
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    rewrite_entry_2(&f);
    // Root on the audit host points the on-host verify at a forged mount.
    let forged = DirTarget {
        dir: f.dir.path().join("forged"),
    };
    std::fs::create_dir(&forged.dir).unwrap();
    f.log.checkpoint(&forged).unwrap().unwrap();
    assert_eq!(f.log.verify(&forged).unwrap().entries, 3);
    // The off-host verifier checks a copy of the log against its own store.
    let copy = f.dir.path().join("copy.db");
    Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let e = AuditLog::open(&copy)
        .unwrap()
        .verify(&f.target)
        .unwrap_err();
    assert!(
        format!("{e:#}").contains("tampered: checkpoint at seq 3"),
        "{e:#}"
    );
}

/// Appends a correctly hashed retention entry through raw SQL, then clears
/// seq 1's payload: what a host attacker can do without going through purge.
fn forge_retention(f: &common::Fixture, payload: &Value) {
    let c = f.attacker();
    let (seq, prev): (i64, String) = c
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let text = payload.to_string();
    let ph = sha256_hex(&text);
    let hash = entry_hash(&prev, seq as u64 + 1, 0, RETENTION, &ph).unwrap();
    c.execute(
        "INSERT INTO audit VALUES (?1, 0, ?2, ?3, ?4, ?5)",
        params![seq + 1, RETENTION, text, ph, hash],
    )
    .unwrap();
    c.execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
        .unwrap();
}

#[test]
fn a_forged_retention_record_does_not_excuse_a_removed_payload() {
    for (payload, want) in [
        (json!({"seqs": [1]}), "malformed retention record"),
        (
            json!({"seqs": "no", "record_type": RUN_AUDIT, "policy_version": "v"}),
            "malformed retention record",
        ),
        (
            json!({"seqs": [1], "record_type": "prompts", "policy_version": "v"}),
            "not an earlier prompts entry",
        ),
        (
            json!({"seqs": [1, 9], "record_type": RUN_AUDIT, "policy_version": "v"}),
            "not an earlier run_audit entry",
        ),
    ] {
        let f = filled(2);
        forge_retention(&f, &payload);
        let e = format!("{:#}", f.log.verify(&f.target).unwrap_err());
        assert!(e.contains(want), "{payload}: {e}");
    }
}

#[test]
fn verify_does_not_take_the_write_lock() {
    let f = filled(2);
    let writer = Connection::open(f.db()).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 2);
    writer.execute_batch("ROLLBACK").unwrap();
}

/// On its first list, another process appends an entry and checkpoints it,
/// as the checkpoint timer can while verify runs.
struct Racing<'a> {
    f: &'a common::Fixture,
    raced: Cell<bool>,
}

impl CheckpointTarget for Racing<'_> {
    fn put(&self, cp: &Checkpoint) -> Result<()> {
        self.f.target.put(cp)
    }

    fn list(&self) -> Result<Vec<Checkpoint>> {
        if !self.raced.replace(true) {
            let other = AuditLog::open(&self.f.db())?;
            other.append(RUN_AUDIT, &json!({"n": "raced"}))?;
            other.checkpoint(&self.f.target)?;
        }
        self.f.target.list()
    }
}

#[test]
fn a_checkpoint_taken_during_verify_is_not_reported_as_truncation() {
    let f = filled(2);
    let racing = Racing {
        f: &f,
        raced: Cell::new(false),
    };
    f.log.verify(&racing).unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 3);
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn truncation_before_the_last_checkpoint_is_caught() {
    let f = filled(5);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.attacker()
        .execute("DELETE FROM audit WHERE seq > 3", [])
        .unwrap();
    assert!(err(&f).contains("truncated: checkpoint at seq 5 but the log ends at seq 3"));
}

#[test]
fn truncation_after_the_last_checkpoint_is_not_detectable() {
    // The honest ceiling: entries newer than the last checkpoint can vanish.
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 3})).unwrap();
    f.attacker()
        .execute("DELETE FROM audit WHERE seq = 4", [])
        .unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 3);
}

#[test]
fn a_replaced_empty_log_is_caught() {
    let f = filled(2);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.attacker().execute("DELETE FROM audit", []).unwrap();
    assert!(err(&f).contains("truncated: checkpoint at seq 2 but the log ends at seq 0"));
}

#[test]
fn an_unreadable_checkpoint_store_fails_verification() {
    let f = filled(1);
    let junk = f.target.dir.join("00000000000000000007.json");
    std::fs::write(&junk, "not json").unwrap();
    assert!(err(&f).contains("unreadable checkpoint"));
    std::fs::remove_file(&junk).unwrap();
    std::fs::write(
        f.target.dir.join("00000000000000000009.json"),
        r#"{"seq":1,"hash":"00","at":0}"#,
    )
    .unwrap();
    assert!(err(&f).contains("names the wrong seq"));
    std::fs::remove_dir_all(&f.target.dir).unwrap();
    assert!(err(&f).contains("checkpoint directory"));
}

#[test]
fn foreign_files_in_the_checkpoint_store_are_ignored() {
    let f = filled(1);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    for name in [".DS_Store", "lost+found", "notes.json", "1.json"] {
        std::fs::write(f.target.dir.join(name), "x").unwrap();
    }
    assert_eq!(f.log.verify(&f.target).unwrap().last_checkpoint, Some(1));
}
