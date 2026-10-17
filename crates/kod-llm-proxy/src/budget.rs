//! Per-run spend in tokens and picodollars (1e-12 USD). A request reserves
//! its worst case before it is forwarded and settles to what the response
//! reported once its body ends, so concurrent requests can't together pass
//! the budget.
use crate::config::{Budget, Price};
use serde_json::{Map, Value};

/// Input tokens reserved on top of the body's length, for what the API adds
/// itself (the tool-use system prompt).
// ponytail: assumes a request has at most one input token per body byte plus
// this slack; a request that breaks it halts its run (see Ledger::settle).
// Ask count_tokens first if that ever happens.
pub const RESERVE_SLACK: u64 = 4096;

/// Tokens and picodollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub tokens: u64,
    pub pico_usd: u64,
}

impl Spend {
    fn add(self, o: Spend) -> Spend {
        Spend {
            tokens: self.tokens.saturating_add(o.tokens),
            pico_usd: self.pico_usd.saturating_add(o.pico_usd),
        }
    }

    fn sub(self, o: Spend) -> Spend {
        Spend {
            tokens: self.tokens.saturating_sub(o.tokens),
            pico_usd: self.pico_usd.saturating_sub(o.pico_usd),
        }
    }

    fn exceeds(self, o: Spend) -> bool {
        self.tokens > o.tokens || self.pico_usd > o.pico_usd
    }
}

/// `tokens` at `rate` micro-USD per million tokens, in picodollars.
fn pico(tokens: u64, rate: u64) -> u64 {
    tokens.saturating_mul(rate)
}

/// Input tokens reserved per image block. An image's tokens follow its pixel
/// size, not its bytes: a small PNG can be large and cost far more than its
/// length.
// ponytail: the largest image the newest models accept costs about 4800
// tokens; raise this if a model takes bigger ones.
pub const IMAGE_TOKENS: u64 = 5000;

/// The most a /v1/messages request with this body can cost: every body byte
/// a token (plus RESERVE_SLACK and IMAGE_TOKENS per image) at the dearest
/// input rate, and `max_tokens` of output.
pub fn reservation(price: &Price, body_len: usize, images: u64, max_tokens: u64) -> Spend {
    let input = (body_len as u64)
        .saturating_add(RESERVE_SLACK)
        .saturating_add(images.saturating_mul(IMAGE_TOKENS));
    let rate = price.input.max(price.cache_write).max(price.cache_read);
    Spend {
        tokens: input.saturating_add(max_tokens),
        pico_usd: pico(input, rate).saturating_add(pico(max_tokens, price.output)),
    }
}

/// What a response's usage (from `Usage::finish`) cost. None when input or
/// output tokens are missing; missing cache fields count as zero.
pub fn cost(price: &Price, usage: &Map<String, Value>) -> Option<Spend> {
    let n = |k: &str| usage.get(k).and_then(Value::as_u64);
    let (input, output) = (n("input_tokens")?, n("output_tokens")?);
    let write = n("cache_creation_input_tokens").unwrap_or(0);
    let read = n("cache_read_input_tokens").unwrap_or(0);
    Some(Spend {
        tokens: input
            .saturating_add(output)
            .saturating_add(write)
            .saturating_add(read),
        pico_usd: pico(input, price.input)
            .saturating_add(pico(output, price.output))
            .saturating_add(pico(write, price.cache_write))
            .saturating_add(pico(read, price.cache_read)),
    })
}

/// One run's spend. `spent + reserved` never passes the budget.
#[derive(Debug)]
pub struct Ledger {
    limit: Spend,
    spent: Spend,
    reserved: Spend,
    halted: bool,
}

impl Ledger {
    pub fn new(b: Budget) -> Ledger {
        Ledger {
            limit: Spend {
                tokens: b.max_tokens,
                pico_usd: b.max_cost_micro_usd.saturating_mul(1_000_000),
            },
            spent: Spend::default(),
            reserved: Spend::default(),
            halted: false,
        }
    }

    /// Holds `r` for one request. False, holding nothing, when the run is
    /// halted or `r` would take it past its budget.
    pub fn reserve(&mut self, r: Spend) -> bool {
        if self.halted || self.spent.add(self.reserved).add(r).exceeds(self.limit) {
            return false;
        }
        self.reserved = self.reserved.add(r);
        true
    }

    /// Gives back a reservation for a request that was never answered.
    pub fn release(&mut self, r: Spend) {
        self.reserved = self.reserved.sub(r);
    }

    /// Replaces reservation `r` with what the response cost, or with `r`
    /// itself when the cost is unknown. A cost above `r` is still charged and
    /// halts the run. Returns false when the run is halted, whether by this
    /// call or before it.
    pub fn settle(&mut self, r: Spend, actual: Option<Spend>) -> bool {
        self.release(r);
        let charged = actual.unwrap_or(r);
        self.spent = self.spent.add(charged);
        if charged.exceeds(r) {
            self.halted = true;
        }
        !self.halted
    }

    /// Stops the run: every later request is refused.
    pub fn halt(&mut self) {
        self.halted = true;
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    pub fn spent(&self) -> Spend {
        self.spent
    }
}
