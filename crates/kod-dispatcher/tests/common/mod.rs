#![allow(dead_code)]
use anyhow::{Result, bail};
use kod_audit::AuditLog;
use kod_dispatcher::{Config, Dispatcher, Gateway, RunStart};
use kod_policy::Policy;
use kod_tracker::{
    Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook,
    fake::{self, FakeIssue, FakeTracker},
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const CONFIG: &str = r#"{"repo": "acme/app", "owner": "kiennt23", "backup": null}"#;

/// The owner's key for the signed CLI.
pub const OWNER_KEY: &[u8] = b"owner-cli-key";

/// The time the tests run at, in unix seconds.
pub const NOW: i64 = 1_800_000_000;

/// Records what the dispatcher asked of the gateway.
pub struct FakeGateway {
    pub calls: Mutex<Vec<String>>,
    pub fail_start: AtomicBool,
    pub fail_publish: AtomicBool,
    pub fail_stop_all: AtomicBool,
    pub fail_stop: AtomicBool,
    /// What `find_published` answers; `None` inside is "no PR".
    pub published: Mutex<Result<Option<String>, String>>,
    /// The runs whose credentials were revoked, kept apart from `calls`.
    pub revoked: Mutex<Vec<String>>,
    /// Runs inside `publish`, after the gateway's last check.
    pub on_publish: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl FakeGateway {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn revoked(&self) -> Vec<String> {
        self.revoked.lock().unwrap().clone()
    }

    fn record(&self, call: String, fail: &AtomicBool) -> Result<()> {
        self.calls.lock().unwrap().push(call);
        if fail.load(Ordering::SeqCst) {
            bail!("fake gateway: down");
        }
        Ok(())
    }
}

impl Gateway for FakeGateway {
    fn start(&self, run: &RunStart) -> Result<()> {
        self.record(
            format!("start {} {} {}", run.id, run.ticket, run.repo),
            &self.fail_start,
        )
    }

    fn revoke(&self, run_id: &str) -> Result<()> {
        self.revoked.lock().unwrap().push(run_id.to_owned());
        Ok(())
    }

    fn stop(&self, run_id: &str) -> Result<()> {
        self.record(format!("stop {run_id}"), &self.fail_stop)
    }

    fn stop_all(&self) -> Result<()> {
        self.record("stop_all".into(), &self.fail_stop_all)
    }

    fn publish(&self, run_id: &str) -> Result<String> {
        self.record(format!("publish {run_id}"), &self.fail_publish)?;
        if let Some(f) = &*self.on_publish.lock().unwrap() {
            f();
        }
        Ok("https://github.example/acme/app/pull/1".into())
    }

    fn find_published(&self, run: &RunStart) -> Result<Option<String>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("find_published {} {}", run.id, run.ticket));
        self.published
            .lock()
            .unwrap()
            .clone()
            .map_err(anyhow::Error::msg)
    }
}

impl Default for FakeGateway {
    fn default() -> FakeGateway {
        FakeGateway {
            calls: Mutex::default(),
            fail_start: AtomicBool::default(),
            fail_publish: AtomicBool::default(),
            fail_stop_all: AtomicBool::default(),
            fail_stop: AtomicBool::default(),
            published: Mutex::new(Ok(None)),
            revoked: Mutex::default(),
            on_publish: Mutex::default(),
        }
    }
}

/// The fake tracker, with faults to switch on.
pub struct Flaky {
    pub t: Arc<FakeTracker>,
    /// `transition` and `comment` fail.
    pub fail_writes: AtomicBool,
    /// `get_issue` fails for this key.
    pub fail_issue: Mutex<Option<String>>,
    /// Takes the delivery ID from an unsigned `x-delivery` header, as a
    /// careless adapter might.
    pub header_ids: AtomicBool,
}

impl Flaky {
    fn writes(&self) -> Result<()> {
        if self.fail_writes.load(Ordering::SeqCst) {
            bail!("fake tracker: down");
        }
        Ok(())
    }
}

impl Tracker for Flaky {
    fn name(&self) -> &str {
        self.t.name()
    }
    fn get_issue(&self, key: &str) -> Result<Issue> {
        if self.fail_issue.lock().unwrap().as_deref() == Some(key) {
            bail!("fake tracker: {key} timed out");
        }
        self.t.get_issue(key)
    }
    fn comment(&self, key: &str, text: &str) -> Result<()> {
        self.writes()?;
        self.t.comment(key, text)
    }
    fn transition(&self, key: &str, state: State) -> Result<()> {
        self.writes()?;
        self.t.transition(key, state)
    }
    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        self.t.link_pr(key, url)
    }
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
        self.t.create_linked(story, kind, fields)
    }
    fn is_team_member(&self, user: &str) -> Result<bool> {
        self.t.is_team_member(user)
    }
    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>> {
        let d = self.t.parse_webhook(request)?;
        if !self.header_ids.load(Ordering::SeqCst) {
            return Ok(d);
        }
        Ok(d.map(|d| Delivery {
            id: request.header("x-delivery").unwrap_or_default().to_owned(),
            ..d
        }))
    }
}

pub struct Harness {
    pub d: Arc<Dispatcher>,
    pub tracker: Arc<FakeTracker>,
    pub flaky: Arc<Flaky>,
    pub gw: Arc<FakeGateway>,
    pub db: PathBuf,
    pub audit: PathBuf,
    pub dir: tempfile::TempDir,
}

pub fn policy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

/// A copy of the repo's policy with `people.json` replaced.
pub fn policy_with_people(dir: &Path, people: serde_json::Value) -> PathBuf {
    let out = dir.join("policy");
    std::fs::create_dir_all(&out).unwrap();
    for e in std::fs::read_dir(policy_dir()).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), out.join(e.file_name())).unwrap();
    }
    std::fs::write(out.join("people.json"), people.to_string()).unwrap();
    out
}

/// A second ticket as complete as the sandbox's FAKE-1.
pub fn ready_ticket(t: &FakeTracker, key: &str) {
    let mut i = t.issue("FAKE-1").unwrap();
    i.title = format!("Story {key}");
    t.add_issue(key, i);
}

pub fn dispatcher(
    dir: &Path,
    tracker: Arc<dyn Tracker>,
    gw: Arc<FakeGateway>,
    policy: Result<Policy>,
) -> Arc<Dispatcher> {
    let audit = Arc::new(AuditLog::open(&dir.join("audit.db")).unwrap());
    let d = Dispatcher::open(
        Config::parse(CONFIG).unwrap(),
        &dir.join("dispatcher.db"),
        policy.map(Arc::new),
        audit,
        tracker,
        gw,
    )
    .unwrap();
    let d = d.with_cli_key("kiennt23", OWNER_KEY);
    Arc::new(d)
}

pub fn harness_with(policy: impl FnOnce(&Path) -> Result<Policy>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let (t, _) = fake::sandbox();
    let tracker = Arc::new(t);
    ready_ticket(&tracker, "FAKE-2");
    tracker.add_issue(
        "FAKE-3",
        FakeIssue {
            title: "No criteria".into(),
            workflow_state: "Open".into(),
            ..FakeIssue::default()
        },
    );
    let gw = Arc::new(FakeGateway::default());
    let flaky = Arc::new(Flaky {
        t: tracker.clone(),
        fail_writes: AtomicBool::default(),
        fail_issue: Mutex::default(),
        header_ids: AtomicBool::default(),
    });
    let d = dispatcher(dir.path(), flaky.clone(), gw.clone(), policy(dir.path()));
    Harness {
        d,
        tracker,
        flaky,
        gw,
        db: dir.path().join("dispatcher.db"),
        audit: dir.path().join("audit.db"),
        dir,
    }
}

pub fn harness() -> Harness {
    harness_with(|_| Policy::load(&policy_dir()))
}

pub fn label(t: &FakeTracker, delivery: &str, issue: &str, user: &str) -> Webhook {
    t.webhook(&json!({"delivery": delivery, "type": "label_added", "issue": issue, "label": "kod", "user": user}))
}

pub fn unlabel(t: &FakeTracker, delivery: &str, issue: &str, user: &str) -> Webhook {
    t.webhook(&json!({"delivery": delivery, "type": "label_removed", "issue": issue, "label": "kod", "user": user}))
}

pub fn edited(t: &FakeTracker, delivery: &str, issue: &str) -> Webhook {
    t.webhook(&json!({"delivery": delivery, "type": "issue_edited", "issue": issue}))
}

pub fn said(t: &FakeTracker, delivery: &str, issue: &str, user: &str, body: &str) -> Webhook {
    t.webhook(&json!({"delivery": delivery, "type": "comment", "issue": issue, "user": user, "body": body}))
}

/// A signed CLI request and its signature.
pub fn cli(command: &str, by: &str, at: i64, key: &[u8]) -> (Vec<u8>, String) {
    let body = json!({"command": command, "by": by, "at": at}).to_string();
    let sig = kod_dispatcher::command::sign(key, body.as_bytes());
    (body.into_bytes(), sig)
}

impl Harness {
    /// Receives the webhook and works the queue until it is empty.
    pub fn deliver(&self, w: &Webhook) {
        self.d.receive(w).unwrap();
        while self.d.work_once().unwrap() {}
    }

    /// Admits and starts run-1 for FAKE-1.
    pub fn running(&self) {
        self.deliver(&label(&self.tracker, "d-1", "FAKE-1", "dev"));
        assert_eq!(self.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    }

    pub fn states(&self) -> Vec<(String, &'static str)> {
        self.d
            .runs()
            .unwrap()
            .into_iter()
            .map(|r| (r.id, r.state.name()))
            .collect()
    }

    pub fn workflow(&self, key: &str) -> String {
        self.tracker.issue(key).unwrap().workflow_state
    }

    pub fn last_comment(&self, key: &str) -> String {
        self.tracker
            .comments(key)
            .last()
            .cloned()
            .unwrap_or_default()
    }

    /// Puts a queued run straight into the store, as an admission that
    /// committed just before the kill switch or a cancel arrived.
    pub fn queued(&self, ticket: &str) {
        self.sql(&format!(
            "INSERT INTO runs (ticket, revision, state) VALUES ('{ticket}', 'r', 'queued')"
        ));
    }

    pub fn sql(&self, sql: &str) {
        let c = kod_dispatcher::store::open(&self.db).unwrap();
        c.execute_batch(sql).unwrap();
    }

    pub fn count(&self, sql: &str) -> i64 {
        let c = kod_dispatcher::store::open(&self.db).unwrap();
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// A second dispatcher on the same store, as after a restart.
    pub fn reopen(&self) -> Arc<Dispatcher> {
        dispatcher(
            self.dir.path(),
            self.flaky.clone(),
            self.gw.clone(),
            Policy::load(&policy_dir()),
        )
    }

    /// Delivers a signed comment from `user` on FAKE-1.
    pub fn say(&self, delivery: &str, user: &str, body: &str) {
        self.deliver(&said(&self.tracker, delivery, "FAKE-1", user, body));
    }
}

/// Keeps `Tracker` in scope for tests that call it on the fake.
pub fn issue(t: &FakeTracker, key: &str) -> kod_tracker::Issue {
    t.get_issue(key).unwrap()
}
