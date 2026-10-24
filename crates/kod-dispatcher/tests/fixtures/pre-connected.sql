
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

