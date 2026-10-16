//! Kod's audit log: an append-only, hash-chained SQLite table with secret
//! redaction before storage, external write-once checkpoints of the chain
//! head, and retention decided by the compliance profile through kod-policy.
//!
//! Tamper-evident, not tamper-proof. Anyone with write access to the host can
//! still change or delete rows; `verify` shows it, as long as the change
//! touches an entry at or before the last checkpoint and `verify` reads
//! checkpoints the host cannot forge (run it under the checkpoint account,
//! against a copy of the log). Entries after the last checkpoint can be
//! truncated without trace.
pub mod redact;

use anyhow::{Context, Result, anyhow, bail};
use kod_policy::Decision;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The `prev` hash of entry 1.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Policy decisions and gateway calls.
pub const RUN_AUDIT: &str = "run_audit";
/// Deletion records, written only by `purge`. Never deleted themselves:
/// `verify` needs them to account for every removed payload.
pub const RETENTION: &str = "retention";
/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum serialized payload size, checked before redaction and locking.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit (
    seq INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    record_type TEXT NOT NULL,
    payload TEXT,
    payload_sha256 TEXT NOT NULL,
    hash TEXT NOT NULL
) STRICT;
CREATE TRIGGER IF NOT EXISTS audit_append_only BEFORE INSERT ON audit
WHEN NEW.seq IS NOT (SELECT COALESCE(MAX(seq), 0) + 1 FROM audit)
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_no_delete BEFORE DELETE ON audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_only_purge BEFORE UPDATE ON audit
WHEN NEW.payload IS NOT NULL OR NEW.seq IS NOT OLD.seq OR NEW.at IS NOT OLD.at
  OR NEW.record_type IS NOT OLD.record_type
  OR NEW.payload_sha256 IS NOT OLD.payload_sha256 OR NEW.hash IS NOT OLD.hash
  OR NOT EXISTS (
    SELECT 1 FROM audit AS r, json_each(r.payload, '$.seqs') AS s
    WHERE r.record_type = 'retention' AND r.seq > OLD.seq
      AND json_extract(r.payload, '$.record_type') = OLD.record_type
      AND s.value = OLD.seq)
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
";

pub struct AuditLog {
    conn: Mutex<Connection>,
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let b = s.as_bytes();
    if b.len() != 64 {
        bail!("not a sha256 hex string: {s:?}");
    }
    b.chunks(2)
        .map(|p| match (nibble(p[0]), nibble(p[1])) {
            (Some(h), Some(l)) => Ok(h << 4 | l),
            _ => Err(anyhow!("not a lower-case sha256 hex string: {s:?}")),
        })
        .collect()
}

/// sha256(prev || seq || at || len(record_type) || record_type || payload_sha256),
/// with the integers as little-endian 8-byte values and the hashes as raw bytes.
pub fn entry_hash(
    prev: &str,
    seq: u64,
    at: i64,
    record_type: &str,
    payload_sha256: &str,
) -> Result<String> {
    let mut h = Sha256::new();
    h.update(unhex(prev)?);
    h.update(seq.to_le_bytes());
    h.update(at.to_le_bytes());
    h.update((record_type.len() as u64).to_le_bytes());
    h.update(record_type.as_bytes());
    h.update(unhex(payload_sha256)?);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Unix seconds.
pub fn now() -> Result<i64> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before 1970")?;
    Ok(d.as_secs() as i64)
}

fn prepare_payload(payload: &Value) -> Result<String> {
    if serde_json::to_vec(payload)?.len() > MAX_PAYLOAD_BYTES {
        bail!("audit payload exceeds {MAX_PAYLOAD_BYTES} bytes");
    }
    let text = serde_json::to_string(&redact::redact_value(payload))?;
    if text.len() > MAX_PAYLOAD_BYTES {
        bail!("redacted audit payload exceeds {MAX_PAYLOAD_BYTES} bytes");
    }
    Ok(text)
}

fn append_text_in(tx: &Transaction, record_type: &str, text: &str, at: i64) -> Result<u64> {
    if record_type.is_empty()
        || !record_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        bail!("record type must be lower-case letters and underscores: {record_type:?}");
    }
    let (last, prev): (i64, String) = tx
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, GENESIS.to_string()));
    let seq = last + 1;
    let payload_sha256 = sha256_hex(text.as_bytes());
    let hash = entry_hash(&prev, seq as u64, at, record_type, &payload_sha256)?;
    tx.execute(
        "INSERT INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq, at, record_type, text, payload_sha256, hash],
    )?;
    Ok(seq as u64)
}

impl AuditLog {
    /// Opens or creates the log. WAL with synchronous=FULL, so an entry that
    /// `append` returned survives a crash.
    /// The file is made 0600 before SQLite opens it, and SQLite gives the
    /// -wal and -shm files the same mode.
    pub fn open(path: &Path) -> Result<AuditLog> {
        let context = || format!("audit store {}", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .with_context(context)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let conn = Connection::open(path).with_context(context)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            bail!(
                "audit store {} could not enter WAL mode: {mode}",
                path.display()
            );
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(AuditLog {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("audit log lock poisoned"))
    }

    /// Runs `f` in one transaction. Writers use Immediate, which takes the
    /// write lock up front; readers use Deferred, which in WAL mode reads a
    /// snapshot without blocking writers.
    pub(crate) fn tx<T>(
        &self,
        behavior: TransactionBehavior,
        f: impl FnOnce(&Transaction) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(behavior)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Redacts secrets in `payload`, then appends it. Returns the new seq.
    /// Refuses RETENTION: only `purge` writes deletion records.
    pub fn append(&self, record_type: &str, payload: &Value) -> Result<u64> {
        if record_type == RETENTION {
            bail!("retention records are written only by purge");
        }
        let text = prepare_payload(payload)?;
        self.tx(TransactionBehavior::Immediate, |tx| {
            append_text_in(tx, record_type, &text, now()?)
        })
    }

    /// Records a policy decision with its input and policy_version.
    pub fn record_decision(&self, input: &Value, decision: &Decision) -> Result<u64> {
        self.append(
            RUN_AUDIT,
            &json!({"kind": "policy_decision", "input": input, "decision": decision}),
        )
    }

    /// Writes the audit record, then runs `call`. If the write fails, `call`
    /// never runs: the external write is refused. This records the intent
    /// only; the caller appends the call's result as its own entry.
    pub fn before<T>(
        &self,
        record_type: &str,
        payload: &Value,
        call: impl FnOnce() -> T,
    ) -> Result<T> {
        self.append(record_type, payload)?;
        Ok(call())
    }
}
