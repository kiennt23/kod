//! Kod's dispatcher: tracker webhooks in, runs out. A verified delivery is
//! queued in a SQLite table, admitted or refused (with the reason commented
//! on the ticket), and started through the tool gateway. Runs move forward
//! only; a label removal, a ticket edit or the kill switch cancels them.
//!
//! Every outside call that follows a state change (stopping a sandbox,
//! moving or commenting on the ticket) is written as a follow-up in the same
//! transaction as the change, then made and retried until it succeeds. Stops
//! go first, and a tracker that is down never holds up a stop.
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Router,
    body::Bytes,
    extract::State as Shared,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use kod_audit::{AuditLog, RUN_AUDIT, redact::redact};
use kod_policy::Policy;
use kod_tracker::{Issue, State, TaskEvent, Tracker, Webhook};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use store::LIVE;

pub mod config;
pub mod store;

pub use config::Config;

/// Attempts before a delivery or follow-up becomes a dead letter.
pub const MAX_ATTEMPTS: i64 = 5;

/// Seconds before attempt `n + 1`: 2, 4, 8, ... up to about 4 minutes.
fn backoff(attempts: i64) -> i64 {
    1 << attempts.clamp(1, 8)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// An error as it may be stored or shown: secrets redacted.
fn scrub(e: &anyhow::Error) -> String {
    redact(&format!("{e:#}"))
}

/// The states of a run. `queued`, `running` and `publishing` are live; the
/// rest are ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running,
    Publishing,
    Done,
    Blocked,
    NeedsHuman,
    OverBudget,
    Canceled,
}

impl RunState {
    pub const ALL: [RunState; 8] = [
        RunState::Queued,
        RunState::Running,
        RunState::Publishing,
        RunState::Done,
        RunState::Blocked,
        RunState::NeedsHuman,
        RunState::OverBudget,
        RunState::Canceled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Running => "running",
            RunState::Publishing => "publishing",
            RunState::Done => "done",
            RunState::Blocked => "blocked",
            RunState::NeedsHuman => "needs_human",
            RunState::OverBudget => "over_budget",
            RunState::Canceled => "canceled",
        }
    }

    fn parse(name: &str) -> Result<RunState> {
        RunState::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .with_context(|| format!("unknown run state {name:?}"))
    }
}

/// A run as the dispatcher stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub ticket: String,
    pub revision: String,
    pub state: RunState,
    pub reason: String,
}

/// What the gateway is asked to start.
#[derive(Clone, Debug, PartialEq)]
pub struct RunStart {
    pub id: String,
    pub ticket: String,
    pub revision: String,
    pub repo: String,
}

/// The tool gateway as the dispatcher sees it. Every method must be safe to
/// repeat. The gateway also reads the run's state from the store (see
/// `store::stopped`) before each call it serves and twice inside publish, so
/// a run canceled here is refused there even if `stop` has not arrived yet.
///
/// How sub-plan 6b wires it: the runner calls `Dispatcher::finished` on the
/// host after the sandbox exits and kod-eval's collector has written
/// `collect_out` next to it, outside the sandbox. `publish` takes only the
/// run ID; the gateway reads the change from that collector output, never
/// from the sandbox.
pub trait Gateway: Send + Sync {
    fn start(&self, run: &RunStart) -> Result<()>;
    /// Ends the run's tool-gateway token and revokes its forge credentials
    /// at once, without waiting for the sandbox. A never-started run is Ok.
    fn revoke(&self, run_id: &str) -> Result<()>;
    /// Stops the run's sandbox. Ok only once confirmed gone; a never-started run is already gone.
    fn stop(&self, run_id: &str) -> Result<()>;
    /// The kill switch: stop every sandbox and revoke every agent identity.
    fn stop_all(&self) -> Result<()>;
    /// Publishes the run's collected change; returns the draft PR's URL.
    fn publish(&self, run_id: &str) -> Result<String>;
    /// After a restart: the draft PR a publish of this run opened, if any.
    fn find_published(&self, run: &RunStart) -> Result<Option<String>>;
}

/// Adapter contract: the caller supplies a run ID that was never started.
/// Revoking and stopping that absent run must succeed, including on retry.
pub fn check_absent_run(gateway: &dyn Gateway, run_id: &str) -> Result<()> {
    for _ in 0..2 {
        gateway
            .revoke(run_id)
            .context("revoking a never-started run")?;
        gateway
            .stop(run_id)
            .context("stopping a never-started run")?;
    }
    Ok(())
}

/// What `receive` did with a webhook.
#[derive(Clone, Debug, PartialEq)]
pub enum Received {
    Queued,
    /// The same signed body was queued before.
    Duplicate,
    /// Genuine, but not an event the dispatcher acts on.
    Ignored,
    /// Not genuine or malformed: the adapter's reason.
    Refused(String),
}

pub fn run_id(seq: i64) -> String {
    format!("run-{seq}")
}

fn seq_of(run_id: &str) -> Result<i64> {
    run_id
        .strip_prefix("run-")
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("{run_id:?} is not a run ID"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The ticket revision: a hash of what a run reads from the ticket. Workflow
/// state and links are left out, since the run itself changes them.
pub fn revision(i: &Issue) -> String {
    let fields = (
        &i.title,
        &i.body,
        &i.acceptance_criteria,
        &i.requirement_id,
        &i.risk_class,
        &i.approved_versions,
    );
    hex(&Sha256::digest(format!("{fields:?}").as_bytes())[..8])
}

/// A queued event, as the store keeps it.
#[derive(Clone, Debug, PartialEq)]
enum Event {
    Labeled { issue: String, by: String },
    Unlabeled { issue: String, by: String },
    Edited { issue: String },
}

impl Event {
    fn from(event: &TaskEvent) -> Option<Event> {
        Some(match event {
            TaskEvent::Labeled { issue, by } => Event::Labeled {
                issue: issue.clone(),
                by: by.clone(),
            },
            TaskEvent::Unlabeled { issue, by } => Event::Unlabeled {
                issue: issue.clone(),
                by: by.clone(),
            },
            TaskEvent::Edited { issue } => Event::Edited {
                issue: issue.clone(),
            },
            // Comments are instructions for a live run: sub-plan 8.
            TaskEvent::Commented { .. } => return None,
        })
    }

    fn ticket(&self) -> &str {
        match self {
            Event::Labeled { issue, .. } | Event::Unlabeled { issue, .. } => issue,
            Event::Edited { issue } => issue,
        }
    }

    /// Lower goes first: anything that can stop a run jumps the queue.
    fn priority(&self) -> i64 {
        match self {
            Event::Labeled { .. } => 1,
            _ => 0,
        }
    }

    fn encode(&self) -> Value {
        match self {
            Event::Labeled { issue, by } => json!({"type": "labeled", "issue": issue, "by": by}),
            Event::Unlabeled { issue, by } => {
                json!({"type": "unlabeled", "issue": issue, "by": by})
            }
            Event::Edited { issue } => json!({"type": "edited", "issue": issue}),
        }
    }

    fn decode(v: &Value) -> Result<Event> {
        let text = |k: &str| {
            v[k].as_str()
                .map(str::to_owned)
                .with_context(|| format!("queued event has no {k}"))
        };
        Ok(match v["type"].as_str() {
            Some("labeled") => Event::Labeled {
                issue: text("issue")?,
                by: text("by")?,
            },
            Some("unlabeled") => Event::Unlabeled {
                issue: text("issue")?,
                by: text("by")?,
            },
            Some("edited") => Event::Edited {
                issue: text("issue")?,
            },
            _ => bail!("unknown queued event {v}"),
        })
    }
}

/// Queues a call to make once the transaction commits.
fn follow(c: &Connection, kind: &str, target: &str, arg: &str) -> Result<()> {
    if kind == "transition" {
        c.execute(
            "UPDATE followups SET done = 1 WHERE kind = 'transition' AND target = ?1 AND done = 0",
            [target],
        )?;
    }
    c.execute(
        "INSERT INTO followups (kind, target, arg) VALUES (?1, ?2, ?3)",
        params![kind, target, arg],
    )?;
    Ok(())
}

fn state_named(name: &str) -> Result<State> {
    State::ALL
        .into_iter()
        .find(|s| s.name() == name)
        .with_context(|| format!("unknown ticket state {name:?}"))
}

fn live_run(c: &Connection, ticket: &str) -> Result<Option<(i64, String)>> {
    Ok(c.query_row(
        &format!("SELECT seq, revision FROM runs WHERE ticket = ?1 AND state IN {LIVE}"),
        [ticket],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

pub struct Dispatcher {
    config: Config,
    /// A policy that failed to load is kept as its error: everything that
    /// needs it is refused, and the owner's or backup's kill switch still works.
    policy: Result<Arc<Policy>, String>,
    audit: Arc<AuditLog>,
    tracker: Arc<dyn Tracker>,
    gateway: Arc<dyn Gateway>,
    db: Mutex<Connection>,
    global_stop: Mutex<()>,
}

impl Dispatcher {
    /// Opens the store and finishes what a crash interrupted: a run left in
    /// `publishing` ends done if the gateway finds its draft PR, and
    /// needs_human otherwise. It is never left live.
    pub fn open(
        config: Config,
        db: &Path,
        policy: Result<Arc<Policy>>,
        audit: Arc<AuditLog>,
        tracker: Arc<dyn Tracker>,
        gateway: Arc<dyn Gateway>,
    ) -> Result<Dispatcher> {
        let d = Dispatcher {
            config,
            policy: policy.map_err(|e| scrub(&e)),
            audit,
            tracker,
            gateway,
            db: Mutex::new(store::open(db)?),
            global_stop: Mutex::new(()),
        };
        d.recover()?;
        Ok(d)
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recover(&self) -> Result<()> {
        let publishing: Vec<RunStart> = {
            let c = self.db();
            let mut s =
                c.prepare("SELECT seq, ticket, revision FROM runs WHERE state = 'publishing'")?;
            s.query_map([], |r| {
                Ok(RunStart {
                    id: run_id(r.get(0)?),
                    ticket: r.get(1)?,
                    revision: r.get(2)?,
                    repo: self.config.repo.clone(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for run in publishing {
            let (to, why) = match self.gateway.find_published(&run) {
                Ok(Some(pr)) => (
                    RunState::Done,
                    format!("the dispatcher restarted during publish; the draft PR is {pr}"),
                ),
                Ok(None) => (
                    RunState::NeedsHuman,
                    "the dispatcher restarted during publish and found no draft PR".to_owned(),
                ),
                Err(e) => (
                    RunState::NeedsHuman,
                    format!(
                        "the dispatcher restarted during publish and could not check for a draft PR: {}",
                        scrub(&e)
                    ),
                ),
            };
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::end(&tx, seq_of(&run.id)?, to, &why, &[RunState::Publishing])?;
            tx.commit()?;
        }
        self.db().execute(
            "UPDATE followups SET next_at = 0 WHERE kind = 'check_publish' AND done = 0",
            [],
        )?;
        self.follow_up(now())?;
        self.follow_up(now())
    }

    /// Checks the webhook and queues its event. Errors are store errors only;
    /// a failed write spends nothing, so the tracker's retry is queued.
    pub fn receive(&self, webhook: &Webhook) -> Result<Received> {
        let delivery = match self.tracker.parse_webhook(webhook) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(Received::Ignored),
            Err(e) => return Ok(Received::Refused(scrub(&e))),
        };
        let Some(event) = Event::from(&delivery.event) else {
            return Ok(Received::Ignored);
        };
        let added = self.db().execute(
            "INSERT OR IGNORE INTO deliveries (id, delivery, ticket, event, priority)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                hex(&Sha256::digest(&webhook.body)),
                delivery.id,
                event.ticket(),
                event.encode().to_string(),
                event.priority()
            ],
        )?;
        Ok(if added == 1 {
            Received::Queued
        } else {
            Received::Duplicate
        })
    }

    /// `work_at` now.
    pub fn work_once(&self) -> Result<bool> {
        self.work_at(now())
    }

    /// Handles the next due delivery, makes the due follow-ups, then starts
    /// queued runs. Returns whether there was a delivery. Deliveries that can
    /// stop a run go first and never wait; a ticket's labels keep their
    /// order behind its earlier deliveries. One that
    /// fails is retried with backoff, without holding up other tickets, and
    /// after `MAX_ATTEMPTS` it is a dead letter, reported on its ticket.
    pub fn work_at(&self, now: i64) -> Result<bool> {
        let next: Option<(String, String, String, i64)> = self
            .db()
            .query_row(
                "SELECT id, ticket, event, attempts FROM deliveries d
                 WHERE done = 0 AND next_at <= ?1 AND (d.priority = 0 OR NOT EXISTS (
                     SELECT 1 FROM deliveries e
                     WHERE e.done = 0 AND e.ticket = d.ticket AND e.rowid < d.rowid))
                 ORDER BY priority, rowid LIMIT 1",
                [now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((id, ticket, event, attempts)) = &next
            && let Err(e) = self.handle(id, event)
        {
            let attempts = attempts + 1;
            let error = scrub(&e);
            let dead = attempts >= MAX_ATTEMPTS;
            let c = self.db();
            c.execute(
                "UPDATE deliveries SET attempts = ?2, next_at = ?3, error = ?4, done = ?5
                 WHERE id = ?1 AND done = 0",
                params![
                    id,
                    attempts,
                    now + backoff(attempts),
                    error,
                    if dead { 2 } else { 0 }
                ],
            )?;
            if dead {
                follow(
                    &c,
                    "comment",
                    ticket,
                    &format!(
                        "Kod gave up on an event for this ticket after {attempts} attempts: {error}."
                    ),
                )?;
            }
        }
        self.follow_up(now)?;
        self.start_queued()?;
        self.follow_up(now)?;
        Ok(next.is_some())
    }

    fn handle(&self, id: &str, event: &str) -> Result<()> {
        match Event::decode(&serde_json::from_str(event)?)? {
            Event::Labeled { issue, by } => self.admit(id, &issue, &by),
            Event::Unlabeled { .. } | Event::Edited { .. } => self.done(id),
        }
    }

    fn done(&self, delivery: &str) -> Result<()> {
        self.db()
            .execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        Ok(())
    }

    /// Makes the due follow-ups: revokes, then stops, then the tracker. One
    /// that fails is retried with backoff. A tracker call is a dead letter
    /// after `MAX_ATTEMPTS`; a revoke or a stop is never given up, and when
    /// it reaches `MAX_ATTEMPTS` Kod raises the alarm once, on the ticket and
    /// in the audit log. A stop of every sandbox from before the last
    /// `resume` is dropped: the kill's own runs have their own stops.
    fn follow_up(&self, now: i64) -> Result<()> {
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
                "transition" => state_named(&arg).and_then(|s| self.tracker.transition(&target, s)),
                "comment" => self.tracker.comment(&target, &arg),
                k => Err(anyhow!("unknown follow-up {k}")),
            };
            let c = self.db();
            match r {
                Ok(()) => c.execute("UPDATE followups SET done = 1 WHERE id = ?1", [id])?,
                Err(e) => {
                    let attempts = attempts + 1;
                    let error = scrub(&e);
                    let stopping = matches!(kind.as_str(), "revoke" | "stop" | "stop_all");
                    let dead = !stopping && kind != "check_publish" && attempts >= MAX_ATTEMPTS;
                    c.execute(
                        "UPDATE followups SET attempts = ?2, next_at = ?3, error = ?4, done = ?5
                         WHERE id = ?1",
                        params![
                            id,
                            attempts,
                            now + backoff(attempts),
                            error,
                            if dead { 2 } else { 0 }
                        ],
                    )?;
                    if stopping && attempts == MAX_ATTEMPTS {
                        Self::alarm(&c, &self.audit, &kind, &target, attempts, &error)?;
                    }
                    0
                }
            };
        }
        Ok(())
    }

    /// A revoke or a stop has failed `MAX_ATTEMPTS` times: say so once on
    /// the run's ticket and in the audit log (best effort), and keep trying.
    fn alarm(
        c: &Connection,
        audit: &AuditLog,
        kind: &str,
        target: &str,
        attempts: i64,
        error: &str,
    ) -> Result<()> {
        let record = json!({"kind": "stop_overdue", "call": kind, "run": target, "attempts": attempts, "error": error});
        eprintln!("{record}");
        let _ = audit.append(RUN_AUDIT, &record);
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
        Ok(())
    }

    /// The kill switch's epoch: moved on by each `resume`.
    fn epoch(c: &Connection) -> Result<i64> {
        Ok(
            c.query_row("SELECT epoch FROM control WHERE id = 1", [], |r| r.get(0))
                .optional()?
                .unwrap_or(0),
        )
    }

    fn halted(c: &Connection) -> Result<bool> {
        Ok(
            c.query_row("SELECT halted FROM control WHERE id = 1", [], |r| {
                r.get::<_, bool>(0)
            })
            .optional()?
            .unwrap_or(false),
        )
    }

    fn admit(&self, delivery: &str, ticket: &str, by: &str) -> Result<()> {
        let mut reasons = vec![];
        if !self.tracker.is_team_member(by)? {
            reasons.push(format!("{by} is not on the team"));
        }
        let issue = self.tracker.get_issue(ticket)?;
        if issue.acceptance_criteria.is_empty() {
            reasons.push("the ticket has no acceptance criteria".to_owned());
        }
        if issue.requirement_id.is_none() {
            reasons.push("the ticket has no requirement ID".to_owned());
        }
        if issue.approved_versions.is_empty() {
            reasons.push("the ticket links no approved document".to_owned());
        }
        let risk = issue.risk_class.clone().unwrap_or_default();
        if risk.is_empty() {
            reasons.push("the ticket has no risk class".to_owned());
        }
        let mut c = self.db();
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: bool = tx.query_row(
            "SELECT done = 0 FROM deliveries WHERE id = ?1",
            [delivery],
            |r| r.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        if Self::halted(&tx)? {
            reasons.push(
                "Kod is stopped by the kill switch until the owner or backup resumes it".to_owned(),
            );
        }
        if let Some((seq, _)) = live_run(&tx, ticket)? {
            reasons.push(format!(
                "the ticket already has a live run, {}",
                run_id(seq)
            ));
        }
        match &self.policy {
            Err(e) => reasons.push(format!("the policy is unavailable: {e}")),
            Ok(policy) if reasons.is_empty() => {
                let wip: i64 = tx.query_row(
                    &format!("SELECT count(*) FROM runs WHERE state IN {LIVE}"),
                    [],
                    |r| r.get(0),
                )?;
                // A bounce is a run that ended blocked: the work was sent
                // back. A failed publish ends needs_human and is not one.
                let bounces: i64 = tx.query_row(
                    "SELECT count(*) FROM runs WHERE ticket = ?1 AND state = 'blocked'",
                    [ticket],
                    |r| r.get(0),
                )?;
                let input = json!({
                    "action": "delegate",
                    "ticket": {
                        "repo": self.config.repo,
                        "risk_class": risk,
                        "acceptance_criteria": true,
                        "bounces": bounces,
                    },
                    "wip_in_progress": wip,
                });
                let decision = policy.decide(&input);
                if let Err(e) = self.audit.record_decision(&input, &decision) {
                    reasons.push(format!("the decision could not be recorded: {}", scrub(&e)));
                } else if !decision.allow {
                    reasons.extend(decision.reasons);
                }
            }
            Ok(_) => {}
        }
        if reasons.is_empty() {
            tx.execute(
                "INSERT INTO runs (ticket, revision, state) VALUES (?1, ?2, 'queued')",
                params![ticket, revision(&issue)],
            )?;
            follow(&tx, "transition", ticket, State::InProgress.name())?;
        } else {
            follow(
                &tx,
                "comment",
                ticket,
                &redact(&format!(
                    "Kod did not start this ticket: {}.",
                    reasons.join("; ")
                )),
            )?;
        }
        tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        tx.commit()?;
        Ok(())
    }

    /// Starts queued runs. Each is checked again just before its start, and
    /// one canceled or halted while starting is stopped.
    fn start_queued(&self) -> Result<()> {
        let queued: Vec<RunStart> = {
            let c = self.db();
            let mut s = c.prepare(
                "SELECT seq, ticket, revision FROM runs WHERE state = 'queued' ORDER BY seq",
            )?;
            s.query_map([], |r| {
                Ok(RunStart {
                    id: run_id(r.get(0)?),
                    ticket: r.get(1)?,
                    revision: r.get(2)?,
                    repo: self.config.repo.clone(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for run in queued {
            let seq = seq_of(&run.id)?;
            let claimed = {
                let mut c = self.db();
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if Self::halted(&tx)? {
                    return Ok(());
                }
                let claimed = tx.execute(
                    "UPDATE runs SET state = 'running' WHERE seq = ?1 AND state = 'queued'",
                    [seq],
                )?;
                tx.commit()?;
                claimed == 1
            };
            if !claimed {
                continue;
            }
            let started = self.gateway.start(&run);
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match started {
                Ok(()) => {
                    if Self::halted(&tx)? || store::stopped(&tx, &run.id)? {
                        // Canceled while starting.
                        follow(&tx, "revoke", &run.id, "")?;
                        follow(&tx, "stop", &run.id, "")?;
                    }
                }
                Err(e) => {
                    Self::end(
                        &tx,
                        seq,
                        RunState::NeedsHuman,
                        &format!("the run did not start: {}", scrub(&e)),
                        &[RunState::Running],
                    )?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Ends the run if it is in one of `from`, and queues its stop, the
    /// ticket's move and a comment. Returns whether the run ended here.
    fn end(
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
            _ => State::Blocked,
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

    /// Ends the run in its own transaction, then makes the follow-ups.
    fn end_now(&self, seq: i64, to: RunState, reason: &str, from: &[RunState]) -> Result<bool> {
        let ended = {
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ended = Self::end(&tx, seq, to, reason, from)?;
            tx.commit()?;
            ended
        };
        self.follow_up(now())?;
        Ok(ended)
    }

    /// The runner reports how a running run ended. `Done` publishes through
    /// the gateway first; a canceled run is never published, and a failed
    /// publish ends needs_human.
    pub fn finished(&self, run: &str, outcome: RunState, reason: &str) -> Result<()> {
        let seq = seq_of(run)?;
        match outcome {
            RunState::Done => {
                {
                    let mut c = self.db();
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let moved = tx.execute(
                        "UPDATE runs SET state = 'publishing' WHERE seq = ?1 AND state = 'running'",
                        [seq],
                    )?;
                    if moved == 0 {
                        bail!("{run} is not running");
                    }
                    tx.execute(
                        "INSERT INTO followups (kind, target, arg, next_at)
                         VALUES ('check_publish', ?1, '', ?2)",
                        params![run, i64::MAX],
                    )?;
                    tx.commit()?;
                }
                let published = self.gateway.publish(run);
                let (to, why) = match &published {
                    Ok(pr) => (RunState::Done, format!("{reason}; published {pr}")),
                    Err(e) => (
                        RunState::NeedsHuman,
                        format!("publish failed: {}", scrub(e)),
                    ),
                };
                {
                    let mut c = self.db();
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    Self::end(&tx, seq, to, &why, &[RunState::Publishing])?;
                    tx.execute(
                        "UPDATE followups SET next_at = 0, arg = ?2
                         WHERE kind = 'check_publish' AND target = ?1 AND done = 0",
                        params![run, published.as_ref().map_or("", String::as_str)],
                    )?;
                    tx.commit()?;
                }
                self.follow_up(now())?;
                self.follow_up(now())?;
                Ok(())
            }
            RunState::Blocked | RunState::NeedsHuman | RunState::OverBudget => {
                if !self.end_now(seq, outcome, reason, &[RunState::Running])? {
                    bail!("{run} is not running");
                }
                Ok(())
            }
            _ => bail!("{} is not how a run ends", outcome.name()),
        }
    }

    fn reconcile_publish(&self, followup: i64, run: &str, known_pr: &str) -> Result<()> {
        let (start, canceled) = self.db().query_row(
            "SELECT ticket, revision, state = 'canceled' FROM runs WHERE seq = ?1",
            [seq_of(run)?],
            |r| {
                Ok((
                    RunStart {
                        id: run.to_owned(),
                        ticket: r.get(0)?,
                        revision: r.get(1)?,
                        repo: self.config.repo.clone(),
                    },
                    r.get::<_, bool>(2)?,
                ))
            },
        )?;
        if !canceled {
            return Ok(());
        }
        let pr = if known_pr.is_empty() {
            self.gateway.find_published(&start)?
        } else {
            Some(known_pr.to_owned())
        };
        let Some(pr) = pr else {
            return Ok(());
        };
        let recorded = {
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let recorded = tx.execute(
                "UPDATE followups SET done = 1 WHERE id = ?1 AND done = 0",
                [followup],
            )? == 1;
            if recorded {
                follow(
                    &tx,
                    "comment",
                    &start.ticket,
                    &format!(
                        "Kod {run} was canceled while it published. Its draft PR {pr} is not Kod's work: close it, do not merge it."
                    ),
                )?;
            }
            tx.commit()?;
            recorded
        };
        if recorded {
            let _ = self.audit.append(
                RUN_AUDIT,
                &json!({"kind": "canceled_after_publish", "run": run, "pr": pr}),
            );
        }
        Ok(())
    }

    /// Whether the run must stop: what the gateway's `Stopped` check asks.
    pub fn stopped(&self, run_id: &str) -> Result<bool> {
        store::stopped(&self.db(), run_id)
    }

    pub fn is_halted(&self) -> Result<bool> {
        Self::halted(&self.db())
    }

    pub fn runs(&self) -> Result<Vec<RunRow>> {
        let c = self.db();
        let mut s =
            c.prepare("SELECT seq, ticket, revision, state, reason FROM runs ORDER BY seq")?;
        let rows = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, String>(3)?,
                    r.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(seq, ticket, revision, state, reason)| {
                Ok(RunRow {
                    id: run_id(seq),
                    ticket,
                    revision,
                    state: RunState::parse(&state)?,
                    reason,
                })
            })
            .collect()
    }
}

/// The routes: `POST /webhook` answers 202 queued, 200 duplicate or
/// ignored, 401 refused, 500 when the store fails (the tracker retries).
pub fn router(dispatcher: Arc<Dispatcher>) -> Router {
    Router::new()
        .route("/webhook", post(webhook))
        .with_state(dispatcher)
}

async fn webhook(
    Shared(d): Shared<Arc<Dispatcher>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let w = Webhook {
        headers: headers
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_owned())))
            .collect(),
        body: body.to_vec(),
    };
    match tokio::task::spawn_blocking(move || d.receive(&w)).await {
        Ok(Ok(Received::Queued)) => (StatusCode::ACCEPTED, "queued".into()),
        Ok(Ok(Received::Duplicate)) => (StatusCode::OK, "duplicate".into()),
        Ok(Ok(Received::Ignored)) => (StatusCode::OK, "ignored".into()),
        Ok(Ok(Received::Refused(_))) => (StatusCode::UNAUTHORIZED, "refused".into()),
        Ok(Err(_)) | Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "error".into()),
    }
}
