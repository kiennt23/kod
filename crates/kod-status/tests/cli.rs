mod common;

use common::snapshot;
use kod_status::records::{HostRecords, Source};
use std::process::Command;

#[test]
fn cli_and_comment_renderer_match_and_host_records_are_read_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.json");
    let db = dir.path().join("dispatcher.db");
    let c = kod_dispatcher::store::open(&db).unwrap();
    c.execute(
        "INSERT INTO runs (ticket, revision, state) VALUES ('TASK-1', 'revision', 'running')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO runs (ticket, revision, state, reason) VALUES ('OTHER', 'revision', 'blocked', X'FF')",
        [],
    )
    .unwrap();
    drop(c);
    let before = std::fs::read(&db).unwrap();
    let mut snapshot = snapshot();
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let source = HostRecords {
        snapshot: path.clone(),
        dispatcher: db.clone(),
    };
    let expected = kod_status::load(&source, "FAKE-1").unwrap().render();
    let invoke = |json: bool| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kod"));
        c.args(["status", "FAKE-1", "--records"])
            .arg(&path)
            .arg("--dispatcher")
            .arg(&db);
        if json {
            c.arg("--json");
        }
        c.output().unwrap()
    };
    let output = invoke(false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    assert!(!expected.contains("OTHER"));
    assert!(expected.contains("run-1 running"));
    let json = invoke(true);
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["story"], "FAKE-1");
    snapshot.pulls[0].evidence.pins = vec!["design-v3".into()];
    snapshot.rework.clear();
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert!(
        !String::from_utf8(invoke(false).stdout)
            .unwrap()
            .contains("stale inputs or head")
    );
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(source.load("different").is_err());
}

#[test]
fn missing_or_corrupt_sources_fail_without_creating_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let records = dir.path().join("records.json");
    let db = dir.path().join("missing.db");
    std::fs::write(&records, serde_json::to_vec(&snapshot()).unwrap()).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_kod"))
            .args(["status", "FAKE-1", "--records"])
            .arg(&records)
            .arg("--dispatcher")
            .arg(&db)
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!db.exists());
    std::fs::write(&records, b"{}").unwrap();
    assert!(!invoke().status.success());
    assert!(!db.exists());
}
