//! An in-memory tracker for other sub-plans' tests. It keeps issues in
//! workflow states and custom fields, as a real tracker does, and reads them
//! through a `Mapping`, so it exercises the mapping too.
use crate::{
    Delivery, Issue, Rework, ReworkKind, State, TaskEvent, Tracker, Webhook, contract::Sandbox,
    mapping::Mapping,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Mutex,
};
use subtle::ConstantTimeEq;

/// The fake's mapping, in the registry format.
pub const FAKE_MAPPING: &str = r#"{
  "tracker": "fake",
  "states": {
    "ready": ["Open", "Backlog"],
    "in_progress": ["Doing"],
    "in_review": ["Review"],
    "blocked": ["Waiting"],
    "done": ["Closed"]
  },
  "fields": {"requirement_id": "req", "risk_class": "risk"},
  "trigger_label": "kod",
  "team_group": "kod-team"
}"#;

/// The signature header: hex HMAC-SHA256 of the body, keyed by the secret.
/// The body's "delivery" field is the delivery ID, so it is signed too.
pub const SIGNATURE: &str = "x-fake-signature";

/// HMAC-SHA256 (RFC 2104) over sha2, which the workspace already has.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// An issue as the fake stores it: tracker names, not standard ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FakeIssue {
    pub title: String,
    pub body: String,
    pub acceptance_criteria: Vec<String>,
    pub reporter: String,
    pub workflow_state: String,
    /// Custom field name to value.
    pub fields: BTreeMap<String, String>,
    pub approved_versions: Vec<String>,
    /// Keys of linked issues.
    pub links: Vec<String>,
}

#[derive(Default)]
struct Inner {
    issues: BTreeMap<String, FakeIssue>,
    comments: Vec<(String, String)>,
    prs: Vec<(String, String)>,
    members: HashSet<String>,
    created: u64,
    /// Delivery IDs of the webhooks accepted so far.
    deliveries: HashSet<String>,
}

pub struct FakeTracker {
    mapping: Mapping,
    secret: String,
    inner: Mutex<Inner>,
}

impl FakeTracker {
    pub fn new(mapping: Mapping, secret: &str) -> FakeTracker {
        FakeTracker {
            mapping,
            secret: secret.to_owned(),
            inner: Mutex::default(),
        }
    }

    pub fn add_issue(&self, key: &str, issue: FakeIssue) {
        self.lock().issues.insert(key.to_owned(), issue);
    }

    /// Adds a user to the mapping's team group.
    pub fn add_member(&self, user: &str) {
        self.lock().members.insert(user.to_owned());
    }

    /// The issue as stored, for tests that check what an adapter call wrote.
    pub fn issue(&self, key: &str) -> Option<FakeIssue> {
        self.lock().issues.get(key).cloned()
    }

    pub fn comments(&self, key: &str) -> Vec<String> {
        self.recorded(|i| &i.comments, key)
    }

    pub fn prs(&self, key: &str) -> Vec<String> {
        self.recorded(|i| &i.prs, key)
    }

    /// A webhook with this JSON body and a valid signature.
    pub fn webhook(&self, body: &Value) -> Webhook {
        let body = body.to_string().into_bytes();
        Webhook {
            headers: vec![(SIGNATURE.into(), self.sign(&body))],
            body,
        }
    }

    /// The signature header value for this body.
    pub fn sign(&self, body: &[u8]) -> String {
        hmac_sha256(self.secret.as_bytes(), body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recorded(&self, list: fn(&Inner) -> &Vec<(String, String)>, key: &str) -> Vec<String> {
        let inner = self.lock();
        list(&inner)
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Runs `f` on an existing issue.
    fn with_issue<T>(&self, key: &str, f: impl FnOnce(&mut Inner) -> T) -> Result<T> {
        let mut inner = self.lock();
        if !inner.issues.contains_key(key) {
            bail!("fake: no issue {key}");
        }
        Ok(f(&mut inner))
    }
}

impl Tracker for FakeTracker {
    fn name(&self) -> &str {
        &self.mapping.tracker
    }

    fn get_issue(&self, key: &str) -> Result<Issue> {
        let i = self
            .issue(key)
            .with_context(|| format!("fake: no issue {key}"))?;
        let state = self.mapping.standard(&i.workflow_state).with_context(|| {
            format!(
                "fake: {key} is in workflow state {:?}, which the mapping does not map",
                i.workflow_state
            )
        })?;
        Ok(Issue {
            key: key.to_owned(),
            requirement_id: i.fields.get(&self.mapping.requirement_id_field).cloned(),
            risk_class: i.fields.get(&self.mapping.risk_class_field).cloned(),
            title: i.title,
            body: i.body,
            acceptance_criteria: i.acceptance_criteria,
            reporter: i.reporter,
            approved_versions: i.approved_versions,
            links: i.links,
            state,
        })
    }

    fn comment(&self, key: &str, text: &str) -> Result<()> {
        self.with_issue(key, |i| i.comments.push((key.into(), text.into())))
    }

    fn transition(&self, key: &str, state: State) -> Result<()> {
        let target = self.mapping.target(state).to_owned();
        self.with_issue(key, |i| {
            i.issues.get_mut(key).unwrap().workflow_state = target;
        })
    }

    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        self.with_issue(key, |i| i.prs.push((key.into(), url.into())))
    }

    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
        let ready = self.mapping.target(State::Ready).to_owned();
        self.with_issue(story, |i| {
            i.created += 1;
            let key = format!("REWORK-{}", i.created);
            let body = format!(
                "kind: {kind:?}\nstage: {}\nartifact: {} at {}\n\n{}\n\nevidence:\n{}",
                fields.stage,
                fields.artifact,
                fields.version,
                fields.text,
                fields.evidence.join("\n")
            );
            let issue = FakeIssue {
                title: format!("Rework for {story}: {}", fields.artifact),
                body,
                workflow_state: ready,
                links: vec![story.to_owned()],
                ..FakeIssue::default()
            };
            i.issues.insert(key.clone(), issue);
            i.issues.get_mut(story).unwrap().links.push(key.clone());
            key
        })
    }

    fn is_team_member(&self, user: &str) -> Result<bool> {
        Ok(self.lock().members.contains(user))
    }

    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>> {
        let sig = request
            .header(SIGNATURE)
            .context("fake: the webhook has no signature")?;
        let want = self.sign(&request.body);
        if !bool::from(sig.as_bytes().ct_eq(want.as_bytes())) {
            bail!("fake: the webhook signature does not match");
        }
        let v: Value =
            serde_json::from_slice(&request.body).context("fake: the webhook is not JSON")?;
        let text = |k: &str| {
            v[k].as_str()
                .map(str::to_owned)
                .with_context(|| format!("fake: the webhook has no {k}"))
        };
        let id = text("delivery").context("fake: the webhook has no delivery ID")?;
        // Spent only now, after the signature: a forged request can't use up
        // the ID of the real one.
        if !self.lock().deliveries.insert(id.clone()) {
            bail!("fake: delivery {id} was already accepted (a replay)");
        }
        let event = match text("type")?.as_str() {
            "label_added" if text("label")? == self.mapping.trigger_label => {
                Some(TaskEvent::Labeled {
                    issue: text("issue")?,
                    by: text("user")?,
                })
            }
            "comment" => Some(TaskEvent::Commented {
                issue: text("issue")?,
                author: text("user")?,
                body: text("body")?,
            }),
            _ => None,
        };
        Ok(event.map(|event| Delivery { id, event }))
    }
}

/// A fake tracker with one story, one team member, and the sandbox that
/// describes them, ready for `contract::check` or another sub-plan's tests.
pub fn sandbox() -> (FakeTracker, Sandbox) {
    let mapping = Mapping::parse("fake mapping", FAKE_MAPPING).expect("the fake mapping is valid");
    let t = FakeTracker::new(mapping, "fake-secret");
    t.add_issue(
        "FAKE-1",
        FakeIssue {
            title: "Refunds over 100 need a second approver".into(),
            body: "As a finance lead I want ...".into(),
            acceptance_criteria: vec!["A refund over 100 waits for a second approver".into()],
            reporter: "po".into(),
            workflow_state: "Backlog".into(),
            fields: [
                ("req".into(), "REQ-7".into()),
                ("risk".into(), "low".into()),
            ]
            .into(),
            approved_versions: vec!["https://docs.example/brief?rev=3".into()],
            links: vec![],
        },
    );
    t.add_member("dev");
    let expected = t.get_issue("FAKE-1").expect("the fake story reads");
    let trigger = t.webhook(
        &json!({"delivery": "d-1", "type": "label_added", "issue": "FAKE-1", "label": "kod", "user": "dev"}),
    );
    // The trigger with "dev" changed to "eve", under the trigger's signature.
    let mut tampered = t.webhook(
        &json!({"delivery": "d-1", "type": "label_added", "issue": "FAKE-1", "label": "kod", "user": "eve"}),
    );
    tampered.headers = trigger.headers.clone();
    // The trigger with one hex digit of its signature changed.
    let mut wrong_signature = trigger.clone();
    let sig = &mut wrong_signature.headers[0].1;
    let first = if sig.starts_with('0') { "1" } else { "0" };
    sig.replace_range(..1, first);
    let s = Sandbox {
        issue: expected.clone(),
        missing: "FAKE-404".into(),
        member: "dev".into(),
        outsider: "mallory".into(),
        transitions: vec![
            State::Ready,
            State::InProgress,
            State::Blocked,
            State::InProgress,
            State::InReview,
            State::Done,
        ],
        trigger,
        trigger_event: Delivery {
            id: "d-1".into(),
            event: TaskEvent::Labeled {
                issue: "FAKE-1".into(),
                by: "dev".into(),
            },
        },
        tampered,
        wrong_signature,
        unidentified: t.webhook(
            &json!({"type": "label_added", "issue": "FAKE-1", "label": "kod", "user": "dev"}),
        ),
        comment: t.webhook(
            &json!({"delivery": "d-2", "type": "comment", "issue": "FAKE-1", "user": "dev", "body": "@kod use the v2 API"}),
        ),
        comment_event: Delivery {
            id: "d-2".into(),
            event: TaskEvent::Commented {
                issue: "FAKE-1".into(),
                author: "dev".into(),
                body: "@kod use the v2 API".into(),
            },
        },
        ignored: t.webhook(
            &json!({"delivery": "d-3", "type": "label_added", "issue": "FAKE-1", "label": "frontend", "user": "dev"}),
        ),
    };
    (t, s)
}
