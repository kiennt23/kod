//! The durable guard around the audited status write.
use crate::{config::FsConfig, deadlines, records};
use anyhow::{Context, Result, bail};
use kod_status::records::{Records, ReworkRecord, Source};
use kod_tracker::github::GitHub;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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

/// Resolves an unresolved status intent from the one owned remote comment. Zero or duplicate
/// owned comments leave it unresolved: an empty list never proves a create did not land.
pub fn reconcile_status(
    c: &mut Connection,
    story: &str,
    owned: impl FnOnce() -> Result<Option<kod_tracker::github::OwnedStatusComment>>,
) -> Result<()> {
    let pending: Option<String> = c
        .query_row(
            "SELECT operation_id FROM tracker_write_intents WHERE story=?1 AND kind='status' AND outcome IS NULL",
            [story],
            |r| r.get(0),
        )
        .optional()?;
    let Some(operation) = pending else {
        return Ok(());
    };
    let comment = owned()?.context("status write unresolved; no owned comment found")?;
    c.execute(
        "UPDATE tracker_write_intents SET outcome='reconciled', remote_id=?2 WHERE operation_id=?1",
        params![operation, comment.id.to_string()],
    )?;
    Ok(())
}

/// The dispatcher's rework records for a story. The dispatcher keeps no resolved rework
/// records yet, so an empty table is a real "none"; any row (an unresolved or resolved create)
/// cannot be rendered and fails the refresh, as does an unreadable table. Never `vec![]` by default.
pub fn rework_records(c: &Connection, story: &str) -> Result<Vec<ReworkRecord>> {
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM tracker_write_intents WHERE story=?1 AND kind='rework'",
            [story],
            |r| r.get(0),
        )
        .context("rework records unavailable")?;
    if n > 0 {
        bail!("rework records unavailable; reconcile {n} rework create(s) for {story}");
    }
    Ok(vec![])
}

/// What the host can truthfully put in a story's status: tracker facts, the pinned approved
/// artifacts, dispatcher runs and rework. Pull evidence (team, policy and runner-config versions)
/// has no host source and the product token cannot read check runs, so a story with an open agent
/// pull request fails closed rather than rendering "no pull requests". Merged or closed pulls are
/// not listed by the live source (ponytail: closed-PR history needs a recorded pull table).
pub struct WorkflowSource {
    pub tracker: Arc<dyn kod_tracker::Tracker + Send + Sync>,
    pub artifacts: Arc<crate::artifacts::Artifacts>,
    pub dispatcher_db: PathBuf,
    pub scratch: PathBuf,
    pub open: Arc<dyn Fn() -> Result<Vec<String>> + Send + Sync>,
}

impl Source for WorkflowSource {
    fn load(&self, story: &str) -> Result<Records> {
        for branch in (self.open)()? {
            if records::story_of_branch(&branch)? == story {
                bail!("open pull request evidence is unavailable for {story}");
            }
        }
        // Merged/closed pulls are not recorded anywhere, so a story whose run reached publishing
        // or done cannot show real PR evidence: fail closed instead of rendering "none".
        let published: bool = Connection::open_with_flags(
            &self.dispatcher_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE ticket=?1 AND state IN ('publishing','done')
                OR EXISTS(SELECT 1 FROM followups f WHERE f.kind='check_publish' AND f.target='run-'||runs.seq))",
            [story],
            |r| r.get(0),
        )?;
        if published {
            bail!("pull request evidence is unavailable for {story}: a run entered publishing");
        }
        let mut stages = std::collections::BTreeMap::new();
        for a in self.artifacts.current_manifest()?.artifacts {
            let stage: kod_status::Stage = serde_json::from_value(a.class.clone().into())
                .with_context(|| format!("artifact class {} has no stage", a.class))?;
            stages.insert(a.link, stage);
        }
        let c = Connection::open_with_flags(
            &self.dispatcher_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        c.busy_timeout(kod_dispatcher::store::BUSY_TIMEOUT)?;
        let snap = records::snapshot(
            self.tracker.as_ref(),
            &self.artifacts,
            story,
            &|l| {
                stages
                    .get(l)
                    .copied()
                    .unwrap_or(kod_status::Stage::Requirements)
            },
            vec![],
            rework_records(&c, story)?,
        )?;
        let file = self
            .scratch
            .join(format!("{}.json", records::story_lock_name(story)?));
        records::write_atomic(&file, &serde_json::to_vec(&snap)?)?;
        kod_status::records::HostRecords {
            snapshot: file,
            dispatcher: self.dispatcher_db.clone(),
        }
        .load(story)
    }
}

/// Stories the host owns: every ticket with a dispatcher run.
pub fn stories(c: &Connection) -> Result<Vec<String>> {
    c.prepare("SELECT DISTINCT ticket FROM runs ORDER BY ticket")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()
        .map_err(Into::into)
}

/// A detached refresh thread. `stop` only raises the flag: a status call stuck on the network
/// cannot delay shutdown, and the thread ends at its next poll.
pub struct Refresher(Arc<AtomicBool>);
impl Refresher {
    pub fn spawn(
        stories: impl Fn() -> Vec<String> + Send + 'static,
        step: impl Fn(&str) -> Result<()> + Send + 'static,
        interval: Duration,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || run_loop(stories, step, interval, &s));
        Self(stop)
    }
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A source that fails after `limit` instead of serving anything stale.
pub struct Timed {
    inner: Arc<dyn Source + Send + Sync>,
    limit: Duration,
}

impl Timed {
    pub fn new(inner: Arc<dyn Source + Send + Sync>, limit: Duration) -> Self {
        Self { inner, limit }
    }
    pub fn production(inner: Arc<dyn Source + Send + Sync>) -> Self {
        Self::new(inner, deadlines::SOURCE)
    }
}

impl Source for Timed {
    fn load(&self, story: &str) -> Result<Records> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (inner, story) = (self.inner.clone(), story.to_owned());
        std::thread::spawn(move || {
            let _ = tx.send(inner.load(&story));
        });
        rx.recv_timeout(self.limit)
            .map_err(|_| anyhow::anyhow!("workflow source timed out"))?
    }
}

/// One audited, serialized refresh: the gates lock and the story lock are held from load through
/// write, so it cannot interleave with `records::activate`.
pub fn refresh_story(
    config: &FsConfig,
    tracker: &GitHub,
    source: &dyn Source,
    audit: &kod_audit::AuditLog,
    c: &mut Connection,
    story: &str,
) -> Result<()> {
    let suffix = |s: &str| {
        let mut p = config.paths.audit_db.as_os_str().to_owned();
        p.push(s);
        PathBuf::from(p)
    };
    let _gates = records::lock(records::gates_lock_path(config))?;
    let _story = records::lock(suffix(&format!(
        ".refresh.{}.lock",
        records::story_lock_name(story)?
    )))?;
    reconcile_status(c, story, || tracker.owned_status_comment(story))?;
    kod_status::refresh_with_writer(source, tracker, audit, story, |key, text, intent| {
        use kod_tracker::Tracker;
        guarded_status_write(c, story, intent, || {
            tracker
                .status_comment(key, text)
                .map_err(WriteFailure::from_tracker)
        })
    })?;
    Ok(())
}

/// Recomputes at once and then every `interval` until `stop`. One story failing never stops the
/// others or the loop; the next pass retries it.
/// Stories whose refresh is blocked, so a persistent block is logged and audited once, not every tick.
#[derive(Default)]
pub struct BlockedLog(std::sync::Mutex<std::collections::HashMap<String, String>>);
impl BlockedLog {
    pub fn record(&self, audit: &kod_audit::AuditLog, story: &str, error: &anyhow::Error) {
        let error = kod_tool_gateway::scrub(&format!("{error:#}"));
        let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if seen.get(story) == Some(&error) {
            return;
        }
        eprintln!("status refresh blocked for {story}: {error}");
        let _ = audit.append(
            kod_audit::RUN_AUDIT,
            &serde_json::json!({"kind": "status_refresh_blocked", "story": story, "error": error}),
        );
        seen.insert(story.into(), error);
    }
    pub fn clear(&self, story: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(story);
    }
}

pub fn run_loop(
    stories: impl Fn() -> Vec<String>,
    step: impl Fn(&str) -> Result<()>,
    interval: Duration,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::SeqCst) {
        for s in stories() {
            let _ = step(&s);
        }
        let until = std::time::Instant::now() + interval;
        while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10).min(interval));
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
