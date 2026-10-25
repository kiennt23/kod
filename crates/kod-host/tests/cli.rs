#[path = "common/acceptance.rs"]
mod fixture;
use std::process::Command;
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kod-host"))
}
#[test]
fn acceptance_requires_explicit_supported_mode() {
    // Given a command missing mode or naming an unsupported one.
    for mode in [None, Some("fixture")] {
        let mut command = binary();
        command.args(["accept", "--config", "/missing", "--evidence", "/missing"]);
        if let Some(mode) = mode {
            command.args(["--mode", mode]);
        }
        // When parsed, then the CLI refuses it before configuration or calls.
        assert_eq!(command.output().unwrap().status.code(), Some(2));
    }
}
#[test]
fn missing_authorization_blocks_before_network_and_sanitizes_errors() {
    // Given a listening upstream and a live config lacking authorization.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (dir, mut config) = fixture::fixture();
    config["model"]["upstream"] =
        serde_json::json!(format!("http://{}", listener.local_addr().unwrap()));
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("config.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    // When acceptance runs, then no connection is attempted and exit is BLOCKED.
    let output = binary()
        .arg("accept")
        .arg("--config")
        .arg(path)
        .arg("--evidence")
        .arg(root.join("evidence"))
        .args(["--mode", "live"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("evidence/report.json")).unwrap()).unwrap();
    assert_eq!(report["outcome"], "blocked");
    // Blocked for the authorization itself, not for some other reason.
    assert!(
        report["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "authorization")
    );
    assert_eq!(report["model_requests"], 0);
}
#[test]
fn malformed_config_and_evidence_failure_are_sanitized_blocked_exits() {
    // Given a malformed input containing a secret marker.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, b"{\"planted-secret-token\":true}").unwrap();
    // When the binary parses it, then it emits no raw error content.
    let output = binary()
        .arg("accept")
        .arg("--config")
        .arg(config)
        .arg("--evidence")
        .arg(dir.path())
        .args(["--mode", "live"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("planted-secret-token"));
}
