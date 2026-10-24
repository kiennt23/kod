use super::*;

pub(super) fn poll_cleanup(
    cell: &Mutex<Option<JoinHandle<Result<()>>>>,
    work: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<()> {
    let mut slot = cell
        .lock()
        .map_err(|_| anyhow::anyhow!("cleanup lock poisoned"))?;
    if slot.as_ref().is_some_and(|j| j.is_finished()) {
        return slot
            .take()
            .context("cleanup worker disappeared")?
            .join()
            .map_err(|_| anyhow::anyhow!("credential cleanup panicked"))?;
    }
    if slot.is_none() {
        *slot = Some(
            std::thread::Builder::new()
                .name("kod-revoke".into())
                .spawn(work)?,
        );
    }
    anyhow::bail!("credential cleanup pending")
}

fn combined(results: impl IntoIterator<Item = Result<()>>) -> Result<()> {
    let errors: Vec<_> = results
        .into_iter()
        .filter_map(Result::err)
        .map(|e| kod_tool_gateway::scrub(&format!("{e:#}")))
        .collect();
    ensure!(errors.is_empty(), "cleanup pending: {}", errors.join("; "));
    Ok(())
}

impl Runtime {
    pub(super) fn revoke_registered(&self, run: &str) -> Result<()> {
        let slot = self.slot(run);
        slot.cancel();
        let local = self.adapters.model.revoke(run);
        self.adapters.tool.retire_run_access(run);
        let bound = self.bound(run);
        if matches!(bound, Ok(false)) {
            // Nothing remote or containerized was ever created for this run.
            slot.cleanup_confirmed.store(true, Ordering::Release);
            slot.stop_confirmed.store(true, Ordering::Release);
            self.clear_confirmed_debt();
            return local;
        }
        self.cleanup_debt.store(true, Ordering::Release);
        let remote = if slot.cleanup_confirmed.load(Ordering::Acquire) {
            Ok(())
        } else {
            let github = self.adapters.github.clone();
            let id = run.to_owned();
            poll_cleanup(&slot.cleanup, move || github.revoke(&id))
                .inspect(|()| slot.cleanup_confirmed.store(true, Ordering::Release))
        };
        let result = combined([local, bound.map(|_| ()), remote]);
        if result.is_ok() {
            self.clear_confirmed_debt();
        }
        result
    }

    pub fn revoke_all(&self) -> Result<()> {
        let github = self.adapters.github.clone();
        self.revoke_all_with(move || github.revoke_all())
    }

    /// `revoke_all` with the remote step supplied, so tests can fail it.
    #[doc(hidden)]
    pub fn revoke_all_with(
        &self,
        remote: impl FnOnce() -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.cleanup_debt.store(true, Ordering::Release);
        for slot in self
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            slot.cancel();
        }
        let local = self.adapters.model.revoke_all();
        self.adapters.tool.retire_all_access();
        self.global_confirmed.store(false, Ordering::Release);
        let remote = poll_cleanup(&self.global_cleanup, remote).inspect(|()| {
            self.global_confirmed.store(true, Ordering::Release);
            self.clear_confirmed_debt();
        });
        combined([local, remote])
    }

    pub(super) fn stop_registered(&self, run: &str) -> Result<()> {
        let slot = self.slot(run);
        slot.cancel();
        if !self.bound(run)? {
            slot.cleanup_confirmed.store(true, Ordering::Release);
            slot.stop_confirmed.store(true, Ordering::Release);
            self.clear_confirmed_debt();
            return Ok(());
        }
        self.cleanup_debt.store(true, Ordering::Release);
        self.binding(run)?;
        let began = slot
            .cancel_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .context("missing cancellation time")?;
        let remaining = crate::deadlines::ABSENCE.saturating_sub(began.elapsed());
        let grace = crate::deadlines::TERM_GRACE.saturating_sub(began.elapsed());
        kod_eval::connected::stop_owned(
            run,
            &kod_eval::connected::names(run)?,
            grace,
            remaining.max(Duration::from_millis(100)),
        )?;
        if !slot.execution_done.load(Ordering::Acquire) {
            let deadline = began + crate::deadlines::ABSENCE;
            loop {
                let finished = slot
                    .worker
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_none_or(|worker| worker.is_finished());
                if finished {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "container launch worker still pending"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            if let Some(worker) = slot.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("connected worker panicked"))?;
            }
            kod_eval::connected::stop_owned(
                run,
                &kod_eval::connected::names(run)?,
                Duration::ZERO,
                Duration::from_millis(100),
            )?;
        }
        slot.stop_confirmed.store(true, Ordering::Release);
        self.clear_confirmed_debt();
        Ok(())
    }

    fn clear_confirmed_debt(&self) {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let clean = slots.values().all(|s| {
            !s.canceled.load(Ordering::Acquire)
                || (s.cleanup_confirmed.load(Ordering::Acquire)
                    && s.stop_confirmed.load(Ordering::Acquire))
        });
        if clean
            && self.global_confirmed.load(Ordering::Acquire)
            && self
                .global_cleanup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            self.cleanup_debt.store(false, Ordering::Release);
        }
    }

    fn bound_ids(&self) -> Result<Vec<String>> {
        let c = rusqlite::Connection::open_with_flags(
            &self.config.paths.dispatcher_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement = c.prepare("SELECT run_seq FROM run_bindings ORDER BY run_seq")?;
        Ok(statement
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(kod_dispatcher::run_id)
            .collect())
    }

    pub(super) fn stop_everything(&self) -> Result<()> {
        let mut results = vec![self.revoke_all()];
        let mut ids = self.bound_ids()?;
        ids.extend(
            self.slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned(),
        );
        ids.sort();
        ids.dedup();
        for id in ids {
            results.push(self.revoke_registered(&id));
            results.push(self.stop_registered(&id));
        }
        combined(results)
    }

    pub fn recover_local(&self) -> Result<()> {
        let mut results = vec![self.revoke_all(), self.adapters.github.recover()];
        for id in self.bound_ids()? {
            results.push(self.adapters.model.revoke(&id));
            results.push(self.revoke_registered(&id));
            results.push(self.stop_registered(&id));
        }
        let result = combined(results);
        if result.is_ok() {
            self.clear_confirmed_debt();
        }
        result
    }
}
