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
    acceptance::{
        AcceptanceConfig, Observation, Spend, SuiteBudget, SuiteSink, authorize, preflight_report,
    },
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

#[test]
fn valid_authorization_clears_only_the_authorization_blocker() {
    // Given an exact owner record, the specific blocker is what disappears (not just the exit code).
    let (_dir, config, auth) = authorized();
    write_authorization(&config, &auth);
    let report = preflight_report(Mode::Live, &config);
    assert!(!report.blocked.iter().any(|s| s == "authorization"));
    assert!(
        report
            .blocked
            .iter()
            .any(|s| s == "live_driver_and_gate_prerequisites")
    );
    assert_eq!(report.exit_code(), 2);
    // And an invalid record puts exactly that blocker back.
    let mut bad = auth;
    bad["repo"] = serde_json::json!("acme/other");
    write_authorization(&config, &bad);
    assert!(
        preflight_report(Mode::Live, &config)
            .blocked
            .iter()
            .any(|s| s == "authorization")
    );
}
#[test]
fn acceptance_caps_must_be_positive_and_within_model_limits() {
    // Model limits in the fixture are 10 requests / 1000 tokens / 1000 micro-USD.
    for (field, value) in [
        ("max_requests", 0),
        ("max_tokens", 0),
        ("max_cost_micro_usd", 0),
        ("max_requests", 11),
        ("max_tokens", 1001),
        ("max_cost_micro_usd", 1001),
    ] {
        let (_dir, config, _) = authorized();
        let mut acceptance = config.acceptance.clone().unwrap();
        match field {
            "max_requests" => acceptance.max_requests = value,
            "max_tokens" => acceptance.max_tokens = value,
            _ => acceptance.max_cost_micro_usd = value,
        }
        let error = acceptance.validate(&config).unwrap_err().to_string();
        assert_eq!(error, "invalid acceptance caps", "{field}={value}");
    }
    let (_dir, config, _) = authorized();
    assert!(
        config
            .acceptance
            .as_ref()
            .unwrap()
            .validate(&config)
            .is_ok()
    );
}
#[derive(Default)]
struct CountingSink(std::sync::atomic::AtomicU64);
impl kod_llm_proxy::ReservationSink for CountingSink {
    fn reserve(&self, _: &str, _: &str, _: kod_llm_proxy::Spend) -> anyhow::Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn settle(&self, _: &str, _: &str, _: kod_llm_proxy::Spend) -> anyhow::Result<()> {
        Ok(())
    }
}
#[test]
fn suite_sink_refuses_before_the_durable_reservation_once_the_cap_is_used() {
    use kod_llm_proxy::ReservationSink;
    let cap = Spend {
        requests: 1,
        tokens: 100,
        cost_micro_usd: 100,
    };
    let budget = Arc::new(SuiteBudget::new(cap).unwrap());
    let inner = Arc::new(CountingSink::default());
    let sink = SuiteSink::new(budget.clone(), inner.clone());
    let reserved = kod_llm_proxy::Spend {
        tokens: 40,
        pico_usd: 1_500_000,
    };
    sink.reserve("run", "a", reserved).unwrap();
    // The cost rounds up to whole micro-USD.
    assert_eq!(
        budget.consumed().unwrap(),
        Spend {
            requests: 1,
            tokens: 40,
            cost_micro_usd: 2
        }
    );
    assert!(!budget.refused());
    // The second request is refused and never reaches the next sink (so is never forwarded).
    assert!(sink.reserve("run", "b", reserved).is_err());
    assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(budget.refused());
    // Settlement replaces the reservation with confirmed usage.
    sink.settle(
        "run",
        "a",
        kod_llm_proxy::Spend {
            tokens: 10,
            pico_usd: 500_000,
        },
    )
    .unwrap();
    assert_eq!(budget.consumed().unwrap().tokens, 10);
}
