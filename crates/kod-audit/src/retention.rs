use crate::{AuditLog, RETENTION, RUN_AUDIT, append_in};
use anyhow::{Result, bail};
use kod_policy::Policy;
use rusqlite::{Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

/// Payloads logically cleared per transaction, so a purge never holds the write lock
/// long enough to time out a gateway append.
pub const PURGE_BATCH: usize = 500;

impl AuditLog {
    /// Deletes the payloads of `record_type` entries whose retention has
    /// ended, oldest first, asking the policy about each one. The first
    /// refusal is recorded and ends the purge. Works in batches of
    /// PURGE_BATCH; each batch appends one RETENTION entry listing what it
    /// deletes, before clearing the payloads (the store refuses to clear a
    /// payload no retention entry covers). Rows and hashes stay, so the chain
    /// still verifies. Refuses to run when `now` is behind the newest entry:
    /// the clock is wrong. Returns how many payloads were logically cleared,
    /// not how many were erased from storage or expired from backups.
    pub fn purge(&self, policy: &Policy, record_type: &str, now: i64) -> Result<usize> {
        if record_type == RETENTION {
            bail!("retention records are kept for the life of the log");
        }
        let mut deleted = 0;
        loop {
            let (n, refused) = self.tx(TransactionBehavior::Immediate, |tx| {
                purge_batch(tx, policy, record_type, now)
            })?;
            deleted += n;
            if refused || n < PURGE_BATCH {
                return Ok(deleted);
            }
        }
    }

    /// Checks every retention entry against the released policies: its
    /// policy_version must be one of theirs, and that policy must allow
    /// deleting each entry it lists at the age the entry had when the
    /// retention entry was written. A forged but well-formed deletion record
    /// fails here. Run by the off-host verifier after `verify`, with one
    /// loaded policy per released version. Returns how many retention
    /// entries it checked.
    pub fn verify_retention(&self, released: &[Policy]) -> Result<usize> {
        let rows = self.rows()?;
        let mut checked = 0;
        for (seq, at, record_type, payload, _, _) in &rows {
            let (RETENTION, Some(p)) = (record_type.as_str(), payload) else {
                continue;
            };
            let v: Value = serde_json::from_str(p)?;
            let version = v["policy_version"].as_str().unwrap_or_default();
            let Some(policy) = released.iter().find(|p| p.version() == version) else {
                bail!(
                    "tampered at seq {seq}: retention record cites policy {version:?}, \
                     which is not a released version"
                );
            };
            for s in v["seqs"].as_array().into_iter().flatten() {
                let Some(row) = s
                    .as_u64()
                    .and_then(|s| rows.get((s as usize).wrapping_sub(1)))
                else {
                    bail!("tampered at seq {seq}: malformed retention record");
                };
                let age = (at - row.1) / 86_400;
                let input =
                    json!({"action": "retention_delete", "record_type": row.2, "age_days": age});
                if !policy.decide(&input).allow {
                    bail!(
                        "tampered at seq {seq}: policy {version} does not allow deleting \
                         seq {s} at {age} days"
                    );
                }
            }
            checked += 1;
        }
        Ok(checked)
    }
}

/// One batch: returns how many payloads it cleared and whether the policy
/// refused one.
fn purge_batch(
    tx: &Transaction,
    policy: &Policy,
    record_type: &str,
    now: i64,
) -> Result<(usize, bool)> {
    let newest: Option<i64> = tx.query_row("SELECT MAX(at) FROM audit", [], |r| r.get(0))?;
    if newest.is_some_and(|at| now < at) {
        bail!("clock is behind the newest audit entry; refusing to purge");
    }
    let candidates = tx
        .prepare(
            "SELECT seq, at FROM audit WHERE record_type = ?1 AND payload IS NOT NULL
             ORDER BY at, seq LIMIT ?2",
        )?
        .query_map(params![record_type, PURGE_BATCH as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seqs = Vec::new();
    let mut ages = Vec::new();
    let mut last = None;
    let mut refused = false;
    for (seq, at) in candidates {
        // Whole days, rounded down; never negative, since now >= every `at`.
        let age = (now - at) / 86_400;
        let input =
            json!({"action": "retention_delete", "record_type": record_type, "age_days": age});
        let decision = policy.decide(&input);
        if !decision.allow {
            append_in(
                tx,
                RUN_AUDIT,
                &json!({"kind": "policy_decision", "input": input, "decision": decision}),
                now,
            )?;
            refused = true;
            break;
        }
        seqs.push(seq);
        ages.push(age);
        last = Some(decision);
    }
    if let Some(decision) = last {
        append_in(
            tx,
            RETENTION,
            &json!({
                "kind": "retention_delete",
                "record_type": record_type,
                "seqs": seqs,
                "oldest_age_days": ages.first(),
                "youngest_age_days": ages.last(),
                "policy_version": decision.policy_version,
            }),
            now,
        )?;
        for seq in &seqs {
            tx.execute("UPDATE audit SET payload = NULL WHERE seq = ?1", [seq])?;
        }
    }
    Ok((seqs.len(), refused))
}
