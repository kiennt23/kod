//! The durable guard around the audited status write.
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub struct WriteFailure {
    not_applied: bool,
    message: String,
}

impl WriteFailure {
    pub fn uncertain(message: &str) -> Self {
        Self {
            not_applied: false,
            message: kod_tool_gateway::scrub(message),
        }
    }
    pub fn rejected(message: &str) -> Self {
        Self {
            not_applied: true,
            message: kod_tool_gateway::scrub(message),
        }
    }
    pub fn from_tracker(error: anyhow::Error) -> Self {
        let message = format!("{error:#}");
        match kod_tracker::github::write_outcome(&error) {
            kod_tracker::github::WriteOutcome::NotApplied => Self::rejected(&message),
            kod_tracker::github::WriteOutcome::Uncertain => Self::uncertain(&message),
        }
    }
}

/// Persists the intent before `apply` runs; an unresolved earlier intent blocks the write.
pub fn guarded_status_write(
    c: &mut Connection,
    story: &str,
    audit_intent: u64,
    apply: impl FnOnce() -> std::result::Result<(), WriteFailure>,
) -> Result<()> {
    let intent = i64::try_from(audit_intent)?;
    let operation = format!("status:{story}:{intent}");
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Option<String> = tx
        .query_row(
            "SELECT operation_id FROM tracker_write_intents WHERE story=?1 AND kind='status' AND outcome IS NULL",
            [story],
            |r| r.get(0),
        )
        .optional()?;
    if pending.is_some() {
        bail!("status write unresolved; reconcile before retry");
    }
    tx.execute(
        "INSERT INTO tracker_write_intents(operation_id,story,kind,audit_intent) VALUES(?1,?2,'status',?3)",
        params![operation, story, intent],
    )?;
    tx.commit()?; // never hold a DB transaction during HTTP
    match apply() {
        Ok(()) => {
            c.execute(
                "UPDATE tracker_write_intents SET outcome='posted' WHERE operation_id=?1",
                [&operation],
            )?;
            Ok(())
        }
        Err(failure) => {
            if failure.not_applied {
                c.execute(
                    "UPDATE tracker_write_intents SET outcome='not_applied' WHERE operation_id=?1",
                    [&operation],
                )?;
            }
            bail!("{}", failure.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn open(dir: &tempfile::TempDir) -> Connection {
        kod_dispatcher::store::open(&dir.path().join("dispatcher.db")).unwrap()
    }

    #[test]
    fn uncertain_status_survives_reopen_and_blocks_a_second_create() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open(&dir);
        let calls = AtomicUsize::new(0);
        let first = guarded_status_write(&mut c, "acme/app#1", 41, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(WriteFailure::uncertain("response lost"))
        });
        assert!(first.is_err());
        drop(c);
        let mut c = open(&dir);
        assert!(
            guarded_status_write(&mut c, "acme/app#1", 42, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_blocked_write_reports_unresolved_and_leaves_no_second_intent() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open(&dir);
        assert!(
            guarded_status_write(&mut c, "s", 1, || Err(WriteFailure::uncertain("lost"))).is_err()
        );
        let error = guarded_status_write(&mut c, "s", 2, || Ok(())).unwrap_err();
        // The explicit check, not the unique index's constraint error, must answer.
        assert!(format!("{error:#}").contains("unresolved"), "{error:#}");
        let rows: i64 = c
            .query_row("SELECT count(*) FROM tracker_write_intents", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn rejected_status_allows_retry_and_success_clears_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open(&dir);
        assert!(
            guarded_status_write(&mut c, "s", 1, || Err(WriteFailure::rejected("403"))).is_err()
        );
        guarded_status_write(&mut c, "s", 2, || Ok(())).unwrap();
        guarded_status_write(&mut c, "s", 3, || Ok(())).unwrap();
    }

    #[test]
    fn the_guard_is_per_story() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open(&dir);
        assert!(
            guarded_status_write(&mut c, "a", 1, || Err(WriteFailure::uncertain("x"))).is_err()
        );
        guarded_status_write(&mut c, "b", 2, || Ok(())).unwrap();
    }

    #[test]
    fn typed_tracker_errors_decide_certainty_not_message_text() {
        use kod_tracker::github::WriteOutcome;
        let rejected = anyhow::anyhow!("unknown outcome").context(WriteOutcome::NotApplied);
        assert!(WriteFailure::from_tracker(rejected).not_applied);
        assert!(!WriteFailure::from_tracker(anyhow::anyhow!("HTTP 403")).not_applied);
    }
}
