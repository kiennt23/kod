mod common;

use kod_audit::AuditLog;
use kod_status::{
    records::{Records, Snapshot, Source},
    refresh,
};
use kod_tracker::{
    Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook,
    fake::{FakeTracker, sandbox},
};
use serde_json::Value;
use std::path::{Path, PathBuf};

struct OutcomeTracker {
    inner: FakeTracker,
    error: Option<String>,
    remove_audit: Option<PathBuf>,
}

impl Tracker for OutcomeTracker {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn get_issue(&self, key: &str) -> anyhow::Result<Issue> {
        self.inner.get_issue(key)
    }
    fn comment(&self, key: &str, text: &str) -> anyhow::Result<()> {
        self.inner.comment(key, text)
    }
    fn transition(&self, key: &str, state: State) -> anyhow::Result<()> {
        self.inner.transition(key, state)
    }
    fn link_pr(&self, key: &str, url: &str) -> anyhow::Result<()> {
        self.inner.link_pr(key, url)
    }
    fn create_linked(
        &self,
        story: &str,
        kind: ReworkKind,
        fields: &Rework,
    ) -> anyhow::Result<String> {
        self.inner.create_linked(story, kind, fields)
    }
    fn is_team_member(&self, user: &str) -> anyhow::Result<bool> {
        self.inner.is_team_member(user)
    }
    fn parse_webhook(&self, request: &Webhook) -> anyhow::Result<Option<Delivery>> {
        self.inner.parse_webhook(request)
    }
    fn status_comment(&self, key: &str, text: &str) -> anyhow::Result<()> {
        let result = match &self.error {
            Some(error) => Err(anyhow::anyhow!(error.clone())),
            None => self.inner.status_comment(key, text),
        };
        if let Some(path) = &self.remove_audit {
            std::fs::remove_file(path)?;
        }
        result
    }
}

struct Memory(Snapshot);
impl Source for Memory {
    fn load(&self, _: &str) -> anyhow::Result<Records> {
        Ok(Records {
            snapshot: self.0.clone(),
            runs: vec![],
        })
    }
}

fn events(path: &Path) -> Vec<Value> {
    rusqlite::Connection::open(path)
        .unwrap()
        .prepare("SELECT payload FROM audit ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}

#[test]
fn successful_comment_has_an_intent_and_a_confirmed_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let audit = AuditLog::open(&path).unwrap();
    let (tracker, _) = sandbox();
    refresh(&Memory(common::snapshot()), &tracker, &audit, "FAKE-1").unwrap();
    let records = events(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "workflow_status_comment_intent");
    assert_eq!(records[1]["kind"], "workflow_status_comment_posted");
    assert_eq!(records[1]["intent"], 1);
    assert_eq!(tracker.comments("FAKE-1").len(), 1);
}

#[test]
fn unsupported_tracker_records_failure_instead_of_a_posted_comment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let audit = AuditLog::open(&path).unwrap();
    let snapshot_path = dir.path().join("gate-records.json");
    std::fs::write(
        &snapshot_path,
        serde_json::to_vec(&kod_gates::records::Snapshot {
            story: "FAKE-1".into(),
            requester: "po".into(),
            required: vec!["design".into()],
            current: [("design".into(), "design-v3".into())].into(),
        })
        .unwrap(),
    )
    .unwrap();
    let tracker = kod_gates::records::HostRecords(snapshot_path);
    assert!(refresh(&Memory(common::snapshot()), &tracker, &audit, "FAKE-1").is_err());
    let records = events(&path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "workflow_status_comment_intent");
    assert_eq!(records[1]["kind"], "workflow_status_comment_failed");
    assert_eq!(records[1]["intent"], 1);
    assert!(
        records[1]["error"]
            .as_str()
            .unwrap()
            .contains("does not support status comments")
    );
}

#[test]
fn failed_comment_outcomes_and_errors_are_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let audit = AuditLog::open(&path).unwrap();
    let tracker = OutcomeTracker {
        inner: sandbox().0,
        error: Some("remote failure: api_key=super-secret".into()),
        remove_audit: None,
    };
    let error = refresh(&Memory(common::snapshot()), &tracker, &audit, "FAKE-1").unwrap_err();
    assert!(!format!("{error:#}").contains("super-secret"));
    let records = events(&path);
    assert_eq!(records[1]["kind"], "workflow_status_comment_failed");
    assert!(!records[1].to_string().contains("super-secret"));
    assert!(tracker.inner.comments("FAKE-1").is_empty());
}

#[test]
fn an_outcome_audit_failure_is_explicit_after_a_remote_attempt() {
    for error in [
        None,
        Some("remote failure: api_key=super-secret".to_owned()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let audit = AuditLog::open(&path).unwrap();
        let failed = error.is_some();
        let tracker = OutcomeTracker {
            inner: sandbox().0,
            error,
            remove_audit: Some(path),
        };
        let error = refresh(&Memory(common::snapshot()), &tracker, &audit, "FAKE-1").unwrap_err();
        let text = format!("{error:#}");
        assert!(!text.contains("super-secret"));
        assert!(
            text.contains(if failed {
                "recording failed comment outcome"
            } else {
                "comment posted but recording outcome failed"
            }),
            "{text}"
        );
        assert_eq!(tracker.inner.comments("FAKE-1").len(), usize::from(!failed));
    }
}
