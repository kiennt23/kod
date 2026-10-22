//! The tracker adapter contract: everything the harness core may ask of an
//! issue tracker, in standard states and fields. Tracker-specific names live
//! in each tracker's mapping (see `mapping`), never in the core.
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub mod contract;
pub mod fake;
pub mod github;
pub mod mapping;
mod signature;
mod strict_json;
pub use signature::hmac_sha256;

/// The standard workflow states. The core uses only these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ready,
    InProgress,
    InReview,
    Blocked,
    Done,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Ready,
        State::InProgress,
        State::InReview,
        State::Blocked,
        State::Done,
    ];

    /// The name used in mapping files.
    pub fn name(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::InProgress => "in_progress",
            State::InReview => "in_review",
            State::Blocked => "blocked",
            State::Done => "done",
        }
    }
}

/// An issue in standard fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    /// The tracker's own key, e.g. PAY-123.
    pub key: String,
    pub title: String,
    pub body: String,
    pub acceptance_criteria: Vec<String>,
    /// None when the issue has no value in the mapped field. The caller
    /// decides whether that blocks it; a story without one never starts a run.
    pub requirement_id: Option<String>,
    pub risk_class: Option<String>,
    pub reporter: String,
    /// Links to the approved artifact versions the issue pins.
    pub approved_versions: Vec<String>,
    /// Keys of the issues linked to this one, e.g. its rework requests.
    pub links: Vec<String>,
    pub state: State,
}

/// The kind of a rework request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReworkKind {
    Question,
    Amendment,
    Replan,
}

/// What a rework request records, besides the story it is linked to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rework {
    /// The stage that found the problem, e.g. "build".
    pub stage: String,
    /// The target artifact and the approved version it refers to.
    pub artifact: String,
    pub version: String,
    /// The question or the proposed change.
    pub text: String,
    /// Failing test, review comment, run ID.
    pub evidence: Vec<String>,
}

/// A task event from a webhook.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskEvent {
    /// The trigger label was added. `by` is who added it; the dispatcher
    /// checks `is_team_member(by)`.
    Labeled { issue: String, by: String },
    /// A comment on an issue. The dispatcher checks the author is a team
    /// member before it counts as an instruction.
    Commented {
        issue: String,
        author: String,
        body: String,
    },
    /// The trigger label was removed. The dispatcher cancels the ticket's
    /// live run.
    Unlabeled { issue: String, by: String },
    /// The issue's title, body or fields changed. The dispatcher re-reads it
    /// and stops a run whose inputs are now stale.
    Edited { issue: String },
}

/// A verified task event and the tracker's ID for its delivery. The ID must
/// come from the signed payload, never from an unsigned header. The adapter
/// keeps no record of IDs: the dispatcher dedups in its own store, after
/// the write that queues the event, so a failed write never spends an ID.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    pub id: String,
    pub event: TaskEvent,
}

/// A webhook request as received: headers and the raw body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Webhook {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Webhook {
    /// The first header with this name, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One tracker. Every operation on an issue that doesn't exist is an error.
/// Blocking: async callers use `tokio::task::spawn_blocking`.
pub trait Tracker: Send + Sync {
    /// The tracker's name, stored with every key in audit records.
    fn name(&self) -> &str;
    /// Errors if the issue's workflow state is not in the mapping.
    fn get_issue(&self, key: &str) -> Result<Issue>;
    fn comment(&self, key: &str, text: &str) -> Result<()>;
    /// Upsert the single Kod-owned status comment; never select a human comment by its text.
    /// Implementations serialize updates per story and reconcile uncertain creates before retrying.
    fn status_comment(&self, _key: &str, _text: &str) -> Result<()> {
        anyhow::bail!("tracker does not support status comments")
    }
    fn transition(&self, key: &str, state: State) -> Result<()>;
    fn link_pr(&self, key: &str, url: &str) -> Result<()>;
    /// Opens a rework request linked to the story, in a state mapped to
    /// ready; returns its key.
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String>;
    fn is_team_member(&self, user: &str) -> Result<bool>;
    /// Checks the signature first: a bad or missing one is an error. A
    /// signed request with no delivery ID is an error too. A redelivery
    /// parses the same again: the adapter keeps no IDs. A signed request
    /// that is not a task event (another label, another event type) is
    /// `Ok(None)`.
    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>>;
}
