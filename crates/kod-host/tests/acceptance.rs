use kod_host::acceptance::{Mode, Outcome, Report, Scenario};
#[test]
fn partial_report_cannot_pass() {
    // Given an incomplete Docker execution.
    let mut report = Report::new(Mode::Docker);
    report.scenarios = vec![Scenario {
        name: "mcp-tool-call".into(),
        outcome: Outcome::Pass,
        observations: vec![],
    }];
    // When the final status is computed, then incomplete evidence blocks.
    assert_eq!(report.exit_code(), 2);
    assert!(report.curated_live().is_err());
}

#[path = "common/acceptance.rs"]
mod fixture;
use kod_host::{
    acceptance::{AcceptanceConfig, Observation, Spend, SuiteBudget, authorize, preflight_report},
    config::{FsConfig, strict_json},
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Barrier},
};

fn authorized() -> (tempfile::TempDir, FsConfig, serde_json::Value) {
    let (dir, mut value) = fixture::fixture();
    let root = dir.path().canonicalize().unwrap();
    value["acceptance"] = serde_json::json!({"disposable_repo":"acme/app","authorization_ref":root.join("authorization.json"),"expires_at":"2099-01-01T00:00:00Z","max_requests":10,"max_tokens":1000,"max_cost_micro_usd":1000,"scenario_deadline_secs":60});
    let auth = serde_json::json!({"authorized_by":"kiennt23","recorded_at":"2020-01-01T00:00:00Z","expires_at":"2099-01-01T00:00:00Z","repo":"acme/app","endpoint":"approved","model":"model","max_requests":10,"max_tokens":1000,"max_cost_micro_usd":1000,"actions":["issue","draft_pr","checks","comment","cancel","restart","kill","cleanup"]});
    (
        dir,
        strict_json(&serde_json::to_vec(&value).unwrap()).unwrap(),
        auth,
    )
}
fn write_authorization(config: &FsConfig, auth: &serde_json::Value) {
    let path = &config.acceptance.as_ref().unwrap().authorization_ref;
    std::fs::write(path, serde_json::to_vec(auth).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn missing_live_authorization_makes_zero_calls() {
    // Given an otherwise typed live configuration with no authorization file.
    let (_dir, config, _) = authorized();
    // When preflight evaluates it.
    let report = preflight_report(Mode::Live, &config);
    // Then execution is blocked without calls.
    assert_eq!(report.exit_code(), 2);
    assert!(report.blocked.iter().any(|s| s == "authorization"));
    assert_eq!((report.external_writes, report.model_requests), (0, 0));
}
#[test]
fn authorization_rejects_mismatched_expired_and_incomplete_records() {
    // Given owner records with one invalid boundary value each.
    for (field, value) in [
        ("authorized_by", serde_json::json!("intruder")),
        ("expires_at", serde_json::json!("2020-01-01T00:00:00Z")),
        ("repo", serde_json::json!("acme/production")),
        ("model", serde_json::json!("other")),
        ("endpoint", serde_json::json!("other")),
        ("max_requests", serde_json::json!(0)),
        ("actions", serde_json::json!(["issue"])),
    ] {
        let (_dir, config, mut auth) = authorized();
        auth[field] = value;
        write_authorization(&config, &auth);
        // When verified, then each record is rejected.
        assert!(authorize(&config).is_err(), "{field}");
    }
}
#[test]
fn exact_owner_record_is_accepted_without_executing_live_drills() {
    // Given an exact owner record.
    let (_dir, config, auth) = authorized();
    write_authorization(&config, &auth);
    // When verified, then it authorizes only this configuration.
    assert!(authorize(&config).is_ok());
}
#[test]
fn strict_acceptance_rejects_duplicate_missing_and_overflow_fields() {
    // Given invalid serialized boundary inputs.
    for bytes in [
        b"{}".as_slice(),
        br#"{"max_requests":1,"max_requests":2}"#,
        br#"{"max_requests":18446744073709551616}"#,
    ] {
        // When parsed, then none is accepted.
        assert!(strict_json::<AcceptanceConfig>(bytes).is_err());
    }
}
#[test]
fn simultaneous_last_allowance_forwards_at_most_one_request() {
    // Given one remaining reservation and two simultaneous callers.
    let amount = Spend {
        requests: 1,
        tokens: 10,
        cost_micro_usd: 10,
    };
    let budget = Arc::new(SuiteBudget::new(amount).unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let budget = budget.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                budget.reserve(amount).is_ok()
            })
        })
        .collect();
    // When both reserve, then only one receives forwarding permission.
    let admitted = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(admitted, 1);
    assert_eq!(budget.consumed().unwrap(), amount);
}
#[test]
fn uncertain_reservations_are_never_refunded_or_overflowed() {
    // Given a fully reserved maximum suite allowance.
    let cap = Spend {
        requests: u64::MAX,
        tokens: u64::MAX,
        cost_micro_usd: u64::MAX,
    };
    let budget = SuiteBudget::new(cap).unwrap();
    budget.reserve(cap).unwrap();
    // When another request reserves, then overflow cannot restore allowance.
    assert!(
        budget
            .reserve(Spend {
                requests: 1,
                tokens: 1,
                cost_micro_usd: 1
            })
            .is_err()
    );
    assert_eq!(budget.consumed().unwrap(), cap);
}
#[test]
fn only_actual_live_report_is_exportable() {
    // Given externally fabricated complete rows in either mode.
    for mode in [Mode::Docker, Mode::Live] {
        let mut report = Report::new(mode);
        for scenario in &mut report.scenarios {
            scenario.outcome = Outcome::Pass;
            scenario.observations.push(Observation::default());
        }
        // When export is requested, then unminted live provenance is refused.
        assert!(report.curated_live().is_err());
    }
}
#[test]
fn planted_provider_secret_cannot_enter_evidence() {
    // Given malformed untrusted provider data placed in an observed digest.
    let mut report = Report::new(Mode::Docker);
    report.scenarios[0].observations.push(Observation {
        head_sha: Some("Bearer planted-secret-token".into()),
        ..Observation::default()
    });
    // When serialized, then arbitrary provider text is refused.
    assert!(report.json().is_err());
}
#[tokio::test]
async fn private_evidence_refuses_reuse_and_symlink_destinations() {
    // Given a live run with missing prerequisites.
    let (dir, config, _) = authorized();
    let path = dir.path().canonicalize().unwrap().join("evidence");
    // When run, then blocked evidence is private and cannot be overwritten.
    let report = kod_host::acceptance::run(&config, &path, Mode::Live)
        .await
        .unwrap();
    assert_eq!(report.exit_code(), 2);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(path.join("report.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        kod_host::acceptance::run(&config, &path, Mode::Live)
            .await
            .is_err()
    );
    let link = path.with_file_name("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(
        kod_host::acceptance::run(&config, &link, Mode::Live)
            .await
            .is_err()
    );
}
