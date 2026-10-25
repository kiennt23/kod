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
    assert_eq!(out.status.code(), Some(0), "report: {report}");
    assert_eq!(report["executed"].as_u64().unwrap(), 15, "report: {report}");
    assert!(report["blocked"].as_array().unwrap().is_empty());
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
        "cancel-revokes-queued-and-streaming-requests",
        "label-removal-revokes-run",
        "kill-confirms-container-absence",
        "restart-never-resumes-and-reconciles-publish",
        "kill-during-dependency-outages",
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

/// Runs the real host against real Docker and requires the named lifecycle scenario to pass
/// with recorded observations (the whole suite runs; the named one is asserted).
fn scenario_passes(name: &str) {
    assert_eq!(std::env::var("KOD_DOCKER_TESTS").as_deref(), Ok("1"));
    let (_, report) = connected::ConnectedFixture::docker().accept();
    let s = report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing in {report}"));
    assert_eq!(s["outcome"], "pass", "{s}");
    assert!(!s["observations"].as_array().unwrap().is_empty());
}

#[test]
#[ignore = "requires real Docker; run explicitly with --ignored"]
fn kill_confirms_container_absence() {
    scenario_passes("kill-confirms-container-absence");
}

#[test]
#[ignore = "requires real Docker; run explicitly with --ignored"]
fn restart_never_resumes_and_reconciles_publish() {
    scenario_passes("restart-never-resumes-and-reconciles-publish");
}

#[test]
#[ignore = "requires real Docker; run explicitly with --ignored"]
fn suite_stops_forwarding_once_the_authorized_budget_is_used() {
    assert_eq!(std::env::var("KOD_DOCKER_TESTS").as_deref(), Ok("1"));
    // Given an authorization for exactly one model request across the whole suite.
    let h = connected::ConnectedFixture::docker();
    h.cap_requests(1);
    // When the full suite runs.
    let (out, report) = h.accept();
    // Then it fails closed: non-zero, not every scenario passed, the cap is the recorded reason.
    assert_ne!(out.status.code(), Some(0), "report: {report}");
    assert!(
        report["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "suite_budget_exhausted"),
        "report: {report}"
    );
    assert!(
        report["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["outcome"] != "pass")
    );
    // And the ledger shows exactly the one authorized request, while the provider received no more.
    assert_eq!(report["actual_spend"]["requests"], 1, "report: {report}");
    assert_eq!(report["model_requests"], 1);
    let seen: serde_json::Value = serde_json::from_slice(
        &std::fs::read(h.dir.path().join("evidence/provider-observations.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(seen["model_requests_received"], 1, "provider saw {seen}");
}

#[test]
fn hung_scenario_hits_the_suite_deadline_and_keeps_completed_results() {
    if !enabled() {
        return;
    }
    // Given a scenario that never finishes after the first two complete, and a short deadline.
    let h = connected::ConnectedFixture::docker();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_kod-host"));
    cmd.env("KOD_ACCEPTANCE_TEST_HANG", "1")
        .env("KOD_ACCEPTANCE_SUITE_DEADLINE_MS", "240000");
    let began = std::time::Instant::now();
    let (out, report) = h.accept_with(cmd);
    // Then it fails fast (exit 1, not blocked), names what completed, and keeps results and counters.
    assert!(began.elapsed() < std::time::Duration::from_secs(400));
    assert_eq!(out.status.code(), Some(1), "report: {report}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("scenarios completed before the hang: [connected-ingress-safety,")
            && err.contains("draft-pr-story-agent-trailers]"),
        "stderr: {err}"
    );
    assert!(
        passed(&report, "connected-ingress-safety"),
        "report: {report}"
    );
    assert!(
        passed(&report, "draft-pr-story-agent-trailers"),
        "report: {report}"
    );
    assert!(report["executed"].as_u64().unwrap() >= 2);
    assert!(
        report["external_writes"].as_u64().unwrap() > 0,
        "report: {report}"
    );
}
