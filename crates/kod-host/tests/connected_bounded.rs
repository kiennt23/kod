//! A timed-out suite and a hung docker client must end `accept` promptly, keep results, and exit 1.
#[path = "common/connected.rs"]
mod connected;
use std::{
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn hung_suite_and_hung_docker_end_accept_within_a_bound() {
    let h = connected::ConnectedFixture::local();
    // A `docker` that never answers, first on PATH.
    let bin = h.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("docker"), "#!/bin/sh\nexec sleep 600\n").unwrap();
    std::fs::set_permissions(bin.join("docker"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kod-host"));
    cmd.env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    )
    .env("KOD_ACCEPTANCE_SUITE_DEADLINE_MS", "1500")
    .env("KOD_ACCEPTANCE_TEST_HANG", "1");
    let began = Instant::now();
    let (out, report) = h.accept_with(cmd);
    assert!(
        began.elapsed() < Duration::from_secs(90),
        "accept stayed alive: {:?}",
        began.elapsed()
    );
    // An execution failure is a failure (1), never merely blocked (2), and the report is complete.
    assert_eq!(out.status.code(), Some(1), "report: {report}");
    let scenarios = report["scenarios"].as_array().unwrap();
    assert!(
        scenarios.iter().any(|s| s["outcome"] == "fail"),
        "report: {report}"
    );
    assert!(
        report["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "scenario_execution_failed")
    );
}
