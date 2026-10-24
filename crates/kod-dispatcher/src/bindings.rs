use crate::store::seq_of;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS run_bindings (
    run_seq INTEGER PRIMARY KEY REFERENCES runs(seq),
    binding TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS binding_is_fixed BEFORE UPDATE ON run_bindings
BEGIN SELECT RAISE(ABORT, 'a run keeps its binding'); END;
CREATE TRIGGER IF NOT EXISTS bindings_are_kept BEFORE DELETE ON run_bindings
BEGIN SELECT RAISE(ABORT, 'run bindings are never deleted'); END;
CREATE TABLE IF NOT EXISTS model_requests (
    run_seq INTEGER NOT NULL REFERENCES runs(seq),
    request_id TEXT NOT NULL,
    reserved_tokens TEXT NOT NULL,
    reserved_cost TEXT NOT NULL,
    settled_usage TEXT,
    PRIMARY KEY (run_seq, request_id)
);
CREATE TABLE IF NOT EXISTS tracker_write_intents (
    operation_id TEXT PRIMARY KEY,
    story TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('status', 'rework')),
    audit_intent INTEGER,
    outcome TEXT,
    remote_id TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_uncertain_status ON tracker_write_intents(story)
    WHERE kind='status' AND outcome IS NULL;
";

/// Stores a version-one execution binding once; an identical retry changes nothing.
pub fn bind_once(c: &Connection, run: &str, binding: &Value) -> Result<()> {
    let seq = seq_of(run).context("invalid run ID")?;
    ensure!(
        binding.is_object() && binding.get("version").and_then(Value::as_u64) == Some(1),
        "invalid binding version"
    );
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO run_bindings(run_seq,binding) VALUES(?1,?2) ON CONFLICT(run_seq) DO NOTHING",
        params![seq, binding.to_string()],
    )?;
    ensure!(self::binding(&tx, run)? == *binding, "run binding changed");
    tx.commit()?;
    Ok(())
}

pub fn binding(c: &Connection, run: &str) -> Result<Value> {
    let seq = seq_of(run).context("invalid run ID")?;
    let text: String = c
        .query_row(
            "SELECT binding FROM run_bindings WHERE run_seq=?1",
            [seq],
            |r| r.get(0),
        )
        .optional()?
        .context("run has no execution binding")?;
    let value: Value = serde_json::from_str(&text).context("invalid stored execution binding")?;
    ensure!(
        value.is_object() && value.get("version").and_then(Value::as_u64) == Some(1),
        "invalid stored binding version"
    );
    Ok(value)
}
