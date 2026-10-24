use super::*;

pub fn retire_active(c: &mut Connection, reason: &str) -> Result<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seqs = {
        let mut statement =
            tx.prepare("SELECT seq FROM runs WHERE state IN ('queued','running') ORDER BY seq")?;
        statement
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for seq in seqs {
        Dispatcher::end(
            &tx,
            seq,
            RunState::NeedsHuman,
            reason,
            &[RunState::Queued, RunState::Running],
        )?;
    }
    tx.commit()?;
    Ok(())
}

impl Dispatcher {
    /// Ends the run if it is in one of `from`, and queues its stop, the
    /// ticket's move and a comment. Returns whether the run ended here.
    pub(super) fn end(
        c: &Connection,
        seq: i64,
        to: RunState,
        reason: &str,
        from: &[RunState],
    ) -> Result<bool> {
        let reason = &redact(reason);
        let from: Vec<String> = from.iter().map(|s| format!("'{}'", s.name())).collect();
        let ticket: Option<String> = c
            .query_row(
                &format!(
                    "UPDATE runs SET state = ?2, reason = ?3 WHERE seq = ?1 AND state IN ({}) RETURNING ticket",
                    from.join(", ")
                ),
                params![seq, to.name(), reason],
                |r| r.get(0),
            )
            .optional()?;
        let Some(ticket) = ticket else {
            return Ok(false);
        };
        let state = match to {
            RunState::Done => State::InReview,
            RunState::Canceled => State::Ready,
            RunState::Queued
            | RunState::Running
            | RunState::Publishing
            | RunState::Blocked
            | RunState::NeedsHuman
            | RunState::OverBudget => State::Blocked,
        };
        follow(c, "revoke", &run_id(seq), "")?;
        follow(c, "stop", &run_id(seq), "")?;
        follow(c, "transition", &ticket, state.name())?;
        follow(
            c,
            "comment",
            &ticket,
            &format!("Kod {} ended {}: {reason}.", run_id(seq), to.name()),
        )?;
        Ok(true)
    }
}
