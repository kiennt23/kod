use super::*;
use crate::broker::Broker;
impl GitHub {
    pub fn recover(&self) -> Result<()> {
        self.revoke_all()
    }
    pub fn verify_repository(&self) -> Result<()> {
        self.register_run(RunPermit {
            run: "activation-probe".into(),
            ticket: "activation".into(),
            repo: self.config.repo.clone(),
            base: "0".repeat(40),
        })?;
        self.mint("activation-probe", &self.config.repo)?;
        self.revoke("activation-probe")
    }
}
impl Broker for GitHub {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        ensure!(repo == self.config.repo, "GitHub repository mismatch");
        self.with_run(run, || {
            let (record, now) = {
                let mut s = self.lock()?;
                let now = self.now(&mut s)?;
                let r = s.records.get(run).context("unregistered GitHub run")?;
                ensure!(
                    r.pending.is_none() && r.tokens.iter().all(|t| t.usable),
                    "GitHub run credential cleanup pending"
                );
                (r.clone(), now)
            };
            self.issue_record(record, now)
        })
    }
    fn revoke(&self, run: &str) -> Result<()> {
        self.revoke_runs(vec![run.to_owned()])
    }
    fn revoke_all(&self) -> Result<()> {
        let ids = {
            let s = self.lock()?;
            s.cleanup.union(&s.inflight).cloned().collect()
        };
        self.revoke_runs(ids)
    }
}
