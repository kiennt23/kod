mod common;

use common::fixture;
use kod_audit::RUN_AUDIT;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

/// Runs the kod-audit binary against the fixture; returns (exit ok, stdout JSON).
fn run(f: &common::Fixture, args: &[&str]) -> (bool, Value) {
    run_at(&f.db(), &f.target.dir, args)
}

fn run_at(db: &Path, checkpoints: &Path, args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_kod-audit"))
        .arg("--db")
        .arg(db)
        .arg("--checkpoints")
        .arg(checkpoints)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

#[test]
fn checkpoint_and_verify_succeed_then_tampering_exits_non_zero() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    let (ok, v) = run(&f, &["checkpoint"]);
    assert!(ok, "{v}");
    assert_eq!(v["checkpoint"]["seq"], 2);
    let (ok, v) = run(&f, &["verify"]);
    assert!(ok, "{v}");
    assert_eq!(v["entries"], 2);
    f.attacker()
        .execute("UPDATE audit SET payload = '{}' WHERE seq = 1", [])
        .unwrap();
    let (ok, v) = run(&f, &["verify"]);
    assert!(!ok);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("tampered at seq 1"));
    let (ok, _) = run(&f, &["checkpoint"]);
    assert!(!ok);
}

#[test]
fn purge_refuses_under_the_placeholder_profile() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let policy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy");
    let policy = policy.to_str().unwrap();
    let (ok, v) = run(
        &f,
        &["purge", "--policy", policy, "--record-type", RUN_AUDIT],
    );
    assert!(ok, "{v}");
    assert_eq!(v, json!({"ok": true, "payloads_cleared": 0}));
}

#[test]
fn captured_checkpoints_accept_an_older_snapshot_after_live_appends() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let frozen = f.dir.path().join("frozen");
    let missing_db = f.dir.path().join("not-yet.db");
    let (ok, v) = run_at(
        &missing_db,
        &f.target.dir,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(ok, "{v}");
    assert!(!missing_db.exists());
    let snapshot = f.dir.path().join("snapshot.db");
    rusqlite::Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let (ok, v) = run_at(&snapshot, &frozen, &["verify"]);
    assert!(ok, "{v}");
    assert_eq!(v["entries"], 1);
    assert_eq!(v["last_checkpoint"], 1);
}

#[test]
fn captured_checkpoints_still_detect_a_truncated_snapshot() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let snapshot = f.dir.path().join("snapshot.db");
    rusqlite::Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let frozen = f.dir.path().join("frozen");
    let (ok, v) = run(
        &f,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(ok, "{v}");
    let (ok, v) = run_at(&snapshot, &frozen, &["verify"]);
    assert!(!ok, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("checkpoint at seq 2"),
        "{v}"
    );
}

#[test]
fn checkpoint_capture_never_reuses_a_previous_directory() {
    let f = fixture();
    let frozen = f.dir.path().join("frozen");
    std::fs::create_dir(&frozen).unwrap();
    let (ok, v) = run(
        &f,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(!ok, "{v}");
}

#[test]
fn verify_checks_retention_records_against_the_released_policies() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let released = f.dir.path().join("released");
    std::fs::create_dir(&released).unwrap();
    let flag = ["verify", "--released-policies", released.to_str().unwrap()];
    // No retention records yet: nothing to check against.
    let (ok, v) = run(&f, &flag);
    assert!(ok, "{v}");
    // One policy version allowing immediate deletion, released and used.
    let policy = released.join("v1");
    std::fs::create_dir(&policy).unwrap();
    for entry in
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")).unwrap()
    {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, policy.join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(
        policy.join("compliance.json"),
        json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}).to_string(),
    )
    .unwrap();
    let (ok, v) = run(
        &f,
        &[
            "purge",
            "--policy",
            policy.to_str().unwrap(),
            "--record-type",
            RUN_AUDIT,
        ],
    );
    assert!(ok, "{v}");
    let (ok, v) = run(&f, &flag);
    assert!(ok, "{v}");
    assert_eq!(v["retention_records"], 1);
    // Withdrawn from the released set: the record no longer checks out.
    std::fs::remove_dir_all(&policy).unwrap();
    let (ok, v) = run(&f, &flag);
    assert!(!ok);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("not a released version"),
        "{v}"
    );
}
