use anyhow::{Context, Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
mod files;
mod validate;
pub use files::{private_file, read_local, read_private};
pub fn strict_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let text = std::str::from_utf8(bytes).context("configuration must be UTF-8")?;
    let value = kod_tracker::parse_json_unique(text)
        .map_err(|_| anyhow::anyhow!("invalid or duplicate JSON field"))?;
    serde_json::from_value(value).map_err(|error| {
        let message = error.to_string();
        if message.starts_with("missing field") || message.starts_with("unknown field") {
            anyhow::anyhow!(message)
        } else {
            anyhow::anyhow!("invalid configuration field type")
        }
    })
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsPaths {
    pub dispatcher_db: PathBuf,
    pub audit_db: PathBuf,
    pub policy_dir: PathBuf,
    pub run_root: PathBuf,
    pub mirror: PathBuf,
    pub approval_manifest: PathBuf,
    pub gates_config: PathBuf,
    pub credential_dir: PathBuf,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsGithub {
    pub product: kod_tool_gateway::github::Config,
    pub app: kod_tool_gateway::github::AppCredentials,
    pub tracker: kod_tracker::github::Config,
    pub tracker_repository_id: u64,
    pub tracker_installation_id: u64,
    pub mapping: PathBuf,
    pub webhook_secret_file: PathBuf,
    pub owner_key_file: PathBuf,
    #[serde(deserialize_with = "required_nullable")]
    pub backup_key_file: Option<PathBuf>,
    pub agent_name: String,
    pub agent_email: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsModel {
    pub endpoint_name: String,
    pub upstream: String,
    pub api_key_file: PathBuf,
    pub models: Vec<String>,
    pub selected_model: String,
    pub prices: PathBuf,
    pub allowed_paths: String,
    pub max_tokens: u64,
    pub max_body: usize,
    pub max_requests: u64,
    pub max_concurrent: usize,
    pub run_max_tokens: u64,
    pub run_max_cost_micro_usd: u64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsNetworking {
    pub ingress_listen: std::net::SocketAddr,
    pub gateway_listen: std::net::SocketAddr,
    pub accepted_hosts: Vec<String>,
    pub sandbox_network: String,
    pub sandbox_gateway_url: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsSandbox {
    pub agent_image: String,
    pub collector_image: String,
    pub language: String,
    pub memory: String,
    pub cpus: f64,
    pub disk: String,
    pub max_turns: u32,
    pub wall_seconds: u64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsConfig {
    pub paths: FsPaths,
    pub github: FsGithub,
    pub model: FsModel,
    pub networking: FsNetworking,
    pub sandbox: FsSandbox,
    pub dispatcher_config: PathBuf,
}
fn required_nullable<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

impl FsConfig {
    /// Parse only. Callers must validate (`prepare` validates paths first, then the rest under the lock).
    pub fn parse(path: &Path) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        strict_json(&read_local(&absolute)?)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let config: Self = strict_json(&read_local(&absolute)?)?;
        config.validate_local()?;
        Ok(config)
    }
}
