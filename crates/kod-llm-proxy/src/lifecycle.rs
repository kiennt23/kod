use crate::{
    Run,
    budget::{Ledger, Spend},
    config::RunSpec,
    hex,
};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex, atomic::AtomicU64},
};
use tokio::sync::{Semaphore, watch};

pub const MODEL_CANCEL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// The host must durably reserve before returning success. Settlement replaces
/// that charge only when provider usage is confirmed; errors retain it.
pub trait ReservationSink: Send + Sync {
    fn reserve(&self, run_id: &str, request_id: &str, spend: Spend) -> Result<()>;
    fn settle(&self, run_id: &str, request_id: &str, spend: Spend) -> Result<()>;
}

#[derive(Default)]
struct Entries {
    runs: HashMap<String, Arc<Run>>,
    revoked: HashSet<String>,
    sink: Option<Arc<dyn ReservationSink>>,
}

#[derive(Clone)]
pub struct RunRegistry {
    entries: Arc<Mutex<Entries>>,
    max_concurrent: usize,
}

impl RunRegistry {
    pub(crate) fn new(max_concurrent: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Entries::default())),
            max_concurrent,
        }
    }

    pub fn set_reservation_sink(&self, sink: Arc<dyn ReservationSink>) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("registry poisoned"))?;
        if entries.sink.is_some() || !entries.runs.is_empty() {
            bail!("install sink once before registration");
        }
        entries.sink = Some(sink);
        Ok(())
    }

    pub fn register(&self, token: &str, spec: RunSpec) -> Result<()> {
        if token.len() < crate::config::MIN_TOKEN_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!("invalid run token");
        }
        if [&spec.id, &spec.ticket, &spec.repo]
            .iter()
            .any(|s| s.is_empty() || s.trim() != s.as_str())
            || spec
                .data_classes
                .iter()
                .any(|s| s.is_empty() || s.trim() != s)
            || spec.budget.max_cost_micro_usd > crate::config::MAX_COST_MICRO_USD
        {
            bail!("invalid run specification");
        }
        let key = hex(Sha256::digest(token));
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("registry poisoned"))?;
        if entries.revoked.contains(&spec.id) {
            bail!("run revoked");
        }
        if let Some(run) = entries.runs.get(&key) {
            if run.spec != spec {
                bail!("token binding changed");
            }
            return Ok(());
        }
        if entries.runs.values().any(|run| run.spec.id == spec.id) {
            bail!("run already bound to another token");
        }
        let (cancel, _) = watch::channel(false);
        let run = Arc::new(Run {
            ledger: Mutex::new(Ledger::new(spec.budget)),
            spec,
            requests: AtomicU64::new(0),
            in_flight: Arc::new(Semaphore::new(self.max_concurrent)),
            cancel,
            sink: entries.sink.clone(),
        });
        entries.runs.insert(key, run);
        Ok(())
    }

    pub fn revoke(&self, id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("registry poisoned"))?;
        entries.revoked.insert(id.to_owned());
        for run in entries.runs.values().filter(|run| run.spec.id == id) {
            run.cancel.send_replace(true);
        }
        Ok(())
    }

    pub fn revoke_all(&self) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("registry poisoned"))?;
        let ids: Vec<_> = entries
            .runs
            .values()
            .map(|run| {
                run.cancel.send_replace(true);
                run.spec.id.clone()
            })
            .collect();
        entries.revoked.extend(ids);
        Ok(())
    }

    pub(crate) fn lookup(&self, token: &str) -> Option<Arc<Run>> {
        let entries = self.entries.lock().ok()?;
        let run = entries.runs.get(&hex(Sha256::digest(token)))?;
        if *run.cancel.borrow() {
            return None;
        }
        Some(run.clone())
    }
}

pub(crate) async fn until_revoked<T>(
    mut canceled: watch::Receiver<bool>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    if *canceled.borrow() {
        bail!("run revoked");
    }
    tokio::select! { biased; _ = canceled.changed() => bail!("run revoked"), result = work => result }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_model_cancellation_deadline_is_fixed() {
        assert_eq!(MODEL_CANCEL_DEADLINE.as_millis(), 2_000);
    }

    #[tokio::test(start_paused = true)]
    async fn silent_work_is_canceled() {
        let (cancel, receiver) = watch::channel(false);
        let task = tokio::spawn(until_revoked(
            receiver,
            std::future::pending::<Result<()>>(),
        ));
        tokio::task::yield_now().await;
        cancel.send_replace(true);
        assert!(
            tokio::time::timeout(MODEL_CANCEL_DEADLINE, task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
