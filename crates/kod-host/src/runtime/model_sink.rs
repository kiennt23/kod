use anyhow::{Context, Result};
use kod_dispatcher::store::{self, ModelCharge, ModelRequest};
use kod_llm_proxy::{ReservationSink, Spend};
use std::path::PathBuf;

pub struct DurableSink(pub PathBuf);

impl ReservationSink for DurableSink {
    fn reserve(&self, run: &str, id: &str, spend: Spend) -> Result<()> {
        let c = store::open(&self.0)?;
        store::reserve_model(
            &c,
            &ModelRequest {
                run,
                id,
                reservation: ModelCharge {
                    tokens: spend.tokens,
                    pico_usd: spend.pico_usd,
                },
            },
        )
    }
    fn settle(&self, run: &str, id: &str, spend: Spend) -> Result<()> {
        let c = store::open(&self.0)?;
        let seq = store::seq_of(run).context("invalid run")?;
        let (tokens,cost):(String,String)=c.query_row("SELECT reserved_tokens,reserved_cost FROM model_requests WHERE run_seq=?1 AND request_id=?2",rusqlite::params![seq,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        store::settle_model(
            &c,
            &ModelRequest {
                run,
                id,
                reservation: ModelCharge {
                    tokens: tokens.parse()?,
                    pico_usd: cost.parse()?,
                },
            },
            ModelCharge {
                tokens: spend.tokens,
                pico_usd: spend.pico_usd,
            },
        )
    }
}
