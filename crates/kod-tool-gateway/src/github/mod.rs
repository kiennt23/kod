//! Host-only GitHub App credentials and publication. Construct locally, recover
//! outstanding leases, then verify repository protection before admitting work.
use crate::broker::Secret;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

mod auth;
mod commit;
mod config;
mod forge;
mod http;
mod issuance;
mod operations;
mod pr_errors;
mod preflight;
mod protection;
mod pulls;
mod refs;
mod revocation;
mod signing;
mod state;
mod store;
use state::State;
#[cfg(test)]
mod tests;

pub use config::{AppCredentials, Config, RunPermit};
use config::{identity, sha, valid_ref};
const PRUNE_AFTER: i64 = 7 * 24 * 3600;
pub(super) type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
#[cfg(any(test, feature = "test-support"))]
pub type TestClock = Clock;

pub struct GitHub {
    config: Config,
    app: AppCredentials,
    http: http::Http,
    store: store::Store,
    clock: Clock,
    state: Mutex<State>,
}
impl GitHub {
    pub fn new(config: Config, app: AppCredentials, leases: impl AsRef<Path>) -> Result<Self> {
        Self::build(
            config,
            app,
            leases.as_ref(),
            "https://api.github.com",
            Arc::new(|| chrono::Utc::now().timestamp()),
        )
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_api(
        config: Config,
        app: AppCredentials,
        leases: impl AsRef<Path>,
        api: &str,
        clock: TestClock,
    ) -> Result<Self> {
        let u = reqwest::Url::parse(api).context("invalid test API")?;
        let ip = u
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok());
        ensure!(
            u.scheme() == "http"
                && ip.is_some_and(|ip| ip.is_loopback())
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.path() == "/"
                && u.query().is_none()
                && u.fragment().is_none(),
            "test API must be literal loopback"
        );
        Self::build(config, app, leases.as_ref(), api, clock)
    }
    fn build(
        config: Config,
        app: AppCredentials,
        leases: &Path,
        api: &str,
        clock: Clock,
    ) -> Result<Self> {
        config.validate()?;
        ensure!(app.app_id > 0, "missing App identity");
        signing::validate(&app.private_key)?;
        let store = store::Store::open(leases)?;
        let mut records = store.load()?;
        let last_time = records.values().map(|r| r.updated_at).max().unwrap_or(0);
        // Settled runs (no token, no issuance in doubt, no PR awaiting review) are pruned
        // so the kill switch does not scan every historical run.
        // ponytail: startup-only pruning; add a periodic sweep if a gateway lives for weeks.
        let cutoff = clock().saturating_sub(PRUNE_AFTER);
        let settled: Vec<String> = records
            .values()
            .filter(|r| {
                r.tokens.is_empty()
                    && r.pending.is_none()
                    && r.pr.as_ref().is_none_or(|pr| !pr.blocked)
                    && r.updated_at < cutoff
            })
            .map(|r| r.permit.run.clone())
            .collect();
        for run in settled {
            store.remove(&run)?;
            records.remove(&run);
        }
        for r in records.values() {
            ensure!(
                identity(&r.permit.run)
                    && identity(&r.permit.ticket)
                    && r.permit.repo == config.repo
                    && sha(&r.permit.base),
                "invalid persisted run permit"
            );
            valid_ref(&r.permit.branch())?;
            ensure!(
                r.permit.branch() != config.base_branch && r.expected.as_deref().is_none_or(sha),
                "invalid persisted publication"
            );
            if let Some(pr) = &r.pr {
                ensure!(
                    pr.branch == r.permit.branch()
                        && pr.base == config.base_branch
                        && r.expected.as_deref() == Some(&pr.sha),
                    "invalid persisted PR intent"
                );
            }
        }
        Ok(Self {
            config,
            app,
            http: http::Http::new(api)?,
            store,
            clock,
            state: Mutex::new(State::new(records, last_time)?),
        })
    }
    pub fn register_run(&self, permit: RunPermit) -> Result<()> {
        ensure!(
            identity(&permit.run)
                && identity(&permit.ticket)
                && permit.repo == self.config.repo
                && sha(&permit.base),
            "invalid run permit"
        );
        valid_ref(&permit.branch())?;
        ensure!(
            permit.branch() != self.config.base_branch,
            "default branch is not a run branch"
        );
        let mut s = self.lock()?;
        let now = self.now(&mut s)?;
        if let Some(old) = s.records.get(&permit.run) {
            ensure!(old.permit == permit, "run binding changed");
            return Ok(());
        }
        ensure!(
            !s.branches.contains_key(&permit.branch()),
            "GitHub run branch already bound"
        );
        let record = store::Record::new(permit, now);
        self.store.save(&record)?;
        s.insert(record)
    }
    fn repo_path(&self, tail: &[&str]) -> Vec<String> {
        let mut p = vec!["repos".into()];
        p.extend(self.config.repo.split('/').map(str::to_owned));
        p.extend(tail.iter().map(|s| (*s).to_owned()));
        p
    }
}
