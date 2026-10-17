//! The dispatcher's SQLite store: the delivery queue, the runs, the
//! follow-up calls, and the kill switch. The database itself keeps run states
//! moving forward and allows one live run per ticket. A run's state is also
//! its durable cancel flag: the gateway reads it before every call.
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::{path::Path, time::Duration};

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
CREATE TRIGGER IF NOT EXISTS run_key_is_fixed BEFORE UPDATE OF ticket, revision ON runs
BEGIN SELECT RAISE(ABORT, 'a run keeps its ticket and revision'); END;
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
    epoch INTEGER NOT NULL DEFAULT 0
);
";

/// SQL for the states of a run that has not ended.
pub const LIVE: &str = "('queued', 'running', 'publishing')";

/// Opens (creating if needed) the store at `path`.
pub fn open(path: &Path) -> Result<Connection> {
    let c = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    c.pragma_update(None, "synchronous", "FULL")?;
    c.execute_batch(SCHEMA)
        .with_context(|| format!("creating the schema in {}", path.display()))?;
    Ok(c)
}

/// Whether the run must stop: it has ended, or there is no such run. The
/// gateway's `Stopped` check reads this on its own connection to the store.
pub fn stopped(c: &Connection, run_id: &str) -> Result<bool> {
    let Some(seq) = run_id
        .strip_prefix("run-")
        .and_then(|n| n.parse::<i64>().ok())
    else {
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
