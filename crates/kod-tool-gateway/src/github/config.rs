use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: String,
    pub repository_id: u64,
    pub installation_id: u64,
    pub base_branch: String,
    pub guard_ruleset_id: u64,
    pub bot_user_id: u64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCredentials {
    pub app_id: u64,
    pub private_key: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPermit {
    pub run: String,
    pub ticket: String,
    pub repo: String,
    pub base: String,
}
impl RunPermit {
    pub(super) fn branch(&self) -> String {
        format!("agent/{}-{}", self.ticket, self.run)
    }
}
pub(super) fn identity(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control)
}
pub(super) fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn valid_ref(branch: &str) -> Result<()> {
    ensure!(identity(branch), "invalid Git ref");
    ensure!(
        std::process::Command::new("git")
            .args(["check-ref-format", &format!("refs/heads/{branch}")])
            .output()
            .context("git unavailable")?
            .status
            .success(),
        "invalid Git ref"
    );
    Ok(())
}
impl Config {
    /// Local validation only; no files, credentials or provider calls.
    pub fn validate(&self) -> Result<()> {
        let parts: Vec<_> = self.repo.split('/').collect();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|s| !s.is_empty()
                    && !matches!(*s, "." | "..")
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
            "invalid GitHub repository"
        );
        ensure!(
            [
                self.repository_id,
                self.installation_id,
                self.guard_ruleset_id,
                self.bot_user_id
            ]
            .iter()
            .all(|n| *n > 0),
            "missing GitHub identity"
        );
        valid_ref(&self.base_branch)
    }
}
