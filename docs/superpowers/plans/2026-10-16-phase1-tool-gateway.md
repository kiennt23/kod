# Phase 1.5 Tool Gateway and Publish (`kod-tool-gateway`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development; *Native* tasks use superpowers:executing-plans. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `kod-tool-gateway` crate that serves a run's tracker, docs and publish tools over MCP (rmcp), checks every call with kod-policy and writes it to the audit log before it acts, gets the git credential from a token broker so the sandbox never holds one, and publishes idempotently (push `agent/<ticket>-<run>`, open a draft PR); plus an MCP client in `kod-loop`.

**Architecture:** The gateway is a library: `Gateway` holds the policy, the audit log, the adapters (tracker, docs, broker, forge) and the live runs, keyed by the SHA-256 of a per-run token as in the LLM gateway. `Gateway::call(token, tool, args)` is the one entry point: it builds the policy input from the run and the target, records the decision, writes a `tool_call` record with the exact target through `AuditLog::before`, and only then calls the adapter. `router(gateway, hosts)` mounts rmcp's Streamable HTTP service at `/r/{token}/mcp` in stateless JSON mode. A test harness starts runs (`start_run`) and cancels them (`cancel`); the dispatcher (6) and the merge into the LLM gateway's process (6b) wire it later. `kod-loop` gets a small blocking MCP client and adds the gateway's tools to its own when `KOD_MCP_URL` is set.

**Tech Stack:** Rust 1.99.0 (edition 2024). New: rmcp 3.5 (server, Streamable HTTP server). Already in `Cargo.lock`: axum 0.8, tokio, tower 0.5, serde_json, sha2 0.10, anyhow, rusqlite 0.40 (tests), reqwest 0.12 (tests and kod-loop). Crates from this phase: kod-policy, kod-audit (PR #14 / `feat/phase1-audit`), kod-tracker (the 6a plan), kod-llm-proxy (sub-plan 2, for the execution order only).

**Spec:** `docs/kod-harness-design.md` (the PR flow, Run identity and lifecycle, the tool gateway, the token broker, Trust boundaries, the action matrix rows for tracker reads and writes and for publish), row 5 of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/superpowers/plans/2026-10-15-phase1-policy.md` (the `read`, `tracker_write` and `publish` inputs), `docs/superpowers/plans/2026-10-15-phase1-audit.md`, `docs/superpowers/plans/2026-10-15-phase1-tracker-contract.md`, `docs/adr/0001-per-task-execution-mode.md`.

## Decisions

| | Decision | Why |
|---|---|---|
| D1 | A library crate with a router, no binary yet. 6b mounts `router` in the LLM gateway's process (the overview's "same process at first") when real adapters and a run source exist. | Today there are only fakes and no dispatcher; a binary would serve fakes. The router takes the allowed hosts so the process that mounts it sets them. |
| D2 | One token per run in the path, `/r/{token}/mcp`, at least 32 lower-case hex characters, kept as its SHA-256. An unknown token gets 404 before rmcp sees the request. | Same scheme as the LLM gateway (sub-plan 2), so 6b can give a sandbox one token for both routes. |
| D3 | rmcp 3.5, stateless Streamable HTTP (protocol 2026-07-28), JSON responses, no SSE keep-alive. | No session state to lose or forge; every call carries its run token. |
| D4 | The `kod-loop` client is a hand-written blocking JSON-RPC client over the reqwest it already has, tested against the rmcp server. | `kod-loop` is synchronous and runs in the sandbox; the rmcp client would pull tokio into it for three request shapes. |
| D5 | Tracker tools: `get_issue`, `read_doc`, `comment`, `transition`, `create_linked`. Policy inputs are the policy plan's `read` and `tracker_write`; `linked_docs` is the run ticket's `approved_versions` and `linked_tickets` its `links`, both read live from the tracker on each call. | The matrix limits reads to the ticket and its linked docs and writes to its own ticket and linked requests; reading them live means a link removed mid-run is honoured. |
| D6 | Every call: `Policy::decide`, then `AuditLog::record_decision`, then `AuditLog::before(run_audit, {"kind": "tool_call", run, tool, target})` around the adapter call, then a best-effort `tool_result` record. A failed decision write or `before` write refuses the call. Malformed arguments and unknown tools or tokens are refused before any record. | The trust boundary: a record with the exact target exists before the external call, and an unrecordable call does not happen (policy plan D9). |
| D7 | `publish{title, summary, test_evidence, files: [{path, content}]}` carries the change set as UTF-8 file contents (`null` deletes). The gateway commits it through the forge's git data API onto `run.base`; the sandbox never pushes. At most 1000 files and 8 MiB. | The sandbox has no git credential to push with. Binary files are a known ceiling (`ponytail:` note in the code). |
| D8 | `publish` is not offered to the model: `kod-loop` hides it. The caller that ends the run calls it (the 6b runner; the test harness here). | The design's publish happens at the end of a run, not when the model decides. |
| D9 | The token broker is `trait Broker { fn mint(&self, repo) -> Result<Secret> }`, called inside the publish operation only. `Secret` has a redacted `Debug`, no `Serialize`, and is dropped when the operation ends. Tracker and docs adapters hold their own credentials outside the sandbox. | The design's "git token exists only inside the publish op". The real broker (a GitHub App installation token scoped to one repo) is 6b's. |
| D10 | Idempotent publish: the commit is a pure function of the change set, the base, the message and the committer, so a retry makes the same commit; `set_branch` moves `agent/<ticket>-<run>` to it; `find_pr` before `open_draft_pr`; `link_pr` on every publish. | A publish that died after the push and before the PR completes on retry with one branch and one PR. Real adapters must treat a repeated `link_pr` as a no-op. |
| D11 | Cancel is checked twice: in the policy input (`run.canceled`), and again after the commit and before the branch moves. | A cancel that lands during publish stops it before anything is visible. |
| D12 | The commit message ends with `Kod-Ticket: <tracker>:<key>`, `Kod-Run: <id>` and `Co-Authored-By: <agent>`; the committer is the agent identity, validated at `Gateway::new`. The PR title is `<key>: <title>` and its body carries the summary, ticket, requirement, run ID, pinned inputs and test evidence. | The design's PR package. |

Out of scope here: the real GitHub, tracker and docs adapters and the real broker (6b, 6c); serving the router from a process (6b); the dispatcher (6); revoking identities on the kill switch beyond `cancel_all` (the dispatcher plan).

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Tracker and docs tools | Subagent + review | Owns policy-then-audit gating and the planted-secret conformance test |
| 2. Publish and the token broker | Subagent + review | Owns the publish refusals, credential handling and idempotency; four Review Focus tests |
| 3. The MCP endpoint | Subagent + review | Owns the run-token check in front of rmcp |
| 4. The MCP client in kod-loop | Subagent + review | Owns the conformance test that a tool call goes through the endpoint, and hiding `publish` from the model |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged. The one new dependency is rmcp 3.5 (MIT/Apache-2.0); every crate it adds is under a licence `deny.toml` allows.
- Fail closed: an unknown token, tool or argument is refused; a decision or audit write that fails refuses the call; the gateway never acts without a policy allow.
- No credential in a tool result, an audit record, a log line or the sandbox.
- No network: fakes only (the 6a fake tracker, `FakeDocs`, `FakeGitHub`).
- Code against kod-audit's plan API (`open`, `record_decision`, `before`, `append`, `RUN_AUDIT`) and kod-tracker's plan API.
- Commit messages are plain, with no trailers.

## Review Focus

- A publish that dies after the branch moved but before the PR opened must complete on retry with one branch and one PR, not fail or open a second: `a_publish_that_died_before_the_pr_completes_on_retry` (Task 2).
- A cancel that lands while publish is running must stop it before the branch moves: `a_cancel_during_publish_stops_it_before_the_branch_moves` (Task 2).
- An audit database that is locked must refuse publish with nothing pushed: `publish_is_refused_when_the_audit_write_fails` (Task 2).
- A minted git credential must never appear in a tool reply or an audit record: `the_publish_credential_never_leaves_the_gateway` (Task 2).
- An unknown token, an unknown tool or malformed arguments must be refused without reaching policy or an adapter: `unknown_tokens_tools_and_bad_arguments_are_refused` (Task 1).

## File Structure

```
Cargo.toml                                   modify: add crates/kod-tool-gateway to members
crates/kod-tool-gateway/
  Cargo.toml                                 create (Task 1), modify (Task 3)
  src/lib.rs                                 create: Run, Adapters, Gateway, the tools (Task 1); publish (Task 2); mcp (Task 3)
  src/docs.rs                                create: the Docs trait and FakeDocs
  src/broker.rs                              create: Secret and the Broker trait (Task 2)
  src/forge.rs                               create: Change, Identity, Commit, the Forge trait and FakeGitHub (Task 2)
  src/mcp.rs                                 create: the rmcp handler and router (Task 3)
  tests/common/mod.rs                        create (Task 1), modify (Tasks 2, 3)
  tests/tools.rs                             create (Task 1)
  tests/publish.rs                           create (Task 2)
  tests/mcp.rs                               create (Task 3)
crates/kod-loop/
  Cargo.toml                                 modify: dev-dependencies (Task 4)
  src/mcp.rs                                 create: the MCP client (Task 4)
  src/lib.rs                                 modify: run_with and tool routing (Task 4)
  src/main.rs                                modify: KOD_MCP_URL (Task 4)
  tests/mcp.rs                               create (Task 4)
```

Execution order: this plan runs after sub-plans 3 (policy), 4 (audit), 6a (tracker) and 2 (LLM gateway). Its diffs apply on top of those.

---

### Task 1: Tracker and docs tools

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/kod-tool-gateway/Cargo.toml`, `src/lib.rs`, `src/docs.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs`, `tests/tools.rs`

**Interfaces:**
- Consumes: `kod_policy::Policy::{load, decide}`; `kod_audit::{AuditLog::{open, record_decision, before, append}, RUN_AUDIT}`; `kod_tracker::{Tracker, Issue, State, Rework, ReworkKind, fake::{sandbox, FakeTracker, FakeIssue}}`.
- Produces: `Run { id, ticket, repo, base, data_classes }`; `Adapters { tracker: Arc<dyn Tracker>, docs: Arc<dyn Docs> }`; `Gateway::new(Arc<Policy>, Arc<AuditLog>, Adapters) -> anyhow::Result<Gateway>`; `Gateway::{start_run(&str, Run) -> anyhow::Result<()>, cancel(&str) -> bool, cancel_all(), run(&str) -> Option<(Run, bool)>, call(&str, &str, &Value) -> Reply}` with `type Reply = Result<Value, String>`; `MIN_TOKEN_LEN`; `docs::{Docs, FakeDocs}`.

- [ ] **Step 1: Write the failing tests**

Apply to `Cargo.toml` with `git apply`:

````diff
diff --git a/Cargo.toml b/Cargo.toml
index 086e6c7..56a355c 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,3 +1,3 @@
 [workspace]
-members = ["crates/kod-audit", "crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy", "crates/kod-tracker"]
+members = ["crates/kod-audit", "crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy", "crates/kod-tool-gateway", "crates/kod-tracker"]
 resolver = "3"
````

Create `crates/kod-tool-gateway/Cargo.toml`:

````toml
[package]
name = "kod-tool-gateway"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
kod-audit = { path = "../kod-audit" }
kod-policy = { path = "../kod-policy" }
kod-tracker = { path = "../kod-tracker" }
serde_json = "1"
sha2 = "0.10"

[dev-dependencies]
rusqlite = "0.40"
tempfile = "3"
````

Create `crates/kod-tool-gateway/tests/common/mod.rs`:

````rust
#![allow(dead_code)]
use kod_audit::AuditLog;
use kod_policy::Policy;
use kod_tool_gateway::{Adapters, Gateway, Run, docs::FakeDocs};
use kod_tracker::fake::{self, FakeIssue, FakeTracker};
use std::{path::PathBuf, sync::Arc};

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const LINKED_DOC: &str = "https://docs.example/brief?rev=3";
pub const SECRET_DOC: &str = "https://docs.example/payroll?rev=1";
/// Text planted where the run may not read: an unlinked ticket and doc.
pub const PLANTED: &str = "PLANTED-SECRET-7f3a";

pub struct Harness {
    pub gw: Arc<Gateway>,
    pub tracker: Arc<FakeTracker>,
    pub docs: Arc<FakeDocs>,
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
    }
}

/// A gateway over the fakes with run-1 started under TOKEN. FAKE-1 links
/// FAKE-2; FAKE-9 and SECRET_DOC are not linked and hold PLANTED.
pub fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(Policy::load(&root.join("policy")).unwrap());
    let audit = dir.path().join("audit.db");
    let log = Arc::new(AuditLog::open(&audit).unwrap());
    let (tracker, _) = fake::sandbox();
    let mut story = tracker.issue("FAKE-1").unwrap();
    story.links = vec!["FAKE-2".into()];
    tracker.add_issue("FAKE-1", story);
    for (key, body) in [("FAKE-2", "the linked story"), ("FAKE-9", PLANTED)] {
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
    let adapters = Adapters {
        tracker: tracker.clone(),
        docs: docs.clone(),
    };
    let gw = Arc::new(Gateway::new(policy, log, adapters).unwrap());
    gw.start_run(TOKEN, run()).unwrap();
    Harness {
        gw,
        tracker,
        docs,
        audit,
        _dir: dir,
    }
}

impl Harness {
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
}
````

Create `crates/kod-tool-gateway/tests/tools.rs`:

````rust
mod common;

use common::*;
use serde_json::json;

#[test]
fn reads_the_runs_ticket_and_its_linked_docs_only() {
    let h = harness();
    let own =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-1"}))
            .unwrap();
    assert_eq!(own["requirement_id"], "REQ-7");
    let doc =
        h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}))
            .unwrap();
    assert_eq!(doc["text"], "the approved brief");
    // The read rule is the ticket and its linked docs; a linked ticket is a
    // write target, not a read one.
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-2"}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
}

#[test]
fn a_planted_secret_cannot_be_read_and_the_attempt_is_audited() {
    let h = harness();
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-9"}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    let e =
        h.gw.call(TOKEN, "read_doc", &json!({"id": SECRET_DOC}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    assert!(!e.contains(PLANTED));
    let refused: Vec<_> = h.decisions().into_iter().filter(|(a, _)| !a).collect();
    assert_eq!(refused.len(), 2);
    assert_eq!(
        refused[0].1["target"],
        json!({"kind": "ticket", "id": "FAKE-9"})
    );
    assert_eq!(
        refused[1].1["target"],
        json!({"kind": "doc", "id": SECRET_DOC})
    );
    let log = std::fs::read(&h.audit).unwrap();
    assert!(!String::from_utf8_lossy(&log).contains(PLANTED));
}

#[test]
fn writes_reach_the_tracker_only_for_the_runs_tickets() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    assert_eq!(h.tracker.comments("FAKE-1"), ["on it"]);
    h.gw.call(
        TOKEN,
        "transition",
        &json!({"key": "FAKE-2", "state": "blocked"}),
    )
    .unwrap();
    assert_eq!(h.tracker.issue("FAKE-2").unwrap().workflow_state, "Waiting");
    let e =
        h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-9", "text": "hi"}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    assert!(h.tracker.comments("FAKE-9").is_empty());
    let e =
        h.gw.call(
            TOKEN,
            "transition",
            &json!({"key": "FAKE-1", "state": "shipped"}),
        )
        .unwrap_err();
    assert_eq!(e, "unknown state \"shipped\"");
}

#[test]
fn every_tracker_write_is_audited_with_its_target_before_it_happens() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    let kinds: Vec<_> = h.records().iter().map(|r| r["kind"].clone()).collect();
    assert_eq!(kinds, ["policy_decision", "tool_call", "tool_result"]);
    assert_eq!(h.records()[1]["target"], json!({"ticket": "FAKE-1"}));
}

#[test]
fn create_linked_opens_a_request_under_the_runs_ticket() {
    let h = harness();
    let args = json!({"kind": "question", "stage": "build", "artifact": "brief",
        "version": "3", "text": "Which approver?", "evidence": ["src/refund.rs:12"]});
    let r = h.gw.call(TOKEN, "create_linked", &args).unwrap();
    let key = r["key"].as_str().unwrap();
    assert!(
        h.tracker
            .issue(key)
            .unwrap()
            .body
            .contains("Which approver?")
    );
    let bad = json!({"kind": "rumour", "stage": "", "artifact": "", "version": "",
        "text": "", "evidence": []});
    let e = h.gw.call(TOKEN, "create_linked", &bad).unwrap_err();
    assert_eq!(e, "unknown kind \"rumour\"");
}

#[test]
fn a_canceled_run_cannot_write_to_the_tracker() {
    let h = harness();
    assert!(h.gw.cancel("run-1"));
    let e =
        h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "x"}))
            .unwrap_err();
    assert!(e.contains("the run is canceled"), "{e}");
    assert!(h.tracker.comments("FAKE-1").is_empty());
}

#[test]
fn unknown_tokens_tools_and_bad_arguments_are_refused() {
    let h = harness();
    let other = "f".repeat(64);
    let e =
        h.gw.call(&other, "get_issue", &json!({"key": "FAKE-1"}))
            .unwrap_err();
    assert_eq!(e, "unknown run");
    let e = h.gw.call(TOKEN, "run", &json!({})).unwrap_err();
    assert_eq!(e, "unknown tool \"run\"");
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": 7}))
            .unwrap_err();
    assert_eq!(e, "key must be a string");
    assert!(h.records().is_empty());
}

#[test]
fn start_run_validates_the_run() {
    let h = harness();
    let mut r = run();
    r.id = "run-2".into();
    let bad_token = h.gw.start_run("ABC", r.clone()).unwrap_err();
    assert!(bad_token.to_string().contains("lower-case hex"));
    let mut short = r.clone();
    short.base = "abc".into();
    assert!(h.gw.start_run(&"1".repeat(64), short).is_err());
    let mut classless = r.clone();
    classless.data_classes = vec![];
    assert!(h.gw.start_run(&"1".repeat(64), classless).is_err());
    let again = h.gw.start_run(&"1".repeat(64), run()).unwrap_err();
    assert_eq!(again.to_string(), "run run-1 is already started");
    h.gw.start_run(&"1".repeat(64), r).unwrap();
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-tool-gateway`
Expected: FAIL to compile: `cannot find module or crate kod_tool_gateway` (the crate has no library yet).

- [ ] **Step 3: Implement the gateway and the tools**

Create `crates/kod-tool-gateway/src/docs.rs`:

````rust
//! The docs adapter: read one approved document by its pinned ID.
use anyhow::{Context, Result};
use std::{collections::HashMap, sync::Mutex};

pub trait Docs: Send + Sync {
    fn read(&self, id: &str) -> Result<String>;
}

/// In-memory docs for tests.
#[derive(Default)]
pub struct FakeDocs(Mutex<HashMap<String, String>>);

impl FakeDocs {
    pub fn add(&self, id: &str, text: &str) {
        let mut docs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        docs.insert(id.to_owned(), text.to_owned());
    }
}

impl Docs for FakeDocs {
    fn read(&self, id: &str) -> Result<String> {
        let docs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        docs.get(id)
            .cloned()
            .with_context(|| format!("no doc {id}"))
    }
}
````

Create `crates/kod-tool-gateway/src/lib.rs`:

````rust
//! Kod's tool gateway: the MCP endpoint a run's sandbox reaches for tracker
//! and docs tools and for the publish operation. Every call is checked by
//! kod-policy and recorded in the audit log before it acts; a call that
//! can't be decided or recorded doesn't happen.
use anyhow::bail;
use docs::Docs;
use kod_audit::{AuditLog, RUN_AUDIT};
use kod_policy::Policy;
use kod_tracker::{Issue, Rework, ReworkKind, State, Tracker};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub mod docs;
/// A run token is at least this many lower-case hex characters, as in the
/// LLM gateway.
pub const MIN_TOKEN_LEN: usize = 32;
/// A run the gateway serves. `base` is the commit its workspace started from.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: String,
    pub ticket: String,
    pub repo: String,
    pub base: String,
    pub data_classes: Vec<String>,
}

struct Live {
    run: Run,
    canceled: bool,
}

/// The adapters behind the tools. Each holds its own credentials; none of
/// them is ever handed to the sandbox.
pub struct Adapters {
    pub tracker: Arc<dyn Tracker>,
    pub docs: Arc<dyn Docs>,
}

pub struct Gateway {
    policy: Arc<Policy>,
    audit: Arc<AuditLog>,
    adapters: Adapters,
    /// Keyed by the token's SHA-256, as in the LLM gateway.
    runs: Mutex<HashMap<String, Live>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key(token: &str) -> String {
    hex(&Sha256::digest(token))
}

/// A non-empty single line with no spaces at the ends.
fn named(s: &str) -> bool {
    !s.is_empty() && s == s.trim() && !s.contains(['\n', '\r'])
}

fn lower_hex(s: &str, min: usize) -> bool {
    s.len() >= min && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

type Reply = Result<Value, String>;

fn text<'a>(args: &'a Value, k: &str) -> Result<&'a str, String> {
    args[k]
        .as_str()
        .ok_or_else(|| format!("{k} must be a string"))
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "key": i.key,
        "title": i.title,
        "body": i.body,
        "acceptance_criteria": i.acceptance_criteria,
        "requirement_id": i.requirement_id,
        "risk_class": i.risk_class,
        "approved_versions": i.approved_versions,
        "links": i.links,
        "state": i.state.name(),
    })
}

impl Gateway {
    pub fn new(
        policy: Arc<Policy>,
        audit: Arc<AuditLog>,
        adapters: Adapters,
    ) -> anyhow::Result<Gateway> {
        Ok(Gateway {
            policy,
            audit,
            adapters,
            runs: Mutex::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Live>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts serving a run under its token.
    pub fn start_run(&self, token: &str, run: Run) -> anyhow::Result<()> {
        if !lower_hex(token, MIN_TOKEN_LEN) {
            bail!("a run token must be at least {MIN_TOKEN_LEN} lower-case hex characters");
        }
        for (k, v) in [
            ("id", &run.id),
            ("ticket", &run.ticket),
            ("repo", &run.repo),
        ] {
            if !named(v) {
                bail!("run {}: {k} must be a name", run.id);
            }
        }
        if !lower_hex(&run.base, 40) || run.base.len() != 40 {
            bail!("run {}: base must be a 40-character commit SHA", run.id);
        }
        if run.data_classes.is_empty() || !run.data_classes.iter().all(|c| named(c)) {
            bail!(
                "run {}: data_classes must be a non-empty list of names",
                run.id
            );
        }
        let mut runs = self.lock();
        if runs.values().any(|l| l.run.id == run.id) || runs.contains_key(&key(token)) {
            bail!("run {} is already started", run.id);
        }
        runs.insert(
            key(token),
            Live {
                run,
                canceled: false,
            },
        );
        Ok(())
    }

    /// Cancels a run; false if there is no such run.
    pub fn cancel(&self, run_id: &str) -> bool {
        let mut runs = self.lock();
        let live = runs.values_mut().find(|l| l.run.id == run_id);
        live.map(|l| l.canceled = true).is_some()
    }

    /// Cancels every run: the kill switch.
    pub fn cancel_all(&self) {
        self.lock().values_mut().for_each(|l| l.canceled = true);
    }

    /// The run and whether it is canceled, or None for an unknown token.
    pub fn run(&self, token: &str) -> Option<(Run, bool)> {
        let runs = self.lock();
        runs.get(&key(token)).map(|l| (l.run.clone(), l.canceled))
    }

    /// Runs one tool call for the run with this token. Blocking.
    pub fn call(&self, token: &str, tool: &str, args: &Value) -> Reply {
        let (run, canceled) = self.run(token).ok_or("unknown run")?;
        match tool {
            "get_issue" => self.get_issue(&run, args),
            "read_doc" => self.read_doc(&run, args),
            "comment" | "transition" => self.write(&run, canceled, tool, args),
            "create_linked" => self.create_linked(&run, canceled, args),
            other => Err(format!("unknown tool {other:?}")),
        }
    }

    /// Asks the policy and records the decision. Refuses if the record can't
    /// be written or the policy says no.
    fn allow(&self, input: Value) -> Result<(), String> {
        let d = self.policy.decide(&input);
        self.audit
            .record_decision(&input, &d)
            .map_err(|e| format!("refused: the audit write failed: {e:#}"))?;
        if d.allow {
            Ok(())
        } else {
            Err(format!("refused by policy: {}", d.reasons.join("; ")))
        }
    }

    /// Writes the intent with its exact target, then runs `call`; a failed
    /// write means `call` never runs. The result is recorded after.
    fn act<T>(
        &self,
        run: &Run,
        tool: &str,
        target: Value,
        call: impl FnOnce() -> anyhow::Result<T>,
    ) -> Result<T, String> {
        let intent = json!({"kind": "tool_call", "run": run.id, "tool": tool, "target": target});
        let r = self
            .audit
            .before(RUN_AUDIT, &intent, call)
            .map_err(|e| format!("refused: the audit write failed: {e:#}"))?;
        let result = r
            .as_ref()
            .map(|_| "ok".to_owned())
            .unwrap_or_else(|e| format!("{e:#}"));
        // The call already happened; a failed result record can't undo it.
        let _ = self.audit.append(
            RUN_AUDIT,
            &json!({"kind": "tool_result", "run": run.id, "tool": tool, "result": result}),
        );
        r.map_err(|e| format!("{tool} failed: {e:#}"))
    }

    /// The run's own ticket, read for the policy inputs (its linked docs and
    /// tickets) and the PR text.
    fn own_issue(&self, run: &Run) -> Result<Issue, String> {
        self.adapters
            .tracker
            .get_issue(&run.ticket)
            .map_err(|e| format!("reading the run's ticket failed: {e:#}"))
    }

    fn get_issue(&self, run: &Run, args: &Value) -> Reply {
        let k = text(args, "key")?;
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket},
            "target": {"kind": "ticket", "id": k},
        }))?;
        let i = self
            .adapters
            .tracker
            .get_issue(k)
            .map_err(|e| format!("{e:#}"))?;
        Ok(issue_json(&i))
    }

    fn read_doc(&self, run: &Run, args: &Value) -> Reply {
        let id = text(args, "id")?;
        let issue = self.own_issue(run)?;
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "linked_docs": issue.approved_versions},
            "target": {"kind": "doc", "id": id},
        }))?;
        let t = self.adapters.docs.read(id).map_err(|e| format!("{e:#}"))?;
        Ok(json!({"id": id, "text": t}))
    }

    fn write_input(
        &self,
        run: &Run,
        canceled: bool,
        kind: &str,
        target: Value,
    ) -> Result<(), String> {
        let issue = self.own_issue(run)?;
        self.allow(json!({
            "action": "tracker_write",
            "kind": kind,
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "canceled": canceled, "linked_tickets": issue.links},
            "target": target,
        }))
    }

    fn write(&self, run: &Run, canceled: bool, tool: &str, args: &Value) -> Reply {
        let k = text(args, "key")?;
        let t = &self.adapters.tracker;
        if tool == "comment" {
            let body = text(args, "text")?;
            self.write_input(run, canceled, tool, json!({"ticket": k}))?;
            self.act(run, tool, json!({"ticket": k}), || t.comment(k, body))?;
        } else {
            let name = text(args, "state")?;
            let state = State::ALL
                .into_iter()
                .find(|s| s.name() == name)
                .ok_or_else(|| format!("unknown state {name:?}"))?;
            self.write_input(run, canceled, tool, json!({"ticket": k}))?;
            let target = json!({"ticket": k, "state": name});
            self.act(run, tool, target, || t.transition(k, state))?;
        }
        Ok(json!({"ok": true}))
    }

    fn create_linked(&self, run: &Run, canceled: bool, args: &Value) -> Reply {
        let kind = match text(args, "kind")? {
            "question" => ReworkKind::Question,
            "amendment" => ReworkKind::Amendment,
            "replan" => ReworkKind::Replan,
            other => return Err(format!("unknown kind {other:?}")),
        };
        let evidence = args["evidence"]
            .as_array()
            .and_then(|l| l.iter().map(|e| e.as_str().map(str::to_owned)).collect())
            .ok_or("evidence must be a list of strings")?;
        let fields = Rework {
            stage: text(args, "stage")?.to_owned(),
            artifact: text(args, "artifact")?.to_owned(),
            version: text(args, "version")?.to_owned(),
            text: text(args, "text")?.to_owned(),
            evidence,
        };
        let target = json!({"parent": run.ticket});
        self.write_input(run, canceled, "create_linked", target.clone())?;
        let t = &self.adapters.tracker;
        let key = self.act(run, "create_linked", target, || {
            t.create_linked(&run.ticket, kind, &fields)
        })?;
        Ok(json!({"key": key}))
    }
}
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-tool-gateway && cargo clippy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (tools 8), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kod-tool-gateway
git commit -m "kod-tool-gateway: tracker and docs tools behind policy and the audit log"
```

### Task 2: Publish and the token broker

**Files:**
- Create: `crates/kod-tool-gateway/src/broker.rs`, `src/forge.rs`
- Modify: `crates/kod-tool-gateway/src/lib.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs` (modify), `tests/publish.rs`

**Interfaces:**
- Consumes: Task 1's `Gateway`, `Run`, `Adapters`, `Reply`; the policy plan's `publish` input (`run.{id, ticket, repo, canceled, data_classes}`, `target.{repo, branch, draft}`, `changed_paths`).
- Produces: `broker::{Secret::{new, expose}, Broker::mint(&str) -> anyhow::Result<Secret>}`; `forge::{Change { path, content: Option<String> }, Identity { name, email }, Commit { repo, base, changes, message, committer }, Forge::{commit, set_branch, find_pr, open_draft_pr}, FakeGitHub, FakePr}`; `Adapters` gains `broker: Arc<dyn Broker>` and `forge: Arc<dyn Forge>`; `Gateway::new` gains `agent: Identity`; the `publish` tool returning `{"branch", "commit", "pr"}`; `MAX_FILES`, `MAX_PUBLISH_BYTES`, `MAX_TITLE`.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-tool-gateway/tests/common/mod.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/tests/common/mod.rs b/crates/kod-tool-gateway/tests/common/mod.rs
index 5ecc930..4c590d1 100644
--- a/crates/kod-tool-gateway/tests/common/mod.rs
+++ b/crates/kod-tool-gateway/tests/common/mod.rs
@@ -1,7 +1,11 @@
 #![allow(dead_code)]
 use kod_audit::AuditLog;
 use kod_policy::Policy;
-use kod_tool_gateway::{Adapters, Gateway, Run, docs::FakeDocs};
+use kod_tool_gateway::{
+    Adapters, Gateway, Run,
+    docs::FakeDocs,
+    forge::{FakeGitHub, Identity},
+};
 use kod_tracker::fake::{self, FakeIssue, FakeTracker};
 use std::{path::PathBuf, sync::Arc};
 
@@ -16,6 +20,7 @@ pub struct Harness {
     pub gw: Arc<Gateway>,
     pub tracker: Arc<FakeTracker>,
     pub docs: Arc<FakeDocs>,
+    pub github: Arc<FakeGitHub>,
     pub audit: PathBuf,
     _dir: tempfile::TempDir,
 }
@@ -56,16 +61,25 @@ pub fn harness() -> Harness {
     let docs = Arc::new(FakeDocs::default());
     docs.add(LINKED_DOC, "the approved brief");
     docs.add(SECRET_DOC, PLANTED);
+    let github = Arc::new(FakeGitHub::default());
+    github.add_repo("acme/app", BASE);
     let adapters = Adapters {
         tracker: tracker.clone(),
         docs: docs.clone(),
+        broker: github.clone(),
+        forge: github.clone(),
     };
-    let gw = Arc::new(Gateway::new(policy, log, adapters).unwrap());
+    let agent = Identity {
+        name: "kod-agent".into(),
+        email: "kod-agent@users.noreply.example".into(),
+    };
+    let gw = Arc::new(Gateway::new(policy, log, adapters, agent).unwrap());
     gw.start_run(TOKEN, run()).unwrap();
     Harness {
         gw,
         tracker,
         docs,
+        github,
         audit,
         _dir: dir,
     }
````

Create `crates/kod-tool-gateway/tests/publish.rs`:

````rust
mod common;

use common::*;
use kod_tool_gateway::Run;
use serde_json::{Value, json};

fn change(path: &str) -> Value {
    json!({
        "title": "Hold refunds over 100",
        "summary": "Refunds over 100 wait for a second approver.",
        "test_evidence": "cargo test: 12 passed",
        "files": [{"path": path, "content": "fn hold() {}\n"}, {"path": "old.rs", "content": null}],
    })
}

/// The run's branch and the PRs: nothing a refused publish may change.
fn nothing_published(h: &Harness) {
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
    assert!(h.github.prs().is_empty());
    assert!(
        h.github.minted().is_empty(),
        "no token is minted for a refusal"
    );
}

#[test]
fn publish_pushes_the_agent_branch_and_opens_a_draft_pr() {
    let h = harness();
    let r =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap();
    assert_eq!(r["branch"], "agent/FAKE-1-run-1");
    let sha = r["commit"].as_str().unwrap();
    assert_eq!(
        h.github.branch("acme/app", "agent/FAKE-1-run-1").as_deref(),
        Some(sha)
    );
    let commit = h.github.commit_of(sha).unwrap();
    assert_eq!(commit.base, BASE);
    assert_eq!(commit.committer.name, "kod-agent");
    assert!(commit.message.ends_with(
        "Kod-Ticket: fake:FAKE-1\nKod-Run: run-1\nCo-Authored-By: kod-agent <kod-agent@users.noreply.example>\n"
    ));
    let prs = h.github.prs();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].draft);
    assert_eq!(prs[0].title, "FAKE-1: Hold refunds over 100");
    for part in [
        "Ticket: fake:FAKE-1",
        "Requirement: REQ-7",
        "Run: run-1",
        LINKED_DOC,
        "12 passed",
    ] {
        assert!(
            prs[0].body.contains(part),
            "{part} missing from {}",
            prs[0].body
        );
    }
    assert_eq!(r["pr"], prs[0].url);
    assert!(h.tracker.prs("FAKE-1").contains(&prs[0].url));
}

#[test]
fn the_publish_credential_never_leaves_the_gateway() {
    let h = harness();
    let r =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap();
    let minted = h.github.minted();
    assert_eq!(minted.len(), 1);
    let log = String::from_utf8_lossy(&std::fs::read(&h.audit).unwrap()).into_owned();
    for t in &minted {
        assert!(!r.to_string().contains(t.as_str()));
        assert!(!log.contains(t.as_str()));
    }
    let call = h
        .records()
        .into_iter()
        .find(|r| r["tool"] == "publish")
        .unwrap();
    assert_eq!(
        call["target"]["changed_paths"],
        json!(["src/refund.rs", "old.rs"])
    );
}

#[test]
fn publishing_again_reuses_the_branch_and_the_pr() {
    let h = harness();
    let first =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap();
    let again =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap();
    assert_eq!(first, again);
    assert_eq!(h.github.prs().len(), 1);
}

#[test]
fn a_publish_that_died_before_the_pr_completes_on_retry() {
    let h = harness();
    h.github.fail_next_pr();
    let e =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap_err();
    assert!(e.contains("connection reset"), "{e}");
    let r =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap();
    assert_eq!(h.github.prs().len(), 1);
    assert_eq!(r["pr"], h.github.prs()[0].url);
}

#[test]
fn publish_is_refused_for_a_canceled_run() {
    let h = harness();
    h.gw.cancel("run-1");
    let e =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap_err();
    assert!(e.contains("the run is canceled"), "{e}");
    nothing_published(&h);
}

#[test]
fn publish_is_refused_for_a_path_that_raises_the_risk_class() {
    let h = harness();
    let e =
        h.gw.call(TOKEN, "publish", &change("auth/session.rs"))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    nothing_published(&h);
}

#[test]
fn publish_is_refused_for_a_data_class_not_cleared() {
    let h = harness();
    let token = "2".repeat(64);
    let run = Run {
        id: "run-2".into(),
        data_classes: vec!["customer-pii".into()],
        ..run()
    };
    h.gw.start_run(&token, run).unwrap();
    let e =
        h.gw.call(&token, "publish", &change("src/refund.rs"))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    assert!(h.github.prs().is_empty());
    assert!(h.github.minted().is_empty());
}

#[test]
fn publish_is_refused_when_the_audit_write_fails() {
    let h = harness();
    // Another writer holds the log, so every append times out.
    let db = rusqlite::Connection::open(&h.audit).unwrap();
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let e =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap_err();
    assert!(e.starts_with("refused: the audit write failed"), "{e}");
    nothing_published(&h);
}

#[test]
fn publish_checks_the_shape_of_the_change() {
    let h = harness();
    let mut twice = change("a.rs");
    twice["files"] = json!([{"path": "a.rs", "content": "1"}, {"path": "a.rs", "content": "2"}]);
    let e = h.gw.call(TOKEN, "publish", &twice).unwrap_err();
    assert_eq!(e, "a.rs is listed twice");
    let mut empty = change("a.rs");
    empty["files"] = json!([]);
    assert!(h.gw.call(TOKEN, "publish", &empty).is_err());
    let mut title = change("a.rs");
    title["title"] = json!("two\nlines");
    assert!(h.gw.call(TOKEN, "publish", &title).is_err());
    let mut binary = change("a.rs");
    binary["files"] = json!([{"path": "a.rs", "content": [1, 2]}]);
    assert!(h.gw.call(TOKEN, "publish", &binary).is_err());
    nothing_published(&h);
}

#[test]
fn a_cancel_during_publish_stops_it_before_the_branch_moves() {
    let h = harness();
    let gw = std::sync::Arc::downgrade(&h.gw);
    h.github.on_commit(move || {
        gw.upgrade().unwrap().cancel("run-1");
    });
    let e =
        h.gw.call(TOKEN, "publish", &change("src/refund.rs"))
            .unwrap_err();
    assert!(e.contains("the run was canceled during publish"), "{e}");
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
    assert!(h.github.prs().is_empty());
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-tool-gateway --test publish`
Expected: FAIL to compile: unresolved import `kod_tool_gateway::forge`.

- [ ] **Step 3: Implement the broker, the forge and publish**

Create `crates/kod-tool-gateway/src/broker.rs`:

````rust
//! The token broker. The gateway asks it for a credential inside the one
//! operation that needs it and drops it when the operation ends, so no
//! credential is ever in a tool result, an audit record or the sandbox.
use anyhow::Result;
use std::fmt;

/// A credential. It can't be printed or serialized; `expose` is for the
/// adapter call that sends it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Mints a short-lived credential that can push agent/* branches and open
/// draft PRs in one repo, and nothing else.
pub trait Broker: Send + Sync {
    fn mint(&self, repo: &str) -> Result<Secret>;
}
````

Create `crates/kod-tool-gateway/src/forge.rs`:

````rust
//! The git host behind the publish operation, and a fake of it for tests.
use crate::broker::{Broker, Secret};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

/// One file in a publish: its new text, or None to delete it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    pub content: Option<String>,
}

/// The agent's committer identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// A commit to create on top of `base`.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub repo: String,
    pub base: String,
    pub changes: Vec<Change>,
    pub message: String,
    pub committer: Identity,
}

/// The git host calls the publish operation makes. Every call carries the
/// credential the broker minted for this publish.
pub trait Forge: Send + Sync {
    /// Creates the commit and returns its SHA. The same commit twice gives
    /// the same SHA.
    fn commit(&self, token: &Secret, commit: &Commit) -> Result<String>;
    /// Points the branch at `sha`, creating it if needed.
    fn set_branch(&self, token: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()>;
    /// The URL of the open PR from `branch`, if there is one.
    fn find_pr(&self, token: &Secret, repo: &str, branch: &str) -> Result<Option<String>>;
    /// Opens a draft PR from `branch` and returns its URL.
    fn open_draft_pr(
        &self,
        token: &Secret,
        repo: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String>;
}

/// A PR as the fake stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct FakePr {
    pub repo: String,
    pub branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub url: String,
}

#[derive(Default)]
struct Inner {
    /// Token to the repo it was minted for.
    tokens: HashMap<String, String>,
    minted: u64,
    /// Repo to the commits that exist in it.
    repos: HashMap<String, Vec<String>>,
    commits: HashMap<String, Commit>,
    branches: BTreeMap<(String, String), String>,
    prs: Vec<FakePr>,
    fail_next_pr: bool,
    on_commit: Option<Box<dyn FnOnce() + Send>>,
}

/// An in-memory GitHub: it mints repo-scoped tokens and refuses any call
/// without a live token for that repo.
#[derive(Default)]
pub struct FakeGitHub(Mutex<Inner>);

impl FakeGitHub {
    /// Adds a repo whose default branch is at `base`.
    pub fn add_repo(&self, repo: &str, base: &str) {
        self.lock()
            .repos
            .insert(repo.to_owned(), vec![base.to_owned()]);
    }

    /// Every token minted so far, live or revoked.
    pub fn minted(&self) -> Vec<String> {
        (1..=self.lock().minted).map(token).collect()
    }

    /// Revokes every token, as the kill switch does.
    pub fn revoke_all(&self) {
        self.lock().tokens.clear();
    }

    pub fn branch(&self, repo: &str, branch: &str) -> Option<String> {
        let key = (repo.to_owned(), branch.to_owned());
        self.lock().branches.get(&key).cloned()
    }

    pub fn commit_of(&self, sha: &str) -> Option<Commit> {
        self.lock().commits.get(sha).cloned()
    }

    pub fn prs(&self) -> Vec<FakePr> {
        self.lock().prs.clone()
    }

    /// The next `open_draft_pr` fails, as if the process died mid-publish.
    pub fn fail_next_pr(&self) {
        self.lock().fail_next_pr = true;
    }

    /// Runs `f` once, at the start of the next `commit`.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.lock().on_commit = Some(Box::new(f));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Locks after checking the token is live and minted for `repo`.
    fn authorized(&self, t: &Secret, repo: &str) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let inner = self.lock();
        if inner.tokens.get(t.expose()).map(String::as_str) != Some(repo) {
            bail!("fake github: bad credential for {repo}");
        }
        Ok(inner)
    }
}

fn token(n: u64) -> String {
    format!("ghs_fake{n:032}")
}

impl Broker for FakeGitHub {
    fn mint(&self, repo: &str) -> Result<Secret> {
        let mut inner = self.lock();
        if !inner.repos.contains_key(repo) {
            bail!("fake github: no repo {repo}");
        }
        inner.minted += 1;
        let t = token(inner.minted);
        inner.tokens.insert(t.clone(), repo.to_owned());
        Ok(Secret::new(t))
    }
}

impl Forge for FakeGitHub {
    fn commit(&self, t: &Secret, c: &Commit) -> Result<String> {
        let hook = self.lock().on_commit.take();
        if let Some(f) = hook {
            f();
        }
        let mut inner = self.authorized(t, &c.repo)?;
        if !inner.repos[&c.repo].contains(&c.base) {
            bail!("fake github: no commit {} in {}", c.base, c.repo);
        }
        let digest = Sha256::digest(format!("{c:?}"));
        let sha: String = digest[..20].iter().map(|b| format!("{b:02x}")).collect();
        inner.repos.get_mut(&c.repo).unwrap().push(sha.clone());
        inner.commits.insert(sha.clone(), c.clone());
        Ok(sha)
    }

    fn set_branch(&self, t: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()> {
        let mut inner = self.authorized(t, repo)?;
        if !inner.repos[repo].iter().any(|s| s == sha) {
            bail!("fake github: no commit {sha} in {repo}");
        }
        let key = (repo.to_owned(), branch.to_owned());
        inner.branches.insert(key, sha.to_owned());
        Ok(())
    }

    fn find_pr(&self, t: &Secret, repo: &str, branch: &str) -> Result<Option<String>> {
        let inner = self.authorized(t, repo)?;
        let pr = inner
            .prs
            .iter()
            .find(|p| p.repo == repo && p.branch == branch);
        Ok(pr.map(|p| p.url.clone()))
    }

    fn open_draft_pr(
        &self,
        t: &Secret,
        repo: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let mut inner = self.authorized(t, repo)?;
        if std::mem::take(&mut inner.fail_next_pr) {
            bail!("fake github: connection reset");
        }
        let key = (repo.to_owned(), branch.to_owned());
        if !inner.branches.contains_key(&key) {
            bail!("fake github: no branch {branch} in {repo}");
        }
        let url = format!("https://github.example/{repo}/pull/{}", inner.prs.len() + 1);
        inner.prs.push(FakePr {
            repo: repo.to_owned(),
            branch: branch.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            draft: true,
            url: url.clone(),
        });
        Ok(url)
    }
}
````

Apply to `crates/kod-tool-gateway/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/src/lib.rs b/crates/kod-tool-gateway/src/lib.rs
index 4c24775..5bc4243 100644
--- a/crates/kod-tool-gateway/src/lib.rs
+++ b/crates/kod-tool-gateway/src/lib.rs
@@ -2,22 +2,33 @@
 //! and docs tools and for the publish operation. Every call is checked by
 //! kod-policy and recorded in the audit log before it acts; a call that
 //! can't be decided or recorded doesn't happen.
-use anyhow::bail;
+use anyhow::{Context, bail};
+use broker::Broker;
 use docs::Docs;
+use forge::{Change, Commit, Forge, Identity};
 use kod_audit::{AuditLog, RUN_AUDIT};
 use kod_policy::Policy;
 use kod_tracker::{Issue, Rework, ReworkKind, State, Tracker};
 use serde_json::{Value, json};
 use sha2::{Digest, Sha256};
 use std::{
-    collections::HashMap,
+    collections::{HashMap, HashSet},
     sync::{Arc, Mutex},
 };
 
+pub mod broker;
 pub mod docs;
+pub mod forge;
 /// A run token is at least this many lower-case hex characters, as in the
 /// LLM gateway.
 pub const MIN_TOKEN_LEN: usize = 32;
+/// At most this many files in one publish.
+pub const MAX_FILES: usize = 1000;
+/// At most this many bytes of file text in one publish.
+pub const MAX_PUBLISH_BYTES: usize = 8 << 20;
+/// At most this many bytes in a publish title.
+pub const MAX_TITLE: usize = 200;
+
 /// A run the gateway serves. `base` is the commit its workspace started from.
 #[derive(Clone, Debug, PartialEq)]
 pub struct Run {
@@ -38,12 +49,15 @@ struct Live {
 pub struct Adapters {
     pub tracker: Arc<dyn Tracker>,
     pub docs: Arc<dyn Docs>,
+    pub broker: Arc<dyn Broker>,
+    pub forge: Arc<dyn Forge>,
 }
 
 pub struct Gateway {
     policy: Arc<Policy>,
     audit: Arc<AuditLog>,
     adapters: Adapters,
+    agent: Identity,
     /// Keyed by the token's SHA-256, as in the LLM gateway.
     runs: Mutex<HashMap<String, Live>>,
 }
@@ -87,16 +101,55 @@ fn issue_json(i: &Issue) -> Value {
     })
 }
 
+/// The files of a publish, checked for shape and size. Path rules are the
+/// policy's.
+fn changes(args: &Value) -> Result<Vec<Change>, String> {
+    let files = args["files"]
+        .as_array()
+        .filter(|f| !f.is_empty() && f.len() <= MAX_FILES)
+        .ok_or_else(|| format!("files must be a list of 1 to {MAX_FILES} files"))?;
+    let mut seen = HashSet::new();
+    let mut bytes = 0;
+    let mut out = vec![];
+    for f in files {
+        let path = text(f, "path")?.to_owned();
+        let content = match &f["content"] {
+            Value::Null => None,
+            Value::String(s) => Some(s.clone()),
+            _ => {
+                return Err(format!(
+                    "{path}: content must be a string, or null to delete"
+                ));
+            }
+        };
+        if !seen.insert(path.clone()) {
+            return Err(format!("{path} is listed twice"));
+        }
+        bytes += content.as_ref().map_or(0, String::len);
+        out.push(Change { path, content });
+    }
+    if bytes > MAX_PUBLISH_BYTES {
+        return Err(format!("the files are over {MAX_PUBLISH_BYTES} bytes"));
+    }
+    Ok(out)
+}
+
 impl Gateway {
+    /// Fails if the agent identity is not a usable committer.
     pub fn new(
         policy: Arc<Policy>,
         audit: Arc<AuditLog>,
         adapters: Adapters,
+        agent: Identity,
     ) -> anyhow::Result<Gateway> {
+        if !named(&agent.name) || !named(&agent.email) || !agent.email.contains('@') {
+            bail!("the agent identity needs a name and an email address, each on one line");
+        }
         Ok(Gateway {
             policy,
             audit,
             adapters,
+            agent,
             runs: Mutex::default(),
         })
     }
@@ -168,6 +221,7 @@ impl Gateway {
             "read_doc" => self.read_doc(&run, args),
             "comment" | "transition" => self.write(&run, canceled, tool, args),
             "create_linked" => self.create_linked(&run, canceled, args),
+            "publish" => self.publish(token, &run, canceled, args),
             other => Err(format!("unknown tool {other:?}")),
         }
     }
@@ -313,4 +367,78 @@ impl Gateway {
         })?;
         Ok(json!({"key": key}))
     }
+
+    fn publish(&self, token: &str, run: &Run, canceled: bool, args: &Value) -> Reply {
+        let title = text(args, "title")?;
+        if !named(title) || title.len() > MAX_TITLE {
+            return Err(format!(
+                "title must be one line of at most {MAX_TITLE} bytes"
+            ));
+        }
+        let summary = text(args, "summary")?;
+        let evidence = text(args, "test_evidence")?;
+        let files = changes(args)?;
+        let paths: Vec<&str> = files.iter().map(|c| c.path.as_str()).collect();
+        let branch = format!("agent/{}-{}", run.ticket, run.id);
+        self.allow(json!({
+            "action": "publish",
+            "actor": {"kind": "agent"},
+            "run": {
+                "id": run.id,
+                "ticket": run.ticket,
+                "repo": run.repo,
+                "canceled": canceled,
+                "data_classes": run.data_classes,
+            },
+            "target": {"repo": run.repo, "branch": branch, "draft": true},
+            "changed_paths": paths,
+        }))?;
+        let issue = self.own_issue(run)?;
+        let tracker = self.adapters.tracker.name();
+        let a = &self.agent;
+        let message = format!(
+            "{title}\n\n{summary}\n\nKod-Ticket: {tracker}:{}\nKod-Run: {}\nCo-Authored-By: {} <{}>\n",
+            run.ticket, run.id, a.name, a.email
+        );
+        let pinned: String = issue
+            .approved_versions
+            .iter()
+            .map(|v| format!("- {v}\n"))
+            .collect();
+        let body = format!(
+            "{summary}\n\nTicket: {tracker}:{}\nRequirement: {}\nRun: {}\n\nPinned inputs:\n{pinned}\nTest evidence:\n{evidence}\n",
+            run.ticket,
+            issue.requirement_id.as_deref().unwrap_or("none"),
+            run.id,
+        );
+        let pr_title = format!("{}: {title}", run.ticket);
+        let target =
+            json!({"repo": run.repo, "branch": branch, "draft": true, "changed_paths": paths});
+        let (f, t) = (&self.adapters.forge, &self.adapters.tracker);
+        let (sha, pr) = self.act(run, "publish", target, || {
+            let token_ = self.adapters.broker.mint(&run.repo)?;
+            let commit = Commit {
+                repo: run.repo.clone(),
+                base: run.base.clone(),
+                changes: files,
+                message,
+                committer: a.clone(),
+            };
+            let sha = f.commit(&token_, &commit)?;
+            // A cancel can land while the commit is made; the branch is the
+            // first thing anyone else can see, so check again here.
+            if self.run(token).is_none_or(|(_, c)| c) {
+                bail!("the run was canceled during publish");
+            }
+            f.set_branch(&token_, &run.repo, &branch, &sha)?;
+            let pr = match f.find_pr(&token_, &run.repo, &branch)? {
+                Some(url) => url,
+                None => f.open_draft_pr(&token_, &run.repo, &branch, &pr_title, &body)?,
+            };
+            t.link_pr(&run.ticket, &pr)
+                .context("linking the PR to the ticket")?;
+            Ok((sha, pr))
+        })?;
+        Ok(json!({"branch": branch, "commit": sha, "pr": pr}))
+    }
 }
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-tool-gateway && cargo clippy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (publish 10, tools 8), clean. `publish_is_refused_when_the_audit_write_fails` takes about 2 s (the audit log's busy timeout).

- [ ] **Step 5: Commit**

```bash
git add crates/kod-tool-gateway
git commit -m "kod-tool-gateway: idempotent publish with a brokered credential"
```

### Task 3: The MCP endpoint

**Files:**
- Modify: `crates/kod-tool-gateway/Cargo.toml`, `src/lib.rs`
- Create: `crates/kod-tool-gateway/src/mcp.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs` (modify), `tests/mcp.rs`

**Interfaces:**
- Consumes: Task 1 and 2's `Gateway::call` and `Reply`.
- Produces: `kod_tool_gateway::router(Arc<Gateway>, Vec<String>) -> axum::Router` serving `POST /r/{token}/mcp`; `mcp::tools() -> Vec<rmcp::model::Tool>` (the six tools with their input schemas); test helper `common::serve(Arc<Gateway>) -> String` (base URL).

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-tool-gateway/Cargo.toml` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/Cargo.toml b/crates/kod-tool-gateway/Cargo.toml
index 7244391..e6880e3 100644
--- a/crates/kod-tool-gateway/Cargo.toml
+++ b/crates/kod-tool-gateway/Cargo.toml
@@ -6,12 +6,17 @@ publish = false
 
 [dependencies]
 anyhow = "1"
+axum = "0.8"
 kod-audit = { path = "../kod-audit" }
 kod-policy = { path = "../kod-policy" }
 kod-tracker = { path = "../kod-tracker" }
+rmcp = { version = "3.5", default-features = false, features = ["server", "transport-streamable-http-server"] }
 serde_json = "1"
 sha2 = "0.10"
+tokio = { version = "1", features = ["rt-multi-thread", "net"] }
+tower = { version = "0.5", features = ["util"] }
 
 [dev-dependencies]
+reqwest = { version = "0.12", default-features = false, features = ["json"] }
 rusqlite = "0.40"
 tempfile = "3"
````

Apply to `crates/kod-tool-gateway/tests/common/mod.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/tests/common/mod.rs b/crates/kod-tool-gateway/tests/common/mod.rs
index 4c590d1..bba941c 100644
--- a/crates/kod-tool-gateway/tests/common/mod.rs
+++ b/crates/kod-tool-gateway/tests/common/mod.rs
@@ -107,3 +107,19 @@ impl Harness {
             .collect()
     }
 }
+
+/// Serves the gateway's routes on a free local port from a thread of its
+/// own and returns the base URL.
+pub fn serve(gw: Arc<Gateway>) -> String {
+    let (tx, rx) = std::sync::mpsc::channel();
+    std::thread::spawn(move || {
+        let rt = tokio::runtime::Runtime::new().unwrap();
+        rt.block_on(async move {
+            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
+            tx.send(listener.local_addr().unwrap()).unwrap();
+            let app = kod_tool_gateway::router(gw, vec!["127.0.0.1".into()]);
+            axum::serve(listener, app).await.unwrap();
+        });
+    });
+    format!("http://{}", rx.recv().unwrap())
+}
````

Create `crates/kod-tool-gateway/tests/mcp.rs`:

````rust
mod common;

use common::*;
use serde_json::{Value, json};

/// One stateless MCP request (protocol 2026-07-28) to `url`.
fn mcp(url: &str, method: &str, name: Option<&str>, params: Value) -> (u16, Value) {
    let mut params = params;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut req = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", method)
            .json(&body);
        if let Some(n) = name {
            req = req.header("mcp-name", n);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    })
}

#[test]
fn the_endpoint_lists_the_tools() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let (status, r) = mcp(&url, "tools/list", None, json!({}));
    assert_eq!(status, 200, "{r}");
    let names: Vec<_> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let want = [
        "get_issue",
        "read_doc",
        "comment",
        "transition",
        "create_linked",
        "publish",
    ];
    assert_eq!(names, want);
}

#[test]
fn a_tool_call_goes_through_the_endpoint_and_the_policy() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let params = json!({"name": "get_issue", "arguments": {"key": "FAKE-1"}});
    let (status, r) = mcp(&url, "tools/call", Some("get_issue"), params);
    assert_eq!(status, 200, "{r}");
    assert_ne!(r["result"]["isError"], true, "{r}");
    let issue: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(issue["key"], "FAKE-1");
    let refused = json!({"name": "get_issue", "arguments": {"key": "FAKE-9"}});
    let (_, r) = mcp(&url, "tools/call", Some("get_issue"), refused);
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(!r.to_string().contains(PLANTED));
    assert_eq!(
        h.decisions().iter().map(|d| d.0).collect::<Vec<_>>(),
        [true, false]
    );
}

#[test]
fn an_unknown_token_gets_no_endpoint() {
    let h = harness();
    let url = format!("{}/r/{}/mcp", serve(h.gw.clone()), "e".repeat(64));
    let (status, _) = mcp(&url, "tools/list", None, json!({}));
    assert_eq!(status, 404);
    assert!(h.records().is_empty());
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-tool-gateway --test mcp`
Expected: FAIL to compile: unresolved import `kod_tool_gateway::router`.

- [ ] **Step 3: Implement the endpoint**

Create `crates/kod-tool-gateway/src/mcp.rs`:

````rust
//! The MCP endpoint. One route per run token, `/r/{token}/mcp`, served by
//! rmcp's Streamable HTTP transport in stateless JSON mode.
use crate::Gateway;
use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::any,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

/// The run token of the request, put there by the route after it checked
/// the token names a run.
#[derive(Clone)]
struct RunToken(String);

#[derive(Clone)]
struct Handler(Arc<Gateway>);

fn schema(props: Value, required: &[&str]) -> Arc<serde_json::Map<String, Value>> {
    let s = json!({"type": "object", "properties": props, "required": required});
    match s {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!(),
    }
}

/// The tools the endpoint lists. `publish` is listed too; the client in
/// kod-loop decides not to offer it to the model.
pub fn tools() -> Vec<Tool> {
    let s = json!({"type": "string"});
    vec![
        Tool::new(
            "get_issue",
            "Read a ticket: the run's own ticket or one linked to it.",
            schema(json!({"key": s}), &["key"]),
        ),
        Tool::new(
            "read_doc",
            "Read an approved document linked from the run's ticket, by its pinned ID.",
            schema(json!({"id": s}), &["id"]),
        ),
        Tool::new(
            "comment",
            "Comment on the run's ticket or a ticket linked to it.",
            schema(json!({"key": s, "text": s}), &["key", "text"]),
        ),
        Tool::new(
            "transition",
            "Move the run's ticket or a linked ticket to a state: ready, in_progress, in_review, blocked or done.",
            schema(json!({"key": s, "state": s}), &["key", "state"]),
        ),
        Tool::new(
            "create_linked",
            "Open a question, amendment or replan request linked to the run's ticket.",
            schema(
                json!({"kind": s, "stage": s, "artifact": s, "version": s, "text": s,
                       "evidence": {"type": "array", "items": s}}),
                &["kind", "stage", "artifact", "version", "text", "evidence"],
            ),
        ),
        Tool::new(
            "publish",
            "Push the run's changes to its agent branch and open a draft PR.",
            schema(
                json!({"title": s, "summary": s, "test_evidence": s,
                       "files": {"type": "array", "items": {"type": "object",
                         "properties": {"path": s, "content": {"type": ["string", "null"]}},
                         "required": ["path", "content"]}}}),
                &["title", "summary", "test_evidence", "files"],
            ),
        ),
    ]
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        req: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let token = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<RunToken>())
            .map(|t| t.0.clone())
            .ok_or_else(|| ErrorData::invalid_request("no run token", None))?;
        let args = Value::Object(req.arguments.unwrap_or_default());
        let (gateway, name) = (self.0.clone(), req.name.to_string());
        // The adapters block; keep them off the async workers.
        let reply = tokio::task::spawn_blocking(move || gateway.call(&token, &name, &args))
            .await
            .unwrap_or_else(|e| Err(format!("the tool call failed: {e}")));
        Ok(match reply {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}

/// The routes of the tool gateway. `hosts` are the `Host` values the
/// endpoint answers to, as rmcp checks them against DNS rebinding.
pub fn router(gateway: Arc<Gateway>, hosts: Vec<String>) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts(hosts);
    let handler = Handler(gateway.clone());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .route(
            "/r/{token}/mcp",
            any(
                move |State(g): State<Arc<Gateway>>,
                      Path(token): Path<String>,
                      mut req: Request| {
                    let service = service.clone();
                    async move {
                        if g.run(&token).is_none() {
                            return (StatusCode::NOT_FOUND, "unknown run").into_response();
                        }
                        req.extensions_mut().insert(RunToken(token));
                        match service.oneshot(req).await {
                            Ok(r) => r.map(Body::new),
                            Err(e) => match e {},
                        }
                    }
                },
            ),
        )
        .with_state(gateway)
}
````

Apply to `crates/kod-tool-gateway/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/src/lib.rs b/crates/kod-tool-gateway/src/lib.rs
index 5bc4243..d0517ee 100644
--- a/crates/kod-tool-gateway/src/lib.rs
+++ b/crates/kod-tool-gateway/src/lib.rs
@@ -19,6 +19,10 @@ use std::{
 pub mod broker;
 pub mod docs;
 pub mod forge;
+mod mcp;
+
+pub use mcp::router;
+
 /// A run token is at least this many lower-case hex characters, as in the
 /// LLM gateway.
 pub const MIN_TOKEN_LEN: usize = 32;
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-tool-gateway && cargo clippy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (mcp 3, publish 10, tools 8), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-tool-gateway
git commit -m "kod-tool-gateway: MCP endpoint per run token"
```

### Task 4: The MCP client in kod-loop

**Files:**
- Modify: `crates/kod-loop/Cargo.toml`, `src/lib.rs`, `src/main.rs`
- Create: `crates/kod-loop/src/mcp.rs`
- Test: `crates/kod-loop/tests/mcp.rs`

**Interfaces:**
- Consumes: Task 3's `router` and the `/r/{token}/mcp` protocol; kod-loop's `tools::{definitions, call, ToolResult}` and `run`.
- Produces: `kod_loop::mcp::{Mcp::{connect(&str) -> anyhow::Result<Mcp>, definitions, offers, call(&str, &Value) -> ToolResult}, PROTOCOL, TIMEOUT, HIDDEN}`; `kod_loop::run_with(&Config, Option<&Mcp>, &str, &mut dyn Write) -> Outcome` (`run` is `run_with(.., None, ..)`); `kod-loop` reads optional `KOD_MCP_URL`.

- [ ] **Step 1: Write the failing test**

Apply to `crates/kod-loop/Cargo.toml` with `git apply`:

````diff
diff --git a/crates/kod-loop/Cargo.toml b/crates/kod-loop/Cargo.toml
index f9064c3..9af3c83 100644
--- a/crates/kod-loop/Cargo.toml
+++ b/crates/kod-loop/Cargo.toml
@@ -17,5 +17,8 @@ axum = "0.8"
 kod-audit = { path = "../kod-audit" }
 kod-llm-proxy = { path = "../kod-llm-proxy" }
 kod-policy = { path = "../kod-policy" }
+kod-tool-gateway = { path = "../kod-tool-gateway" }
+kod-tracker = { path = "../kod-tracker" }
+rusqlite = "0.40"
 tempfile = "3"
 tokio = { version = "1", features = ["rt-multi-thread", "net"] }
````

Create `crates/kod-loop/tests/mcp.rs`:

````rust
mod common;

use common::*;
use kod_audit::AuditLog;
use kod_loop::{EndState, mcp::Mcp};
use kod_policy::Policy;
use kod_tool_gateway::{
    Adapters, Gateway, Run,
    docs::FakeDocs,
    forge::{FakeGitHub, Identity},
};
use kod_tracker::fake::{self, FakeIssue};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PLANTED: &str = "PLANTED-SECRET-7f3a";

/// A tool gateway over the fakes serving run-1 for FAKE-1; FAKE-9 holds a
/// planted secret. Returns the MCP URL and the audit log's path.
fn gateway(dir: &std::path::Path) -> (String, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(Policy::load(&root.join("policy")).unwrap());
    let audit = dir.join("audit.db");
    let log = Arc::new(AuditLog::open(&audit).unwrap());
    let (tracker, _) = fake::sandbox();
    let planted = FakeIssue {
        body: PLANTED.into(),
        workflow_state: "Backlog".into(),
        ..FakeIssue::default()
    };
    tracker.add_issue("FAKE-9", planted);
    let github = Arc::new(FakeGitHub::default());
    let adapters = Adapters {
        tracker: Arc::new(tracker),
        docs: Arc::new(FakeDocs::default()),
        broker: github.clone(),
        forge: github,
    };
    let agent = Identity {
        name: "kod-agent".into(),
        email: "kod-agent@users.noreply.example".into(),
    };
    let gw = Arc::new(Gateway::new(policy, log, adapters, agent).unwrap());
    let run = Run {
        id: "run-1".into(),
        ticket: "FAKE-1".into(),
        repo: "acme/app".into(),
        base: "b".repeat(40),
        data_classes: vec!["placeholder".into()],
    };
    gw.start_run(TOKEN, run).unwrap();
    let base = serve(kod_tool_gateway::router(gw, vec!["127.0.0.1".into()]));
    (format!("{base}/r/{TOKEN}/mcp"), audit)
}

#[test]
fn the_agents_tracker_calls_go_through_the_gateway() {
    let ws = tempfile::tempdir().unwrap();
    let (mcp_url, audit) = gateway(ws.path());
    let mcp = Mcp::connect(&mcp_url).unwrap();
    let (url, m) = model(vec![
        msg(
            json!([tool("t1", "get_issue", json!({"key": "FAKE-1"}))]),
            "tool_use",
        ),
        msg(
            json!([tool("t2", "get_issue", json!({"key": "FAKE-9"}))]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t3",
                "finish",
                json!({"state": "done", "summary": "read it"})
            )]),
            "tool_use",
        ),
    ]);
    let mut trace = vec![];
    let o = kod_loop::run_with(
        &config(&url, ws.path()),
        Some(&mcp),
        "the ticket",
        &mut trace,
    );
    assert_eq!((o.end_state, o.error), (EndState::Done, None));
    let sent = bodies(&m);
    let offered: Vec<&str> = sent[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(offered.contains(&"get_issue") && offered.contains(&"run"));
    assert!(!offered.contains(&"publish"), "publish is never offered");
    let result =
        |i: usize| sent[i]["messages"].as_array().unwrap().last().unwrap()["content"][0].clone();
    assert_eq!(result(1)["is_error"], false);
    assert!(result(1)["content"].as_str().unwrap().contains("REQ-7"));
    assert_eq!(result(2)["is_error"], true);
    assert!(!result(2).to_string().contains(PLANTED));
    let db = rusqlite::Connection::open(&audit).unwrap();
    let refused: String = db
        .query_row(
            "SELECT payload FROM audit WHERE payload LIKE '%\"allow\":false%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(
        refused["input"]["target"],
        json!({"kind": "ticket", "id": "FAKE-9"})
    );
}

#[test]
fn connecting_to_an_unknown_run_fails() {
    let ws = tempfile::tempdir().unwrap();
    let (mcp_url, _) = gateway(ws.path());
    let wrong = mcp_url.replace(TOKEN, &"e".repeat(64));
    let e = Mcp::connect(&wrong).err().unwrap();
    assert!(format!("{e:#}").contains("404"), "{e:#}");
}
````

- [ ] **Step 2: Run the test to see it fail**

Run: `cargo test -p kod-loop --test mcp`
Expected: FAIL to compile: unresolved import `kod_loop::mcp`.

- [ ] **Step 3: Implement the client and route tool calls through it**

Create `crates/kod-loop/src/mcp.rs`:

````rust
//! A client for the tool gateway's MCP endpoint: stateless Streamable HTTP
//! (protocol 2026-07-28), JSON responses, one POST per call.
use crate::tools::{self, ToolResult};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL: &str = "2026-07-28";
/// One gateway call; a publish of a large change is the slowest.
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Tools the gateway serves that the model is never offered. The run's
/// caller publishes after the run ends.
pub const HIDDEN: &[&str] = &["publish"];

pub struct Mcp {
    url: String,
    http: reqwest::blocking::Client,
    /// The offered tools, in the Messages API's tool format.
    definitions: Vec<Value>,
}

impl Mcp {
    /// Lists the gateway's tools. Fails if the endpoint can't be reached or
    /// a tool would shadow a local one.
    pub fn connect(url: &str) -> Result<Mcp> {
        let http = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("building the MCP client")?;
        let mut mcp = Mcp {
            url: url.to_owned(),
            http,
            definitions: vec![],
        };
        let listed = mcp.rpc("tools/list", None, json!({}))?;
        let local = tools::definitions();
        let local: Vec<&str> = local
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for t in listed["tools"].as_array().context("tools/list: no tools")? {
            let name = t["name"]
                .as_str()
                .context("tools/list: a tool has no name")?;
            if local.contains(&name) {
                bail!("the gateway's tool {name:?} would shadow a local tool");
            }
            if HIDDEN.contains(&name) {
                continue;
            }
            mcp.definitions.push(json!({
                "name": name,
                "description": t["description"],
                "input_schema": t["inputSchema"],
            }));
        }
        Ok(mcp)
    }

    pub fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    pub fn offers(&self, name: &str) -> bool {
        self.definitions.iter().any(|d| d["name"] == name)
    }

    /// Calls a tool. Transport and protocol errors come back as an error
    /// result the model can see; the gateway already refused or acted.
    pub fn call(&self, name: &str, input: &Value) -> ToolResult {
        let params = json!({"name": name, "arguments": input});
        let (text, is_error) = match self.rpc("tools/call", Some(name), params) {
            Ok(r) => {
                let text = r["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                (text, r["isError"] == true)
            }
            Err(e) => (format!("the tool gateway failed: {e:#}"), true),
        };
        ToolResult {
            text,
            is_error,
            finish: None,
        }
    }

    fn rpc(&self, method: &str, name: Option<&str>, mut params: Value) -> Result<Value> {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL,
            "io.modelcontextprotocol/clientInfo": {"name": "kod-loop", "version": env!("CARGO_PKG_VERSION")},
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut req = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL)
            .header("mcp-method", method)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(n) = name {
            req = req.header("mcp-name", n);
        }
        let resp = req.send().context("reaching the tool gateway")?;
        let status = resp.status();
        let text = resp.text().context("reading the tool gateway's reply")?;
        if !status.is_success() {
            bail!("{method}: HTTP {status}: {}", crate::api::head(&text, 200));
        }
        let mut r: Value = serde_json::from_str(&text)
            .with_context(|| format!("{method}: the reply is not JSON"))?;
        if !r["error"].is_null() {
            bail!("{method}: {}", r["error"]);
        }
        match r.get_mut("result").map(Value::take) {
            Some(result) if result.is_object() => Ok(result),
            _ => bail!("{method}: the reply has no result"),
        }
    }
}
````

Apply to `crates/kod-loop/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-loop/src/lib.rs b/crates/kod-loop/src/lib.rs
index b277831..3c9fd89 100644
--- a/crates/kod-loop/src/lib.rs
+++ b/crates/kod-loop/src/lib.rs
@@ -4,6 +4,7 @@
 //! JSONL on stderr, and stdout is one JSON outcome line.
 pub mod api;
 pub mod context;
+pub mod mcp;
 pub mod tools;
 
 use api::{ApiError, Client};
@@ -93,6 +94,16 @@ pub const CUT_OFF: &str =
 pub const CUT_OFF_CALL: &str = "Not run: your reply was cut off at the output token limit, so this call's input may be incomplete. Resend it in smaller steps.";
 
 pub fn run(cfg: &Config, prompt: &str, trace: &mut dyn Write) -> Outcome {
+    run_with(cfg, None, prompt, trace)
+}
+
+/// `run`, with the tool gateway's tools offered beside the local ones.
+pub fn run_with(
+    cfg: &Config,
+    mcp: Option<&mcp::Mcp>,
+    prompt: &str,
+    trace: &mut dyn Write,
+) -> Outcome {
     let client = Client::new(&cfg.base_url);
     // Retries of the current request so far; each one is also a turn.
     let mut retries = 0;
@@ -114,7 +125,7 @@ pub fn run(cfg: &Config, prompt: &str, trace: &mut dyn Write) -> Outcome {
         turns += 1;
         context::elide(&mut messages, cfg.system.len(), cfg.context_bytes);
         let started = Instant::now();
-        let reply = match client.messages(&request(cfg, &messages)) {
+        let reply = match client.messages(&request(cfg, mcp, &messages)) {
             Ok(r) => {
                 retries = 0;
                 r
@@ -206,7 +217,10 @@ pub fn run(cfg: &Config, prompt: &str, trace: &mut dyn Write) -> Outcome {
         for c in &calls {
             let name = c["name"].as_str().unwrap_or("");
             let started = Instant::now();
-            let r = tools::call(&cfg.workspace, cfg.command_timeout, name, &c["input"]);
+            let r = match mcp {
+                Some(m) if m.offers(name) => m.call(name, &c["input"]),
+                _ => tools::call(&cfg.workspace, cfg.command_timeout, name, &c["input"]),
+            };
             let mut e = json!({
                 "event": "tool",
                 "turn": turns,
@@ -246,7 +260,7 @@ pub fn run(cfg: &Config, prompt: &str, trace: &mut dyn Write) -> Outcome {
 
 /// Caches the tools and system prompt, and the conversation up to its last
 /// block. The conversation itself is never changed here.
-fn request(cfg: &Config, messages: &[Value]) -> Value {
+fn request(cfg: &Config, mcp: Option<&mcp::Mcp>, messages: &[Value]) -> Value {
     let mut msgs = messages.to_vec();
     if let Some(last) = msgs.last_mut() {
         if let Some(s) = last["content"].as_str() {
@@ -260,11 +274,19 @@ fn request(cfg: &Config, messages: &[Value]) -> Value {
         "model": cfg.model,
         "max_tokens": cfg.max_tokens,
         "system": [{"type": "text", "text": cfg.system, "cache_control": {"type": "ephemeral"}}],
-        "tools": tools::definitions(),
+        "tools": all_tools(mcp),
         "messages": msgs,
     })
 }
 
+fn all_tools(mcp: Option<&mcp::Mcp>) -> Value {
+    let mut all = tools::definitions();
+    if let (Some(m), Some(list)) = (mcp, all.as_array_mut()) {
+        list.extend(m.definitions().iter().cloned());
+    }
+    all
+}
+
 fn text_of(content: &[Value]) -> String {
     content
         .iter()
````

Apply to `crates/kod-loop/src/main.rs` with `git apply`:

````diff
diff --git a/crates/kod-loop/src/main.rs b/crates/kod-loop/src/main.rs
index b5fd2f2..a1fa4ff 100644
--- a/crates/kod-loop/src/main.rs
+++ b/crates/kod-loop/src/main.rs
@@ -1,6 +1,6 @@
 use anyhow::{Context, Result, bail};
 use clap::Parser;
-use kod_loop::{Config, Outcome, context};
+use kod_loop::{Config, Outcome, context, mcp::Mcp};
 use std::{io::Read, path::PathBuf, process::ExitCode, time::Duration};
 
 /// Kod's agent loop. Reads the prompt on stdin, works in the current
@@ -30,7 +30,9 @@ struct Cli {
 fn main() -> ExitCode {
     let cli = Cli::parse();
     let outcome = match setup(&cli) {
-        Ok((cfg, prompt)) => kod_loop::run(&cfg, &prompt, &mut std::io::stderr()),
+        Ok((cfg, prompt, mcp)) => {
+            kod_loop::run_with(&cfg, mcp.as_ref(), &prompt, &mut std::io::stderr())
+        }
         Err(e) => Outcome::failed(format!("{e:#}")),
     };
     println!(
@@ -44,7 +46,7 @@ fn main() -> ExitCode {
     }
 }
 
-fn setup(cli: &Cli) -> Result<(Config, String)> {
+fn setup(cli: &Cli) -> Result<(Config, String, Option<Mcp>)> {
     let base = std::fs::read_to_string(&cli.system_prompt)
         .with_context(|| format!("reading {}", cli.system_prompt.display()))?;
     let base_url = std::env::var("ANTHROPIC_BASE_URL")
@@ -68,5 +70,11 @@ fn setup(cli: &Cli) -> Result<(Config, String)> {
         command_timeout: Duration::from_secs(cli.command_timeout_secs),
         retry_backoff: [2, 10, 30].map(Duration::from_secs).to_vec(),
     };
-    Ok((cfg, prompt))
+    // Optional: without it the run has only its local tools. Set and
+    // unreachable is a failed run, not a run without the gateway.
+    let mcp = match std::env::var("KOD_MCP_URL") {
+        Ok(url) => Some(Mcp::connect(&url).context("KOD_MCP_URL: the tool gateway")?),
+        Err(_) => None,
+    };
+    Ok((cfg, prompt, mcp))
 }
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-loop mcp 2 and its existing tests unchanged; kod-tool-gateway 21), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-loop
git commit -m "kod-loop: MCP client for the tool gateway"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc's tool gateway, token broker, trust boundary and action matrix sections and this plan, with these questions:
- Is there any path from `Gateway::call` to an adapter that skips `Policy::decide`, `record_decision` or `AuditLog::before`?
- Can a `Secret` reach a reply, an error string, an audit payload or a `Debug` print?
- Is publish idempotent under every partial failure the fake can simulate, and is a cancel at any point honoured before the branch moves?
- Do the policy inputs match the policy plan's `read`, `tracker_write` and `publish` contracts field for field?
- Does `kod-loop` without `KOD_MCP_URL` behave exactly as before?

## Self-review

- **Spec coverage.** MCP endpoint with tracker and docs tools against the 6a fake: Tasks 1 and 3. Token broker: Task 2 (`Broker`, `Secret`, minted inside publish only). Publish (agent branch, draft PR, idempotent, refused when canceled, checked against `changed_paths`): Task 2. MCP client in kod-loop: Task 4. Policy and audit before acting, failing closed: Tasks 1 and 2. Overview exit criteria: a tool call through the endpoint (`a_tool_call_goes_through_the_endpoint_and_the_policy`, `the_agents_tracker_calls_go_through_the_gateway`); a planted secret refused and audited (`a_planted_secret_cannot_be_read_and_the_attempt_is_audited`); the PR carries the story ID and agent trailers (`publish_pushes_the_agent_branch_and_opens_a_draft_pr`); a test per publish refusal in the matrix row: canceled, audit write fails, data class not cleared, a path that raises the risk class (Task 2).
- **Placeholders.** None.
- **Type consistency.** `Gateway`, `Run`, `Adapters`, `Reply`, `Secret`, `Broker`, `Forge`, `Change`, `Identity`, `Commit`, `FakeGitHub`, `Mcp` are spelled the same in every task; the code was compiled and tested in task order on top of sub-plans 3, 4, 6a and 2, and the plan text was replayed from scratch.
- **Review Focus.** Each of the five lines names its test and task above.

## Open items

- **The real broker** (owner, 6b): a GitHub App installation token scoped to the one repo (recommended), or a fine-grained token. The `Broker` trait is the same either way.
- **Which process serves the router** (owner, 6b): the LLM gateway's process as the overview says (D1), with one token per run for both routes.
- **Binary files in publish**: UTF-8 only now (D7). Say if agents must publish binary files; it needs base64 content in the change set.
