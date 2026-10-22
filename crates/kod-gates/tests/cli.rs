use std::process::Command;

#[test]
fn ruleset_cli_emits_an_app_bound_merge_queue_ruleset() {
    let output = Command::new(env!("CARGO_BIN_EXE_kod-gates"))
        .args(["ruleset", "--app-id", "123"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let rules: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rules["enforcement"], "disabled");
    assert!(
        rules["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["type"] == "merge_queue")
    );
}

#[test]
fn scan_config_cli_requires_an_immutable_image() {
    let bad = Command::new(env!("CARGO_BIN_EXE_kod-gates"))
        .args(["scan-config", "--image", "rust:latest"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    let good = Command::new(env!("CARGO_BIN_EXE_kod-gates"))
        .args([
            "scan-config",
            "--image",
            &format!("sha256:{}", "a".repeat(64)),
        ])
        .output()
        .unwrap();
    assert!(good.status.success());
    let config: kod_gates::scanner::ScanConfig = serde_json::from_slice(&good.stdout).unwrap();
    config.validate().unwrap();
}

#[test]
fn bypass_cli_records_the_verified_delivery_for_retry_safe_ingestion() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    let output = Command::new(env!("CARGO_BIN_EXE_kod-gates"))
        .args(["record-bypass", "--audit"])
        .arg(&db)
        .args([
            "--repo",
            "acme/app",
            "--delivery",
            "event-1",
            "--actor",
            "admin",
            "--sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--ruleset",
            "123",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let conn = rusqlite::Connection::open(db).unwrap();
    let payload: String = conn
        .query_row("SELECT payload FROM audit", [], |row| row.get(0))
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["delivery"], "event-1");
}
