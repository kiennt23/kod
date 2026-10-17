#![allow(dead_code)]
use kod_audit::AuditLog;
use kod_policy::Policy;
use kod_tool_gateway::{Adapters, Gateway, Run, docs::FakeDocs};
use kod_tracker::fake::{self, FakeIssue, FakeTracker};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const LINKED_DOC: &str = "https://docs.example/brief?rev=3";
pub const SECRET_DOC: &str = "https://docs.example/payroll?rev=1";
/// Text planted where the run may not read: an unlinked ticket and doc.
pub const PLANTED: &str = "PLANTED-SECRET-7f3a";
/// A rework request Kod linked to FAKE-1 before run-1 started.
pub const REWORK: &str = "FAKE-3";
/// A ticket a human linked to FAKE-1.
pub const HUMAN_LINK: &str = "FAKE-2";

/// The dispatcher's runs table, as the gateway sees it: the stopped runs,
/// and whether reading it fails.
#[derive(Default)]
pub struct Runs {
    pub stopped: Mutex<HashSet<String>>,
    pub broken: AtomicBool,
}

pub struct Harness {
    pub gw: Arc<Gateway>,
    pub tracker: Arc<FakeTracker>,
    pub docs: Arc<FakeDocs>,
    pub runs: Arc<Runs>,
    pub audit: PathBuf,
    _dir: tempfile::TempDir,
}

pub fn run() -> Run {
    Run {
        id: "run-1".into(),
        ticket: "FAKE-1".into(),
        repo: "acme/app".into(),
        base: BASE.into(),
        data_classes: vec!["placeholder".into()],
        linked_tickets: vec![REWORK.into()],
        started_at: 1_700_000_000,
    }
}

/// A gateway over the fakes with run-1 started under TOKEN on FAKE-1, which
/// is in progress. FAKE-1 links HUMAN_LINK and REWORK; FAKE-9 and
/// SECRET_DOC are not linked and hold PLANTED.
pub fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(Policy::load(&root.join("policy")).unwrap());
    let audit = dir.path().join("audit.db");
    let log = Arc::new(AuditLog::open(&audit).unwrap());
    let (tracker, _) = fake::sandbox();
    let mut story = tracker.issue("FAKE-1").unwrap();
    story.links = vec![HUMAN_LINK.into(), REWORK.into()];
    story.workflow_state = "Doing".into();
    tracker.add_issue("FAKE-1", story);
    for (key, body) in [
        (HUMAN_LINK, "an epic a human linked"),
        (REWORK, "a question Kod asked"),
        ("FAKE-9", PLANTED),
    ] {
        let issue = FakeIssue {
            title: key.into(),
            body: body.into(),
            reporter: "po".into(),
            workflow_state: "Backlog".into(),
            ..FakeIssue::default()
        };
        tracker.add_issue(key, issue);
    }
    let tracker = Arc::new(tracker);
    let docs = Arc::new(FakeDocs::default());
    docs.add(LINKED_DOC, "the approved brief");
    docs.add(SECRET_DOC, PLANTED);
    let runs = Arc::new(Runs::default());
    let r = runs.clone();
    let stopped = Arc::new(move |run: &str| {
        if r.broken.load(Ordering::SeqCst) {
            anyhow::bail!("database is locked");
        }
        Ok(r.stopped.lock().unwrap().contains(run))
    });
    let adapters = Adapters {
        tracker: tracker.clone(),
        docs: docs.clone(),
        stopped,
    };
    let gw = Arc::new(Gateway::new(policy, log, adapters).unwrap());
    gw.start_run(TOKEN, run()).unwrap();
    Harness {
        gw,
        tracker,
        docs,
        runs,
        audit,
        _dir: dir,
    }
}

impl Harness {
    /// Cancels the run in the dispatcher's database.
    pub fn stop(&self, run: &str) {
        self.runs.stopped.lock().unwrap().insert(run.into());
    }

    /// Every audit payload, oldest first.
    pub fn records(&self) -> Vec<serde_json::Value> {
        let db = rusqlite::Connection::open(&self.audit).unwrap();
        let mut q = db
            .prepare("SELECT payload FROM audit ORDER BY seq")
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }

    /// The policy decisions recorded, as (allow, input).
    pub fn decisions(&self) -> Vec<(bool, serde_json::Value)> {
        self.records()
            .into_iter()
            .filter(|r| r["kind"] == "policy_decision")
            .map(|r| (r["decision"]["allow"] == true, r["input"].clone()))
            .collect()
    }

    /// The audit log's raw bytes, as text.
    pub fn raw_log(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(&self.audit).unwrap()).into_owned()
    }
}
