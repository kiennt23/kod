use crate::store::{binding, seq_of, write};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelCharge {
    pub tokens: u64,
    pub pico_usd: u64,
}
#[derive(Clone, Copy)]
pub struct ModelRequest<'a> {
    pub run: &'a str,
    pub id: &'a str,
    pub reservation: ModelCharge,
}
fn key(request: &ModelRequest<'_>) -> Result<i64> {
    ensure!(
        !request.id.is_empty()
            && request.id.len() <= 128
            && !request.id.chars().any(char::is_control),
        "invalid model request ID"
    );
    seq_of(request.run).context("invalid run ID")
}
fn decimal(text: &str) -> Result<u64> {
    let value = text.parse::<u64>().context("invalid stored reservation")?;
    ensure!(value.to_string() == text, "noncanonical stored reservation");
    Ok(value)
}
fn row(c: &Connection, seq: i64, id: &str) -> Result<(ModelCharge, Option<String>)> {
    let (tokens,cost,settled):(String,String,Option<String>)=c.query_row(
        "SELECT reserved_tokens,reserved_cost,settled_usage FROM model_requests WHERE run_seq=?1 AND request_id=?2",
        params![seq,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.context("unknown model request")?;
    Ok((
        ModelCharge {
            tokens: decimal(&tokens)?,
            pico_usd: decimal(&cost)?,
        },
        settled,
    ))
}
pub fn reserve_model(c: &Connection, request: &ModelRequest<'_>) -> Result<()> {
    let seq = key(request)?;
    write(c, |c| {
        binding(c, request.run)?;
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_requests WHERE run_seq=?1 AND request_id=?2)",
            params![seq, request.id],
            |r| r.get(0),
        )?;
        if exists {
            ensure!(
                row(c, seq, request.id)?.0 == request.reservation,
                "model reservation changed"
            );
            return Ok(());
        }
        ensure!(
            c.query_row(
                "SELECT state='running' FROM runs WHERE seq=?1",
                [seq],
                |r| r.get::<_, bool>(0)
            )?,
            "run is not executing"
        );
        c.execute("INSERT INTO model_requests(run_seq,request_id,reserved_tokens,reserved_cost) VALUES(?1,?2,?3,?4)",params![seq,request.id,request.reservation.tokens.to_string(),request.reservation.pico_usd.to_string()])?;
        Ok(())
    })
}
/// Records confirmed usage even above the reservation; budget enforcement must
/// halt on an overrun rather than discard known spend.
pub fn settle_model(c: &Connection, request: &ModelRequest<'_>, actual: ModelCharge) -> Result<()> {
    let seq = key(request)?;
    write(c, |c| {
        let (reservation, old) = row(c, seq, request.id)?;
        ensure!(
            reservation == request.reservation,
            "model reservation changed"
        );
        let usage = json!({"tokens":actual.tokens,"pico_usd":actual.pico_usd}).to_string();
        ensure!(
            old.as_ref().is_none_or(|old| old == &usage),
            "model settlement changed"
        );
        c.execute("UPDATE model_requests SET settled_usage=?3 WHERE run_seq=?1 AND request_id=?2 AND settled_usage IS NULL",params![seq,request.id,usage])?;
        Ok(())
    })
}
/// Unsettled requests remain charged at their full reservation after a crash.
/// Totals saturate at u64::MAX (exhausted); individual actuals remain lossless.
pub fn model_charge(c: &Connection, run: &str) -> Result<ModelCharge> {
    let seq = seq_of(run).context("invalid run ID")?;
    ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE seq=?1)",
            [seq],
            |r| r.get::<_, bool>(0)
        )?,
        "unknown run"
    );
    let mut statement = c.prepare(
        "SELECT reserved_tokens,reserved_cost,settled_usage FROM model_requests WHERE run_seq=?1",
    )?;
    let mut rows = statement.query([seq])?;
    let mut total = ModelCharge::default();
    while let Some(row) = rows.next()? {
        binding(c, run)?;
        let reserved = ModelCharge {
            tokens: decimal(&row.get::<_, String>(0)?)?,
            pico_usd: decimal(&row.get::<_, String>(1)?)?,
        };
        let settled: Option<String> = row.get(2)?;
        let charge = match settled {
            None => reserved,
            Some(text) => {
                let v: Value = serde_json::from_str(&text).context("invalid stored model usage")?;
                ensure!(
                    v.as_object().is_some_and(|o| o.len() == 2),
                    "invalid stored model usage"
                );
                ModelCharge {
                    tokens: v
                        .get("tokens")
                        .and_then(Value::as_u64)
                        .context("invalid token usage")?,
                    pico_usd: v
                        .get("pico_usd")
                        .and_then(Value::as_u64)
                        .context("invalid cost usage")?,
                }
            }
        };
        total.tokens = total.tokens.saturating_add(charge.tokens);
        total.pico_usd = total.pico_usd.saturating_add(charge.pico_usd);
    }
    Ok(total)
}
