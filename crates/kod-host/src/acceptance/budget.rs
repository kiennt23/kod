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
        })
    }
    pub fn reserve(&self, amount: Spend) -> Result<()> {
        ensure!(
            amount.requests > 0 && amount.tokens > 0 && amount.cost_micro_usd > 0,
            "invalid reservation"
        );
        let mut used = self
            .used
            .lock()
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))?;
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
    }
    pub fn consumed(&self) -> Result<Spend> {
        self.used
            .lock()
            .map(|used| *used)
            .map_err(|_| anyhow::anyhow!("suite ledger unavailable"))
    }
}
