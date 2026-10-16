use crate::{
    AuditLog, GENESIS, RETENTION,
    checkpoint::{Checkpoint, CheckpointTarget},
    entry_hash, now, sha256_hex,
};
use anyhow::{Result, bail};
use rusqlite::TransactionBehavior;
use serde_json::Value;
use std::collections::HashSet;

/// One audit row: seq, at, record_type, payload, payload_sha256, hash.
pub(crate) type Row = (u64, i64, String, Option<String>, String, String);

/// What `verify` checked.
#[derive(Debug, PartialEq)]
pub struct Verified {
    pub entries: u64,
    /// Hash of the last entry, or GENESIS for an empty log.
    pub head: String,
    pub last_checkpoint: Option<u64>,
}

impl AuditLog {
    /// Every row, from one snapshot (no write lock).
    pub(crate) fn rows(&self) -> Result<Vec<Row>> {
        // ponytail: reads every row on each run; page through it if the log
        // grows past what fits in memory.
        self.tx(TransactionBehavior::Deferred, |tx| {
            let mut stmt = tx.prepare(
                "SELECT seq, at, record_type, payload, payload_sha256, hash FROM audit ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Checks the whole chain, then every checkpoint against it. Any mismatch
    /// is an error naming the first problem found. Reads a snapshot, so it
    /// never holds the write lock. Run under the checkpoint account against a
    /// copy of the log, it is the authoritative check; on the audit host it is
    /// an early warning, since that host controls its own view of the
    /// checkpoints.
    pub fn verify(&self, target: &dyn CheckpointTarget) -> Result<Verified> {
        // Checkpoints first: one written while the rows are read then refers
        // to an entry inside the snapshot, not past its end.
        let checkpoints = target.list()?;
        let rows = self.rows()?;
        let mut hashes = Vec::with_capacity(rows.len());
        let mut removed = Vec::new();
        let mut purged = HashSet::new();
        let mut prev = GENESIS.to_string();
        for (i, (seq, at, record_type, payload, payload_sha256, hash)) in rows.iter().enumerate() {
            let expected = i as u64 + 1;
            if *seq != expected {
                bail!("chain broken: expected seq {expected}, found seq {seq}");
            }
            match payload {
                Some(p) if sha256_hex(p.as_bytes()) != *payload_sha256 => {
                    bail!("tampered at seq {seq}: payload does not match its hash")
                }
                Some(p) if record_type == RETENTION => {
                    // A deletion record covers only earlier entries of the
                    // record type it names. purge writes nothing else, so
                    // anything else is tampering.
                    let v: Value = serde_json::from_str(p)?;
                    let (Some(covered), Some(seqs), Some(_)) = (
                        v["record_type"].as_str(),
                        v["seqs"].as_array(),
                        v["policy_version"].as_str(),
                    ) else {
                        bail!("tampered at seq {seq}: malformed retention record");
                    };
                    for s in seqs {
                        let earlier = s
                            .as_u64()
                            .filter(|s| (1..*seq).contains(s))
                            .filter(|s| rows[*s as usize - 1].2 == covered && covered != RETENTION);
                        let Some(s) = earlier else {
                            bail!(
                                "tampered at seq {seq}: retention record covers {s}, \
                                 which is not an earlier {covered} entry"
                            );
                        };
                        purged.insert(s);
                    }
                }
                Some(_) => {}
                None => removed.push(*seq),
            }
            if entry_hash(&prev, *seq, *at, record_type, payload_sha256)? != *hash {
                bail!("tampered at seq {seq}: entry hash does not match the chain");
            }
            prev = hash.clone();
            hashes.push(hash);
        }
        if let Some(seq) = removed.iter().find(|s| !purged.contains(s)) {
            bail!("tampered at seq {seq}: payload removed without a retention record");
        }
        for cp in &checkpoints {
            match hashes.get((cp.seq as usize).wrapping_sub(1)) {
                None => bail!(
                    "truncated: checkpoint at seq {} but the log ends at seq {}",
                    cp.seq,
                    hashes.len()
                ),
                Some(h) if **h != cp.hash => {
                    bail!(
                        "tampered: checkpoint at seq {} does not match the log",
                        cp.seq
                    )
                }
                Some(_) => {}
            }
        }
        Ok(Verified {
            entries: hashes.len() as u64,
            head: prev,
            last_checkpoint: checkpoints.last().map(|cp| cp.seq),
        })
    }

    /// Verifies the log, then writes its head to `target`. Refuses to
    /// checkpoint a log that fails verification. Returns None when the log is
    /// empty or the head is already checkpointed.
    pub fn checkpoint(&self, target: &dyn CheckpointTarget) -> Result<Option<Checkpoint>> {
        let v = self.verify(target)?;
        if v.entries == 0 || v.last_checkpoint == Some(v.entries) {
            return Ok(None);
        }
        let cp = Checkpoint {
            seq: v.entries,
            hash: v.head,
            at: now()?,
        };
        target.put(&cp)?;
        Ok(Some(cp))
    }
}
