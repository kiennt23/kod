//! The dispatcher's SQLite store: the delivery queue, the runs, the
//! follow-up calls, and the kill switch. The database itself keeps run states
//! moving forward and allows one live run per ticket. A run's state is also
//! its durable cancel flag: the gateway reads it before every call.
//!
//! A follow-up that revokes or stops is never given up: it is retried with
//! backoff until the gateway confirms it. A stop of every sandbox carries
//! the kill switch's epoch, which `resume` moves on, so a retry left over
//! from a kill never stops runs started after the resume.
//!
//! A delivery is keyed by a hash of its signed body, so a redelivery, or a
//! replay with a different unsigned delivery header, is a duplicate. `done`
//! is 0 while queued, 1 once its effect is committed, and 2 when it failed
//! `MAX_ATTEMPTS` times (a dead letter, kept with its last error).
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, time::Duration};

pub use crate::bindings::{bind_once, binding};
pub use crate::model_requests::{
    ModelCharge, ModelRequest, model_charge, reserve_model, settle_model,
};

/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS deliveries (
    id TEXT PRIMARY KEY,
    delivery TEXT NOT NULL,
    ticket TEXT NOT NULL,
    event TEXT NOT NULL,
    priority INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1, 2))
);
CREATE TABLE IF NOT EXISTS runs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket TEXT NOT NULL,
    revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'publishing',
        'done', 'blocked', 'needs_human', 'over_budget', 'canceled')),
    reason TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX IF NOT EXISTS one_live_run ON runs (ticket)
    WHERE state IN ('queued', 'running', 'publishing');
CREATE TRIGGER IF NOT EXISTS forward_only BEFORE UPDATE OF state ON runs
WHEN (CASE NEW.state WHEN 'queued' THEN 0 WHEN 'running' THEN 1
        WHEN 'publishing' THEN 2 ELSE 3 END)
  <= (CASE OLD.state WHEN 'queued' THEN 0 WHEN 'running' THEN 1
        WHEN 'publishing' THEN 2 ELSE 3 END)
BEGIN SELECT RAISE(ABORT, 'run states only move forward'); END;
CREATE TRIGGER IF NOT EXISTS run_key_is_fixed BEFORE UPDATE OF seq, ticket, revision ON runs
BEGIN SELECT RAISE(ABORT, 'a run keeps its ticket and revision'); END;
CREATE TRIGGER IF NOT EXISTS runs_are_kept BEFORE DELETE ON runs
BEGIN SELECT RAISE(ABORT, 'runs are never deleted'); END;
CREATE TABLE IF NOT EXISTS followups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('revoke', 'stop', 'stop_all', 'transition', 'comment', 'check_publish')),
    target TEXT NOT NULL,
    arg TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1, 2))
);
CREATE TABLE IF NOT EXISTS control (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    halted INTEGER NOT NULL,
    by TEXT NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 0,
    kills INTEGER NOT NULL DEFAULT 0
);
";

/// SQL for the states of a run that has not ended.
pub const LIVE: &str = "('queued', 'running', 'publishing')";

pub(super) fn retire_labels(c: &Connection, ticket: &str, delivery: &str) -> Result<()> {
    c.execute(
        "UPDATE deliveries SET done = 1 WHERE ticket = ?1 AND priority = 1 AND done = 0
         AND rowid < (SELECT rowid FROM deliveries WHERE id = ?2)",
        params![ticket, delivery],
    )?;
    Ok(())
}

/// An edit retires the labels queued before it, which may have read the old
/// ticket, and queues each again behind it, to read the ticket fresh. A label
/// that has not started is not lost.
pub(super) fn requeue_labels(c: &Connection, ticket: &str, delivery: &str) -> Result<()> {
    c.execute(
        "INSERT INTO deliveries (id, delivery, ticket, event, priority)
         SELECT id || '/' || ?2, delivery, ticket, event, priority FROM deliveries
         WHERE ticket = ?1 AND priority = 1 AND done = 0
         AND rowid < (SELECT rowid FROM deliveries WHERE id = ?2)",
        params![ticket, delivery],
    )?;
    retire_labels(c, ticket, delivery)
}

/// Opens (creating if needed) the store at `path`.
pub fn open(path: &Path) -> Result<Connection> {
    let mut c = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    c.pragma_update(None, "synchronous", "FULL")?;
    // So the delete inside INSERT OR REPLACE fires `runs_are_kept`.
    c.pragma_update(None, "recursive_triggers", true)?;
    c.pragma_update(None, "foreign_keys", true)?;
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version <= 1, "unsupported dispatcher database version");
    if version == 0 {
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version <= 1, "unsupported dispatcher database version");
        if version == 0 {
            tx.execute_batch(SCHEMA)
                .with_context(|| format!("creating the schema in {}", path.display()))?;
            tx.execute_batch(crate::bindings::SCHEMA)?;
            tx.pragma_update(None, "user_version", 1)?;
        }
        tx.commit()?;
    }
    Ok(c)
}

/// The one strict run ID parser: `run-N` as the dispatcher writes it, with
/// no sign and no leading zeros.
pub fn seq_of(run_id: &str) -> Option<i64> {
    run_id
        .strip_prefix("run-")
        .and_then(|n| n.parse::<i64>().ok())
        .filter(|seq| *seq > 0 && format!("run-{seq}") == run_id)
}

pub fn retire_active(c: &mut Connection, reason: &str) -> Result<()> {
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let seqs = {
        let mut statement =
            tx.prepare("SELECT seq FROM runs WHERE state IN ('queued','running') ORDER BY seq")?;
        statement
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for seq in seqs {
        crate::Dispatcher::end(
            &tx,
            seq,
            crate::RunState::NeedsHuman,
            reason,
            &[crate::RunState::Queued, crate::RunState::Running],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Whether the run must stop: it has ended, or there is no such run. The
/// gateway's `Stopped` check reads this on its own connection to the store.
pub fn stopped(c: &Connection, run_id: &str) -> Result<bool> {
    let Some(seq) = seq_of(run_id) else {
        return Ok(true);
    };
    let live: Option<bool> = c
        .query_row(
            &format!("SELECT state IN {LIVE} FROM runs WHERE seq = ?1"),
            [seq],
            |r| r.get(0),
        )
        .optional()?;
    Ok(live != Some(true))
}
