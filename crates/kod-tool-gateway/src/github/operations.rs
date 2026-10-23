use super::*;
impl GitHub {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub adapter state poisoned"))
    }
    pub(super) fn now(&self, s: &mut State) -> Result<i64> {
        let now = (self.clock)();
        ensure!(now > 0 && now >= s.last_time, "GitHub clock rollback");
        s.last_time = now;
        Ok(now)
    }
    pub(super) fn authorized<'a>(
        &self,
        s: &'a mut State,
        token: &Secret,
        repo: &str,
    ) -> Result<&'a mut store::Record> {
        ensure!(repo == self.config.repo, "GitHub credential scope mismatch");
        let now = self.now(s)?;
        let run = s
            .tokens
            .get(token.expose())
            .context("GitHub credential unavailable")?
            .clone();
        let r = s
            .records
            .get_mut(&run)
            .context("GitHub credential unavailable")?;
        ensure!(
            r.tokens.iter().any(|t| t.secret == token.expose()
                && t.usable
                && t.expires.saturating_sub(60) > now),
            "GitHub credential unavailable"
        );
        Ok(r)
    }
    pub(super) fn with_run<T>(&self, run: &str, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let gate = self
            .lock()?
            .operations
            .get(run)
            .context("unregistered GitHub run")?
            .clone();
        let _guard = gate
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub run operation poisoned"))?;
        self.lock()?.inflight.insert(run.into());
        let result = work();
        self.lock()?.inflight.remove(run);
        result
    }
    pub(super) fn with_token<T>(
        &self,
        token: &Secret,
        repo: &str,
        work: impl FnOnce(store::Record) -> Result<T>,
    ) -> Result<T> {
        let run = {
            let mut s = self.lock()?;
            self.authorized(&mut s, token, repo)?.permit.run.clone()
        };
        self.with_run(&run, || {
            let record = {
                let mut s = self.lock()?;
                self.authorized(&mut s, token, repo)?.clone()
            };
            work(record)
        })
    }
    pub(super) fn ensure_live(&self, token: &Secret, record: &store::Record) -> Result<()> {
        let mut s = self.lock()?;
        let current = self.authorized(&mut s, token, &record.permit.repo)?;
        ensure!(
            current.generation == record.generation,
            "GitHub operation revoked"
        );
        Ok(())
    }
    pub(super) fn persist_lease(&self, record: &store::Record) -> Result<()> {
        let mut s = self.lock()?;
        let current = s
            .records
            .get(&record.permit.run)
            .context("unregistered GitHub run")?;
        ensure!(
            current.generation == record.generation,
            "GitHub operation revoked"
        );
        self.store.save(record)?;
        s.insert(record.clone())
    }
    pub(super) fn publish_state(&self, record: &store::Record) -> Result<()> {
        let mut s = self.lock()?;
        let mut current = s
            .records
            .get(&record.permit.run)
            .context("unregistered GitHub run")?
            .clone();
        current.input = record.input.clone();
        current.expected = record.expected.clone();
        current.pr = record.pr.clone();
        self.store.save(&current)?;
        s.insert(current)
    }
}
