use super::*;

impl Dispatcher {
    fn transition(&self, id: i64, ticket: &str, state: &str) -> Result<()> {
        let _transition = self
            .tracker_transitions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let pending: bool =
            self.db()
                .query_row("SELECT done = 0 FROM followups WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
        if pending {
            self.tracker.transition(ticket, state_named(state)?)?;
        }
        Ok(())
    }

    /// Makes the due follow-ups: revokes, then stops, then the tracker. One
    /// that fails is retried with backoff. A tracker call is a dead letter
    /// after `MAX_ATTEMPTS`; a revoke or a stop is never given up, and when
    /// it reaches `MAX_ATTEMPTS` Kod raises the alarm once, on the ticket and
    /// in the audit log. A stop of every sandbox from before the last
    /// `resume` is dropped: the kill's own runs have their own stops.
    pub(super) fn follow_up(&self, now: i64) -> Result<()> {
        let due: Vec<(i64, String, String, String, i64)> = {
            let c = self.db();
            let mut s = c.prepare(
                "SELECT id, kind, target, arg, attempts FROM followups
                 WHERE done = 0 AND next_at <= ?1
                 ORDER BY CASE kind WHEN 'revoke' THEN 0 WHEN 'check_publish' THEN 2
                     WHEN 'transition' THEN 2 WHEN 'comment' THEN 2 ELSE 1 END, id",
            )?;
            s.query_map([now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (id, kind, target, arg, attempts) in due {
            let r = match kind.as_str() {
                "revoke" => self.gateway.revoke(&target),
                "stop" => self.gateway.stop(&target),
                "stop_all" => {
                    let _stop = self.global_stop.lock().unwrap_or_else(|e| e.into_inner());
                    if arg != Self::epoch(&self.db())?.to_string() {
                        self.db().execute(
                            "UPDATE followups SET done = 1, error = 'the kill switch was lifted' WHERE id = ?1",
                            [id],
                        )?;
                        continue;
                    }
                    self.gateway.stop_all()
                }
                "check_publish" => self.reconcile_publish(id, &target, &arg),
                "transition" => self.transition(id, &target, &arg),
                "comment" => self.tracker.comment(&target, &arg),
                k => Err(anyhow!("unknown follow-up {k}")),
            };
            // The audit record is appended after the store guard is dropped: a
            // slow audit log must not hold up the store.
            let overdue = {
                let c = self.db();
                match r {
                    Ok(()) => {
                        c.execute("UPDATE followups SET done = 1 WHERE id = ?1", [id])?;
                        None
                    }
                    Err(e) => {
                        let attempts = attempts + 1;
                        let error = scrub(&e);
                        let stopping = matches!(kind.as_str(), "revoke" | "stop" | "stop_all");
                        let dead = !stopping && kind != "check_publish" && attempts >= MAX_ATTEMPTS;
                        c.execute(
                            "UPDATE followups SET attempts = ?2, next_at = ?3, error = ?4, done = ?5
                             WHERE id = ?1 AND done = 0",
                            params![
                                id,
                                attempts,
                                now + backoff(attempts),
                                error,
                                if dead { 2 } else { 0 }
                            ],
                        )?;
                        if stopping && attempts == MAX_ATTEMPTS {
                            Some(Self::alarm(&c, &kind, &target, attempts, &error)?)
                        } else {
                            None
                        }
                    }
                }
            };
            if let Some(record) = overdue {
                let _ = self.audit.append(RUN_AUDIT, &record);
            }
        }
        Ok(())
    }

    /// A revoke or a stop has failed `MAX_ATTEMPTS` times: say so once on
    /// the run's ticket and in the audit log (best effort), and keep trying.
    fn alarm(
        c: &Connection,
        kind: &str,
        target: &str,
        attempts: i64,
        error: &str,
    ) -> Result<Value> {
        let record = json!({"kind": "stop_overdue", "call": kind, "run": target, "attempts": attempts, "error": error});
        eprintln!("{record}");
        let ticket: Option<String> = match seq_of(target) {
            Ok(seq) => c
                .query_row("SELECT ticket FROM runs WHERE seq = ?1", [seq], |r| {
                    r.get(0)
                })
                .optional()?,
            Err(_) => None,
        };
        if let Some(ticket) = ticket {
            follow(
                c,
                "comment",
                &ticket,
                &format!(
                    "Kod has not confirmed the {kind} of {target} after {attempts} attempts and keeps trying: {error}."
                ),
            )?;
        }
        Ok(record)
    }
}
