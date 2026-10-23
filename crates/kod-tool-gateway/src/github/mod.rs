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
mod preflight;
mod protection;
mod pulls;
mod refs;
mod signing;
mod store;
#[cfg(test)]
mod tests;

pub use config::{AppCredentials, Config, RunPermit};
use config::{identity, sha, valid_ref};
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
#[derive(Default)]
struct State {
    records: BTreeMap<String, store::Record>,
    last_time: i64,
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
        let records = store.load()?;
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
        let last_time = records.values().map(|r| r.updated_at).max().unwrap_or(0);
        Ok(Self {
            config,
            app,
            http: http::Http::new(api)?,
            store,
            clock,
            state: Mutex::new(State { records, last_time }),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub adapter state poisoned"))
    }
    fn now(&self, s: &mut State) -> Result<i64> {
        let now = (self.clock)();
        ensure!(now > 0 && now >= s.last_time, "GitHub clock rollback");
        s.last_time = now;
        Ok(now)
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
            s.records
                .values()
                .all(|r| r.permit.branch() != permit.branch()),
            "GitHub run branch already bound"
        );
        let record = store::Record::new(permit, now);
        self.store.save(&record)?;
        s.records.insert(record.permit.run.clone(), record);
        Ok(())
    }
    fn authorized<'a>(
        &self,
        s: &'a mut State,
        token: &Secret,
        repo: &str,
    ) -> Result<&'a mut store::Record> {
        ensure!(repo == self.config.repo, "GitHub credential scope mismatch");
        let now = self.now(s)?;
        s.records
            .values_mut()
            .find(|r| {
                r.tokens
                    .iter()
                    .any(|t| t.usable && t.expires > now && t.secret == token.expose())
            })
            .context("GitHub credential unavailable")
    }
    fn repo_path(&self, tail: &[&str]) -> Vec<String> {
        let mut p = vec!["repos".into()];
        p.extend(self.config.repo.split('/').map(str::to_owned));
        p.extend(tail.iter().map(|s| (*s).to_owned()));
        p
    }
}
