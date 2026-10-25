//! A malformed policy carrying a secret must not leak through stderr or the report, only a code.
#[path = "common/connected.rs"]
mod connected;

#[test]
fn malformed_policy_secret_is_never_echoed() {
    const SECRET: &str = "PLANTED_POLICY_SECRET_4417";
    let h = connected::ConnectedFixture::local();
    let policy = h.dir.path().join(format!("policy-{SECRET}"));
    // A copy of the real policy plus one malformed file that carries the secret.
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../policy"))
            .arg(&policy)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        policy.join(format!("{SECRET}.json")),
        format!("{{\"{SECRET}\": "),
    )
    .unwrap();
    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&h.config).unwrap()).unwrap();
    cfg["paths"]["policy_dir"] = policy.to_str().unwrap().into();
    connected::put(&h.config, cfg.to_string());
    let (out, report) = h.accept();
    assert_eq!(out.status.code(), Some(2), "report: {report}");
    assert!(!String::from_utf8_lossy(&out.stderr).contains(SECRET));
    let text = std::fs::read_to_string(h.dir.path().join("evidence/report.json")).unwrap();
    assert!(!text.contains(SECRET));
    assert_eq!(report["blocked"][0], "docker_prerequisites");
    assert_eq!(
        report["blocked_reasons"][0],
        "docker_prerequisites: local_validation"
    );
}
