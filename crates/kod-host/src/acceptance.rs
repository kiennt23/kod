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
pub use budget::{Spend, SuiteBudget};
pub use report::{
    DOCKER_SCENARIOS, Execution, LIVE_SCENARIOS, Mode, Observation, ObservationCode, Outcome,
    Report, Scenario,
};
use std::path::Path;

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
            if docker::preflight(config).is_err() {
                report.blocked.push("docker_prerequisites".into());
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
    if report.blocked.is_empty() {
        match mode {
            Mode::Docker =>
            {
                #[cfg(feature = "connected-test-support")]
                match docker::run(config, &evidence_path).await {
                    Ok(execution) => {
                        report.scenarios = execution.scenarios;
                        report.external_writes = execution.external_writes;
                        report.model_requests = execution.spend.requests;
                        report.actual_spend = execution.spend;
                    }
                    Err(_) => {
                        if let Some(first) = report.scenarios.first_mut() {
                            first.outcome = Outcome::Fail;
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
    evidence::write(&evidence_path, &report.json()?)?;
    Ok(report)
}
