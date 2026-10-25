use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Spend {
    pub requests: u64,
    pub tokens: u64,
    pub cost_micro_usd: u64,
}

/// Reservations remain charged even when the provider response is lost.
pub struct SuiteBudget {
    cap: Spend,
    used: Mutex<Spend>,
    refused: std::sync::atomic::AtomicBool,
}
impl SuiteBudget {
    pub fn new(cap: Spend) -> Result<Self> {
        ensure!(
            cap.requests > 0 && cap.tokens > 0 && cap.cost_micro_usd > 0,
            "invalid suite cap"
        );
        Ok(Self {
            cap,
            used: Mutex::new(Spend::default()),
            refused: Default::default(),
        })
    }
    pub fn reserve(&self, amount: Spend) -> Result<()> {
        ensure!(amount.requests > 0, "invalid reservation");
        let mut used = self
            .used
            .lock()
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))?;
        let result = (|| {
            let add = |a: u64, b: u64, cap: u64| {
                a.checked_add(b)
                    .filter(|v| *v <= cap)
                    .ok_or_else(|| anyhow::anyhow!("suite cap exhausted"))
            };
            let next = Spend {
                requests: add(used.requests, amount.requests, self.cap.requests)?,
                tokens: add(used.tokens, amount.tokens, self.cap.tokens)?,
                cost_micro_usd: add(
                    used.cost_micro_usd,
                    amount.cost_micro_usd,
                    self.cap.cost_micro_usd,
                )?,
            };
            *used = next;
            Ok(())
        })();
        if result.is_err() {
            self.refused
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        result
    }
    /// Replaces a reservation's tokens/cost with confirmed usage; the request itself stays charged.
    /// Usage above the reservation is still charged (never under-reported).
    pub fn settle(&self, reserved: Spend, actual: Spend) -> Result<()> {
        let mut used = self
            .used
            .lock()
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))?;
        used.tokens = used
            .tokens
            .saturating_sub(reserved.tokens)
            .saturating_add(actual.tokens);
        used.cost_micro_usd = used
            .cost_micro_usd
            .saturating_sub(reserved.cost_micro_usd)
            .saturating_add(actual.cost_micro_usd);
        Ok(())
    }
    /// True once any reservation was refused: the suite must stop scheduling work.
    pub fn refused(&self) -> bool {
        self.refused.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn consumed(&self) -> Result<Spend> {
        self.used
            .lock()
            .map(|used| *used)
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))
    }
}

/// Model-proxy reservation sink for acceptance: reserves against the one suite ledger BEFORE the
/// durable per-run reservation (and so before the request is forwarded); refusal fails closed.
pub struct SuiteSink {
    pub budget: std::sync::Arc<SuiteBudget>,
    pub inner: std::sync::Arc<dyn kod_llm_proxy::ReservationSink>,
    reserved: Mutex<std::collections::HashMap<String, Spend>>,
}
impl SuiteSink {
    pub fn new(
        budget: std::sync::Arc<SuiteBudget>,
        inner: std::sync::Arc<dyn kod_llm_proxy::ReservationSink>,
    ) -> Self {
        Self {
            budget,
            inner,
            reserved: Default::default(),
        }
    }
}
fn suite_spend(s: kod_llm_proxy::Spend) -> Spend {
    Spend {
        requests: 0,
        tokens: s.tokens,
        // Round up: never under-charge the suite.
        cost_micro_usd: s.pico_usd.div_ceil(1_000_000),
    }
}
impl kod_llm_proxy::ReservationSink for SuiteSink {
    fn reserve(&self, run: &str, id: &str, spend: kod_llm_proxy::Spend) -> Result<()> {
        let amount = Spend {
            requests: 1,
            ..suite_spend(spend)
        };
        self.budget.reserve(amount)?;
        // A failed durable reservation keeps the suite charge (conservative).
        self.inner.reserve(run, id, spend)?;
        self.reserved
            .lock()
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))?
            .insert(format!("{run}/{id}"), amount);
        Ok(())
    }
    fn settle(&self, run: &str, id: &str, spend: kod_llm_proxy::Spend) -> Result<()> {
        self.inner.settle(run, id, spend)?;
        let reserved = self
            .reserved
            .lock()
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))?
            .remove(&format!("{run}/{id}"))
            .unwrap_or_default();
        self.budget.settle(reserved, suite_spend(spend))
    }
}
