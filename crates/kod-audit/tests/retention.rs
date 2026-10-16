mod common;

use common::fixture;
use kod_audit::{GENESIS, PURGE_BATCH, RETENTION, RUN_AUDIT, entry_hash, now};
use kod_policy::Policy;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const DAY: i64 = 86_400;

/// The repo's policy directory (kod-policy, sub-plan 3).
fn policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

/// A temp copy of the policy directory with compliance.json replaced.
fn policy_with_compliance(compliance: Value) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(dir.path().join("compliance.json"), compliance.to_string()).unwrap();
    dir
}

fn payloads(db: &Path) -> Vec<(i64, String, Option<String>)> {
    Connection::open(db)
        .unwrap()
        .prepare("SELECT seq, record_type, payload FROM audit ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Appends an entry dated `at` through raw SQL with a correct hash, since
/// `append` always stamps the current time.
fn append_dated(db: &Path, at: i64) {
    append_raw(db, at, RUN_AUDIT, r#"{"dated":true}"#);
}

/// Appends any entry through raw SQL, triggers intact, with a correct hash:
/// what someone who knows the (public) hash construction can do.
fn append_raw(db: &Path, at: i64, record_type: &str, text: &str) {
    use sha2::{Digest, Sha256};
    let c = Connection::open(db).unwrap();
    let (seq, prev): (i64, String) = c
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
        .unwrap_or((0, GENESIS.to_string()));
    let ph: String = Sha256::digest(text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let hash = entry_hash(&prev, seq as u64 + 1, at, record_type, &ph).unwrap();
    c.execute(
        "INSERT INTO audit VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq + 1, at, record_type, text, ph, hash],
    )
    .unwrap();
}

#[test]
fn the_placeholder_profile_refuses_every_deletion_and_records_why() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let policy = Policy::load(&policy_dir()).unwrap();
    let in_a_century = now().unwrap() + 36_500 * DAY;
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, in_a_century).unwrap(), 0);
    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 2);
    assert!(rows[0].2.is_some());
    let refusal: Value = serde_json::from_str(rows[1].2.as_ref().unwrap()).unwrap();
    assert_eq!(refusal["kind"], "policy_decision");
    assert_eq!(refusal["input"]["action"], "retention_delete");
    assert_eq!(refusal["decision"]["allow"], false);
    assert!(
        refusal["decision"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("placeholder has no retention_days.run_audit")
    );
    assert_eq!(refusal["decision"]["policy_version"], policy.version());
    f.log.verify(&f.target).unwrap();
}

#[test]
fn expired_payloads_are_deleted_and_the_chain_still_verifies() {
    let f = fixture();
    for n in 0..3 {
        f.log.append(RUN_AUDIT, &json!({"n": n})).unwrap();
    }
    f.log.append("prompts", &json!({"p": "kept"})).unwrap();
    f.log.checkpoint(&f.target).unwrap().unwrap();
    let dir = policy_with_compliance(
        json!({"profiles": {"a": {"retention_days": {"run_audit": 30, "prompts": 30}},
                            "b": {"retention_days": {"run_audit": 90, "prompts": 30}}}}),
    );
    let policy = Policy::load(dir.path()).unwrap();

    // 31 days: profile b still needs 90, so nothing goes, and the refusal is
    // recorded as entry 5.
    assert_eq!(
        f.log
            .purge(&policy, RUN_AUDIT, now().unwrap() + 31 * DAY)
            .unwrap(),
        0
    );
    // 91 days: entries 1-3 are old enough. The refusal, written at 31 days,
    // is 60 days old, so it is refused in turn (entry 6) and kept.
    assert_eq!(
        f.log
            .purge(&policy, RUN_AUDIT, now().unwrap() + 91 * DAY)
            .unwrap(),
        3
    );

    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 7);
    for i in [0, 1, 2] {
        assert_eq!(rows[i].2, None, "seq {}", rows[i].0);
    }
    assert_eq!(rows[3].2.as_deref(), Some(r#"{"p":"kept"}"#));
    assert!(rows[4].2.is_some() && rows[5].2.is_some());
    assert_eq!(rows[6].1, RETENTION);
    let deletion: Value = serde_json::from_str(rows[6].2.as_ref().unwrap()).unwrap();
    assert_eq!(deletion["kind"], "retention_delete");
    assert_eq!(deletion["seqs"], json!([1, 2, 3]));
    assert_eq!(deletion["record_type"], RUN_AUDIT);
    assert_eq!(deletion["policy_version"], policy.version());
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 7);
    assert_eq!(f.log.verify_retention(&[policy]).unwrap(), 1);
}

#[test]
fn retention_records_are_never_purged() {
    let f = fixture();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"retention": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert!(f.log.purge(&policy, RETENTION, now().unwrap()).is_err());
}

#[test]
fn a_clock_behind_the_newest_entry_refuses_to_purge() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    let err = f
        .log
        .purge(&policy, RUN_AUDIT, now().unwrap() - DAY)
        .unwrap_err();
    assert!(err.to_string().contains("clock is behind"), "{err:#}");
    assert!(payloads(&f.db())[0].2.is_some());
}

#[test]
fn a_refusal_after_some_deletions_is_recorded() {
    let f = fixture();
    let t = now().unwrap();
    for days in [40, 40, 10] {
        append_dated(&f.db(), t - days * DAY);
    }
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 30}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, t).unwrap(), 2);
    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 5);
    assert!(rows[2].2.is_some());
    // Entry 4 is the refusal for entry 3; entry 5 lists the deletions.
    let refusal: Value = serde_json::from_str(rows[3].2.as_ref().unwrap()).unwrap();
    assert_eq!(refusal["decision"]["allow"], false);
    assert_eq!(refusal["input"]["age_days"], 10);
    let deletion: Value = serde_json::from_str(rows[4].2.as_ref().unwrap()).unwrap();
    assert_eq!(deletion["seqs"], json!([1, 2]));
    f.log.verify(&f.target).unwrap();
}

#[test]
fn large_purges_run_in_batches() {
    let f = fixture();
    for n in 0..=PURGE_BATCH {
        f.log.append(RUN_AUDIT, &json!({"n": n})).unwrap();
    }
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(
        f.log.purge(&policy, RUN_AUDIT, now().unwrap()).unwrap(),
        PURGE_BATCH + 1
    );
    let rows = payloads(&f.db());
    assert_eq!(rows.iter().filter(|r| r.1 == RETENTION).count(), 2);
    assert!(rows.iter().all(|r| r.1 == RETENTION || r.2.is_none()));
    f.log.verify(&f.target).unwrap();
}

#[test]
fn a_purge_checks_out_only_against_its_released_policy() {
    let f = fixture();
    let t = now().unwrap();
    append_dated(&f.db(), t - 40 * DAY);
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 30}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, t).unwrap(), 1);
    assert_eq!(f.log.verify_retention(&[policy]).unwrap(), 1);
    let placeholder = Policy::load(&policy_dir()).unwrap();
    let err = f.log.verify_retention(&[placeholder]).unwrap_err();
    assert!(
        err.to_string().contains("not a released version"),
        "{err:#}"
    );
}

#[test]
fn a_forged_well_formed_retention_record_fails_automatically() {
    let placeholder = Policy::load(&policy_dir()).unwrap();
    for (version, want) in [
        ("sha256:forged;regorus=0.13.0", "not a released version"),
        (placeholder.version(), "does not allow deleting seq 1"),
    ] {
        let f = fixture();
        f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
        f.log.checkpoint(&f.target).unwrap().unwrap();
        let forged = json!({"kind": "retention_delete", "record_type": RUN_AUDIT,
                            "seqs": [1], "policy_version": version});
        append_raw(&f.db(), now().unwrap(), RETENTION, &forged.to_string());
        // The triggers let the payload go: a chained retention entry covers it.
        Connection::open(f.db())
            .unwrap()
            .execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
            .unwrap();
        f.log.verify(&f.target).unwrap();
        let err = f
            .log
            .verify_retention(std::slice::from_ref(&placeholder))
            .unwrap_err();
        assert!(err.to_string().contains("tampered at seq 2"), "{err:#}");
        assert!(err.to_string().contains(want), "{err:#}");
    }
}
