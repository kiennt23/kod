//! Explicit connected acceptance and private, allowlisted evidence.
mod auth;
mod budget;
#[cfg(feature = "connected-test-support")]
pub mod docker;
mod evidence;
mod report;
use crate::config::FsConfig;
use anyhow::Result;
pub use auth::{AcceptanceConfig, authorize};
pub use budget::{Spend, SuiteBudget, SuiteSink};
pub use report::{
    DOCKER_SCENARIOS, Execution, LIVE_SCENARIOS, Mode, Observation, ObservationCode, Outcome,
    Report, Scenario,
};
use sha2::Digest;
use std::{path::Path, sync::Arc};

/// Docker fixtures spend nothing real; the cap only has to cover all 15 scenarios with margin.
const DOCKER_DEFAULT_CAPS: Spend = Spend {
    requests: 1000,
    tokens: 100_000_000,
    cost_micro_usd: 100_000_000,
};

pub fn preflight_report(mode: Mode, config: &FsConfig) -> Report {
    let mut report = Report::new(mode);
    match mode {
        Mode::Live => {
            if authorize(config).is_err() {
                report.blocked.push("authorization".into());
            }
            if config.validate_local().is_err() {
                report.blocked.push("deployment_prerequisites".into());
            }
            // The deployed live drill driver needs owner-supplied gate, reviewer and queue inputs.
            report
                .blocked
                .push("live_driver_and_gate_prerequisites".into());
        }
        Mode::Docker => {
            if !cfg!(feature = "connected-test-support") {
                report
                    .blocked
                    .push("connected_test_support_disabled".into());
            }
            #[cfg(feature = "connected-test-support")]
            if let Err(e) = docker::preflight(config) {
                report.blocked.push("docker_prerequisites".into());
                // Only the fixed code: the underlying error may echo supplied values.
                report
                    .blocked_reasons
                    .push(format!("docker_prerequisites: {}", e.0));
                eprintln!("acceptance blocked: docker_prerequisites: {}", e.0);
            }
        }
    }
    if let Some(acceptance) = &config.acceptance {
        report.configured_spend = acceptance.caps();
    }
    report
}

pub async fn run(config: &FsConfig, evidence_path: &Path, mode: Mode) -> Result<Report> {
    let evidence_path = evidence::create(evidence_path)?;
    let mut report = preflight_report(mode, config);
    // One suite-wide ledger for every model request of this run; docker fixtures get a generous default.
    let suite = Arc::new(SuiteBudget::new(match &config.acceptance {
        Some(a) => a.caps(),
        None => DOCKER_DEFAULT_CAPS,
    })?);
    if report.blocked.is_empty() {
        match mode {
            Mode::Docker => {
                #[cfg(feature = "connected-test-support")]
                {
                    let (execution, result) =
                        docker::run(config, &evidence_path, suite.clone()).await;
                    report.scenarios = execution.scenarios;
                    report.external_writes = execution.external_writes;
                    if let Err(e) = result {
                        // Fixed text only: error chains can echo supplied values.
                        let stage = e.downcast_ref::<docker::Stage>().map(|s| s.0);
                        eprintln!(
                            "acceptance execution failed{}",
                            stage.map_or(String::new(), |s| format!(" in scenario {s}"))
                        );
                        if let Some(t) = e.downcast_ref::<docker::SuiteTimeout>() {
                            eprintln!("{t}");
                        }
                        // Blame the running scenario, else the first unfinished one: a failure
                        // is never reported as merely blocked.
                        let at = stage
                            .and_then(|n| report.scenarios.iter().position(|s| s.name == n))
                            .or_else(|| {
                                report
                                    .scenarios
                                    .iter()
                                    .position(|s| s.outcome == Outcome::Blocked)
                            });
                        if let Some(i) = at {
                            report.scenarios[i].outcome = Outcome::Fail;
                        }
                        report.blocked.push("scenario_execution_failed".into());
                    }
                }
            }
            Mode::Live => {
                report
                    .blocked
                    .push("live_driver_and_gate_prerequisites".into());
            }
        }
    }
    // The real ledger, also when execution errored or was stopped by the cap.
    report.actual_spend = suite.consumed()?;
    report.model_requests = report.actual_spend.requests;
    if suite.refused() {
        report.blocked.push("suite_budget_exhausted".into());
    }
    #[cfg(feature = "connected-test-support")]
    if mode == Mode::Docker {
        (report.deployed_commit, report.image_digests) = docker::provenance(config);
    }
    if let Some(a) = &config.acceptance {
        report.repository = Some(a.disposable_repo.clone());
        report.authorization_sha256 = std::fs::read(&a.authorization_ref)
            .ok()
            .map(|b| format!("{:x}", sha2::Sha256::digest(b)));
    } else if mode == Mode::Docker {
        report.repository = Some(config.github.product.repo.clone());
    }
    evidence::write(&evidence_path, &report.json()?)?;
    Ok(report)
}
