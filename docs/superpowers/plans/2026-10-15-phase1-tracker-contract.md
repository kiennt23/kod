# Phase 1.6a Tracker Adapter Contract (`kod-tracker`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `kod-tracker` crate with the tracker adapter trait, the registry mapping format (validated at load, failing closed), a reusable contract test suite, and an in-memory fake adapter that passes it, so sub-plans 5, 6, 7 and 8 can test against the fake and 6c can prove a real adapter against the same suite.

**Architecture:** The core sees only standard states (`ready`, `in_progress`, `in_review`, `blocked`, `done`) and standard fields. Everything tracker-specific (workflow state names, custom field ids, the trigger label, the team group) lives in one mapping file per tracker, which `Mapping::parse` checks completely before anything uses it. `contract::check(&dyn Tracker, &Sandbox)` is a plain function that panics on the first broken rule, so any adapter's own test calls it with its sandbox project and recorded webhooks. The fake stores issues the way a real tracker does (workflow states and custom fields) and reads them through a `Mapping`, so it exercises the mapping too.

**Tech Stack:** Rust 1.99.0 (edition 2024), anyhow, serde_json, sha2 0.10 and subtle 2.6 (the fake's HMAC-SHA256 webhook signature and its constant-time compare; both already in `Cargo.lock`, `hmac` is not). Tests: std, tempfile.

**Spec:** `docs/kod-harness-design.md` (the tracker adapter section: the operations, `create_linked`, the registry mapping, contract tests), row 6a of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/adr/0001-per-task-execution-mode.md`.

## Decisions

- **D1. The trait is blocking.** `Tracker: Send + Sync` with plain `fn`s returning `anyhow::Result`. Real adapters make a few HTTP calls per task; async callers use `tokio::task::spawn_blocking`. This keeps the crate free of an async runtime and lets the contract be a plain function.
- **D2. The operations** are the design's: `get_issue`, `comment`, `transition`, `link_pr`, `create_linked`, `is_team_member`, `parse_webhook`, plus `name` (the tracker name audit records store next to the story key). The story key is the tracker's own key, as a `String`.
- **D3. `parse_webhook` separates "not for us" from "not genuine", and accepts each delivery once.** A missing or wrong signature, or a signed body that is malformed, is `Err`. A genuine request that is not a task event is `Ok(None)`. A tampered request never turns into a quiet `None`. A genuine request returns a `Delivery { id, event }`; a signed request with no delivery ID is `Err`, and a delivery ID seen before is `Err` (a replay). An ID is spent only by a request whose signature checked out, so a forged request can't burn a real one. Trackers also sign a timestamp; refusing stale deliveries by it is tracker-specific and a 6c contract item.
- **D4. The mapping format is JSON.** The design says registry files are YAML, Markdown or Rego; JSON is valid YAML, matches `policy/*.json`, and needs no new dependency. Shape: `{"tracker", "states": {<standard>: [workflow names]}, "fields": {"requirement_id", "risk_class"}, "trigger_label", "team_group"}`. All five standard states are required, each with at least one workflow name; the first is where `transition` moves an issue. A workflow name mapped to two states, an unknown or missing key, an empty or padded name, or the two fields naming the same custom field is an error at load. Reading an issue in a workflow state the mapping does not list is an error, never a guess.
- **D5. `create_linked(story, kind, &Rework)`** opens the rework request in a `ready`-mapped state, linked to the story, with the stage, the target artifact and its approved version, the kind (question, amendment, re-plan), the text and the evidence. It returns the new key.
- **D6. Membership is a yes/no from the tracker.** `is_team_member(user)` answers for the mapping's team group; the core alone decides what a member's comment may do.
- **D7. The fake signs like a real tracker.** HMAC-SHA256 of the body with a shared secret, hex in `x-fake-signature`, compared in constant time (`subtle`). `fake::hmac_sha256` is written over `sha2` (RFC 2104) and tested against RFC 4231, because `hmac` is not in `Cargo.lock`.
- **D8. No real tracker, no registry file.** The fake's mapping is a const in the crate. Where real mappings live in the registry is decided with 6b and 6c.

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Types and the mapping | Subagent + review | Owns fail-closed mapping validation |
| 2. The contract and the fake | Subagent + review | Owns the contract other adapters are proven by, and the signature rules |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged. No new dependency beyond the ones already in `Cargo.lock`.
- The core uses only standard states and fields; tracker names stay in the mapping.
- Fail closed: an invalid mapping does not load; an unmapped workflow state is an error; a bad signature is an error.
- No real tracker and no network.
- Commit messages are plain, with no trailers.

## Review Focus

- An issue sitting in a workflow state the mapping does not list (someone added "Won't Do") must be an error, not mapped to a guess: `an_unmapped_workflow_state_is_an_error` (Task 2).
- A webhook with a changed (still valid JSON) body, a signature one digit off, or no signature must be an error, and an adapter that only checks the header is present must fail the contract: the `tampered`, `wrong_signature` and unsigned checks in `contract::check` and the `ChecksPresenceOnly` flaw in `the_contract_fails_an_adapter_with_any_one_flaw` (Task 2).
- A recorded webhook sent again must be an error, not a second task: the replay checks in `contract::check`, the `AcceptsReplays` flaw and `delivery_ids_are_spent_only_by_signed_requests` (Task 2).
- An issue without the requirement or risk custom field must read as `None`, not as an empty string: `missing_custom_fields_read_as_none` (Task 2).
- A correctly signed but malformed webhook (missing user, wrong type) must be an error, not `None`: `a_signed_webhook_that_is_malformed_is_an_error` (Task 2).

## File Structure

```
Cargo.toml                       modify: add crates/kod-tracker to members
crates/kod-tracker/
  Cargo.toml                     create
  src/lib.rs                     create: State, Issue, Rework, TaskEvent, Delivery, Webhook, the Tracker trait
  src/mapping.rs                 create: the registry mapping, validated at load
  src/contract.rs                create: Sandbox and check
  src/fake.rs                    create: FakeTracker, FAKE_MAPPING, sandbox()
  tests/mapping.rs               create
  tests/contract.rs              create: the fake passes; a flawed adapter fails
  tests/fake.rs                  create
```

This plan's own PR links it from row 6a of the overview.

---

### Task 1: Types and the mapping

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `crates/kod-tracker/Cargo.toml`, `crates/kod-tracker/src/lib.rs`, `crates/kod-tracker/src/mapping.rs`
- Test: `crates/kod-tracker/tests/mapping.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `State { Ready, InProgress, InReview, Blocked, Done }` with `State::ALL` and `name() -> &'static str`.
  - `Issue { key, title, body: String, acceptance_criteria: Vec<String>, requirement_id, risk_class: Option<String>, reporter: String, approved_versions: Vec<String>, links: Vec<String>, state: State }`.
  - `ReworkKind { Question, Amendment, Replan }`, `Rework { stage, artifact, version, text: String, evidence: Vec<String> }`.
  - `TaskEvent::{Labeled { issue, by }, Commented { issue, author, body }}`, `Delivery { id: String, event: TaskEvent }`, `Webhook { headers: Vec<(String, String)>, body: Vec<u8> }` with `header(name) -> Option<&str>` (case-insensitive).
  - `trait Tracker` (D2), with `parse_webhook(&self, &Webhook) -> Result<Option<Delivery>>` (D3).
  - `mapping::Mapping { pub tracker, pub requirement_id_field, pub risk_class_field, pub trigger_label, pub team_group }` with `Mapping::load(&Path)`, `Mapping::parse(at: &str, text: &str)`, `standard(workflow: &str) -> Option<State>`, `target(State) -> &str`.

In the root `Cargo.toml`, add `"crates/kod-tracker"` to `members`, keeping the list sorted.

- [ ] **Step 1: Write the failing test**

Create `crates/kod-tracker/Cargo.toml`:

````toml
[package]
name = "kod-tracker"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
serde_json = "1"
sha2 = "0.10"

[dev-dependencies]
tempfile = "3"
````

Create `crates/kod-tracker/tests/mapping.rs`:

````rust
use kod_tracker::{State, mapping::Mapping};
use serde_json::{Value, json};

fn good() -> Value {
    json!({
        "tracker": "acme",
        "states": {
            "ready": ["To Do", "Backlog"],
            "in_progress": ["In Progress"],
            "in_review": ["Review"],
            "blocked": ["Blocked"],
            "done": ["Done", "Closed"]
        },
        "fields": {"requirement_id": "customfield_1", "risk_class": "customfield_2"},
        "trigger_label": "kod",
        "team_group": "kod-team"
    })
}

/// A change to the good mapping, and the error it must give.
type Case = (fn(&mut Value), &'static str);

fn err(v: &Value) -> String {
    Mapping::parse("acme.json", &v.to_string())
        .unwrap_err()
        .to_string()
}

#[test]
fn reads_a_mapping() {
    let m = Mapping::parse("acme.json", &good().to_string()).unwrap();
    assert_eq!(m.tracker, "acme");
    assert_eq!(m.standard("Backlog"), Some(State::Ready));
    assert_eq!(m.standard("Closed"), Some(State::Done));
    assert_eq!(m.standard("Review"), Some(State::InReview));
    // Exact names only: no case folding, no trimming.
    assert_eq!(m.standard("review"), None);
    assert_eq!(m.standard("Review "), None);
    assert_eq!(m.standard("Won't Do"), None);
    // The first listed is where a transition goes.
    assert_eq!(m.target(State::Ready), "To Do");
    assert_eq!(m.target(State::Done), "Done");
    assert_eq!(
        (m.requirement_id_field.as_str(), m.risk_class_field.as_str()),
        ("customfield_1", "customfield_2")
    );
    assert_eq!(
        (m.trigger_label.as_str(), m.team_group.as_str()),
        ("kod", "kod-team")
    );
}

#[test]
fn refuses_a_mapping_with_any_fault() {
    let cases: Vec<Case> = vec![
        (|v| *v = json!([]), "acme.json must be an object"),
        (|v| v["extra"] = json!(1), "unexpected key extra"),
        (
            |v| _ = v.as_object_mut().unwrap().remove("team_group"),
            "missing key team_group",
        ),
        (
            |v| _ = v["states"].as_object_mut().unwrap().remove("blocked"),
            "states: missing key blocked",
        ),
        (
            |v| v["states"]["cancelled"] = json!(["Cancelled"]),
            "states: unexpected key cancelled",
        ),
        (
            |v| v["states"]["done"] = json!([]),
            "states.done must be a non-empty list",
        ),
        (
            |v| v["states"]["done"] = json!("Done"),
            "states.done must be a non-empty list",
        ),
        (
            |v| v["states"]["done"] = json!(["Done", ""]),
            "states.done must be a non-empty name",
        ),
        (
            |v| v["states"]["done"] = json!([" Done"]),
            "no spaces at the ends",
        ),
        (
            |v| v["states"]["done"] = json!(["Review"]),
            "states.done: workflow state \"Review\" is mapped twice",
        ),
        (
            |v| v["states"]["ready"] = json!(["To Do", "To Do"]),
            "mapped twice",
        ),
        (
            |v| v["fields"]["risk_class"] = json!("customfield_1"),
            "are the same field",
        ),
        (
            |v| _ = v["fields"].as_object_mut().unwrap().remove("risk_class"),
            "fields: missing key risk_class",
        ),
        (
            |v| v["fields"]["requirement_id"] = json!(7),
            "fields.requirement_id must be a non-empty name",
        ),
        (
            |v| v["tracker"] = json!(""),
            "tracker must be a non-empty name",
        ),
        (
            |v| v["trigger_label"] = json!(null),
            "trigger_label must be a non-empty name",
        ),
    ];
    for (change, want) in cases {
        let mut v = good();
        change(&mut v);
        let e = err(&v);
        assert!(e.contains(want), "want {want:?}, got {e:?}");
    }
    let e = Mapping::parse("acme.json", "{").unwrap_err().to_string();
    assert!(e.contains("acme.json: not JSON"), "{e}");
}

#[test]
fn load_names_the_file_in_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acme.json");
    let e = Mapping::load(&path).unwrap_err().to_string();
    assert!(e.contains("reading") && e.contains("acme.json"), "{e}");
    let mut v = good();
    v["trigger_label"] = json!(" kod");
    std::fs::write(&path, v.to_string()).unwrap();
    let e = format!("{:#}", Mapping::load(&path).unwrap_err());
    assert!(e.contains("acme.json: trigger_label"), "{e}");
    std::fs::write(&path, good().to_string()).unwrap();
    assert_eq!(Mapping::load(&path).unwrap().tracker, "acme");
}
````

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p kod-tracker --test mapping`
Expected: FAIL to compile: cannot find crate `kod_tracker` (there is no `src/lib.rs` yet).

- [ ] **Step 3: Write the types and the mapping**

Create `crates/kod-tracker/src/lib.rs`:

````rust
//! The tracker adapter contract: everything the harness core may ask of an
//! issue tracker, in standard states and fields. Tracker-specific names live
//! in each tracker's mapping (see `mapping`), never in the core.
use anyhow::Result;

pub mod mapping;

/// The standard workflow states. The core uses only these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReworkKind {
    Question,
    Amendment,
    Replan,
}

/// What a rework request records, besides the story it is linked to.
#[derive(Clone, Debug, PartialEq)]
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
}

/// A verified task event and the tracker's ID for its delivery. The
/// dispatcher keeps the IDs it has acted on, so a redelivery is not a second
/// instruction.
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
    fn transition(&self, key: &str, state: State) -> Result<()>;
    fn link_pr(&self, key: &str, url: &str) -> Result<()>;
    /// Opens a rework request linked to the story, in a state mapped to
    /// ready; returns its key.
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String>;
    fn is_team_member(&self, user: &str) -> Result<bool>;
    /// Checks the signature first: a bad or missing one is an error. A
    /// signed request with no delivery ID, or with the ID of a delivery this
    /// adapter already accepted, is an error too (a replay). A signed request
    /// that is not a task event (another label, another event type) is
    /// `Ok(None)`.
    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>>;
}
````

Create `crates/kod-tracker/src/mapping.rs`:

````rust
//! A tracker's mapping, from its registry file: workflow states to standard
//! states, custom fields to requirement ID and risk class, the trigger label
//! and the team group. Checked whole at load; any fault refuses the file.
use crate::State;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::{collections::HashMap, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    pub tracker: String,
    /// Workflow state to standard state.
    standard: HashMap<String, State>,
    /// Standard state to the workflow state `transition` moves to.
    target: HashMap<State, String>,
    /// The custom field holding the requirement ID.
    pub requirement_id_field: String,
    pub risk_class_field: String,
    pub trigger_label: String,
    pub team_group: String,
}

impl Mapping {
    /// Reads and checks a mapping file; errors name the file.
    pub fn load(path: &Path) -> Result<Mapping> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Mapping::parse(&path.display().to_string(), &text)
    }

    /// Checks a mapping. `at` names its source in errors.
    pub fn parse(at: &str, text: &str) -> Result<Mapping> {
        let v: Value = serde_json::from_str(text).with_context(|| format!("{at}: not JSON"))?;
        let o = exact_keys(
            at,
            &v,
            &["tracker", "states", "fields", "trigger_label", "team_group"],
        )?;
        let names: Vec<&str> = State::ALL.iter().map(|s| s.name()).collect();
        let states = exact_keys(&format!("{at}: states"), &o["states"], &names)?;
        let mut standard = HashMap::new();
        let mut target = HashMap::new();
        for s in State::ALL {
            let at = format!("{at}: states.{}", s.name());
            let list = states[s.name()]
                .as_array()
                .filter(|l| !l.is_empty())
                .with_context(|| format!("{at} must be a non-empty list of workflow states"))?;
            for w in list {
                let w = name(&at, w)?;
                if standard.insert(w.clone(), s).is_some() {
                    bail!("{at}: workflow state {w:?} is mapped twice");
                }
                // The first one listed is where `transition` moves to.
                target.entry(s).or_insert(w);
            }
        }
        let fields = exact_keys(
            &format!("{at}: fields"),
            &o["fields"],
            &["requirement_id", "risk_class"],
        )?;
        let requirement_id_field = name(
            &format!("{at}: fields.requirement_id"),
            &fields["requirement_id"],
        )?;
        let risk_class_field = name(&format!("{at}: fields.risk_class"), &fields["risk_class"])?;
        if requirement_id_field == risk_class_field {
            bail!("{at}: fields.requirement_id and fields.risk_class are the same field");
        }
        Ok(Mapping {
            tracker: name(&format!("{at}: tracker"), &o["tracker"])?,
            standard,
            target,
            requirement_id_field,
            risk_class_field,
            trigger_label: name(&format!("{at}: trigger_label"), &o["trigger_label"])?,
            team_group: name(&format!("{at}: team_group"), &o["team_group"])?,
        })
    }

    /// The standard state of a workflow state; None if it is not mapped,
    /// which the adapter must treat as an error.
    pub fn standard(&self, workflow: &str) -> Option<State> {
        self.standard.get(workflow).copied()
    }

    /// The workflow state `transition` moves an issue to.
    pub fn target(&self, s: State) -> &str {
        &self.target[&s]
    }
}

/// A non-empty string with no spaces at the ends.
fn name(at: &str, v: &Value) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s == s.trim() => Ok(s.to_owned()),
        _ => bail!("{at} must be a non-empty name with no spaces at the ends"),
    }
}

/// An object with exactly these keys.
fn exact_keys<'a>(at: &str, v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let o = v
        .as_object()
        .with_context(|| format!("{at} must be an object"))?;
    if let Some(k) = o.keys().find(|k| !keys.contains(&k.as_str())) {
        bail!("{at}: unexpected key {k}");
    }
    if let Some(k) = keys.iter().find(|k| !o.contains_key(**k)) {
        bail!("{at}: missing key {k}");
    }
    Ok(o)
}
````

- [ ] **Step 4: Run the test to see it pass**

Run: `cargo test -p kod-tracker && cargo clippy -p kod-tracker --all-targets -- -D warnings`
Expected: PASS (mapping 3), clippy clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kod-tracker
git commit -m "kod-tracker: adapter trait and the registry mapping format"
```

### Task 2: The contract and the fake

**Files:**
- Modify: `crates/kod-tracker/src/lib.rs` (two `pub mod` lines), `crates/kod-tracker/Cargo.toml` (`subtle`)
- Create: `crates/kod-tracker/src/contract.rs`, `crates/kod-tracker/src/fake.rs`
- Test: `crates/kod-tracker/tests/contract.rs`, `crates/kod-tracker/tests/fake.rs`

**Interfaces:**
- Consumes: Task 1's types, `Tracker` and `Mapping`.
- Produces:
  - `contract::Sandbox { issue: Issue, missing, member, outsider: String, transitions: Vec<State>, trigger: Webhook, trigger_event: Delivery, comment: Webhook, comment_event: Delivery, ignored: Webhook, tampered, wrong_signature, unidentified: Webhook }` and `contract::check(t: &dyn Tracker, s: &Sandbox)`. `transitions` is a walk that must visit all five states; `tampered` is a valid-JSON body changed under the original signature, `wrong_signature` the original body with one signature digit changed, `unidentified` a signed request with no delivery ID. 6c builds a `Sandbox` from its sandbox project and recorded webhooks and calls `check`.
  - `fake::FAKE_MAPPING`, `fake::SIGNATURE`, `fake::hmac_sha256(key, data) -> [u8; 32]`, `fake::FakeIssue`, `fake::FakeTracker::{new, add_issue, add_member, issue, comments, prs, sign, webhook}`, and `fake::sandbox() -> (FakeTracker, Sandbox)` for other sub-plans' tests.

The contract test also runs `check` against the fake with one flaw at a time (accepts any signature, checks only that a signature is present, accepts replays, everyone is a member, transition does nothing, a missing issue is fine, the requirement ID is dropped, links are dropped) and expects each to fail, so a weakened contract shows up as a failing test.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-tracker/tests/contract.rs`:

````rust
use anyhow::Result;
use kod_tracker::{
    Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook,
    contract::check,
    fake::{FakeTracker, SIGNATURE, sandbox},
};
use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

#[test]
fn the_fake_passes_the_contract() {
    let (t, s) = sandbox();
    check(&t, &s);
    // What the check wrote is where a reader of the fake can see it.
    assert_eq!(t.comments("FAKE-1"), ["kod contract test"]);
    assert_eq!(t.prs("FAKE-1"), ["https://github.com/acme/app/pull/1"]);
    let story = t.issue("FAKE-1").unwrap();
    assert_eq!(story.workflow_state, "Closed");
    assert_eq!(story.links, ["REWORK-1"]);
    assert_eq!(t.issue("REWORK-1").unwrap().links, ["FAKE-1"]);
}

/// One way an adapter can be wrong.
#[derive(Clone, Copy, Debug)]
enum Flaw {
    IgnoresSignatures,
    /// Accepts any request that has a signature header, whatever its value.
    ChecksPresenceOnly,
    /// Keeps no delivery IDs, so a recorded webhook can be sent again.
    AcceptsReplays,
    EveryoneIsAMember,
    TransitionDoesNothing,
    MissingIssueIsOk,
    DropsRequirementId,
    DropsLinks,
}

/// The fake with one flaw, to show the contract catches it. `seen` holds
/// what AcceptsReplays answered, by body.
struct Broken(FakeTracker, Flaw, Mutex<HashMap<Vec<u8>, Option<Delivery>>>);

impl Tracker for Broken {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn get_issue(&self, key: &str) -> Result<Issue> {
        let mut i = self.0.get_issue(key)?;
        match self.1 {
            Flaw::DropsRequirementId => i.requirement_id = None,
            Flaw::DropsLinks => i.links.clear(),
            _ => {}
        }
        Ok(i)
    }
    fn comment(&self, key: &str, text: &str) -> Result<()> {
        match self.1 {
            Flaw::MissingIssueIsOk => Ok(()),
            _ => self.0.comment(key, text),
        }
    }
    fn transition(&self, key: &str, state: State) -> Result<()> {
        match self.1 {
            Flaw::TransitionDoesNothing => Ok(()),
            _ => self.0.transition(key, state),
        }
    }
    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        self.0.link_pr(key, url)
    }
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
        self.0.create_linked(story, kind, fields)
    }
    fn is_team_member(&self, user: &str) -> Result<bool> {
        match self.1 {
            Flaw::EveryoneIsAMember => Ok(true),
            _ => self.0.is_team_member(user),
        }
    }
    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>> {
        let mut request = request.clone();
        match self.1 {
            // As if the check were skipped: every request is signed afresh.
            Flaw::IgnoresSignatures => {
                request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
            }
            Flaw::ChecksPresenceOnly if request.header(SIGNATURE).is_some() => {
                request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
            }
            Flaw::AcceptsReplays => {
                let mut seen = self.2.lock().unwrap();
                if let Some(d) = seen.get(&request.body) {
                    return Ok(d.clone());
                }
                let d = self.0.parse_webhook(&request)?;
                seen.insert(request.body.clone(), d.clone());
                return Ok(d);
            }
            _ => {}
        }
        self.0.parse_webhook(&request)
    }
}

#[test]
fn the_contract_fails_an_adapter_with_any_one_flaw() {
    for flaw in [
        Flaw::IgnoresSignatures,
        Flaw::ChecksPresenceOnly,
        Flaw::AcceptsReplays,
        Flaw::EveryoneIsAMember,
        Flaw::TransitionDoesNothing,
        Flaw::MissingIssueIsOk,
        Flaw::DropsRequirementId,
        Flaw::DropsLinks,
    ] {
        let (t, s) = sandbox();
        let broken = Broken(t, flaw, Mutex::default());
        let r = catch_unwind(AssertUnwindSafe(|| check(&broken, &s)));
        assert!(
            r.is_err(),
            "the contract passed an adapter whose flaw is {flaw:?}"
        );
    }
}

#[test]
fn the_contract_refuses_a_walk_that_misses_a_state() {
    let (t, mut s) = sandbox();
    s.transitions.retain(|st| *st != State::Blocked);
    let r = catch_unwind(AssertUnwindSafe(|| check(&t, &s)));
    assert!(r.is_err(), "a walk without blocked passed");
}
````

Create `crates/kod-tracker/tests/fake.rs`:

````rust
use kod_tracker::{
    Delivery, State, TaskEvent, Tracker,
    fake::{FakeIssue, hmac_sha256, sandbox},
};
use serde_json::json;

/// A workflow state the mapping doesn't know is an error, not a guess.
#[test]
fn an_unmapped_workflow_state_is_an_error() {
    let (t, _) = sandbox();
    t.add_issue(
        "FAKE-2",
        FakeIssue {
            workflow_state: "Won't Do".into(),
            ..FakeIssue::default()
        },
    );
    let e = t.get_issue("FAKE-2").unwrap_err().to_string();
    assert!(
        e.contains("\"Won't Do\", which the mapping does not map"),
        "{e}"
    );
    // Transition still works, and makes it readable again.
    t.transition("FAKE-2", State::Blocked).unwrap();
    assert_eq!(t.issue("FAKE-2").unwrap().workflow_state, "Waiting");
    assert_eq!(t.get_issue("FAKE-2").unwrap().state, State::Blocked);
}

#[test]
fn missing_custom_fields_read_as_none() {
    let (t, _) = sandbox();
    t.add_issue(
        "FAKE-3",
        FakeIssue {
            workflow_state: "Open".into(),
            fields: [("other".into(), "x".into())].into(),
            ..FakeIssue::default()
        },
    );
    let i = t.get_issue("FAKE-3").unwrap();
    assert_eq!((i.requirement_id, i.risk_class), (None, None));
}

#[test]
fn a_signed_webhook_that_is_malformed_is_an_error() {
    let (t, _) = sandbox();
    for body in [
        json!({"delivery": "m1", "type": "comment", "issue": "FAKE-1", "user": "dev"}),
        json!({"delivery": "m2", "type": "label_added", "issue": "FAKE-1", "label": "kod"}),
        json!({"delivery": "m3", "type": "comment", "issue": 1, "user": "dev", "body": "x"}),
        json!({"delivery": 4, "type": "comment", "issue": "FAKE-1", "user": "dev", "body": "x"}),
        json!({"delivery": "m6", "type": 4}),
        json!({"delivery": "m7"}),
    ] {
        assert!(t.parse_webhook(&t.webhook(&body)).is_err(), "{body}");
    }
    // Header names are case-insensitive, as in HTTP.
    let mut w = t.webhook(
        &json!({"delivery": "m5", "type": "comment", "issue": "FAKE-1", "user": "u", "body": "b"}),
    );
    w.headers[0].0 = "X-Fake-Signature".into();
    assert_eq!(
        t.parse_webhook(&w).unwrap(),
        Some(Delivery {
            id: "m5".into(),
            event: TaskEvent::Commented {
                issue: "FAKE-1".into(),
                author: "u".into(),
                body: "b".into()
            }
        })
    );
}

/// RFC 4231, test case 2.
#[test]
fn the_signature_is_hmac_sha256() {
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // A key longer than a block is hashed first (RFC 4231, test case 6).
    let mac = hmac_sha256(
        &[0xaa; 131],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(mac[..4], [0x60, 0xe4, 0x31, 0x59]);
}

/// A request that fails the signature check doesn't use up its delivery ID,
/// so a forger can't block the real delivery; the ID is spent once the real
/// one is accepted, and a signed request with no ID is refused.
#[test]
fn delivery_ids_are_spent_only_by_signed_requests() {
    let (t, s) = sandbox();
    assert!(t.parse_webhook(&s.tampered).is_err());
    assert!(t.parse_webhook(&s.trigger).unwrap().is_some());
    let e = t.parse_webhook(&s.trigger).unwrap_err().to_string();
    assert!(e.contains("already accepted"), "{e}");
    let e = t.parse_webhook(&s.unidentified).unwrap_err().to_string();
    assert!(e.contains("no delivery ID"), "{e}");
}
````

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kod-tracker --test contract --test fake`
Expected: FAIL to compile: unresolved imports `kod_tracker::contract` and `kod_tracker::fake`.

- [ ] **Step 3: Write the contract and the fake**

Create `crates/kod-tracker/src/contract.rs`:

````rust
//! The contract every adapter must pass, against a recorded webhook and a
//! sandbox project. An adapter counts as supported once `check` passes.
//! It panics on the first broken rule, like a test.
use crate::{Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook};

/// What the sandbox project holds. For a real tracker: a story set up by
/// hand, and webhooks recorded from it.
pub struct Sandbox {
    /// A story as `get_issue` must read it, with no links yet. Its state is
    /// not compared: the check moves the story along `transitions`.
    pub issue: Issue,
    /// A walk through the tracker's workflow from the story's state, as its
    /// rules allow; it must reach all 5 standard states.
    pub transitions: Vec<State>,
    /// A key with no issue.
    pub missing: String,
    /// A user in the team group, and one outside it.
    pub member: String,
    pub outsider: String,
    /// A recorded webhook adding the trigger label, and its delivery.
    pub trigger: Webhook,
    pub trigger_event: Delivery,
    /// The trigger with a value in its body changed (still valid JSON, same
    /// delivery ID) and its signature kept.
    pub tampered: Webhook,
    /// The trigger with one character of its signature value changed.
    pub wrong_signature: Webhook,
    /// A signed webhook with no delivery ID.
    pub unidentified: Webhook,
    /// A recorded comment webhook, and its delivery.
    pub comment: Webhook,
    pub comment_event: Delivery,
    /// A recorded, signed webhook that is not a task event.
    pub ignored: Webhook,
}

pub fn check(t: &dyn Tracker, s: &Sandbox) {
    let key = s.issue.key.as_str();
    let missing = s.missing.as_str();
    assert!(!t.name().is_empty(), "the tracker has no name");

    let got = t.get_issue(key).expect("get_issue on the sandbox story");
    assert_eq!(
        got,
        Issue {
            state: got.state,
            ..s.issue.clone()
        },
        "get_issue"
    );
    assert!(
        t.get_issue(missing).is_err(),
        "get_issue on a missing issue"
    );

    for state in State::ALL {
        assert!(
            s.transitions.contains(&state),
            "the sandbox walk never reaches {}",
            state.name()
        );
    }
    for &state in &s.transitions {
        t.transition(key, state).expect("transition");
        let now = t.get_issue(key).expect("get_issue after transition").state;
        assert_eq!(now, state, "transition to {}", state.name());
    }
    assert!(
        t.transition(missing, State::Ready).is_err(),
        "transition on a missing issue"
    );

    t.comment(key, "kod contract test").expect("comment");
    assert!(
        t.comment(missing, "x").is_err(),
        "comment on a missing issue"
    );
    t.link_pr(key, "https://github.com/acme/app/pull/1")
        .expect("link_pr");
    assert!(
        t.link_pr(missing, "https://github.com/acme/app/pull/1")
            .is_err(),
        "link_pr on a missing issue"
    );

    let rework = Rework {
        stage: "build".into(),
        artifact: "design".into(),
        version: "rev-3".into(),
        text: "kod contract test: is the limit 100 or 1000?".into(),
        evidence: vec!["run r1".into()],
    };
    let created = t
        .create_linked(key, ReworkKind::Question, &rework)
        .expect("create_linked");
    assert_ne!(created, key, "create_linked returned the story's key");
    let r = t
        .get_issue(&created)
        .expect("get_issue on the rework request");
    assert_eq!(r.state, State::Ready, "a new rework request is ready");
    assert!(
        r.body.contains(&rework.text),
        "the rework request lacks its text: {r:?}"
    );
    let story = t.get_issue(key).expect("get_issue after create_linked");
    assert!(
        story.links.contains(&created),
        "the story does not link its rework request: {story:?}"
    );
    assert!(
        t.create_linked(missing, ReworkKind::Question, &rework)
            .is_err(),
        "create_linked on a missing story"
    );

    assert!(
        t.is_team_member(&s.member).expect("is_team_member"),
        "member"
    );
    assert!(
        !t.is_team_member(&s.outsider).expect("is_team_member"),
        "outsider"
    );

    let parse = |w: &Webhook| t.parse_webhook(w);
    // Forgeries first, while the trigger's delivery ID is unspent, so only a
    // signature check can refuse them; they are errors, never a quiet None.
    assert!(
        parse(&s.tampered).is_err(),
        "a changed body passed the signature check"
    );
    assert!(
        parse(&s.wrong_signature).is_err(),
        "a wrong signature value was accepted"
    );
    let unsigned = Webhook {
        headers: vec![],
        body: s.trigger.body.clone(),
    };
    assert!(
        parse(&unsigned).is_err(),
        "an unsigned webhook was accepted"
    );
    assert!(
        parse(&s.unidentified).is_err(),
        "a webhook with no delivery ID was accepted"
    );
    assert_eq!(
        parse(&s.trigger).expect("trigger webhook"),
        Some(s.trigger_event.clone())
    );
    assert_eq!(
        parse(&s.comment).expect("comment webhook"),
        Some(s.comment_event.clone())
    );
    assert_eq!(parse(&s.ignored).expect("ignored webhook"), None);
    assert!(
        parse(&s.trigger).is_err(),
        "a replayed trigger was accepted"
    );
    assert!(
        parse(&s.comment).is_err(),
        "a replayed comment was accepted"
    );
}
````

Create `crates/kod-tracker/src/fake.rs`:

````rust
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
````

Apply to `crates/kod-tracker/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/lib.rs b/crates/kod-tracker/src/lib.rs
index 0b78940..d38c7b1 100644
--- a/crates/kod-tracker/src/lib.rs
+++ b/crates/kod-tracker/src/lib.rs
@@ -3,6 +3,8 @@
 //! in each tracker's mapping (see `mapping`), never in the core.
 use anyhow::Result;
 
+pub mod contract;
+pub mod fake;
 pub mod mapping;
 
 /// The standard workflow states. The core uses only these.
````

Apply to `crates/kod-tracker/Cargo.toml` with `git apply`:

````diff
diff --git a/crates/kod-tracker/Cargo.toml b/crates/kod-tracker/Cargo.toml
index 5e38c1e..c67e2dd 100644
--- a/crates/kod-tracker/Cargo.toml
+++ b/crates/kod-tracker/Cargo.toml
@@ -8,6 +8,7 @@ publish = false
 anyhow = "1"
 serde_json = "1"
 sha2 = "0.10"
+subtle = "2.6"
 
 [dev-dependencies]
 tempfile = "3"
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-tracker: contract 3, fake 5, mapping 3), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-tracker
git commit -m "kod-tracker: contract test suite and a fake adapter that passes it"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc's tracker section and this plan, with these questions:
- Does any standard name leak a tracker name, or any tracker name reach the core types?
- Can an invalid mapping load, or an unmapped state read as a standard one?
- Would a real adapter that passes `check` be safe to trust with webhooks and membership?
- Does the real adapter compare signatures in constant time? The contract can't see that from outside; 6c's review must check it in the code.

## Self-review

- **Spec coverage.** The seven operations plus `name`: Task 1. The mapping (states, the two custom fields, trigger label, team group) validated at load: Task 1. The contract test: Task 2. A fake for other sub-plans' tests that passes the contract: Task 2. A real tracker: not built, as the overview says.
- **Placeholders.** None.
- **Type consistency.** `State`, `Issue`, `Rework`, `ReworkKind`, `TaskEvent`, `Delivery`, `Webhook`, `Mapping`, `Sandbox`, `FakeTracker` are spelled the same in both tasks; the code was compiled and tested in this order.
- **Review Focus.** Each of the five lines names its test and task above.

## Open items

- **Mapping file format** (owner): JSON now (D4). Say if the registry must be YAML; the shape stays the same and only the parser changes.
- **Where mappings live in the registry** (owner, with 6b): the file path and how a tracker is chosen per repo.
- **Stale deliveries**: refusing old deliveries by the tracker's signed timestamp is a 6c contract item (D3).
- **Rework request fields**: the fake writes them into the body. A real adapter may use custom fields instead; 6c decides per tracker.
