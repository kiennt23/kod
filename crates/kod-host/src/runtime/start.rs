use super::*;

impl Runtime {
    pub(super) fn start_registered(&self, start: &RunStart) -> Result<()> {
        ensure!(
            !self.cleanup_pending(),
            "credential or container cleanup pending"
        );
        let dispatcher = self
            .dispatcher
            .get()
            .context("dispatcher detached")?
            .clone();
        let slot = self.slot(&start.id);
        let deadline = Instant::now() + crate::deadlines::SETUP_WAIT;
        let mut setup = loop {
            match slot.setup.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    ensure!(Instant::now() < deadline, "run setup pending");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => anyhow::bail!("run setup failed"),
            }
        };
        if let Some(result) = setup.as_ref() {
            return result.clone().map_err(anyhow::Error::msg);
        }
        let stopped = stopped_reader(self.config.paths.dispatcher_db.clone());
        let result = (|| -> Result<()> {
            slot.live(&start.id, &stopped)?;
            let admitted = (self.adapters.resolve)(start)?;
            let b = &admitted.binding;
            ensure!(
                (&b.run, &b.ticket, &b.revision, &b.repo)
                    == (&start.id, &start.ticket, &start.revision, &start.repo),
                "admission identity changed"
            );
            b.validate(&self.config.paths.run_root)?;
            ensure!(
                b.model_token_hash == crate::binding::token_hash(&admitted.model_token)
                    && b.mcp_token_hash == crate::binding::token_hash(&admitted.mcp_token),
                "admission token mismatch"
            );
            let mut c = kod_dispatcher::store::open(&self.config.paths.dispatcher_db)?;
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM run_bindings WHERE run_seq=?1)",
                [kod_dispatcher::store::seq_of(&start.id).context("invalid run")?],
                |r| r.get(0),
            )?;
            ensure!(!exists, "old execution cannot resume");
            kod_dispatcher::store::bind_once(&tx, &start.id, &serde_json::to_value(b)?)?;
            tx.commit()?;
            drop(c);
            slot.live(&start.id, &stopped)?;
            self.adapters.github.register_run(b.permit())?;
            self.barrier("github");
            slot.live(&start.id, &stopped)?;
            self.adapters.model.register(
                &admitted.model_token,
                kod_llm_proxy::config::RunSpec {
                    id: b.run.clone(),
                    ticket: b.ticket.clone(),
                    repo: b.repo.clone(),
                    data_classes: b.data_classes.clone(),
                    budget: kod_llm_proxy::config::Budget {
                        max_tokens: b.max_tokens,
                        max_cost_micro_usd: b.max_cost_micro_usd,
                    },
                },
            )?;
            self.barrier("model");
            slot.live(&start.id, &stopped)?;
            self.adapters.tool.start_run(
                &admitted.mcp_token,
                kod_tool_gateway::Run {
                    id: b.run.clone(),
                    ticket: b.ticket.clone(),
                    repo: b.repo.clone(),
                    base: b.base.clone(),
                    data_classes: b.data_classes.clone(),
                    pins: b.pins.clone(),
                    linked_tickets: b.linked_tickets.clone(),
                    started_at: b.started_at,
                },
            )?;
            self.barrier("tool");
            slot.live(&start.id, &stopped)?;
            let runtime = self.weak.upgrade().context("runtime detached")?;
            let id = start.id.clone();
            let worker_slot = slot.clone();
            let canceled = slot.clone();
            let live: Stopped =
                Arc::new(move |run| Ok(canceled.canceled.load(Ordering::SeqCst) || stopped(run)?));
            let worker = background(move || {
                runtime.barrier("worker");
                let outcome = worker_slot
                    .run_if_live(&id, &live, || {
                        (runtime.adapters.execute)(
                            &admitted,
                            &runtime.adapters.gate,
                            &live,
                            &runtime.adapters.workspace,
                        )
                    })
                    .and_then(|state| {
                        if state == RunState::Done {
                            runtime.adapters.workspace.collected(&id)?;
                        }
                        Ok(state)
                    });
                worker_slot.execution_done.store(true, Ordering::SeqCst);
                let (state, reason) = match outcome {
                    Ok(state) => (state, "connected runner completed".into()),
                    Err(error) => (
                        RunState::NeedsHuman,
                        kod_tool_gateway::scrub(&format!("{error:#}")),
                    ),
                };
                if let Err(error) = notify_finished(&dispatcher, &id, state, &reason) {
                    eprintln!(
                        "connected completion failed: {}",
                        kod_tool_gateway::scrub(&format!("{error:#}"))
                    );
                    if let Err(error) = runtime.revoke_registered(&id) {
                        eprintln!(
                            "cleanup pending: {}",
                            kod_tool_gateway::scrub(&format!("{error:#}"))
                        );
                    }
                }
            })?;
            *slot.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
            Ok(())
        })();
        *setup = Some(
            result
                .as_ref()
                .map(|_| ())
                .map_err(|error| kod_tool_gateway::scrub(&format!("{error:#}"))),
        );
        drop(setup);
        if result.is_err()
            && let Err(error) = self.revoke_registered(&start.id)
        {
            eprintln!(
                "cleanup pending: {}",
                kod_tool_gateway::scrub(&format!("{error:#}"))
            );
        }
        result
    }
}
