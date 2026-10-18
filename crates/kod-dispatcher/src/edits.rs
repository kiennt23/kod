use super::*;

impl Dispatcher {
    pub(super) fn edited(&self, delivery: &str, ticket: &str) -> Result<()> {
        let live = {
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // An edit must also retire admissions that have read the old ticket.
            store::retire_labels(&tx, ticket, delivery)?;
            let live = live_run(&tx, ticket)?;
            if live.is_none() {
                tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
            }
            tx.commit()?;
            live
        };
        let Some((_, old)) = live else {
            return Ok(());
        };
        // A tracker that cannot answer must not keep a run alive: cancel.
        let why = match self.tracker.get_issue(ticket) {
            Ok(issue) => {
                let new = revision(&issue);
                if new == old {
                    return self.done(delivery);
                }
                format!(
                    "the ticket changed (revision {old} to {new}); it is back in ready at the new revision"
                )
            }
            Err(_) => format!(
                "the ticket was edited and its revision unknown (was {old}); it is back in ready"
            ),
        };
        self.cancel(delivery, ticket, &why, "a ticket edit")?;
        Ok(())
    }
}
