#[path = "common/connected.rs"]
mod connected;

fn enabled() -> bool {
    if std::env::var("KOD_DOCKER_TESTS").as_deref() == Ok("1") {
        return true;
    }
    println!("SKIPPED docker disabled");
    false
}

fn passed(report: &serde_json::Value, name: &str) -> bool {
    report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == name && s["outcome"] == "pass")
}

#[test]
fn connected_conformance_2_4_7() {
    if !enabled() {
        return;
    }
    let h = connected::ConnectedFixture::docker();
    let (out, report) = h.accept();
    println!("{}", String::from_utf8_lossy(&out.stderr));
    // Scenarios not yet implemented stay blocked, so the suite is never green (exit 2), never failed (1).
    assert_eq!(out.status.code(), Some(2), "report: {report}");
    assert!(
        report["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["outcome"] != "fail")
    );
    assert!(report["executed"].as_u64().unwrap() >= 4);
    for name in [
        "connected-ingress-safety",
        "mcp-tool-call",
        "planted-secret-denied-and-audited",
        "draft-pr-story-agent-trailers",
    ] {
        assert!(passed(&report, name), "{name} missing in {report}");
    }
    assert!(report["external_writes"].as_u64().unwrap() > 0);
    let text = report.to_string();
    assert!(!text.contains("PLANTED_CONNECTED_SECRET_9152"));
    let audit =
        std::fs::read_to_string(h.dir.path().join("evidence/audit-observations.json")).unwrap();
    assert!(!audit.contains("PLANTED_CONNECTED_SECRET_9152"));
}
