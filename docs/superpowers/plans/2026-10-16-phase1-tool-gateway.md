# Phase 1.5 Tool Gateway and Publish (`kod-tool-gateway`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development; *Native* tasks use superpowers:executing-plans. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `kod-tool-gateway` crate that serves a run's tracker and docs tools over MCP (rmcp), checks every call with kod-policy and writes it to the audit log before it acts, and publishes a finished run for the host: it takes the run's change from the trusted collector output outside the sandbox, gets a git credential from a token broker and revokes it after, and pushes `agent/<ticket>-<run>` and opens a draft PR idempotently. Plus an MCP client in `kod-loop`.

**Architecture:** The gateway is a library. `Gateway` holds the policy, the audit log, the adapters (tracker, docs, the dispatcher's `stopped` check, broker, forge, workspace) and the live runs, keyed by the SHA-256 of a per-run token as in the LLM gateway. There are two entry points, one per side of the trust boundary:
- **`Gateway::call(token, tool, args)`** is the sandbox's. It offers only the tracker and docs tools. It checks the dispatcher's durable run state first, builds the policy input from the run's start-time snapshot, records the decision, writes a `tool_call` record with the exact target through `AuditLog::before`, and only then calls the adapter. Every error is scrubbed of tokens and credentials.
- **`Gateway::publish(run_id)`** is the host's. It takes no content from the sandbox: the change and the test evidence come from the `Workspace` adapter, which reads the collector output the host wrote outside the sandbox, and `changed_paths` is computed from that change.

`router(gateway, hosts)` mounts rmcp's Streamable HTTP service at `/r/{token}/mcp` in stateless JSON mode, with `call` behind it and never `publish`. The dispatcher (6) owns run state, the cancel flag and when publish happens; 6b wires the processes. `kod-loop` gets a small blocking MCP client and adds the gateway's tools to its own when `KOD_MCP_URL` is set.

**Tech Stack:** Rust 1.99.0 (edition 2024). New: rmcp 3.5 (server, Streamable HTTP server). Already in `Cargo.lock`: axum 0.8, tokio, tower 0.5, serde_json, sha2 0.10, anyhow, rusqlite 0.40 (tests), reqwest 0.12 (tests and kod-loop). Crates from this phase: kod-policy, kod-audit (PR #14 / `feat/phase1-audit`), kod-tracker (the 6a plan), kod-llm-proxy (sub-plan 2, for the execution order only).

**Spec:** `docs/kod-harness-design.md` (the PR flow, Run identity and lifecycle, the tool gateway, the token broker, Trust boundaries, the action matrix rows for tracker reads and writes and for publish), row 5 of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/superpowers/plans/2026-10-15-phase1-policy.md` (the `read`, `tracker_write` and `publish` inputs), `docs/superpowers/plans/2026-10-15-phase1-audit.md`, `docs/superpowers/plans/2026-10-15-phase1-tracker-contract.md`, `docs/adr/0001-per-task-execution-mode.md`.

## Decisions

| | Decision | Why |
|---|---|---|
| D1 | A library crate with a router, no binary yet. 6b mounts `router` in the LLM gateway's process (the overview's "same process at first") when real adapters and a run source exist. | Today there are only fakes; a binary would serve fakes. The router takes the allowed hosts so the process that mounts it sets them. |
| D2 | One token per run in the path, `/r/{token}/mcp`, at least 32 lower-case hex characters, kept as its SHA-256. An unknown token gets 404 before rmcp sees the request. `end_run` and `end_all` drop the token, so it stops working the moment a run ends. | Same scheme as the LLM gateway (sub-plan 2), so 6b can give a sandbox one token for both routes. |
| D3 | rmcp 3.5, stateless Streamable HTTP (protocol 2026-07-28), JSON responses, no SSE keep-alive. | No session state to lose or forge; every call carries its run token. |
| D4 | The `kod-loop` client is a hand-written blocking JSON-RPC client over the reqwest it already has, tested against the rmcp server. Its errors drop the URL (`reqwest::Error::without_url`) and scrub `/r/<token>` from anything the server echoes. | `kod-loop` is synchronous and runs in the sandbox; the rmcp client would pull tokio into it for three request shapes. The run token is in the URL, and the trace is kept. |
| D5 | Sandbox tools: `get_issue`, `read_doc`, `comment`, `transition`, `create_linked`. Policy inputs are the policy plan's `read` and `tracker_write`. `linked_docs` is the ticket's `approved_versions`. `linked_tickets` is a snapshot taken by the dispatcher at `start_run` (the rework requests Kod linked to the run), plus the keys the run creates with `create_linked`. Never the ticket's live links. | The policy grants writes to "own or linked" tickets. Live links would let anyone who links a ticket, or the agent through the tracker, widen the run's reach mid-run. A snapshot is fixed at admission. Reads of the snapshot need a new `read` grant in `policy/agent.rego` (Task 1). |
| D6 | Every call, reads included, goes: the `stopped` check, then `Policy::decide`, then `AuditLog::record_decision`, then `AuditLog::before(run_audit, {"kind": "tool_call", run, tool, target})` around the adapter call, then a best-effort `tool_result` record. A failed decision write or `before` write refuses the call. Malformed arguments and unknown tools or tokens are refused before any record. | The trust boundary: a record with the exact target exists before the external call, and an unrecordable call does not happen (policy plan D9). |
| D7 | The dispatcher's database holds the one durable cancel flag: a run is stopped unless its row is live. The gateway gets it as `Adapters::stopped: Fn(run_id) -> Result<bool>` and checks it on every call and inside publish before the push, before the branch moves, and before the PR opens. A read error counts as stopped. A cancel that lands after the branch moved deletes the branch. | One source of truth that survives a crash, set by the dispatcher before any external call. An in-memory flag in the gateway would be lost on restart and could disagree with the dispatcher. |
| D8 | `publish(run_id)` is host-only. It is not in `call`, not in `tools()`, and not reachable through the MCP endpoint, and a sandbox call to it is refused. Its input is the run ID only. The change comes from `Workspace::collected(run)`: in 6b, kod-eval's collector writes `collect_out/diff.patch` and the check results next to the sandbox, never inside it, and the 6b runner then calls the dispatcher's `finished`, which calls `publish`. The commit message, PR title and PR body come from the ticket and the run. | The sandbox must not choose what is pushed or which paths the policy sees. Hiding a tool from the model is not a boundary. |
| D9 | The change is UTF-8 file contents (`None` deletes), at most 1000 files and 8 MiB, no path twice. The gateway commits it through the forge's git data API onto `run.base`; the sandbox never pushes. | The sandbox has no git credential to push with. Binary files are a known ceiling. |
| D10 | `trait Broker { mint(run, repo), revoke(run), revoke_all() }`. `mint` is called inside the publish operation only. `revoke(run)` is called when publish returns, whatever the outcome, and on `end_run`; `revoke_all` on `end_all` (the kill switch). `Secret` has a redacted `Debug` and no `Serialize`. Tracker and docs adapters hold their own credentials outside the sandbox. | The design's "git token exists only inside the publish op", made true even when a drop is skipped or a token leaks. The real broker (a GitHub App installation token scoped to one repo) is 6b's. |
| D11 | Idempotent publish. The commit pins its author and committer date to `run.started_at`, so it is a pure function of the change, the base, the message, the committer and that date, and a retry makes the same commit. `set_branch` moves `agent/<ticket>-<run>` to it. `find_pr` runs before `open_draft_pr`. `link_pr` runs on every publish. `forge::check` is the contract every forge adapter must pass: a retry after a crash after the push gives the same SHA, and `find_pr` finds a PR whose response was lost. | A publish that died anywhere completes on retry with one branch and one PR. The real adapter must meet this, not just the fake. |
| D12 | Errors that leave the gateway (replies, audit records) go through `scrub`: kod-audit's `redact`, plus any `/r/<hex>` run token, plus the exact minted secret. | A forge or broker error can carry a token or a URL with one. |
| D13 | The admitted data classes are fixed for both the tool and LLM gateways. `read_doc` refuses a document outside them before returning its text. Reading it requires a newly classified run; publish checks that same admitted set. | Updating only the publisher's classes would still let restricted text reach a model token admitted for the old classes. |
| D14 | The agent may move only its own ticket, and only `in_progress` to `blocked` or back (`AGENT_TRANSITIONS`). Review and done are the dispatcher's. | The policy allows writes on own or linked tickets. Moving a ticket to done or review is a human-visible claim the agent must not make. |
| D15 | The commit message ends with `Kod-Ticket: <tracker>:<key>`, `Kod-Run: <id>` and `Co-Authored-By: <agent>`; the committer is the agent identity, validated at `Gateway::new`. The PR title is `<key>: <title>` and its body carries the ticket, requirement, run ID, pinned inputs, changed files and test evidence. | The design's PR package. |

Out of scope here: the real GitHub, tracker, docs and workspace adapters and the real broker (6b, 6c); serving the router from a process (6b); run state and the kill switch (the dispatcher, 6).

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Tracker and docs tools | Subagent + review | Owns policy-then-audit gating, the run snapshot, the transition allowlist and the planted-secret conformance test |
| 2. Publish and the token broker | Subagent + review | Owns host-only publish, the cancel checks, credential revocation and scrubbing, and the forge contract |
| 3. The MCP endpoint | Subagent + review | Owns the run-token check in front of rmcp and keeping publish off it |
| 4. The MCP client in kod-loop | Subagent + review | Owns the conformance test that a tool call goes through the endpoint, and keeping the token out of errors |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged. The one new dependency is rmcp 3.5 (MIT/Apache-2.0); every crate it adds is under a licence `deny.toml` allows.
- Fail closed: an unknown token, tool or argument is refused; a decision or audit write that fails refuses the call; a run state that can't be read refuses the call; the gateway never acts without a policy allow.
- No credential or run token in a tool result, an audit record, an error, a log line or the sandbox.
- No network: fakes only (the 6a fake tracker, `FakeDocs`, `FakeGitHub`, `FakeWorkspace`).
- Code against kod-audit's plan API (`open`, `record_decision`, `before`, `append`, `RUN_AUDIT`, `redact::redact`) and kod-tracker's plan API.
- Commit messages are plain, with no trailers.

## Review Focus

- The sandbox must not be able to publish, with a valid token or through the endpoint: `the_sandbox_cannot_publish` (Task 2), `a_publish_call_from_the_sandbox_is_refused` (Task 3).
- A ticket a human linked must not be readable or writable; a rework request Kod created must be: `reads_the_runs_ticket_its_rework_requests_and_linked_docs_only`, `writes_reach_only_the_runs_ticket_and_its_rework_requests`, `create_linked_opens_a_request_the_run_may_then_read_and_comment_on` (Task 1).
- A cancel at any point must stop publish, and a cancel after the push must leave no branch and no PR: `a_cancel_before_the_push_stops_publish`, `a_cancel_after_the_push_deletes_the_branch_and_opens_no_pr`, `a_canceled_run_is_not_published` (Task 2); `a_run_canceled_in_the_dispatchers_database_gets_nothing`, `a_run_whose_state_cannot_be_read_gets_nothing` (Task 1).
- A failure between the decision and the intent record must refuse publish with nothing minted or pushed: `publish_is_refused_when_the_intent_cannot_be_recorded` (Task 2).
- A real-looking token in a forge or broker error must not reach the reply or the audit log, and every minted credential must be revoked: `credentials_never_reach_the_reply_or_the_audit_log`, `every_publish_credential_is_revoked_when_it_returns`, `ending_runs_revokes_their_credentials` (Task 2); `an_unreachable_gateway_does_not_leak_the_run_token`, `an_error_page_that_echoes_the_path_does_not_leak_the_run_token` (Task 4).
- Idempotency must be a property of the forge contract: `the_fake_forge_meets_the_contract`, `the_contract_catches_a_forge_that_dates_commits_by_the_clock`, `a_retry_after_a_lost_pr_response_finds_the_pr` (Task 2).
- A document outside the model token's admitted classes must not be returned: `a_doc_outside_the_runs_admitted_classes_is_not_returned` (Task 1). A run admitted for restricted documents must not publish to an uncleared repo: `a_run_admitted_for_restricted_docs_cannot_publish_to_an_uncleared_repo` (Task 2).
- Canceling and revoking the push token together must still delete the branch: `cancel_revocation_does_not_prevent_branch_cleanup` (Task 2). Cleanup mints a fresh credential inside publish and the final revoke covers it too.
- The agent must not move its ticket to review or done, or move another ticket: `the_agent_moves_only_its_own_ticket_between_in_progress_and_blocked` (Task 1).

## File Structure

```
Cargo.toml                                   modify: add crates/kod-tool-gateway to members
policy/agent.rego                            modify: read grant for the run's rework requests (Task 1)
crates/kod-policy/tests/agent_ops.rs         modify: its test (Task 1)
crates/kod-tool-gateway/
  Cargo.toml                                 create (Task 1), modify (Task 3)
  src/lib.rs                                 create: Run, Adapters, Gateway, scrub, the tools (Task 1); publish, published (Task 2); mcp (Task 3)
  src/docs.rs                                create: Doc, the Docs trait and FakeDocs
  src/broker.rs                              create: Secret and the Broker trait (Task 2)
  src/forge.rs                               create: Change, Collected, Workspace, Identity, Commit, the Forge trait, the forge contract, FakeGitHub and FakeWorkspace (Task 2)
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
- Modify: `Cargo.toml`, `policy/agent.rego`
- Create: `crates/kod-tool-gateway/Cargo.toml`, `src/lib.rs`, `src/docs.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs`, `tests/tools.rs`; `crates/kod-policy/tests/agent_ops.rs` (modify)

**Interfaces:**
- Consumes: `kod_policy::Policy::{load, decide}`; `kod_audit::{AuditLog::{open, record_decision, before, append}, RUN_AUDIT, redact::redact}`; `kod_tracker::{Tracker, Issue, State, Rework, ReworkKind, fake::{sandbox, FakeTracker, FakeIssue}}`.
- Produces: `Run { id, ticket, repo, base, data_classes, linked_tickets, started_at }`; `type Stopped = Arc<dyn Fn(&str) -> anyhow::Result<bool> + Send + Sync>`; `Adapters { tracker, docs, stopped }`; `Gateway::new(Arc<Policy>, Arc<AuditLog>, Adapters) -> anyhow::Result<Gateway>`; `Gateway::{start_run(&str, Run) -> anyhow::Result<()>, end_run(&str) -> anyhow::Result<()>, end_all() -> anyhow::Result<()>, run(&str) -> Option<Run>, call(&str, &str, &Value) -> Reply}` with `type Reply = Result<Value, String>`; `scrub(&str) -> String`; `MIN_TOKEN_LEN`; `AGENT_TRANSITIONS`; `docs::{Doc { text, class }, Docs, FakeDocs::{add, add_classed}}`; a `read` grant in `policy/agent.rego` for `run.linked_tickets`.

- [ ] **Step 1: Write the failing tests**

Apply to `Cargo.toml` with `git apply`:

````diff
diff --git a/Cargo.toml b/Cargo.toml
index 086e6c7..3d38611 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,3 +1,11 @@
 [workspace]
-members = ["crates/kod-audit", "crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy", "crates/kod-tracker"]
+members = [
+    "crates/kod-audit",
+    "crates/kod-eval",
+    "crates/kod-llm-proxy",
+    "crates/kod-loop",
+    "crates/kod-policy",
+    "crates/kod-tool-gateway",
+    "crates/kod-tracker",
+]
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

Apply to `crates/kod-policy/tests/agent_ops.rs` with `git apply`:

````diff
diff --git a/crates/kod-policy/tests/agent_ops.rs b/crates/kod-policy/tests/agent_ops.rs
index 26276c0..4ae9b87 100644
--- a/crates/kod-policy/tests/agent_ops.rs
+++ b/crates/kod-policy/tests/agent_ops.rs
@@ -190,6 +190,20 @@ fn reads_are_limited_to_the_ticket_and_its_linked_docs() {
     );
 }
 
+#[test]
+fn reads_reach_the_rework_requests_kod_linked_to_the_run() {
+    allowed(req(
+        "read",
+        json!({"target": {"kind": "ticket", "id": "KOD-2"}}),
+    ));
+    let none = set(
+        req("read", json!({"target": {"kind": "ticket", "id": "KOD-2"}})),
+        "/run/linked_tickets",
+        json!([]),
+    );
+    denied(none, "does not meet the rule");
+}
+
 #[test]
 fn null_ids_never_match() {
     let read = set(
````

Create `crates/kod-tool-gateway/tests/common/mod.rs`:

````rust
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
````

Create `crates/kod-tool-gateway/tests/tools.rs`:

````rust
mod common;

use common::*;
use kod_tool_gateway::Run;
use serde_json::json;

#[test]
fn reads_the_runs_ticket_its_rework_requests_and_linked_docs_only() {
    let h = harness();
    let own =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-1"}))
            .unwrap();
    assert_eq!(own["requirement_id"], "REQ-7");
    let rework =
        h.gw.call(TOKEN, "get_issue", &json!({"key": REWORK}))
            .unwrap();
    assert_eq!(rework["body"], "a question Kod asked");
    let doc =
        h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}))
            .unwrap();
    assert_eq!(doc["text"], "the approved brief");
    // A human's link is on the live ticket but not in the run's snapshot.
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": HUMAN_LINK}))
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
    assert!(!h.raw_log().contains(PLANTED));
}

#[test]
fn writes_reach_only_the_runs_ticket_and_its_rework_requests() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    assert_eq!(h.tracker.comments("FAKE-1"), ["on it"]);
    h.gw.call(
        TOKEN,
        "comment",
        &json!({"key": REWORK, "text": "still open"}),
    )
    .unwrap();
    assert_eq!(h.tracker.comments(REWORK), ["still open"]);
    for key in [HUMAN_LINK, "FAKE-9"] {
        let e =
            h.gw.call(TOKEN, "comment", &json!({"key": key, "text": "hi"}))
                .unwrap_err();
        assert!(e.starts_with("refused by policy"), "{e}");
        assert!(h.tracker.comments(key).is_empty());
    }
}

#[test]
fn the_agent_moves_only_its_own_ticket_between_in_progress_and_blocked() {
    let h = harness();
    let to = |key: &str, state: &str| {
        h.gw.call(TOKEN, "transition", &json!({"key": key, "state": state}))
    };
    to("FAKE-1", "blocked").unwrap();
    assert_eq!(h.tracker.issue("FAKE-1").unwrap().workflow_state, "Waiting");
    for state in ["done", "in_review", "ready"] {
        let e = to("FAKE-1", state).unwrap_err();
        assert_eq!(
            e,
            format!("the agent may not move its ticket from blocked to {state}")
        );
    }
    to("FAKE-1", "in_progress").unwrap();
    assert_eq!(h.tracker.issue("FAKE-1").unwrap().workflow_state, "Doing");
    let e = to(REWORK, "blocked").unwrap_err();
    assert_eq!(e, "the agent may move only its own ticket");
    assert_eq!(
        to("FAKE-1", "shipped").unwrap_err(),
        "unknown state \"shipped\""
    );
    assert_eq!(h.tracker.issue(REWORK).unwrap().workflow_state, "Backlog");
}

#[test]
fn every_call_is_audited_with_its_target_before_it_happens() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-1"}))
        .unwrap();
    let kinds: Vec<_> = h.records().iter().map(|r| r["kind"].clone()).collect();
    let call = ["policy_decision", "tool_call", "tool_result"];
    assert_eq!(kinds, [call, call].concat());
    assert_eq!(h.records()[1]["target"], json!({"ticket": "FAKE-1"}));
    assert_eq!(
        h.records()[4]["target"],
        json!({"kind": "ticket", "id": "FAKE-1"})
    );
}

#[test]
fn create_linked_opens_a_request_the_run_may_then_read_and_comment_on() {
    let h = harness();
    let args = json!({"kind": "question", "stage": "build", "artifact": "brief",
        "version": "3", "text": "Which approver?", "evidence": ["src/refund.rs:12"]});
    let r = h.gw.call(TOKEN, "create_linked", &args).unwrap();
    let key = r["key"].as_str().unwrap();
    let read = h.gw.call(TOKEN, "get_issue", &json!({"key": key})).unwrap();
    assert!(read["body"].as_str().unwrap().contains("Which approver?"));
    h.gw.call(TOKEN, "comment", &json!({"key": key, "text": "more"}))
        .unwrap();
    assert_eq!(h.tracker.comments(key), ["more"]);
    let bad = json!({"kind": "rumour", "stage": "", "artifact": "", "version": "",
        "text": "", "evidence": []});
    let e = h.gw.call(TOKEN, "create_linked", &bad).unwrap_err();
    assert_eq!(e, "unknown kind \"rumour\"");
}

#[test]
fn a_run_canceled_in_the_dispatchers_database_gets_nothing() {
    let h = harness();
    h.stop("run-1");
    for (tool, args) in [
        ("get_issue", json!({"key": "FAKE-1"})),
        ("comment", json!({"key": "FAKE-1", "text": "x"})),
    ] {
        let e = h.gw.call(TOKEN, tool, &args).unwrap_err();
        assert_eq!(e, "refused: the run is canceled");
    }
    assert!(h.tracker.comments("FAKE-1").is_empty());
    assert!(h.records().is_empty());
}

#[test]
fn a_run_whose_state_cannot_be_read_gets_nothing() {
    let h = harness();
    h.runs
        .broken
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let e =
        h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "x"}))
            .unwrap_err();
    assert!(
        e.starts_with("refused: the run's state could not be read"),
        "{e}"
    );
    assert!(h.tracker.comments("FAKE-1").is_empty());
}

#[test]
fn an_ended_runs_token_stops_working() {
    let h = harness();
    let other = "2".repeat(64);
    h.gw.start_run(
        &other,
        Run {
            id: "run-2".into(),
            ..run()
        },
    )
    .unwrap();
    h.gw.end_run("run-1").unwrap();
    let args = json!({"key": "FAKE-1"});
    assert_eq!(
        h.gw.call(TOKEN, "get_issue", &args).unwrap_err(),
        "unknown run"
    );
    h.gw.call(&other, "get_issue", &args).unwrap();
    h.gw.end_all().unwrap();
    assert_eq!(
        h.gw.call(&other, "get_issue", &args).unwrap_err(),
        "unknown run"
    );
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
fn errors_are_scrubbed_of_run_tokens_and_credentials() {
    let leak = format!(
        "error sending request for url (http://gw/r/{TOKEN}/mcp): token ghs_0123456789abcdef0123 refused"
    );
    let clean = kod_tool_gateway::scrub(&leak);
    assert!(!clean.contains(TOKEN), "{clean}");
    assert!(!clean.contains("ghs_0123456789abcdef0123"), "{clean}");
    assert!(clean.contains("http://gw/r/[REDACTED]/mcp"), "{clean}");
    assert_eq!(kod_tool_gateway::scrub("/r/abc/mcp"), "/r/abc/mcp");
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
    let mut links = r.clone();
    links.linked_tickets = vec!["".into()];
    assert!(h.gw.start_run(&"1".repeat(64), links).is_err());
    let again = h.gw.start_run(&"1".repeat(64), run()).unwrap_err();
    assert_eq!(again.to_string(), "run run-1 is already started");
    h.gw.start_run(&"1".repeat(64), r).unwrap();
}

#[test]
fn a_doc_outside_the_runs_admitted_classes_is_not_returned() {
    let h = harness();
    h.docs
        .add_classed(LINKED_DOC, "restricted contents", "customer_pii");
    let result = h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}));
    assert!(result.is_err(), "{result:?}");
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-policy --test agent_ops; cargo test -p kod-tool-gateway`
Expected: FAIL: `reads_reach_the_rework_requests_kod_linked_to_the_run` panics (the policy has no read grant for `run.linked_tickets`), and kod-tool-gateway fails to compile: unresolved import `kod_tool_gateway`.

- [ ] **Step 3: Implement the read grant, the gateway and the tools**

Apply to `policy/agent.rego` with `git apply`:

````diff
diff --git a/policy/agent.rego b/policy/agent.rego
index 583c1b5..3d72588 100644
--- a/policy/agent.rego
+++ b/policy/agent.rego
@@ -145,7 +145,8 @@ deny contains sprintf("data class %v is not allowed in subscription mode", [c])
 	not sub_class_ok(c)
 }
 
-# read: the run's own ticket and the docs it links.
+# read: the run's own ticket, the rework requests Kod linked to it (the
+# run's linked_tickets), and the docs the ticket links.
 
 grant if {
 	input.action == "read"
@@ -155,6 +156,15 @@ grant if {
 	input.target.id == input.run.ticket
 }
 
+grant if {
+	input.action == "read"
+	is_agent
+	input.target.kind == "ticket"
+	named(input.target.id)
+	is_array(input.run.linked_tickets)
+	input.target.id in input.run.linked_tickets
+}
+
 grant if {
 	input.action == "read"
 	is_agent
````

Create `crates/kod-tool-gateway/src/docs.rs`:

````rust
//! The docs adapter: read one approved document by its pinned ID.
use anyhow::{Context, Result};
use std::{collections::HashMap, sync::Mutex};

/// A document and its data class (design: "Data classes").
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub text: String,
    pub class: String,
}

pub trait Docs: Send + Sync {
    fn read(&self, id: &str) -> Result<Doc>;
}

/// In-memory docs for tests.
#[derive(Default)]
pub struct FakeDocs(Mutex<HashMap<String, Doc>>);

impl FakeDocs {
    /// A `placeholder` doc.
    pub fn add(&self, id: &str, text: &str) {
        self.add_classed(id, text, "placeholder");
    }

    pub fn add_classed(&self, id: &str, text: &str, class: &str) {
        let mut docs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let doc = Doc {
            text: text.to_owned(),
            class: class.to_owned(),
        };
        docs.insert(id.to_owned(), doc);
    }
}

impl Docs for FakeDocs {
    fn read(&self, id: &str) -> Result<Doc> {
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
//! and docs tools. Every call is checked by kod-policy and recorded in the
//! audit log before it acts; a call that can't be decided or recorded
//! doesn't happen. A canceled run gets nothing.
use anyhow::bail;
use docs::Docs;
use kod_audit::{AuditLog, RUN_AUDIT, redact::REDACTED};
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
/// The moves the agent may make on its own ticket. Review and done are the
/// dispatcher's, on publish.
pub const AGENT_TRANSITIONS: [(State, State); 2] = [
    (State::InProgress, State::Blocked),
    (State::Blocked, State::InProgress),
];

/// A run the gateway serves. `base` is the commit its workspace started from.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: String,
    pub ticket: String,
    pub repo: String,
    pub base: String,
    /// The fixed classes admitted for both model routing and document reads.
    pub data_classes: Vec<String>,
    /// The rework requests Kod itself linked to the ticket before this run,
    /// from the dispatcher's records: a snapshot taken at start, never the
    /// ticket's live links, which any human can change.
    pub linked_tickets: Vec<String>,
    /// Unix seconds. The commit date, so a retried publish makes the same
    /// commit.
    pub started_at: i64,
}

#[derive(Clone)]
struct Live {
    run: Run,
    /// Rework requests this run created; it may read and comment on them.
    created: Vec<String>,
}

impl Live {
    fn linked(&self) -> Vec<String> {
        let mut all = self.run.linked_tickets.clone();
        all.extend(self.created.iter().cloned());
        all
    }
}

/// Whether a run is stopped (canceled, or ended) in the dispatcher's
/// database. The gateway asks before every call; an error counts as
/// stopped.
pub type Stopped = Arc<dyn Fn(&str) -> anyhow::Result<bool> + Send + Sync>;

/// The adapters behind the tools. Each holds its own credentials; none of
/// them is ever handed to the sandbox.
pub struct Adapters {
    pub tracker: Arc<dyn Tracker>,
    pub docs: Arc<dyn Docs>,
    pub stopped: Stopped,
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

/// Error text made safe for an MCP reply, an audit record or a log: the
/// audit log's redaction, then run tokens in `/r/{token}` paths.
pub fn scrub(text: &str) -> String {
    let text = kod_audit::redact::redact(text);
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(i) = rest.find("/r/") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let n = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        if n >= MIN_TOKEN_LEN {
            out.push_str(REDACTED);
            rest = &rest[n..];
        }
    }
    out.push_str(rest);
    out
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
        if !run.linked_tickets.iter().all(|t| named(t)) {
            bail!("run {}: linked_tickets must be names", run.id);
        }
        let mut runs = self.lock();
        if runs.values().any(|l| l.run.id == run.id) || runs.contains_key(&key(token)) {
            bail!("run {} is already started", run.id);
        }
        runs.insert(
            key(token),
            Live {
                run,
                created: vec![],
            },
        );
        Ok(())
    }

    /// Stops serving a run: its token stops working. Safe to repeat. The
    /// dispatcher calls it when the run ends, however it ends.
    pub fn end_run(&self, run_id: &str) -> anyhow::Result<()> {
        self.lock().retain(|_, l| l.run.id != run_id);
        Ok(())
    }

    /// The kill switch: stop serving every run.
    pub fn end_all(&self) -> anyhow::Result<()> {
        self.lock().clear();
        Ok(())
    }

    /// The run, or None for an unknown token.
    pub fn run(&self, token: &str) -> Option<Run> {
        self.lock().get(&key(token)).map(|l| l.run.clone())
    }

    fn live(&self, token: &str) -> Option<Live> {
        self.lock().get(&key(token)).cloned()
    }

    fn update(&self, run_id: &str, f: impl FnOnce(&mut Live)) {
        if let Some(l) = self.lock().values_mut().find(|l| l.run.id == run_id) {
            f(l);
        }
    }

    /// Refuses unless the dispatcher's database says the run is live.
    fn not_stopped(&self, run_id: &str) -> Result<(), String> {
        match (self.adapters.stopped)(run_id) {
            Ok(false) => Ok(()),
            Ok(true) => Err("refused: the run is canceled".into()),
            Err(e) => Err(scrub(&format!(
                "refused: the run's state could not be read: {e:#}"
            ))),
        }
    }

    /// Runs one tool call for the run with this token. Blocking. Every
    /// error is scrubbed.
    pub fn call(&self, token: &str, tool: &str, args: &Value) -> Reply {
        let live = self.live(token).ok_or("unknown run")?;
        self.not_stopped(&live.run.id)?;
        match tool {
            "get_issue" => self.get_issue(&live, args),
            "read_doc" => self.read_doc(&live, args),
            "comment" => self.comment(&live, args),
            "transition" => self.transition(&live, args),
            "create_linked" => self.create_linked(&live, args),
            other => Err(format!("unknown tool {other:?}")),
        }
        .map_err(|e| scrub(&e))
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
            .map_err(|e| scrub(&format!("refused: the audit write failed: {e:#}")))?;
        let result = r
            .as_ref()
            .map(|_| "ok".to_owned())
            .unwrap_or_else(|e| scrub(&format!("{e:#}")));
        // The call already happened; a failed result record can't undo it.
        let _ = self.audit.append(
            RUN_AUDIT,
            &json!({"kind": "tool_result", "run": run.id, "tool": tool, "result": result}),
        );
        r.map_err(|e| scrub(&format!("{tool} failed: {e:#}")))
    }

    /// The run's own ticket, read for its linked docs, its state and the PR
    /// text.
    fn own_issue(&self, run: &Run) -> Result<Issue, String> {
        self.adapters
            .tracker
            .get_issue(&run.ticket)
            .map_err(|e| format!("reading the run's ticket failed: {e:#}"))
    }

    fn get_issue(&self, live: &Live, args: &Value) -> Reply {
        let (run, k) = (&live.run, text(args, "key")?);
        let target = json!({"kind": "ticket", "id": k});
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "linked_tickets": live.linked()},
            "target": target,
        }))?;
        let t = &self.adapters.tracker;
        let i = self.act(run, "get_issue", target, || t.get_issue(k))?;
        Ok(issue_json(&i))
    }

    fn read_doc(&self, live: &Live, args: &Value) -> Reply {
        let (run, id) = (&live.run, text(args, "id")?);
        let issue = self.own_issue(run)?;
        let target = json!({"kind": "doc", "id": id});
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "linked_docs": issue.approved_versions},
            "target": target,
        }))?;
        let docs = &self.adapters.docs;
        let doc = self.act(run, "read_doc", target, || {
            let doc = docs.read(id)?;
            if !run.data_classes.contains(&doc.class) {
                bail!("document is outside the run's admitted data classes; start a newly classified run");
            }
            Ok(doc)
        })?;
        Ok(json!({"id": id, "text": doc.text}))
    }

    fn write_input(&self, live: &Live, kind: &str, target: Value) -> Result<(), String> {
        let run = &live.run;
        self.allow(json!({
            "action": "tracker_write",
            "kind": kind,
            "actor": {"kind": "agent"},
            // `not_stopped` already checked the dispatcher's database.
            "run": {"id": run.id, "ticket": run.ticket, "canceled": false, "linked_tickets": live.linked()},
            "target": target,
        }))
    }

    fn comment(&self, live: &Live, args: &Value) -> Reply {
        let (k, body) = (text(args, "key")?, text(args, "text")?);
        let target = json!({"ticket": k});
        self.write_input(live, "comment", target.clone())?;
        let t = &self.adapters.tracker;
        self.act(&live.run, "comment", target, || t.comment(k, body))?;
        Ok(json!({"ok": true}))
    }

    fn transition(&self, live: &Live, args: &Value) -> Reply {
        let (run, k, name) = (&live.run, text(args, "key")?, text(args, "state")?);
        let to = State::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| format!("unknown state {name:?}"))?;
        if k != run.ticket {
            return Err("the agent may move only its own ticket".into());
        }
        let from = self.own_issue(run)?.state;
        if !AGENT_TRANSITIONS.contains(&(from, to)) {
            return Err(format!(
                "the agent may not move its ticket from {} to {name}",
                from.name()
            ));
        }
        let target = json!({"ticket": k, "state": name});
        self.write_input(live, "transition", json!({"ticket": k}))?;
        let t = &self.adapters.tracker;
        self.act(run, "transition", target, || t.transition(k, to))?;
        Ok(json!({"ok": true}))
    }

    fn create_linked(&self, live: &Live, args: &Value) -> Reply {
        let run = &live.run;
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
        self.write_input(live, "create_linked", target.clone())?;
        let t = &self.adapters.tracker;
        let key = self.act(run, "create_linked", target, || {
            t.create_linked(&run.ticket, kind, &fields)
        })?;
        self.update(&run.id, |l| l.created.push(key.clone()));
        Ok(json!({"key": key}))
    }
}
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-policy -p kod-tool-gateway && cargo clippy -p kod-policy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-tool-gateway tools 13; kod-policy agent_ops 17), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock policy/agent.rego crates/kod-policy crates/kod-tool-gateway
git commit -m "kod-tool-gateway: tracker and docs tools behind policy and the audit log"
```

### Task 2: Publish and the token broker

**Files:**
- Create: `crates/kod-tool-gateway/src/broker.rs`, `src/forge.rs`
- Modify: `crates/kod-tool-gateway/src/lib.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs` (modify), `tests/publish.rs`

**Interfaces:**
- Consumes: Task 1's `Gateway`, `Run`, `Adapters`, `Stopped`, `scrub`; the policy plan's `publish` input (`run.{id, ticket, repo, canceled, data_classes}`, `target.{repo, branch, draft}`, `changed_paths`).
- Produces: `broker::{Secret::{new, expose}, Broker::{mint(&str, &str) -> anyhow::Result<Secret>, revoke(&str), revoke_all()}}`; `forge::{Change { path, content: Option<String> }, Collected { changes, test_evidence }, Workspace::collected(&str), Identity { name, email }, Commit { repo, base, changes, message, committer, date }, Forge::{commit, set_branch, delete_branch, find_pr, open_draft_pr}, check(&dyn Forge, &dyn Broker, &str, &str), FakeGitHub, FakePr, FakeWorkspace}`; `Adapters` gains `broker`, `forge` and `workspace`; `Gateway::new` gains `agent: Identity`; `Gateway::publish(&str) -> Result<Published, String>` with `Published { branch, commit, pr }`; `Gateway::published(&str, &str, &str) -> Result<Option<String>, String>` (the dispatcher's crash recovery); `MAX_FILES`, `MAX_PUBLISH_BYTES`.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-tool-gateway/tests/common/mod.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/tests/common/mod.rs b/crates/kod-tool-gateway/tests/common/mod.rs
index 401b6f0..9d6433a 100644
--- a/crates/kod-tool-gateway/tests/common/mod.rs
+++ b/crates/kod-tool-gateway/tests/common/mod.rs
@@ -1,6 +1,7 @@
 #![allow(dead_code)]
 use kod_audit::AuditLog;
 use kod_policy::Policy;
+use kod_tool_gateway::forge::{Change, Collected, FakeGitHub, FakeWorkspace, Identity};
 use kod_tool_gateway::{Adapters, Gateway, Run, docs::FakeDocs};
 use kod_tracker::fake::{self, FakeIssue, FakeTracker};
 use std::{
@@ -35,6 +36,8 @@ pub struct Harness {
     pub gw: Arc<Gateway>,
     pub tracker: Arc<FakeTracker>,
     pub docs: Arc<FakeDocs>,
+    pub github: Arc<FakeGitHub>,
+    pub workspace: Arc<FakeWorkspace>,
     pub runs: Arc<Runs>,
     pub audit: PathBuf,
     _dir: tempfile::TempDir,
@@ -52,6 +55,24 @@ pub fn run() -> Run {
     }
 }
 
+/// What the collector found for a run that changed `path` and deleted
+/// old.rs.
+pub fn collected(path: &str) -> Collected {
+    Collected {
+        changes: vec![
+            Change {
+                path: path.into(),
+                content: Some("fn hold() {}\n".into()),
+            },
+            Change {
+                path: "old.rs".into(),
+                content: None,
+            },
+        ],
+        test_evidence: "cargo test: 12 passed".into(),
+    }
+}
+
 /// A gateway over the fakes with run-1 started under TOKEN on FAKE-1, which
 /// is in progress. FAKE-1 links HUMAN_LINK and REWORK; FAKE-9 and
 /// SECRET_DOC are not linked and hold PLANTED.
@@ -92,17 +113,30 @@ pub fn harness() -> Harness {
         }
         Ok(r.stopped.lock().unwrap().contains(run))
     });
+    let github = Arc::new(FakeGitHub::default());
+    github.add_repo("acme/app", BASE);
+    let workspace = Arc::new(FakeWorkspace::default());
+    workspace.set("run-1", collected("src/refund.rs"));
     let adapters = Adapters {
         tracker: tracker.clone(),
         docs: docs.clone(),
         stopped,
+        broker: github.clone(),
+        forge: github.clone(),
+        workspace: workspace.clone(),
+    };
+    let agent = Identity {
+        name: "kod-agent".into(),
+        email: "kod-agent@users.noreply.example".into(),
     };
-    let gw = Arc::new(Gateway::new(policy, log, adapters).unwrap());
+    let gw = Arc::new(Gateway::new(policy, log, adapters, agent).unwrap());
     gw.start_run(TOKEN, run()).unwrap();
     Harness {
         gw,
         tracker,
         docs,
+        github,
+        workspace,
         runs,
         audit,
         _dir: dir,
````

Create `crates/kod-tool-gateway/tests/publish.rs`:

````rust
mod common;

use anyhow::Result;
use common::*;
use kod_tool_gateway::{
    broker::{Broker, Secret},
    forge::{self, Change, Collected, Commit, FakeGitHub, Forge},
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

fn nothing_published(h: &Harness) {
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
    assert!(h.github.prs().is_empty());
}

#[test]
fn publish_pushes_the_collected_change_and_opens_a_draft_pr() {
    let h = harness();
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(p.branch, "agent/FAKE-1-run-1");
    assert_eq!(
        h.github.branch("acme/app", &p.branch),
        Some(p.commit.clone())
    );
    let c = h.github.commit_of(&p.commit).unwrap();
    assert_eq!(c.base, BASE);
    assert_eq!(c.changes, collected("src/refund.rs").changes);
    assert_eq!(c.committer.name, "kod-agent");
    assert_eq!(c.date, 1_700_000_000);
    let title = &h.tracker.issue("FAKE-1").unwrap().title;
    assert_eq!(
        c.message,
        format!(
            "{title}\n\nKod-Ticket: fake:FAKE-1\nKod-Run: run-1\nCo-Authored-By: kod-agent <kod-agent@users.noreply.example>\n"
        )
    );
    let prs = h.github.prs();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].draft);
    assert_eq!(prs[0].url, p.pr);
    assert_eq!(prs[0].title, format!("FAKE-1: {title}"));
    for line in [
        "Ticket: fake:FAKE-1",
        "Requirement: REQ-7",
        "Run: run-1",
        "- https://docs.example/brief?rev=3",
        "- src/refund.rs\n- old.rs",
        "Test evidence:\ncargo test: 12 passed",
    ] {
        assert!(prs[0].body.contains(line), "{line} not in {}", prs[0].body);
    }
    assert_eq!(h.tracker.prs("FAKE-1"), std::slice::from_ref(&p.pr));
}

#[test]
fn the_sandbox_cannot_publish() {
    let h = harness();
    let e =
        h.gw.call(
            TOKEN,
            "publish",
            &json!({"files": [{"path": "x", "content": "y"}]}),
        )
        .unwrap_err();
    assert_eq!(e, "unknown tool \"publish\"");
    nothing_published(&h);
    assert_eq!(h.github.minted().len(), 0);
}

#[test]
fn publish_needs_a_live_run_and_a_collected_change() {
    let h = harness();
    assert_eq!(h.gw.publish("run-9").unwrap_err(), "unknown run");
    h.workspace.set(
        "run-1",
        Collected {
            changes: vec![],
            test_evidence: String::new(),
        },
    );
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "the change must have 1 to 1000 files, it has 0"
    );
    let twice = Collected {
        changes: vec![collected("a.rs").changes[0].clone(); 2],
        test_evidence: String::new(),
    };
    h.workspace.set("run-1", twice);
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "a.rs is in the change twice"
    );
    let big = Collected {
        changes: vec![Change {
            path: "big.bin".into(),
            content: Some("x".repeat(8 * 1024 * 1024 + 1)),
        }],
        test_evidence: String::new(),
    };
    h.workspace.set("run-1", big);
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "the change is over 8388608 bytes"
    );
    nothing_published(&h);
}

#[test]
fn publish_is_refused_for_paths_the_policy_protects() {
    let h = harness();
    h.workspace.set("run-1", collected("policy/agent.rego"));
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    nothing_published(&h);
}

#[test]
fn a_run_admitted_for_restricted_docs_cannot_publish_to_an_uncleared_repo() {
    let h = harness();
    h.gw.end_run("run-1").unwrap();
    let mut r = run();
    r.data_classes.push("customer-pii".into());
    h.gw.start_run(TOKEN, r).unwrap();
    h.docs
        .add_classed(LINKED_DOC, "a payroll export", "customer-pii");
    h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}))
        .unwrap();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    let (_, input) = h.decisions().pop().unwrap();
    assert_eq!(
        input["run"]["data_classes"],
        json!(["placeholder", "customer-pii"])
    );
    nothing_published(&h);
}

#[test]
fn a_retry_after_a_lost_pr_response_finds_the_pr() {
    let h = harness();
    h.github.lose_next_pr();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("connection reset"), "{e}");
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(h.github.prs().len(), 1);
    assert_eq!(h.github.prs()[0].url, p.pr);
}

#[test]
fn every_publish_credential_is_revoked_when_it_returns() {
    let h = harness();
    h.gw.publish("run-1").unwrap();
    h.github.fail_next_pr("rate limited");
    h.gw.publish("run-1").unwrap();
    h.github.fail_next_pr("rate limited");
    let mut fresh = run();
    fresh.id = "run-2".into();
    h.workspace.set("run-2", collected("src/a.rs"));
    h.gw.start_run(&"2".repeat(64), fresh).unwrap();
    assert!(h.gw.publish("run-2").is_err());
    assert_eq!(h.github.minted().len(), 3);
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn ending_runs_revokes_their_credentials() {
    let h = harness();
    // A credential that outlived its publish, as if the revoke had failed.
    let token = h.github.mint("run-1", "acme/app").unwrap();
    h.gw.end_run("run-1").unwrap();
    assert!(h.github.live_tokens().is_empty());
    assert!(h.github.set_branch(&token, "acme/app", "x", BASE).is_err());
    h.github.mint("run-2", "acme/app").unwrap();
    h.gw.end_all().unwrap();
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn credentials_never_reach_the_reply_or_the_audit_log() {
    let h = harness();
    let leak = "ghs_0123456789abcdef0123456789abcdef";
    h.github
        .fail_next_pr(&format!("token {leak} is rate limited"));
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("rate limited"), "{e}");
    h.github
        .fail_mint(&format!("could not mint for url /r/{TOKEN}/mcp ({leak})"));
    let e2 = h.gw.publish("run-1").unwrap_err();
    for text in [e, e2, h.raw_log()] {
        assert!(!text.contains(leak), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("ghs_fake"), "{text}");
    }
}

#[test]
fn a_cancel_before_the_push_stops_publish() {
    let h = harness();
    let runs = h.runs.clone();
    h.github.on_commit(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
    });
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("canceled during publish"), "{e}");
    nothing_published(&h);
}

#[test]
fn a_cancel_after_the_push_deletes_the_branch_and_opens_no_pr() {
    let h = harness();
    let runs = h.runs.clone();
    h.github.on_set_branch(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
    });
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("canceled during publish"), "{e}");
    nothing_published(&h);
    assert!(h.tracker.prs("FAKE-1").is_empty());
}

#[test]
fn a_canceled_run_is_not_published() {
    let h = harness();
    h.stop("run-1");
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "refused: the run is canceled"
    );
    nothing_published(&h);
    assert_eq!(h.github.minted().len(), 0);
}

#[test]
fn publish_is_refused_when_the_intent_cannot_be_recorded() {
    let h = harness();
    // The policy decision is written; the intent row is refused.
    let db = rusqlite::Connection::open(&h.audit).unwrap();
    db.execute_batch(
        "CREATE TRIGGER no_intent BEFORE INSERT ON audit
         WHEN json_extract(NEW.payload, '$.kind') = 'tool_call'
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused: the audit write failed"), "{e}");
    assert_eq!(h.decisions().len(), 1);
    assert_eq!(h.github.minted().len(), 0);
    nothing_published(&h);
}

#[test]
fn published_finds_the_pr_of_a_run_that_died_mid_publish() {
    let h = harness();
    assert_eq!(h.gw.published("acme/app", "FAKE-1", "run-1").unwrap(), None);
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(
        h.gw.published("acme/app", "FAKE-1", "run-1").unwrap(),
        Some(p.pr)
    );
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn the_fake_forge_meets_the_contract() {
    let gh = FakeGitHub::default();
    gh.add_repo("acme/app", BASE);
    forge::check(&gh, &gh, "acme/app", BASE).unwrap();
}

/// A forge that dates each commit with the clock, as `git commit` does when
/// nothing pins the date.
#[derive(Default)]
struct ClockDated(FakeGitHub, AtomicU64);

impl Broker for ClockDated {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        self.0.mint(run, repo)
    }
    fn revoke(&self, run: &str) -> Result<()> {
        self.0.revoke(run)
    }
    fn revoke_all(&self) -> Result<()> {
        self.0.revoke_all()
    }
}

impl Forge for ClockDated {
    fn commit(&self, token: &Secret, commit: &Commit) -> Result<String> {
        let mut c = commit.clone();
        c.date = 1_700_000_000 + self.1.fetch_add(1, Ordering::SeqCst) as i64;
        self.0.commit(token, &c)
    }
    fn set_branch(&self, t: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()> {
        self.0.set_branch(t, repo, branch, sha)
    }
    fn delete_branch(&self, t: &Secret, repo: &str, branch: &str) -> Result<()> {
        self.0.delete_branch(t, repo, branch)
    }
    fn find_pr(&self, t: &Secret, repo: &str, branch: &str) -> Result<Option<String>> {
        self.0.find_pr(t, repo, branch)
    }
    fn open_draft_pr(
        &self,
        t: &Secret,
        r: &str,
        b: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        self.0.open_draft_pr(t, r, b, title, body)
    }
}

#[test]
fn the_contract_catches_a_forge_that_dates_commits_by_the_clock() {
    let f = ClockDated::default();
    f.0.add_repo("acme/app", BASE);
    let e = forge::check(&f, &f, "acme/app", BASE).unwrap_err();
    assert!(format!("{e:#}").contains("got a new SHA"), "{e:#}");
}

#[test]
fn cancel_revocation_does_not_prevent_branch_cleanup() {
    let h = harness();
    let runs = h.runs.clone();
    let gw = h.gw.clone();
    h.github.on_set_branch(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
        gw.end_run("run-1").unwrap();
    });
    assert!(h.gw.publish("run-1").is_err());
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-tool-gateway --test publish`
Expected: FAIL to compile: unresolved imports `kod_tool_gateway::broker`, `kod_tool_gateway::forge`; struct `Adapters` has no field named `broker`.

- [ ] **Step 3: Implement the broker, the forge, its contract and publish**

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

/// Mints short-lived credentials that can push agent/* branches and open
/// draft PRs in one repo, and nothing else. Each belongs to one run, so a
/// run's end or the kill switch can revoke it before it expires.
pub trait Broker: Send + Sync {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret>;
    /// Revokes every credential minted for the run. Safe to repeat.
    fn revoke(&self, run: &str) -> Result<()>;
    /// Revokes every credential this broker minted.
    fn revoke_all(&self) -> Result<()>;
}
````

Create `crates/kod-tool-gateway/src/forge.rs`:

````rust
//! The git host behind the publish operation, the host-side collector that
//! supplies the change, and fakes of both for tests.
use crate::broker::{Broker, Secret};
use anyhow::{Context, Result, bail, ensure};
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

/// What the host collected from a finished run, outside the sandbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Collected {
    /// The workspace diff against the run's base.
    pub changes: Vec<Change>,
    /// The host's own check results, for the PR body.
    pub test_evidence: String,
}

/// A finished run's collected output. In 6b the runner's collector writes
/// it next to the sandbox, never inside it (kod-eval's
/// `collect_out/diff.patch` and check results); the real `Workspace` reads
/// those files. Nothing the sandbox sends ever becomes a change.
pub trait Workspace: Send + Sync {
    fn collected(&self, run: &str) -> Result<Collected>;
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
    /// Unix seconds, used as both the author and the committer date, so
    /// the same commit always has the same SHA.
    pub date: i64,
}

/// The git host calls the publish operation makes. Every call carries the
/// credential the broker minted for this publish. `check` is the contract
/// every adapter must pass.
pub trait Forge: Send + Sync {
    /// Creates the commit and returns its SHA. The same commit twice gives
    /// the same SHA.
    fn commit(&self, token: &Secret, commit: &Commit) -> Result<String>;
    /// Points the branch at `sha`, creating it if needed.
    fn set_branch(&self, token: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()>;
    /// Deletes the branch. Deleting a missing branch is not an error.
    fn delete_branch(&self, token: &Secret, repo: &str, branch: &str) -> Result<()>;
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

/// The forge and broker contract, against a repo whose default branch is at
/// `base`. It replays a publish that died after the push and again after
/// the PR was opened but before its URL came back: the retry must make the
/// same commit and find the PR, not open a second one. It ends by checking
/// that a revoked credential is refused.
pub fn check(forge: &dyn Forge, broker: &dyn Broker, repo: &str, base: &str) -> Result<()> {
    let (run, branch) = ("contract-run", "agent/CONTRACT-1-contract-run");
    let commit = Commit {
        repo: repo.into(),
        base: base.into(),
        changes: vec![Change {
            path: "contract.txt".into(),
            content: Some("contract\n".into()),
        }],
        message: "contract\n".into(),
        committer: Identity {
            name: "kod-agent".into(),
            email: "agent@kod.invalid".into(),
        },
        date: 1_700_000_000,
    };
    let token = broker.mint(run, repo).context("mint")?;
    let first = forge.commit(&token, &commit).context("commit")?;
    forge
        .set_branch(&token, repo, branch, &first)
        .context("set_branch")?;
    ensure!(
        forge.find_pr(&token, repo, branch)?.is_none(),
        "find_pr found a PR nobody opened"
    );
    // The process died here; the retry starts over with a fresh credential.
    let token = broker.mint(run, repo).context("mint on retry")?;
    let again = forge.commit(&token, &commit).context("commit on retry")?;
    ensure!(
        again == first,
        "the same commit got a new SHA ({first} then {again}), so a retry would move the branch"
    );
    forge
        .set_branch(&token, repo, branch, &again)
        .context("set_branch on retry")?;
    let url = forge.open_draft_pr(&token, repo, branch, "contract", "contract")?;
    // The PR's URL was lost on the way back; the retry must find it.
    ensure!(
        forge.find_pr(&token, repo, branch)? == Some(url),
        "find_pr does not find the PR just opened"
    );
    forge
        .delete_branch(&token, repo, branch)
        .context("delete_branch")?;
    forge
        .delete_branch(&token, repo, branch)
        .context("delete_branch twice")?;
    broker.revoke(run).context("revoke")?;
    ensure!(
        forge.set_branch(&token, repo, branch, &first).is_err(),
        "a revoked credential still works"
    );
    Ok(())
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

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Inner {
    /// Live token to the run and repo it was minted for.
    tokens: HashMap<String, (String, String)>,
    minted: u64,
    /// Repo to the commits that exist in it.
    repos: HashMap<String, Vec<String>>,
    commits: HashMap<String, Commit>,
    branches: BTreeMap<(String, String), String>,
    prs: Vec<FakePr>,
    lose_next_pr: bool,
    fail_next_pr: Option<String>,
    fail_mint: Option<String>,
    on_commit: Option<Hook>,
    on_set_branch: Option<Hook>,
}

/// An in-memory GitHub: it mints run- and repo-scoped tokens and refuses
/// any call without a live token for that repo.
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

    /// The tokens not yet revoked.
    pub fn live_tokens(&self) -> Vec<String> {
        let mut live: Vec<String> = self.lock().tokens.keys().cloned().collect();
        live.sort();
        live
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

    /// The next `open_draft_pr` opens the PR and then fails, as if the
    /// process died before the reply came back.
    pub fn lose_next_pr(&self) {
        self.lock().lose_next_pr = true;
    }

    /// The next `open_draft_pr` fails with this message and opens nothing.
    pub fn fail_next_pr(&self, message: &str) {
        self.lock().fail_next_pr = Some(message.to_owned());
    }

    /// The next `mint` fails with this message.
    pub fn fail_mint(&self, message: &str) {
        self.lock().fail_mint = Some(message.to_owned());
    }

    /// Runs `f` once, at the start of the next `commit`.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.lock().on_commit = Some(Box::new(f));
    }

    /// Runs `f` once, right after the next `set_branch` moves the branch.
    pub fn on_set_branch(&self, f: impl FnOnce() + Send + 'static) {
        self.lock().on_set_branch = Some(Box::new(f));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Locks after checking the token is live and minted for `repo`.
    fn authorized(&self, t: &Secret, repo: &str) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let inner = self.lock();
        if inner.tokens.get(t.expose()).map(|(_, r)| r.as_str()) != Some(repo) {
            bail!("fake github: bad credential for {repo}");
        }
        Ok(inner)
    }
}

fn token(n: u64) -> String {
    format!("ghs_fake{n:032}")
}

impl Broker for FakeGitHub {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        let mut inner = self.lock();
        if let Some(m) = inner.fail_mint.take() {
            bail!("fake github: {m}");
        }
        if !inner.repos.contains_key(repo) {
            bail!("fake github: no repo {repo}");
        }
        inner.minted += 1;
        let t = token(inner.minted);
        let scope = (run.to_owned(), repo.to_owned());
        inner.tokens.insert(t.clone(), scope);
        Ok(Secret::new(t))
    }

    fn revoke(&self, run: &str) -> Result<()> {
        self.lock().tokens.retain(|_, (r, _)| r != run);
        Ok(())
    }

    fn revoke_all(&self) -> Result<()> {
        self.lock().tokens.clear();
        Ok(())
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
        // Like git: the SHA covers the content, the identity and the date.
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
        let hook = inner.on_set_branch.take();
        drop(inner);
        if let Some(f) = hook {
            f();
        }
        Ok(())
    }

    fn delete_branch(&self, t: &Secret, repo: &str, branch: &str) -> Result<()> {
        let mut inner = self.authorized(t, repo)?;
        inner.branches.remove(&(repo.to_owned(), branch.to_owned()));
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
        if let Some(m) = inner.fail_next_pr.take() {
            bail!("fake github: {m}");
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
        if std::mem::take(&mut inner.lose_next_pr) {
            bail!("fake github: connection reset");
        }
        Ok(url)
    }
}

/// The collector's output for tests: set a run's by hand.
#[derive(Default)]
pub struct FakeWorkspace(Mutex<HashMap<String, Collected>>);

impl FakeWorkspace {
    pub fn set(&self, run: &str, collected: Collected) {
        let mut runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        runs.insert(run.to_owned(), collected);
    }
}

impl Workspace for FakeWorkspace {
    fn collected(&self, run: &str) -> Result<Collected> {
        let runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        runs.get(run)
            .cloned()
            .with_context(|| format!("no collected change for {run}"))
    }
}
````

Apply to `crates/kod-tool-gateway/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tool-gateway/src/lib.rs b/crates/kod-tool-gateway/src/lib.rs
index 2d27bd8..397fbad 100644
--- a/crates/kod-tool-gateway/src/lib.rs
+++ b/crates/kod-tool-gateway/src/lib.rs
@@ -2,23 +2,37 @@
 //! and docs tools. Every call is checked by kod-policy and recorded in the
 //! audit log before it acts; a call that can't be decided or recorded
 //! doesn't happen. A canceled run gets nothing.
+//!
+//! Publish is not a tool. The host calls `Gateway::publish` with a run ID
+//! once the run has ended; the change comes from the host's own collector
+//! (`Workspace`), never from the sandbox.
 use anyhow::bail;
+use anyhow::{Context, anyhow};
+use broker::Broker;
 use docs::Docs;
+use forge::{Change, Commit, Forge, Identity, Workspace};
 use kod_audit::{AuditLog, RUN_AUDIT, redact::REDACTED};
 use kod_policy::Policy;
 use kod_tracker::{Issue, Rework, ReworkKind, State, Tracker};
 use serde_json::{Value, json};
 use sha2::{Digest, Sha256};
+use std::collections::HashSet;
 use std::{
     collections::HashMap,
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
 /// The moves the agent may make on its own ticket. Review and done are the
 /// dispatcher's, on publish.
 pub const AGENT_TRANSITIONS: [(State, State); 2] = [
@@ -70,16 +84,28 @@ pub struct Adapters {
     pub tracker: Arc<dyn Tracker>,
     pub docs: Arc<dyn Docs>,
     pub stopped: Stopped,
+    pub broker: Arc<dyn Broker>,
+    pub forge: Arc<dyn Forge>,
+    pub workspace: Arc<dyn Workspace>,
 }
 
 pub struct Gateway {
     policy: Arc<Policy>,
     audit: Arc<AuditLog>,
     adapters: Adapters,
+    agent: Identity,
     /// Keyed by the token's SHA-256, as in the LLM gateway.
     runs: Mutex<HashMap<String, Live>>,
 }
 
+/// What a publish made.
+#[derive(Clone, Debug, PartialEq)]
+pub struct Published {
+    pub branch: String,
+    pub commit: String,
+    pub pr: String,
+}
+
 fn hex(bytes: &[u8]) -> String {
     bytes.iter().map(|b| format!("{b:02x}")).collect()
 }
@@ -137,16 +163,45 @@ fn issue_json(i: &Issue) -> Value {
     })
 }
 
+/// The collected change, checked for shape and size. Path rules are the
+/// policy's.
+fn check_changes(changes: &[Change]) -> Result<(), String> {
+    if changes.is_empty() || changes.len() > MAX_FILES {
+        return Err(format!(
+            "the change must have 1 to {MAX_FILES} files, it has {}",
+            changes.len()
+        ));
+    }
+    let mut seen = HashSet::new();
+    if let Some(c) = changes.iter().find(|c| !seen.insert(&c.path)) {
+        return Err(format!("{} is in the change twice", c.path));
+    }
+    let bytes: usize = changes
+        .iter()
+        .map(|c| c.content.as_ref().map_or(0, String::len))
+        .sum();
+    if bytes > MAX_PUBLISH_BYTES {
+        return Err(format!("the change is over {MAX_PUBLISH_BYTES} bytes"));
+    }
+    Ok(())
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
@@ -195,16 +250,20 @@ impl Gateway {
         Ok(())
     }
 
-    /// Stops serving a run: its token stops working. Safe to repeat. The
-    /// dispatcher calls it when the run ends, however it ends.
+    /// Stops serving a run: its token stops working and every credential
+    /// minted for it is revoked. Safe to repeat. The dispatcher calls it
+    /// when the run ends, however it ends.
     pub fn end_run(&self, run_id: &str) -> anyhow::Result<()> {
         self.lock().retain(|_, l| l.run.id != run_id);
+        self.adapters.broker.revoke(run_id)?;
         Ok(())
     }
 
-    /// The kill switch: stop serving every run.
+    /// The kill switch: stop serving every run and revoke every agent
+    /// credential.
     pub fn end_all(&self) -> anyhow::Result<()> {
         self.lock().clear();
+        self.adapters.broker.revoke_all()?;
         Ok(())
     }
 
@@ -406,4 +465,143 @@ impl Gateway {
         self.update(&run.id, |l| l.created.push(key.clone()));
         Ok(json!({"key": key}))
     }
+
+    /// Publishes a run's collected change to `agent/<ticket>-<run>` and
+    /// opens a draft PR. Host only: the dispatcher calls it when the runner
+    /// reports the run done. Idempotent: a retry makes the same commit and
+    /// finds the same PR. The run's credential is revoked when it returns.
+    pub fn publish(&self, run_id: &str) -> Result<Published, String> {
+        let live = self
+            .lock()
+            .values()
+            .find(|l| l.run.id == run_id)
+            .cloned()
+            .ok_or("unknown run")?;
+        let r = self.publish_live(&live);
+        if let Err(e) = self.adapters.broker.revoke(run_id) {
+            let _ = self.audit.append(
+                RUN_AUDIT,
+                &json!({"kind": "revoke_failed", "run": run_id, "error": scrub(&format!("{e:#}"))}),
+            );
+        }
+        r.map_err(|e| scrub(&e))
+    }
+
+    fn publish_live(&self, live: &Live) -> Result<Published, String> {
+        let run = &live.run;
+        self.not_stopped(&run.id)?;
+        let collected = self
+            .adapters
+            .workspace
+            .collected(&run.id)
+            .map_err(|e| format!("reading the collected change failed: {e:#}"))?;
+        let files = collected.changes;
+        check_changes(&files)?;
+        let paths: Vec<&str> = files.iter().map(|c| c.path.as_str()).collect();
+        let branch = format!("agent/{}-{}", run.ticket, run.id);
+        self.allow(json!({
+            "action": "publish",
+            "actor": {"kind": "agent"},
+            "run": {
+                "id": run.id,
+                "ticket": run.ticket,
+                "repo": run.repo,
+                "canceled": false,
+                "data_classes": live.run.data_classes,
+            },
+            "target": {"repo": run.repo, "branch": branch, "draft": true},
+            "changed_paths": paths,
+        }))?;
+        let issue = self.own_issue(run)?;
+        let tracker = self.adapters.tracker.name();
+        let a = &self.agent;
+        let message = format!(
+            "{}\n\nKod-Ticket: {tracker}:{}\nKod-Run: {}\nCo-Authored-By: {} <{}>\n",
+            issue.title, run.ticket, run.id, a.name, a.email
+        );
+        let pinned: String = issue
+            .approved_versions
+            .iter()
+            .map(|v| format!("- {v}\n"))
+            .collect();
+        let changed: String = paths.iter().map(|p| format!("- {p}\n")).collect();
+        let body = format!(
+            "Ticket: {tracker}:{}\nRequirement: {}\nRun: {}\n\nPinned inputs:\n{pinned}\nChanged files:\n{changed}\nTest evidence:\n{}\n",
+            run.ticket,
+            issue.requirement_id.as_deref().unwrap_or("none"),
+            run.id,
+            collected.test_evidence,
+        );
+        let pr_title = format!("{}: {}", run.ticket, issue.title);
+        let target =
+            json!({"repo": run.repo, "branch": branch, "draft": true, "changed_paths": paths});
+        let (f, t) = (&self.adapters.forge, &self.adapters.tracker);
+        let (sha, pr) = self.act(run, "publish", target, || {
+            let token = self.adapters.broker.mint(&run.id, &run.repo)?;
+            let canceled = || self.not_stopped(&run.id).is_err();
+            let push = || -> anyhow::Result<(String, String)> {
+                let commit = Commit {
+                    repo: run.repo.clone(),
+                    base: run.base.clone(),
+                    changes: files,
+                    message,
+                    committer: a.clone(),
+                    date: run.started_at,
+                };
+                let sha = f.commit(&token, &commit)?;
+                // A cancel can land at any time; check right before each
+                // step anyone else can see.
+                if canceled() {
+                    bail!("the run was canceled during publish");
+                }
+                f.set_branch(&token, &run.repo, &branch, &sha)?;
+                if canceled() {
+                    // Cancellation may already have revoked the push token.
+                    let cleanup = self.adapters.broker.mint(&run.id, &run.repo)?;
+                    f.delete_branch(&cleanup, &run.repo, &branch)
+                        .map_err(|e| {
+                            anyhow!("{}", format!("{e:#}").replace(cleanup.expose(), REDACTED))
+                        })
+                        .context("the run was canceled during publish; deleting its branch")?;
+                    bail!("the run was canceled during publish");
+                }
+                let pr = match f.find_pr(&token, &run.repo, &branch)? {
+                    Some(url) => url,
+                    None => f.open_draft_pr(&token, &run.repo, &branch, &pr_title, &body)?,
+                };
+                t.link_pr(&run.ticket, &pr)
+                    .context("linking the PR to the ticket")?;
+                Ok((sha, pr))
+            };
+            // An adapter error can echo the credential; it never leaves here.
+            push().map_err(|e| anyhow!("{}", format!("{e:#}").replace(token.expose(), REDACTED)))
+        })?;
+        Ok(Published {
+            branch,
+            commit: sha,
+            pr,
+        })
+    }
+
+    /// The draft PR a publish of this run opened, if any: how the dispatcher
+    /// settles a run it found in `publishing` after a restart.
+    pub fn published(
+        &self,
+        repo: &str,
+        ticket: &str,
+        run_id: &str,
+    ) -> Result<Option<String>, String> {
+        let branch = format!("agent/{ticket}-{run_id}");
+        let b = &self.adapters.broker;
+        let found = b.mint(run_id, repo).and_then(|token| {
+            self.adapters
+                .forge
+                .find_pr(&token, repo, &branch)
+                .map_err(|e| anyhow!("{}", format!("{e:#}").replace(token.expose(), REDACTED)))
+        });
+        let revoked = b.revoke(run_id);
+        let found = found.map_err(|e| scrub(&format!("{e:#}")))?;
+        revoked.map_err(|e| scrub(&format!("{e:#}")))?;
+        Ok(found)
+    }
 }
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-tool-gateway && cargo clippy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (publish 17, tools 13), clean.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-tool-gateway
git commit -m "kod-tool-gateway: host-only idempotent publish with a brokered, revoked credential"
```

### Task 3: The MCP endpoint

**Files:**
- Modify: `crates/kod-tool-gateway/Cargo.toml`, `src/lib.rs`
- Create: `crates/kod-tool-gateway/src/mcp.rs`
- Test: `crates/kod-tool-gateway/tests/common/mod.rs` (modify), `tests/mcp.rs`

**Interfaces:**
- Consumes: Task 1's `Gateway::call` and `Reply`.
- Produces: `kod_tool_gateway::router(Arc<Gateway>, Vec<String>) -> axum::Router` serving `POST /r/{token}/mcp`; `mcp::tools() -> Vec<rmcp::model::Tool>` (the five sandbox tools with their input schemas, never `publish`); test helper `common::serve(Arc<Gateway>) -> String` (base URL).

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
index 9d6433a..ebdacf5 100644
--- a/crates/kod-tool-gateway/tests/common/mod.rs
+++ b/crates/kod-tool-gateway/tests/common/mod.rs
@@ -175,3 +175,19 @@ impl Harness {
         String::from_utf8_lossy(&std::fs::read(&self.audit).unwrap()).into_owned()
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
fn a_publish_call_from_the_sandbox_is_refused() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let files = json!([{"path": "src/evil.rs", "content": "steal()"}]);
    let params = json!({"name": "publish", "arguments": {"title": "t", "summary": "s",
        "test_evidence": "e", "files": files}});
    let (status, r) = mcp(&url, "tools/call", Some("publish"), params);
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(r.to_string().contains("unknown tool"), "{r}");
    assert!(h.github.minted().is_empty());
    assert!(h.github.prs().is_empty());
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
Expected: FAIL to compile: cannot find function `router` in crate `kod_tool_gateway`.

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

/// The tools the endpoint lists: everything a run may call. Publish is not
/// one; only the host publishes, with `Gateway::publish`.
pub fn tools() -> Vec<Tool> {
    let s = json!({"type": "string"});
    vec![
        Tool::new(
            "get_issue",
            "Read a ticket: the run's own ticket, or a rework request Kod linked to it.",
            schema(json!({"key": s}), &["key"]),
        ),
        Tool::new(
            "read_doc",
            "Read an approved document linked from the run's ticket, by its pinned ID.",
            schema(json!({"id": s}), &["id"]),
        ),
        Tool::new(
            "comment",
            "Comment on the run's ticket, or a rework request Kod linked to it.",
            schema(json!({"key": s, "text": s}), &["key", "text"]),
        ),
        Tool::new(
            "transition",
            "Move the run's own ticket from in_progress to blocked, or back.",
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
index 397fbad..f420208 100644
--- a/crates/kod-tool-gateway/src/lib.rs
+++ b/crates/kod-tool-gateway/src/lib.rs
@@ -25,6 +25,9 @@ use std::{
 pub mod broker;
 pub mod docs;
 pub mod forge;
+mod mcp;
+
+pub use mcp::router;
 
 /// A run token is at least this many lower-case hex characters, as in the
 /// LLM gateway.
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-tool-gateway && cargo clippy -p kod-tool-gateway --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (mcp 4, publish 17, tools 13), clean.

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
- Produces: `kod_loop::mcp::{Mcp::{connect(&str) -> anyhow::Result<Mcp>, definitions, offers, call(&str, &Value) -> ToolResult}, PROTOCOL, TIMEOUT, scrub}`; `kod_loop::run_with(&Config, Option<&Mcp>, &str, &mut dyn Write) -> Outcome` (`run` is `run_with(.., None, ..)`); `kod-loop` reads optional `KOD_MCP_URL`.

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
    forge::{FakeGitHub, FakeWorkspace, Identity},
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
        stopped: Arc::new(|_: &str| Ok(false)),
        broker: github.clone(),
        forge: github,
        workspace: Arc::new(FakeWorkspace::default()),
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
        linked_tickets: vec![],
        started_at: 1_700_000_000,
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

#[test]
fn an_unreachable_gateway_does_not_leak_the_run_token() {
    // Nothing listens on port 9.
    let e = Mcp::connect(&format!("http://127.0.0.1:9/r/{TOKEN}/mcp"))
        .err()
        .unwrap();
    let e = format!("{e:#}");
    assert!(e.contains("reaching the tool gateway"), "{e}");
    assert!(!e.contains(TOKEN), "{e}");
}

#[test]
fn an_error_page_that_echoes_the_path_does_not_leak_the_run_token() {
    let app = axum::Router::new().fallback(|uri: axum::http::Uri| async move {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("no handler for {uri}"),
        )
    });
    let base = serve(app);
    let e = Mcp::connect(&format!("{base}/r/{TOKEN}/mcp"))
        .err()
        .unwrap();
    let e = format!("{e:#}");
    assert!(e.contains("HTTP 500"), "{e}");
    assert!(e.contains("/r/[REDACTED]/mcp"), "{e}");
    assert!(!e.contains(TOKEN), "{e}");
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
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL: &str = "2026-07-28";
/// One gateway call; a publish of a large change is the slowest.
pub const TIMEOUT: Duration = Duration::from_secs(120);

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
            Err(e) => (scrub(&format!("the tool gateway failed: {e:#}")), true),
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
        // reqwest puts the URL, and so the run token, in its errors.
        let resp = req
            .send()
            .map_err(|e| anyhow!("reaching the tool gateway: {}", e.without_url()))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| anyhow!("reading the tool gateway's reply: {}", e.without_url()))?;
        if !status.is_success() {
            let head = scrub(crate::api::head(&text, 200));
            bail!("{method}: HTTP {status}: {head}");
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

/// Replaces a run token in a `/r/<token>/` path with `[REDACTED]`: the
/// token is the run's credential and must not reach the trace.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("/r/") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let hex = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            .count();
        if hex >= 32 {
            out.push_str("[REDACTED]");
            rest = &rest[hex..];
        }
    }
    out.push_str(rest);
    out
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
Expected: PASS (kod-loop mcp 4 and its existing tests unchanged; kod-tool-gateway 34), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-loop
git commit -m "kod-loop: MCP client for the tool gateway"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc's tool gateway, token broker, trust boundary and action matrix sections and this plan, with these questions:
- Is there any path from `Gateway::call` to an adapter that skips the `stopped` check, `Policy::decide`, `record_decision` or `AuditLog::before`?
- Is there any way for the sandbox to reach `publish`, or to choose what it pushes or which paths the policy sees?
- Does any policy input use the ticket's live links instead of the run's snapshot?
- Can a `Secret` or a run token reach a reply, an error string, an audit payload or a `Debug` print? Is every minted credential revoked?
- Is publish idempotent under every partial failure the fake can simulate, and does `forge::check` require it of any adapter? Is a cancel at any point honoured, with no branch or PR left behind?
- Do the policy inputs match the policy plan's `read`, `tracker_write` and `publish` contracts field for field?
- Does `kod-loop` without `KOD_MCP_URL` behave exactly as before?

## Self-review

- **Spec coverage.**
  - MCP endpoint with tracker and docs tools against the 6a fake: Tasks 1 and 3.
  - Token broker: Task 2 (`Broker`, `Secret`, minted inside publish only, revoked after it and on run end).
  - Publish (agent branch, draft PR, idempotent, refused when canceled, checked against `changed_paths` computed from the collected change, host-only): Task 2.
  - MCP client in kod-loop: Task 4.
  - Policy and audit before acting, failing closed: Tasks 1 and 2.
  - Overview exit criteria:
    - a tool call through the endpoint (`a_tool_call_goes_through_the_endpoint_and_the_policy`, `the_agents_tracker_calls_go_through_the_gateway`);
    - a planted secret refused and audited (`a_planted_secret_cannot_be_read_and_the_attempt_is_audited`);
    - the PR carries the story ID and agent trailers (`publish_pushes_the_collected_change_and_opens_a_draft_pr`);
    - a test per publish refusal in the matrix row: canceled, intent not recordable, data class not cleared, a path that raises the risk class (Task 2).
- **Placeholders.** None.
- **Type consistency.** `Gateway`, `Run`, `Adapters`, `Stopped`, `Reply`, `Published`, `Secret`, `Broker`, `Forge`, `Workspace`, `Change`, `Collected`, `Identity`, `Commit`, `FakeGitHub`, `FakeWorkspace` and `Mcp` are spelled the same in every task. The code was compiled and tested in task order on top of sub-plans 3, 4, 6a and 2. Each task's tests were run against the previous task's code to see them fail. The plan text was replayed from scratch.
- **Review Focus.** Each line names its tests and task above.

## Open items

- **The real broker** (owner, 6b): a GitHub App installation token scoped to the one repo (recommended), or a fine-grained token. Either must support revocation. The `Broker` trait is the same either way.
- **The real forge adapter** (6b) must pass `forge::check`, including the pinned commit date.
- **Which process serves the router** (owner, 6b): the LLM gateway's process as the overview says (D1), with one token per run for both routes.
- **Binary files in publish**: UTF-8 only now (D9). Say if agents must publish binary files; it needs base64 content in the change set.
