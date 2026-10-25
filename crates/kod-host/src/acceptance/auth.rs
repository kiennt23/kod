use super::Spend;
use crate::config::{FsConfig, read_local, read_private, strict_json};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceConfig {
    pub disposable_repo: String,
    pub authorization_ref: PathBuf,
    pub expires_at: DateTime<Utc>,
    pub max_requests: u64,
    pub max_tokens: u64,
    pub max_cost_micro_usd: u64,
    pub scenario_deadline_secs: u64,
}
impl AcceptanceConfig {
    pub fn caps(&self) -> Spend {
        Spend {
            requests: self.max_requests,
            tokens: self.max_tokens,
            cost_micro_usd: self.max_cost_micro_usd,
        }
    }
    pub fn validate(&self, config: &FsConfig) -> Result<()> {
        ensure!(
            self.disposable_repo == config.github.product.repo,
            "acceptance repository mismatch"
        );
        ensure!(self.expires_at > Utc::now(), "acceptance expired");
        ensure!(
            self.scenario_deadline_secs > 0 && self.scenario_deadline_secs <= 86400,
            "invalid scenario deadline"
        );
        ensure!(
            self.max_requests > 0
                && self.max_requests <= config.model.max_requests
                && self.max_tokens > 0
                && self.max_tokens <= config.model.run_max_tokens
                && self.max_cost_micro_usd > 0
                && self.max_cost_micro_usd <= config.model.run_max_cost_micro_usd,
            "invalid acceptance caps"
        );
        ensure!(
            self.authorization_ref.is_absolute(),
            "authorization reference must be absolute"
        );
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authorization {
    authorized_by: String,
    recorded_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    repo: String,
    endpoint: String,
    model: String,
    max_requests: u64,
    max_tokens: u64,
    max_cost_micro_usd: u64,
    actions: Vec<String>,
}
pub fn authorize(config: &FsConfig) -> Result<()> {
    let acceptance = config
        .acceptance
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("authorization missing"))?;
    acceptance.validate(config)?;
    let auth: Authorization = strict_json(&read_private(&acceptance.authorization_ref)?)?;
    let dispatcher = read_local(&config.dispatcher_config)?;
    let dispatcher = kod_dispatcher::Config::parse(std::str::from_utf8(&dispatcher)?)?;
    ensure!(
        auth.authorized_by == dispatcher.owner
            || dispatcher.backup.as_ref() == Some(&auth.authorized_by),
        "unauthorized identity"
    );
    let now = Utc::now();
    ensure!(
        auth.recorded_at <= now
            && auth.recorded_at < auth.expires_at
            && auth.expires_at >= acceptance.expires_at
            && auth.expires_at > now,
        "authorization expired"
    );
    ensure!(
        auth.repo == acceptance.disposable_repo
            && auth.endpoint == config.model.endpoint_name
            && auth.model == config.model.selected_model,
        "authorization target mismatch"
    );
    ensure!(
        auth.max_requests == acceptance.max_requests
            && auth.max_tokens == acceptance.max_tokens
            && auth.max_cost_micro_usd == acceptance.max_cost_micro_usd,
        "authorization caps mismatch"
    );
    for action in [
        "issue", "draft_pr", "checks", "comment", "cancel", "restart", "kill", "cleanup",
    ] {
        ensure!(
            auth.actions.iter().any(|a| a == action),
            "authorization action missing"
        );
    }
    Ok(())
}
