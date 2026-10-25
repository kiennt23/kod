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
    // Only the later lifecycle scenarios stay blocked, so the suite is never green (exit 2), never failed (1).
    assert_eq!(out.status.code(), Some(2), "report: {report}");
    assert!(
        report["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["outcome"] != "fail")
    );
    assert_eq!(report["executed"].as_u64().unwrap(), 10, "report: {report}");
    let blocked: Vec<_> = report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["outcome"] == "blocked")
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        blocked,
        [
            "cancel-revokes-queued-and-streaming-requests",
            "label-removal-revokes-run",
            "kill-confirms-container-absence",
            "restart-never-resumes-and-reconciles-publish",
            "kill-during-dependency-outages"
        ]
    );
    for s in report["scenarios"].as_array().unwrap() {
        if s["outcome"] == "pass" {
            assert!(!s["observations"].as_array().unwrap().is_empty());
        }
    }
    for name in [
        "connected-ingress-safety",
        "mcp-tool-call",
        "planted-secret-denied-and-audited",
        "draft-pr-story-agent-trailers",
        "connected-gateway-safety",
        "collector-integrity",
        "approval-refresh",
        "current-source-fail-closed",
        "status-comment-reconciliation",
        "gate-container-isolation",
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
