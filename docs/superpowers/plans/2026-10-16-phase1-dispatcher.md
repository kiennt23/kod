# Phase 1.6 Dispatcher (`kod-dispatcher`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development; *Native* tasks use superpowers:executing-plans. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `kod-dispatcher` crate that takes tracker webhooks, checks their signature, dedups deliveries, queues them in a SQLite table, admits a ticket atomically against the WIP limit (placeholder 1) or comments why not, starts and ends runs through a gateway with states that only move forward, cancels a run on label removal or a ticket edit, and holds the kill switch and `resume` per the policy plan's D14.

**Architecture:** A library: `Dispatcher` holds the dispatcher config (repo, owner, backup), the policy (or the error it failed to load with), the audit log, the tracker adapter, a `Gateway` trait object and one SQLite connection. `receive` verifies and stores a delivery (`INSERT OR IGNORE` on the delivery ID); `work_once` handles the oldest queued delivery and starts queued runs. Admission does the WIP count and the run insert in one `IMMEDIATE` transaction, and the database enforces one live run per ticket and forward-only states with a unique index and triggers. `finished` is how the run's caller (6b) ends it; `kill` and `resume` are the stop and restart commands; `router` serves `POST /webhook`. Tests use the 6a fake tracker and a fake gateway that records starts, stops and publish calls.

**Tech Stack:** Rust 1.99.0 (edition 2024). No new crates: rusqlite 0.40 (bundled, as kod-audit), axum 0.8, tokio, serde_json, sha2 0.10, anyhow; tower 0.5 in tests. Crates from this phase: kod-policy, kod-audit (PR #14 / `feat/phase1-audit`), kod-tracker (the 6a plan, extended here).

**Spec:** `docs/kod-harness-design.md` (the PR flow, Run identity and lifecycle, Rework and staleness, Ownership and the kill switch, the action matrix rows for admit, cancel and resume, Workflow status), row 6 of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/superpowers/plans/2026-10-15-phase1-policy.md` (D14 and the `delegate`, `cancel` and `resume` inputs), `docs/superpowers/plans/2026-10-15-phase1-audit.md`, `docs/superpowers/plans/2026-10-15-phase1-tracker-contract.md`, `docs/adr/0001-per-task-execution-mode.md`.

## Decisions

| | Decision | Why |
|---|---|---|
| D1 | SQLite (rusqlite 0.40, bundled), one database file owned by the dispatcher, `synchronous = FULL`, a 2 s busy timeout. | The same engine and settings as kod-audit; the dispatcher is one process (the design), so a server database buys nothing in phase 1. Postgres is the upgrade if the dispatcher ever runs as more than one process. |
| D2 | The queue is the `deliveries` table keyed by the delivery ID. `receive` stores a verified delivery with `INSERT OR IGNORE`; a stored ID is a `Duplicate` even after a restart, when the adapter's in-memory dedup is empty. A delivery is marked done in the same transaction as the work it caused. | Dedup that survives a restart, and a crash between the work and the mark cannot run it twice. |
| D3 | Admission does the WIP count, the live-run check, the kill-switch check, the `delegate` decision and the run insert in one `IMMEDIATE` transaction. The WIP count is every live run (`queued`, `running`, `publishing`). Bounces are the ticket's runs that ended `blocked`. | The design's atomic admission: two deliveries can't both take the last WIP slot. |
| D4 | The database enforces the run rules: a `CHECK` on the eight state names, a unique index allowing one live run per ticket, a trigger refusing any state move that isn't forward (`queued` 0, `running` 1, `publishing` 2, ended 3), and a trigger refusing a change to a run's ticket or revision. | Forward-only holds even against a bug in the code above it. |
| D5 | The revision of a ticket is the first 8 bytes of the SHA-256 of its title, body, acceptance criteria, requirement ID, risk class and approved document versions. Workflow state and links are left out. | The 6a tracker has no revision field; these are the run's inputs. The run itself moves the state and adds links, which must not look like an edit. |
| D6 | kod-tracker gains `TaskEvent::Unlabeled { issue, by }` and `TaskEvent::Edited { issue }`; the fake parses `label_removed` (trigger label only) and `issue_edited`. | 6a's events cover labelling and comments only, and the overview's cancel needs these two. The contract tests gain them in 6c. |
| D7 | A label removal cancels the ticket's live run through the policy's `cancel` rule (`actor.kind = "label_removal"`). If the policy is unavailable or the decision can't be recorded, nothing is canceled and the ticket gets a comment naming the kill switch. | The matrix puts cancel through policy except for the owner's and backup's kill switch. |
| D8 | An edit that changes the revision ends the live run `canceled` and returns the ticket to ready at its new revision. A new run needs a new label. An edit that doesn't change the revision does nothing. It needs no policy decision. | The design's "a ticket edit during a run stops the run and returns the ticket to ready at its new revision". Staleness isn't a permission. |
| D9 | `kill(login)`: the owner or backup named in the dispatcher's own config is honoured without the policy and without a working audit log (the audit record is best effort). Anyone else goes through the policy's `cancel` `scope: "all"` rule with a recorded decision. The kill switch cancels every live run and sets a persisted `halted` flag in one transaction, then calls `Gateway::stop_all`, then moves and comments the tickets on a best-effort basis. | Policy plan D14: the owner's or backup's kill switch must not depend on the policy. Canceling in the database first means the runs are stopped from the dispatcher's view even if the gateway call fails, and the error says so. |
| D10 | `resume(login)` goes through the policy's `resume` rule (owner or backup) with a recorded decision; it fails while the policy is unavailable. While halted, admission refuses every ticket and queued runs aren't started. | D14: restart only by the owner or backup. Restarting is not urgent, so it stays behind the policy. |
| D11 | Run outcomes: `finished(run, outcome, reason)` takes an ended state for a `running` run. `Done` moves through `publishing`, calls `Gateway::publish` and ends `done` (ticket to in review) or `blocked` if publish fails. Other endings are recorded as given; `canceled` comes only from the cancel paths. A run whose start fails ends `needs_human`. | The design's lifecycle, with publish refused for a canceled run because a canceled run can't reach `publishing`. |
| D12 | Ticket moves: admitted → in progress; done → in review; canceled → ready; blocked, needs_human, over_budget → blocked. Every ending is commented with the run ID, the state and the reason. | The 6a adapter's five states. |
| D13 | A library and a router, no binary. `Config` is `{repo, owner, backup}` JSON with exact keys, logins lower-cased and the backup different from the owner, checked at load. The kill command's login is given by the caller. | 6b wires the process and the real adapters, and decides how a kill command proves who sent it (an open item). |

Out of scope here: the real tracker and gateway adapters and the process (6b); the remaining admission checks that need real documents (approved and current versions; 6b with the docs adapter); revoking agent identities (the gateway's `stop_all` in 6b); the bounce limit beyond counting `blocked` runs (rework is sub-plan 7).

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Unlabel and edit events in kod-tracker | Native | A two-variant enum change and fake parsing, no safety guarantee of its own |
| 2. Config and the store | Subagent + review | Owns forward-only states and one live run per ticket in the database, and config validation at load |
| 3. Admission, runs and the webhook route | Subagent + review | Owns signature, dedup, atomic WIP admission and refusal comments; two Review Focus tests |
| 4. Cancel, the kill switch and resume | Subagent + review | Owns D14: the kill switch without policy; three Review Focus tests |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged. No new crates.
- Fail closed: an unverified webhook is refused; a policy that failed to load refuses admission, label-removal cancels, a tech lead's kill switch and resume; a decision that can't be recorded refuses the action. The one exception is the owner's or backup's kill switch (D9).
- Config is validated at load; an unknown, missing or malformed key is an error naming the file.
- No network: fakes only (the 6a fake tracker and a fake gateway).
- Code against kod-audit's plan API (`open`, `record_decision`, `append`, `RUN_AUDIT`) and kod-tracker's plan API.
- Commit messages are plain, with no trailers.

## Review Focus

- A delivery replayed after a restart, when the adapter's in-memory dedup is empty, must not start a second run: `a_duplicate_delivery_never_starts_a_second_run` (Task 3).
- A policy whose files fail to load must refuse every admission with the reason commented, not admit with defaults: `a_policy_that_failed_to_load_refuses_every_admission` (Task 3).
- The owner's kill switch must work when the policy fails to load: `the_owners_kill_switch_works_when_the_policy_fails_to_load` (Task 4).
- The owner's kill switch must still cancel the runs and hold when the audit log is locked and the gateway is down, and say the gateway didn't confirm: `the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail` (Task 4).
- A label removal while the policy is down must leave the run alone and point at the kill switch, not cancel silently or fail without a word: `a_label_removal_while_the_policy_is_down_is_refused_and_says_so` (Task 4).

## File Structure

```
Cargo.toml                                   modify: add crates/kod-dispatcher to members (Task 2)
crates/kod-tracker/
  src/lib.rs                                 modify: TaskEvent::{Unlabeled, Edited} (Task 1)
  src/fake.rs                                modify: parse label_removed and issue_edited (Task 1)
  tests/fake.rs                              modify (Task 1)
crates/kod-dispatcher/
  Cargo.toml                                 create (Task 2), replace (Task 3)
  src/config.rs                              create: Config, parse, load (Task 2)
  src/store.rs                               create: the schema, open (Task 2)
  src/lib.rs                                 create (Task 2), replace (Task 3), modify (Task 4)
  tests/config.rs                            create (Task 2)
  tests/store.rs                             create (Task 2)
  tests/common/mod.rs                        create: FakeGateway and the harness (Task 3)
  tests/admission.rs                         create (Task 3)
  tests/cancel.rs                            create (Task 4)
```

Execution order: this plan runs after sub-plans 3 (policy), 4 (audit), 6a (tracker), 2 (LLM gateway) and 5 (tool gateway). It doesn't use 5's code; its diffs apply on top of it.

---

### Task 1: Unlabel and edit events in kod-tracker

**Files:**
- Modify: `crates/kod-tracker/src/lib.rs`, `crates/kod-tracker/src/fake.rs`
- Test: `crates/kod-tracker/tests/fake.rs`

**Interfaces:**
- Consumes: 6a's `TaskEvent`, `Delivery`, `FakeTracker::{webhook, parse_webhook}`, `FAKE_MAPPING` (trigger label `kod`).
- Produces: `TaskEvent::Unlabeled { issue: String, by: String }` and `TaskEvent::Edited { issue: String }`; the fake maps `{"type": "label_removed", "label": "kod", "issue", "user"}` and `{"type": "issue_edited", "issue"}` to them.

- [ ] **Step 1: Write the failing test**

Apply to `crates/kod-tracker/tests/fake.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/tests/fake.rs b/crates/kod-tracker/tests/fake.rs
index a73926d..f0d4cd9 100644
--- a/crates/kod-tracker/tests/fake.rs
+++ b/crates/kod-tracker/tests/fake.rs
@@ -100,3 +100,42 @@ fn delivery_ids_are_spent_only_by_signed_requests() {
     let e = t.parse_webhook(&s.unidentified).unwrap_err().to_string();
     assert!(e.contains("no delivery ID"), "{e}");
 }
+
+#[test]
+fn label_removal_and_edits_are_task_events() {
+    let (t, _) = sandbox();
+    let parse = |body| t.parse_webhook(&t.webhook(&body)).unwrap();
+    assert_eq!(
+        parse(
+            json!({"delivery": "e1", "type": "label_removed", "issue": "FAKE-1", "label": "kod", "user": "dev"})
+        ),
+        Some(Delivery {
+            id: "e1".into(),
+            event: TaskEvent::Unlabeled {
+                issue: "FAKE-1".into(),
+                by: "dev".into()
+            }
+        })
+    );
+    assert_eq!(
+        parse(json!({"delivery": "e2", "type": "issue_edited", "issue": "FAKE-1"})),
+        Some(Delivery {
+            id: "e2".into(),
+            event: TaskEvent::Edited {
+                issue: "FAKE-1".into()
+            }
+        })
+    );
+    // Another label coming off is not ours.
+    assert_eq!(
+        parse(
+            json!({"delivery": "e3", "type": "label_removed", "issue": "FAKE-1", "label": "frontend", "user": "dev"})
+        ),
+        None
+    );
+    // Signed but without its issue: an error, not a quiet None.
+    assert!(
+        t.parse_webhook(&t.webhook(&json!({"delivery": "e4", "type": "issue_edited"})))
+            .is_err()
+    );
+}
````

- [ ] **Step 2: Run the test to see it fail**

Run: `cargo test -p kod-tracker --test fake`
Expected: FAIL to compile: `no variant named Unlabeled found for enum TaskEvent`.

- [ ] **Step 3: Add the events**

Apply to `crates/kod-tracker/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/lib.rs b/crates/kod-tracker/src/lib.rs
index d38c7b1..4bd0510 100644
--- a/crates/kod-tracker/src/lib.rs
+++ b/crates/kod-tracker/src/lib.rs
@@ -93,6 +93,12 @@ pub enum TaskEvent {
         author: String,
         body: String,
     },
+    /// The trigger label was removed. The dispatcher cancels the ticket's
+    /// live run.
+    Unlabeled { issue: String, by: String },
+    /// The issue's title, body or fields changed. The dispatcher re-reads it
+    /// and stops a run whose inputs are now stale.
+    Edited { issue: String },
 }
 
 /// A verified task event and the tracker's ID for its delivery. The
````

Apply to `crates/kod-tracker/src/fake.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/fake.rs b/crates/kod-tracker/src/fake.rs
index 7df0f22..fd768d6 100644
--- a/crates/kod-tracker/src/fake.rs
+++ b/crates/kod-tracker/src/fake.rs
@@ -258,6 +258,15 @@ impl Tracker for FakeTracker {
                     by: text("user")?,
                 })
             }
+            Some("label_removed") if text("label")? == self.mapping.trigger_label => {
+                Some(TaskEvent::Unlabeled {
+                    issue: text("issue")?,
+                    by: text("user")?,
+                })
+            }
+            Some("issue_edited") => Some(TaskEvent::Edited {
+                issue: text("issue")?,
+            }),
             Some("comment") => Some(TaskEvent::Commented {
                 issue: text("issue")?,
                 author: text("user")?,
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-tracker fake gains 1; nothing else changes), clean.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-tracker
git commit -m "kod-tracker: unlabel and edit events"
```

### Task 2: Config and the store

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/kod-dispatcher/Cargo.toml`, `src/lib.rs`, `src/config.rs`, `src/store.rs`
- Test: `crates/kod-dispatcher/tests/config.rs`, `tests/store.rs`

**Interfaces:**
- Consumes: nothing from this plan.
- Produces: `Config { repo: String, owner: String, backup: Option<String> }` with `Config::{parse(&str), load(&Path)} -> anyhow::Result<Config>` and `may_always_stop(&str) -> bool`; `store::{open(&Path) -> anyhow::Result<rusqlite::Connection>, BUSY_TIMEOUT, LIVE}`; tables `deliveries(id, event, done)`, `runs(seq, ticket, revision, state, reason)`, `control(id, halted, by)`.

- [ ] **Step 1: Write the failing tests**

Apply to `Cargo.toml` with `git apply`:

````diff
diff --git a/Cargo.toml b/Cargo.toml
index 56a355c..6efd8cd 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,3 +1,3 @@
 [workspace]
-members = ["crates/kod-audit", "crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy", "crates/kod-tool-gateway", "crates/kod-tracker"]
+members = ["crates/kod-audit", "crates/kod-dispatcher", "crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy", "crates/kod-tool-gateway", "crates/kod-tracker"]
 resolver = "3"
````

Create `crates/kod-dispatcher/Cargo.toml`:

````toml
[package]
name = "kod-dispatcher"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
rusqlite = { version = "0.40", features = ["bundled"] }
serde_json = "1"

[dev-dependencies]
rusqlite = "0.40"
tempfile = "3"
````

Create `crates/kod-dispatcher/tests/config.rs`:

````rust
use kod_dispatcher::Config;

#[test]
fn a_valid_config_loads_with_logins_lower_cased() {
    let c =
        Config::parse(r#"{"repo": "acme/app", "owner": "KienNT23", "backup": "Bob-2"}"#).unwrap();
    assert_eq!(
        c,
        Config {
            repo: "acme/app".into(),
            owner: "kiennt23".into(),
            backup: Some("bob-2".into()),
        }
    );
    assert!(c.may_always_stop(" KIENNT23 "));
    assert!(c.may_always_stop("bob-2"));
    assert!(!c.may_always_stop("mallory"));
    assert!(!c.may_always_stop(""));
}

#[test]
fn a_bad_config_does_not_load() {
    for (text, why) in [
        ("[]", "must be an object"),
        ("{", "not JSON"),
        (
            r#"{"repo": "acme/app", "owner": "k"}"#,
            "missing key \"backup\"",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": null, "wip": 2}"#,
            "unknown key \"wip\"",
        ),
        (
            r#"{"repo": "Acme/App", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme/app/x", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "", "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": " k", "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": null, "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": ""}"#,
            "backup must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": "K"}"#,
            "someone other",
        ),
    ] {
        let e = format!("{:#}", Config::parse(text).unwrap_err());
        assert!(e.contains(why), "{text}: {e}");
    }
}

#[test]
fn load_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.json");
    std::fs::write(&path, "{}").unwrap();
    let e = format!("{:#}", Config::load(&path).unwrap_err());
    assert!(e.contains("dispatcher.json"), "{e}");
}
````

Create `crates/kod-dispatcher/tests/store.rs`:

````rust
use kod_dispatcher::store;

fn set(c: &rusqlite::Connection, state: &str) -> Result<usize, String> {
    c.execute("UPDATE runs SET state = ?1 WHERE seq = 1", [state])
        .map_err(|e| e.to_string())
}

#[test]
fn run_states_only_move_forward() {
    let dir = tempfile::tempdir().unwrap();
    let c = store::open(&dir.path().join("d.db")).unwrap();
    c.execute(
        "INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'r1', 'queued')",
        [],
    )
    .unwrap();
    assert_eq!(set(&c, "running"), Ok(1));
    let e = set(&c, "queued").unwrap_err();
    assert!(e.contains("run states only move forward"), "{e}");
    assert!(set(&c, "running").is_err());
    assert_eq!(set(&c, "publishing"), Ok(1));
    assert_eq!(set(&c, "canceled"), Ok(1));
    for later in ["done", "running", "canceled", "blocked"] {
        assert!(set(&c, later).is_err(), "canceled -> {later}");
    }
    assert!(set(&c, "finished").is_err(), "an unknown state");
    let e = c
        .execute("UPDATE runs SET revision = 'r2' WHERE seq = 1", [])
        .unwrap_err()
        .to_string();
    assert!(e.contains("keeps its ticket and revision"), "{e}");
}

#[test]
fn a_ticket_has_at_most_one_live_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.db");
    let c = store::open(&path).unwrap();
    let add = |c: &rusqlite::Connection| {
        c.execute(
            "INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'r1', 'queued')",
            [],
        )
    };
    add(&c).unwrap();
    assert!(add(&c).is_err());
    set(&c, "done").unwrap();
    add(&c).unwrap();
    // Reopening keeps the schema and the rows.
    drop(c);
    let c = store::open(&path).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-dispatcher`
Expected: FAIL to compile: unresolved import `kod_dispatcher` (the crate has no library yet).

- [ ] **Step 3: Implement the config and the store**

Create `crates/kod-dispatcher/src/config.rs`:

````rust
//! The dispatcher's own config, checked whole at load. The owner and backup
//! are here, not only in the policy's `people.json`, so their kill switch
//! works when the policy doesn't load (policy plan D14).
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The repo this tracker's tickets delegate to, lower-case `owner/name`.
    pub repo: String,
    /// GitHub logins, lower-cased.
    pub owner: String,
    pub backup: Option<String>,
}

const KEYS: [&str; 3] = ["repo", "owner", "backup"];

fn login(v: &Value, key: &str) -> Result<String> {
    match v.as_str() {
        Some(s)
            if !s.is_empty()
                && !s.starts_with('-')
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') =>
        {
            Ok(s.to_ascii_lowercase())
        }
        _ => bail!("{key} must be a GitHub login, got {v}"),
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let v: Value = serde_json::from_str(text).context("the dispatcher config is not JSON")?;
        let obj = v
            .as_object()
            .context("the dispatcher config must be an object")?;
        if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            bail!("unknown key {k:?}");
        }
        if let Some(k) = KEYS.iter().find(|k| !obj.contains_key(**k)) {
            bail!("missing key {k:?}");
        }
        let repo = obj["repo"].as_str().unwrap_or_default();
        let valid = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c))
        };
        if !repo
            .split_once('/')
            .is_some_and(|(o, n)| valid(o) && valid(n))
        {
            bail!("repo must be lower-case owner/name, got {}", obj["repo"]);
        }
        let owner = login(&obj["owner"], "owner")?;
        let backup = match &obj["backup"] {
            Value::Null => None,
            b => Some(login(b, "backup")?),
        };
        if backup.as_ref() == Some(&owner) {
            bail!("backup must be someone other than the owner");
        }
        Ok(Config {
            repo: repo.to_owned(),
            owner,
            backup,
        })
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Config::parse(&text).with_context(|| path.display().to_string())
    }

    /// The owner or backup, whose kill switch never waits on the policy.
    pub fn may_always_stop(&self, login: &str) -> bool {
        let login = login.trim().to_ascii_lowercase();
        login == self.owner || self.backup.as_ref() == Some(&login)
    }
}
````

Create `crates/kod-dispatcher/src/store.rs`:

````rust
//! The dispatcher's SQLite store: the delivery queue, the runs, and the kill
//! switch. The database itself keeps run states moving forward and allows
//! one live run per ticket.
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::{path::Path, time::Duration};

/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS deliveries (
    id TEXT PRIMARY KEY,
    event TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS runs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket TEXT NOT NULL,
    revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'publishing',
        'done', 'blocked', 'needs_human', 'over_budget', 'canceled')),
    reason TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX IF NOT EXISTS one_live_run ON runs (ticket)
    WHERE state IN ('queued', 'running', 'publishing');
CREATE TRIGGER IF NOT EXISTS forward_only BEFORE UPDATE OF state ON runs
WHEN (CASE NEW.state WHEN 'queued' THEN 0 WHEN 'running' THEN 1
        WHEN 'publishing' THEN 2 ELSE 3 END)
  <= (CASE OLD.state WHEN 'queued' THEN 0 WHEN 'running' THEN 1
        WHEN 'publishing' THEN 2 ELSE 3 END)
BEGIN SELECT RAISE(ABORT, 'run states only move forward'); END;
CREATE TRIGGER IF NOT EXISTS run_key_is_fixed BEFORE UPDATE OF ticket, revision ON runs
BEGIN SELECT RAISE(ABORT, 'a run keeps its ticket and revision'); END;
CREATE TABLE IF NOT EXISTS control (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    halted INTEGER NOT NULL,
    by TEXT NOT NULL
);
";

/// SQL for the states of a run that has not ended.
pub const LIVE: &str = "('queued', 'running', 'publishing')";

/// Opens (creating if needed) the store at `path`.
pub fn open(path: &Path) -> Result<Connection> {
    let c = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    c.pragma_update(None, "synchronous", "FULL")?;
    c.execute_batch(SCHEMA)
        .with_context(|| format!("creating the schema in {}", path.display()))?;
    Ok(c)
}
````

Create `crates/kod-dispatcher/src/lib.rs`:

````rust
//! Kod's dispatcher: tracker webhooks in, runs out. A verified delivery is
//! queued in a SQLite table, admitted or refused (with the reason commented
//! on the ticket), and started through the tool gateway. Runs move forward
//! only; a label removal, a ticket edit or the kill switch cancels them.
pub mod config;
pub mod store;

pub use config::Config;
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-dispatcher && cargo clippy -p kod-dispatcher --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (config 3, store 2), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kod-dispatcher
git commit -m "kod-dispatcher: config and a store that keeps runs moving forward"
```

### Task 3: Admission, runs and the webhook route

**Files:**
- Modify: `crates/kod-dispatcher/Cargo.toml` (replace), `crates/kod-dispatcher/src/lib.rs` (replace)
- Test: `crates/kod-dispatcher/tests/common/mod.rs`, `tests/admission.rs`

**Interfaces:**
- Consumes: Task 1's events; Task 2's `Config`, `store::{open, LIVE}`; `kod_policy::Policy::{load, decide}` with the policy plan's `delegate` input (`ticket.{repo, risk_class, acceptance_criteria, bounces}`, `wip_in_progress`); `kod_audit::AuditLog::{open, record_decision}`; `kod_tracker::{Tracker, Issue, State, TaskEvent, Webhook}`.
- Produces: `RunState` (`ALL`, `name`), `RunRow { id, ticket, revision, state, reason }`, `RunStart { id, ticket, revision, repo }`, `trait Gateway: Send + Sync { start(&RunStart), stop(&str), stop_all(), publish(&str) -> Result<String> }`, `Received { Queued, Duplicate, Ignored, Refused(String) }`, `run_id(i64) -> String`, `revision(&Issue) -> String`, `Dispatcher::{open(Config, &Path, anyhow::Result<Arc<Policy>>, Arc<AuditLog>, Arc<dyn Tracker>, Arc<dyn Gateway>) -> Result<Dispatcher>, receive(&Webhook) -> Result<Received>, work_once() -> Result<bool>, finished(&str, RunState, &str) -> Result<()>, is_halted() -> Result<bool>, runs() -> Result<Vec<RunRow>>}`, `router(Arc<Dispatcher>) -> axum::Router` (202 queued, 200 duplicate or ignored, 401 refused, 500 on a store error). Test helpers: `FakeGateway`, `harness()`, `harness_with(..)`, `label`, `unlabel`, `edited`, `Harness::{deliver, running, queued, states, workflow, last_comment}`.

- [ ] **Step 1: Write the failing tests**

Replace `crates/kod-dispatcher/Cargo.toml`:

````toml
[package]
name = "kod-dispatcher"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
axum = "0.8"
kod-audit = { path = "../kod-audit" }
kod-policy = { path = "../kod-policy" }
kod-tracker = { path = "../kod-tracker" }
rusqlite = { version = "0.40", features = ["bundled"] }
serde_json = "1"
sha2 = "0.10"
tokio = { version = "1", features = ["rt-multi-thread"] }

[dev-dependencies]
rusqlite = "0.40"
tempfile = "3"
tower = { version = "0.5", features = ["util"] }
````

Create `crates/kod-dispatcher/tests/common/mod.rs`:

````rust
#![allow(dead_code)]
use anyhow::{Result, bail};
use kod_audit::AuditLog;
use kod_dispatcher::{Config, Dispatcher, Gateway, RunStart};
use kod_policy::Policy;
use kod_tracker::{
    Tracker, Webhook,
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

/// Records what the dispatcher asked of the gateway.
#[derive(Default)]
pub struct FakeGateway {
    pub calls: Mutex<Vec<String>>,
    pub fail_start: AtomicBool,
    pub fail_publish: AtomicBool,
    pub fail_stop_all: AtomicBool,
}

impl FakeGateway {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
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

    fn stop(&self, run_id: &str) -> Result<()> {
        self.record(format!("stop {run_id}"), &AtomicBool::new(false))
    }

    fn stop_all(&self) -> Result<()> {
        self.record("stop_all".into(), &self.fail_stop_all)
    }

    fn publish(&self, run_id: &str) -> Result<String> {
        self.record(format!("publish {run_id}"), &self.fail_publish)?;
        Ok("https://github.example/acme/app/pull/1".into())
    }
}

pub struct Harness {
    pub d: Arc<Dispatcher>,
    pub tracker: Arc<FakeTracker>,
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
    tracker: Arc<FakeTracker>,
    gw: Arc<FakeGateway>,
    policy: Result<Policy>,
) -> Arc<Dispatcher> {
    let audit = Arc::new(AuditLog::open(&dir.join("audit.db")).unwrap());
    Arc::new(
        Dispatcher::open(
            Config::parse(CONFIG).unwrap(),
            &dir.join("dispatcher.db"),
            policy.map(Arc::new),
            audit,
            tracker,
            gw,
        )
        .unwrap(),
    )
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
    let d = dispatcher(dir.path(), tracker.clone(), gw.clone(), policy(dir.path()));
    Harness {
        d,
        tracker,
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
        let c = kod_dispatcher::store::open(&self.db).unwrap();
        c.execute(
            "INSERT INTO runs (ticket, revision, state) VALUES (?1, 'r', 'queued')",
            [ticket],
        )
        .unwrap();
    }
}

/// Keeps `Tracker` in scope for tests that call it on the fake.
pub fn issue(t: &FakeTracker, key: &str) -> kod_tracker::Issue {
    t.get_issue(key).unwrap()
}
````

Create `crates/kod-dispatcher/tests/admission.rs`:

````rust
mod common;

use axum::{body::Body, http::Request};
use common::*;
use kod_dispatcher::{Received, RunState, router};
use kod_policy::Policy;
use std::sync::{Arc, atomic::Ordering};
use tower::ServiceExt;

#[test]
fn a_labelled_ticket_is_admitted_and_started() {
    let h = harness();
    h.running();
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    assert_eq!(h.workflow("FAKE-1"), "Doing");
    let run = &h.d.runs().unwrap()[0];
    assert_eq!(
        run.revision,
        kod_dispatcher::revision(&issue(&h.tracker, "FAKE-1"))
    );
    // The delegate decision is in the audit log.
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%\"delegate\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_webhook_that_is_not_genuine_is_refused_and_never_queued() {
    let h = harness();
    let mut w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    w.body = w
        .body
        .iter()
        .map(|b| if *b == b'1' { b'2' } else { *b })
        .collect();
    assert!(matches!(h.d.receive(&w).unwrap(), Received::Refused(_)));
    w.headers.clear();
    assert!(matches!(h.d.receive(&w).unwrap(), Received::Refused(_)));
    assert!(!h.d.work_once().unwrap());
    assert!(h.gw.calls().is_empty());
}

#[test]
fn a_duplicate_delivery_never_starts_a_second_run() {
    let h = harness();
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
    // The adapter refuses a replay to the same process...
    assert!(matches!(h.d.receive(&w).unwrap(), Received::Refused(_)));
    while h.d.work_once().unwrap() {}
    // ...and after a restart, with a fresh adapter, the store does.
    let (t, _) = kod_tracker::fake::sandbox();
    let again = dispatcher(
        h.dir.path(),
        Arc::new(t),
        h.gw.clone(),
        Policy::load(&policy_dir()),
    );
    assert_eq!(again.receive(&w).unwrap(), Received::Duplicate);
    while again.work_once().unwrap() {}
    assert_eq!(h.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    assert_eq!(h.states().len(), 1);
}

#[test]
fn admission_refusals_are_commented_with_the_reason() {
    let h = harness();
    h.deliver(&label(&h.tracker, "d-1", "FAKE-1", "mallory"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("mallory is not on the team")
    );
    h.deliver(&label(&h.tracker, "d-2", "FAKE-3", "dev"));
    let c = h.last_comment("FAKE-3");
    for why in [
        "no acceptance criteria",
        "no requirement ID",
        "links no approved document",
        "no risk class",
    ] {
        assert!(c.contains(why), "{c}");
    }
    assert!(h.states().is_empty());
    assert!(h.gw.calls().is_empty());
}

#[test]
fn wip_is_counted_and_reserved_in_the_admission() {
    let h = harness();
    h.running();
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert!(
        h.last_comment("FAKE-2").contains("WIP limit reached (1)"),
        "{}",
        h.last_comment("FAKE-2")
    );
    // A second label on a ticket with a live run is refused too.
    h.deliver(&label(&h.tracker, "d-3", "FAKE-1", "dev"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("already has a live run, run-1")
    );
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    // When run-1 ends, FAKE-2 is admitted.
    h.d.finished("run-1", RunState::NeedsHuman, "stuck")
        .unwrap();
    h.deliver(&label(&h.tracker, "d-4", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn a_policy_that_failed_to_load_refuses_every_admission() {
    let h = harness_with(|_| {
        Err(anyhow::anyhow!(
            "delegation.json: wip_limit must be a number"
        ))
    });
    h.deliver(&label(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("the policy is unavailable: delegation.json")
    );
    assert!(h.states().is_empty());
}

#[test]
fn a_finished_run_is_published_then_done() {
    let h = harness();
    h.running();
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.gw.calls()[1], "publish run-1");
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert_eq!(h.workflow("FAKE-1"), "Review");
    assert!(
        h.last_comment("FAKE-1")
            .contains("published https://github.example")
    );
    // Ended is final: no second finish, no second publish.
    assert!(h.d.finished("run-1", RunState::Done, "again").is_err());
    assert!(h.d.finished("run-1", RunState::Blocked, "again").is_err());
    assert_eq!(h.gw.calls().len(), 2);
}

#[test]
fn a_failed_publish_or_start_ends_the_run_and_says_why() {
    let h = harness();
    h.running();
    h.gw.fail_publish.store(true, Ordering::SeqCst);
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "blocked")]);
    assert_eq!(h.workflow("FAKE-1"), "Waiting");
    assert!(h.last_comment("FAKE-1").contains("publish failed"));
    h.gw.fail_start.store(true, Ordering::SeqCst);
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "needs_human"));
    assert!(h.last_comment("FAKE-2").contains("the run did not start"));
}

#[test]
fn finished_takes_only_an_ending_for_a_running_run() {
    let h = harness();
    h.running();
    for state in [RunState::Queued, RunState::Running, RunState::Canceled] {
        assert!(h.d.finished("run-1", state, "x").is_err());
    }
    assert!(h.d.finished("run-9", RunState::Done, "x").is_err());
    assert!(h.d.finished("FAKE-1", RunState::Done, "x").is_err());
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
}

#[test]
fn the_webhook_route_answers_by_outcome() {
    let h = harness();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let post = |w: &kod_tracker::Webhook| {
        let mut r = Request::post("/webhook");
        for (k, v) in &w.headers {
            r = r.header(k, v);
        }
        let req = r.body(Body::from(w.body.clone())).unwrap();
        rt.block_on(router(h.d.clone()).oneshot(req))
            .unwrap()
            .status()
            .as_u16()
    };
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    assert_eq!(post(&w), 202);
    assert_eq!(post(&w), 401, "a replay");
    let mut forged = label(&h.tracker, "d-2", "FAKE-1", "dev");
    forged.headers[0].1 = "0".repeat(64);
    assert_eq!(post(&forged), 401);
    let other = h.tracker.webhook(&serde_json::json!({"delivery": "d-3", "type": "label_added", "issue": "FAKE-1", "label": "frontend", "user": "dev"}));
    assert_eq!(post(&other), 200);
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-dispatcher --test admission`
Expected: FAIL to compile: unresolved imports `kod_dispatcher::Dispatcher`, `kod_dispatcher::Gateway`, `kod_dispatcher::RunStart`.

- [ ] **Step 3: Implement admission, runs and the route**

Replace `crates/kod-dispatcher/src/lib.rs`:

````rust
//! Kod's dispatcher: tracker webhooks in, runs out. A verified delivery is
//! queued in a SQLite table, admitted or refused (with the reason commented
//! on the ticket), and started through the tool gateway. Runs move forward
//! only; a label removal, a ticket edit or the kill switch cancels them.
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Bytes,
    extract::State as Shared,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use kod_audit::AuditLog;
use kod_policy::Policy;
use kod_tracker::{Issue, State, TaskEvent, Tracker, Webhook};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};
use store::LIVE;

pub mod config;
pub mod store;

pub use config::Config;

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

/// The tool gateway as the dispatcher sees it. `stop` and `stop_all` must be
/// safe to repeat; after `stop`, the gateway refuses the run's publish.
pub trait Gateway: Send + Sync {
    fn start(&self, run: &RunStart) -> Result<()>;
    fn stop(&self, run_id: &str) -> Result<()>;
    /// The kill switch: stop every sandbox and revoke every agent identity.
    fn stop_all(&self) -> Result<()>;
    /// Publishes the run's change; returns the draft PR's URL.
    fn publish(&self, run_id: &str) -> Result<String>;
}

/// What `receive` did with a webhook.
#[derive(Clone, Debug, PartialEq)]
pub enum Received {
    Queued,
    /// The delivery ID was queued before.
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
    Sha256::digest(format!("{fields:?}").as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn encode(event: &TaskEvent) -> Option<Value> {
    match event {
        TaskEvent::Labeled { issue, by } => {
            Some(json!({"type": "labeled", "issue": issue, "by": by}))
        }
        TaskEvent::Unlabeled { issue, by } => {
            Some(json!({"type": "unlabeled", "issue": issue, "by": by}))
        }
        TaskEvent::Edited { issue } => Some(json!({"type": "edited", "issue": issue})),
        // Comments are instructions for a live run: sub-plan 8.
        TaskEvent::Commented { .. } => None,
    }
}

fn decode(v: &Value) -> Result<TaskEvent> {
    let text = |k: &str| {
        v[k].as_str()
            .map(str::to_owned)
            .with_context(|| format!("queued event has no {k}"))
    };
    Ok(match v["type"].as_str() {
        Some("labeled") => TaskEvent::Labeled {
            issue: text("issue")?,
            by: text("by")?,
        },
        Some("unlabeled") => TaskEvent::Unlabeled {
            issue: text("issue")?,
            by: text("by")?,
        },
        Some("edited") => TaskEvent::Edited {
            issue: text("issue")?,
        },
        _ => bail!("unknown queued event {v}"),
    })
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
}

impl Dispatcher {
    pub fn open(
        config: Config,
        db: &Path,
        policy: Result<Arc<Policy>>,
        audit: Arc<AuditLog>,
        tracker: Arc<dyn Tracker>,
        gateway: Arc<dyn Gateway>,
    ) -> Result<Dispatcher> {
        Ok(Dispatcher {
            config,
            policy: policy.map_err(|e| format!("{e:#}")),
            audit,
            tracker,
            gateway,
            db: Mutex::new(store::open(db)?),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Checks the webhook and queues its event. Errors are store errors only.
    pub fn receive(&self, webhook: &Webhook) -> Result<Received> {
        let delivery = match self.tracker.parse_webhook(webhook) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(Received::Ignored),
            Err(e) => return Ok(Received::Refused(format!("{e:#}"))),
        };
        let Some(event) = encode(&delivery.event) else {
            return Ok(Received::Ignored);
        };
        let added = self.db().execute(
            "INSERT OR IGNORE INTO deliveries (id, event) VALUES (?1, ?2)",
            params![delivery.id, event.to_string()],
        )?;
        Ok(if added == 1 {
            Received::Queued
        } else {
            Received::Duplicate
        })
    }

    /// Handles the oldest queued delivery, then starts queued runs. Returns
    /// whether there was a delivery. A delivery whose handling fails stays
    /// queued and is tried again on the next call.
    // ponytail: one worker and endless retries; add an attempt count if a
    // delivery ever keeps failing.
    pub fn work_once(&self) -> Result<bool> {
        let next: Option<(String, String)> = self
            .db()
            .query_row(
                "SELECT id, event FROM deliveries WHERE done = 0 ORDER BY rowid LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, event)) = &next {
            match decode(&serde_json::from_str(event)?)? {
                TaskEvent::Labeled { issue, by } => self.admit(id, &issue, &by)?,
                _ => self.done(id)?,
            }
        }
        self.start_queued()?;
        Ok(next.is_some())
    }

    fn done(&self, delivery: &str) -> Result<()> {
        self.db()
            .execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        Ok(())
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
        if Self::halted(&tx)? {
            reasons.push(
                "Kod is stopped by the kill switch until the owner or backup resumes it".to_owned(),
            );
        }
        let live: Option<i64> = tx
            .query_row(
                &format!("SELECT seq FROM runs WHERE ticket = ?1 AND state IN {LIVE}"),
                [ticket],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(seq) = live {
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
                    reasons.push(format!("the decision could not be recorded: {e:#}"));
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
        }
        tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        tx.commit()?;
        drop(c);
        if reasons.is_empty() {
            self.tracker.transition(ticket, State::InProgress)
        } else {
            self.tracker.comment(
                ticket,
                &format!("Kod did not start this ticket: {}.", reasons.join("; ")),
            )
        }
    }

    fn start_queued(&self) -> Result<()> {
        let queued: Vec<RunStart> = {
            let c = self.db();
            if Self::halted(&c)? {
                return Ok(());
            }
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
            match self.gateway.start(&run) {
                Ok(()) => {
                    let moved = self.db().execute(
                        "UPDATE runs SET state = 'running' WHERE seq = ?1 AND state = 'queued'",
                        [seq],
                    )?;
                    if moved == 0 {
                        // Canceled while starting.
                        self.gateway.stop(&run.id)?;
                    }
                }
                Err(e) => {
                    self.end(
                        seq,
                        RunState::NeedsHuman,
                        &format!("the run did not start: {e:#}"),
                        &[RunState::Queued],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Ends the run if it is in one of `from`, then moves the ticket and
    /// comments. Returns whether the run ended here.
    fn end(&self, seq: i64, to: RunState, reason: &str, from: &[RunState]) -> Result<bool> {
        let from: Vec<String> = from.iter().map(|s| format!("'{}'", s.name())).collect();
        let ticket: Option<String> = self
            .db()
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
        self.tracker.transition(&ticket, state)?;
        self.tracker.comment(
            &ticket,
            &format!("Kod {} ended {}: {reason}.", run_id(seq), to.name()),
        )?;
        Ok(true)
    }

    /// The runner reports how a running run ended. `Done` publishes through
    /// the gateway first; a canceled run is never published.
    pub fn finished(&self, run: &str, outcome: RunState, reason: &str) -> Result<()> {
        let seq = seq_of(run)?;
        match outcome {
            RunState::Done => {
                let moved = self.db().execute(
                    "UPDATE runs SET state = 'publishing' WHERE seq = ?1 AND state = 'running'",
                    [seq],
                )?;
                if moved == 0 {
                    bail!("{run} is not running");
                }
                match self.gateway.publish(run) {
                    Ok(pr) => self.end(
                        seq,
                        RunState::Done,
                        &format!("{reason}; published {pr}"),
                        &[RunState::Publishing],
                    )?,
                    Err(e) => self.end(
                        seq,
                        RunState::Blocked,
                        &format!("publish failed: {e:#}"),
                        &[RunState::Publishing],
                    )?,
                };
                Ok(())
            }
            RunState::Blocked | RunState::NeedsHuman | RunState::OverBudget => {
                if !self.end(seq, outcome, reason, &[RunState::Running])? {
                    bail!("{run} is not running");
                }
                Ok(())
            }
            _ => bail!("{} is not how a run ends", outcome.name()),
        }
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

/// The webhook route: `POST /webhook`. 202 queued, 200 duplicate or
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
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-dispatcher && cargo clippy -p kod-dispatcher --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (admission 10, config 3, store 2), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-dispatcher
git commit -m "kod-dispatcher: verified deliveries, atomic admission and run endings"
```

### Task 4: Cancel, the kill switch and resume

**Files:**
- Modify: `crates/kod-dispatcher/src/lib.rs`
- Test: `crates/kod-dispatcher/tests/cancel.rs`

**Interfaces:**
- Consumes: Task 3's `Dispatcher`, `Gateway::{stop, stop_all}`, the harness; `Config::may_always_stop`; the policy plan's `cancel` input (`scope` `run` or `all`, `actor` `{kind: "human", login}` or `{kind: "label_removal"}`, `run.id`, `target.run`) and `resume` input (`scope: "all"`, `actor: {kind: "human", login}`); `kod_audit::{AuditLog::append, RUN_AUDIT}`.
- Produces: `Dispatcher::{kill(&str) -> Result<()>, resume(&str) -> Result<()>}`; `Unlabeled` and `Edited` deliveries handled by `work_once`.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-dispatcher/tests/cancel.rs`:

````rust
mod common;

use common::*;
use kod_dispatcher::RunState;
use kod_policy::Policy;
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn removing_the_label_cancels_the_live_run() {
    let h = harness();
    h.running();
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(
        h.last_comment("FAKE-1")
            .contains("the trigger label was removed by dev")
    );
    // A canceled run is never published and never moves again.
    assert!(h.d.finished("run-1", RunState::Done, "tests pass").is_err());
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("publish")));
}

#[test]
fn removing_the_label_cancels_a_queued_run_before_it_starts() {
    let h = harness();
    h.queued("FAKE-1");
    h.deliver(&unlabel(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("start")));
}

#[test]
fn an_edit_that_changes_the_ticket_stops_its_run() {
    let h = harness();
    h.running();
    // A change the run made itself (state, links) is not an edit of its inputs.
    h.tracker.add_issue("FAKE-1", {
        let mut i = h.tracker.issue("FAKE-1").unwrap();
        i.links.push("FAKE-9".into());
        i
    });
    h.deliver(&edited(&h.tracker, "d-2", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    h.tracker.add_issue("FAKE-1", {
        let mut i = h.tracker.issue("FAKE-1").unwrap();
        i.acceptance_criteria
            .push("And a third approver for refunds over 1000".into());
        i
    });
    h.deliver(&edited(&h.tracker, "d-3", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(
        h.last_comment("FAKE-1")
            .contains("back in ready at the new revision")
    );
    // Labelled again, it runs at the new revision.
    h.deliver(&label(&h.tracker, "d-4", "FAKE-1", "dev"));
    let runs = h.d.runs().unwrap();
    assert_eq!(runs[1].state, RunState::Running);
    assert_ne!(runs[0].revision, runs[1].revision);
}

#[test]
fn the_kill_switch_stops_queued_and_running_runs_and_holds() {
    let h = harness();
    h.running();
    h.queued("FAKE-2");
    h.d.kill("kiennt23").unwrap();
    assert_eq!(
        h.states(),
        [
            ("run-1".to_owned(), "canceled"),
            ("run-2".to_owned(), "canceled")
        ]
    );
    assert_eq!(h.gw.calls().last().unwrap(), "stop_all");
    assert!(
        h.last_comment("FAKE-2")
            .contains("the kill switch, by kiennt23")
    );
    // Held across a restart: nothing new is admitted.
    let again = dispatcher(
        h.dir.path(),
        h.tracker.clone(),
        h.gw.clone(),
        Policy::load(&policy_dir()),
    );
    assert!(again.is_halted().unwrap());
    again
        .receive(&label(&h.tracker, "d-2", "FAKE-1", "dev"))
        .unwrap();
    while again.work_once().unwrap() {}
    assert!(
        h.last_comment("FAKE-1")
            .contains("stopped by the kill switch")
    );
    assert_eq!(h.states().len(), 2);
}

#[test]
fn the_owners_kill_switch_works_when_the_policy_fails_to_load() {
    let h = harness_with(|_| Err(anyhow::anyhow!("people.json: owner must be a login")));
    h.queued("FAKE-1");
    h.d.kill(" KienNT23 ").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls(), ["stop_all"]);
    // Nobody else's does, and resume needs the policy.
    assert!(h.d.kill("lead").is_err());
    let e = h.d.resume("kiennt23").unwrap_err().to_string();
    assert!(e.contains("resume needs the policy"), "{e}");
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let e = format!("{:#}", h.d.kill("kiennt23").unwrap_err());
    assert!(e.contains("did not confirm it stopped them"), "{e}");
    // The runs are canceled and new ones refused even so.
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn a_tech_lead_uses_the_kill_switch_through_the_policy_and_cannot_resume() {
    let h = harness_with(|dir| {
        Policy::load(&policy_with_people(
            dir,
            json!({"owner": "kiennt23", "backup": null, "tech_leads": ["Lead"]}),
        ))
    });
    let e = h.d.kill("mallory").unwrap_err().to_string();
    assert!(e.contains("mallory may not use the kill switch"), "{e}");
    assert!(!h.d.is_halted().unwrap());
    h.running();
    h.d.kill("lead").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.d.resume("lead").is_err());
    assert!(h.d.resume("mallory").is_err());
    assert!(h.d.is_halted().unwrap());
    h.d.resume("KIENNT23").unwrap();
    assert!(!h.d.is_halted().unwrap());
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn a_label_removal_while_the_policy_is_down_is_refused_and_says_so() {
    let h = harness_with(|_| Err(anyhow::anyhow!("routing.json is missing")));
    h.queued("FAKE-1");
    h.deliver(&unlabel(&h.tracker, "d-1", "FAKE-1", "dev"));
    // Not canceled: the worker went on to start it.
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    let c = h.last_comment("FAKE-1");
    assert!(c.contains("could not cancel run-1"), "{c}");
    assert!(c.contains("the kill switch"), "{c}");
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-dispatcher --test cancel`
Expected: FAIL to compile: `no method named kill found` and `no method named resume found`.

- [ ] **Step 3: Implement cancel, the kill switch and resume**

Apply to `crates/kod-dispatcher/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-dispatcher/src/lib.rs b/crates/kod-dispatcher/src/lib.rs
index eb6c29d..36208c4 100644
--- a/crates/kod-dispatcher/src/lib.rs
+++ b/crates/kod-dispatcher/src/lib.rs
@@ -2,7 +2,7 @@
 //! queued in a SQLite table, admitted or refused (with the reason commented
 //! on the ticket), and started through the tool gateway. Runs move forward
 //! only; a label removal, a ticket edit or the kill switch cancels them.
-use anyhow::{Context, Result, bail};
+use anyhow::{Context, Result, anyhow, bail};
 use axum::{
     Router,
     body::Bytes,
@@ -10,7 +10,7 @@ use axum::{
     http::{HeaderMap, StatusCode},
     routing::post,
 };
-use kod_audit::AuditLog;
+use kod_audit::{AuditLog, RUN_AUDIT};
 use kod_policy::Policy;
 use kod_tracker::{Issue, State, TaskEvent, Tracker, Webhook};
 use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
@@ -252,7 +252,9 @@ impl Dispatcher {
         if let Some((id, event)) = &next {
             match decode(&serde_json::from_str(event)?)? {
                 TaskEvent::Labeled { issue, by } => self.admit(id, &issue, &by)?,
-                _ => self.done(id)?,
+                TaskEvent::Unlabeled { issue, by } => self.unlabeled(id, &issue, &by)?,
+                TaskEvent::Edited { issue } => self.edited(id, &issue)?,
+                TaskEvent::Commented { .. } => self.done(id)?,
             }
         }
         self.start_queued()?;
@@ -275,6 +277,17 @@ impl Dispatcher {
         )
     }
 
+    fn live_run(&self, ticket: &str) -> Result<Option<(i64, String)>> {
+        Ok(self
+            .db()
+            .query_row(
+                &format!("SELECT seq, revision FROM runs WHERE ticket = ?1 AND state IN {LIVE}"),
+                [ticket],
+                |r| Ok((r.get(0)?, r.get(1)?)),
+            )
+            .optional()?)
+    }
+
     fn admit(&self, delivery: &str, ticket: &str, by: &str) -> Result<()> {
         let mut reasons = vec![];
         if !self.tracker.is_team_member(by)? {
@@ -480,6 +493,153 @@ impl Dispatcher {
         }
     }
 
+    fn cancel(&self, seq: i64, reason: &str) -> Result<()> {
+        let live = [RunState::Queued, RunState::Running, RunState::Publishing];
+        if self.end(seq, RunState::Canceled, reason, &live)? {
+            self.gateway.stop(&run_id(seq))?;
+        }
+        Ok(())
+    }
+
+    fn unlabeled(&self, delivery: &str, ticket: &str, by: &str) -> Result<()> {
+        self.done(delivery)?;
+        let Some((seq, _)) = self.live_run(ticket)? else {
+            return Ok(());
+        };
+        let run = run_id(seq);
+        let input = json!({
+            "action": "cancel",
+            "scope": "run",
+            "actor": {"kind": "label_removal"},
+            "run": {"id": run},
+            "target": {"run": run},
+        });
+        let refused = match &self.policy {
+            Err(e) => Some(format!("the policy is unavailable: {e}")),
+            Ok(policy) => {
+                let decision = policy.decide(&input);
+                match self.audit.record_decision(&input, &decision) {
+                    Err(e) => Some(format!("the decision could not be recorded: {e:#}")),
+                    Ok(_) if !decision.allow => Some(decision.reasons.join("; ")),
+                    Ok(_) => None,
+                }
+            }
+        };
+        match refused {
+            Some(why) => self.tracker.comment(
+                ticket,
+                &format!(
+                    "Kod could not cancel {run}: {why}. The owner or backup can use the kill switch."
+                ),
+            ),
+            None => self.cancel(seq, &format!("the trigger label was removed by {by}")),
+        }
+    }
+
+    fn edited(&self, delivery: &str, ticket: &str) -> Result<()> {
+        let Some((seq, old)) = self.live_run(ticket)? else {
+            return self.done(delivery);
+        };
+        let new = revision(&self.tracker.get_issue(ticket)?);
+        self.done(delivery)?;
+        if new == old {
+            return Ok(());
+        }
+        self.cancel(
+            seq,
+            &format!("the ticket changed (revision {old} to {new}); it is back in ready at the new revision"),
+        )
+    }
+
+    /// The kill switch: cancel every live run, stop every sandbox, and refuse
+    /// new runs until `resume`. The owner's or backup's works without the
+    /// policy or the audit log; anyone else's goes through both.
+    pub fn kill(&self, login: &str) -> Result<()> {
+        let by = login.trim().to_ascii_lowercase();
+        if self.config.may_always_stop(&by) {
+            // Recorded if the log takes it; the stop never waits on it.
+            let _ = self
+                .audit
+                .append(RUN_AUDIT, &json!({"kind": "kill_switch", "by": by}));
+        } else {
+            let policy = self.policy.as_ref().map_err(|e| {
+                anyhow!("{by} may not use the kill switch while the policy is unavailable: {e}")
+            })?;
+            let input = json!({
+                "action": "cancel",
+                "scope": "all",
+                "actor": {"kind": "human", "login": by},
+            });
+            let decision = policy.decide(&input);
+            self.audit
+                .record_decision(&input, &decision)
+                .context("the kill switch decision could not be recorded")?;
+            if !decision.allow {
+                bail!(
+                    "{by} may not use the kill switch: {}",
+                    decision.reasons.join("; ")
+                );
+            }
+        }
+        let ended: Vec<(i64, String)> = {
+            let mut c = self.db();
+            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
+            tx.execute(
+                "INSERT OR REPLACE INTO control (id, halted, by) VALUES (1, 1, ?1)",
+                [&by],
+            )?;
+            let ended = tx
+                .prepare(&format!(
+                    "UPDATE runs SET state = 'canceled', reason = ?1 WHERE state IN {LIVE} RETURNING seq, ticket"
+                ))?
+                .query_map([format!("the kill switch, by {by}")], |r| {
+                    Ok((r.get(0)?, r.get(1)?))
+                })?
+                .collect::<rusqlite::Result<_>>()?;
+            tx.commit()?;
+            ended
+        };
+        let stopped = self.gateway.stop_all();
+        for (seq, ticket) in ended {
+            // Best effort: the tracker being down must not undo the stop.
+            let _ = self.tracker.transition(&ticket, State::Ready);
+            let _ = self.tracker.comment(
+                &ticket,
+                &format!(
+                    "Kod {} ended canceled: the kill switch, by {by}.",
+                    run_id(seq)
+                ),
+            );
+        }
+        stopped.context("the runs are canceled, but the gateway did not confirm it stopped them")
+    }
+
+    /// Lifts the kill switch. Owner or backup only, through the policy.
+    pub fn resume(&self, login: &str) -> Result<()> {
+        let by = login.trim().to_ascii_lowercase();
+        let policy = self
+            .policy
+            .as_ref()
+            .map_err(|e| anyhow!("resume needs the policy, which is unavailable: {e}"))?;
+        let input = json!({
+            "action": "resume",
+            "scope": "all",
+            "actor": {"kind": "human", "login": by},
+        });
+        let decision = policy.decide(&input);
+        self.audit
+            .record_decision(&input, &decision)
+            .context("the resume decision could not be recorded")?;
+        if !decision.allow {
+            bail!("{by} may not resume: {}", decision.reasons.join("; "));
+        }
+        self.db().execute(
+            "INSERT OR REPLACE INTO control (id, halted, by) VALUES (1, 0, ?1)",
+            [&by],
+        )?;
+        Ok(())
+    }
+
     pub fn is_halted(&self) -> Result<bool> {
         Self::halted(&self.db())
     }
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-dispatcher cancel 8, admission 10, config 3, store 2; everything else unchanged), clean. `the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail` takes about 2 s (the audit log's busy timeout).

- [ ] **Step 5: Commit**

```bash
git add crates/kod-dispatcher
git commit -m "kod-dispatcher: cancel on unlabel or edit, the kill switch and resume"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc's run identity and lifecycle, rework and staleness, kill switch and action matrix sections, policy plan D14 and this plan, with these questions:
- Can any sequence of deliveries, restarts and calls start two runs for one delivery, two live runs for one ticket, or more live runs than the WIP limit?
- Can a run's state move backward, or a canceled run reach `publishing`?
- Does the owner's or backup's kill switch depend on the policy, the audit log or the gateway anywhere? Does everyone else's depend on the policy?
- Is every refusal commented on the ticket with its reason, and every decision recorded before it takes effect?
- Do the policy inputs match the policy plan's `delegate`, `cancel` and `resume` contracts field for field?

## Self-review

- **Spec coverage.** Webhook service with a DB table as its queue: Tasks 2 and 3 (SQLite, D1). Signature check and delivery dedup: Task 3 (`a_webhook_that_is_not_genuine_is_refused_and_never_queued`, `a_duplicate_delivery_never_starts_a_second_run`). Atomic WIP admission with the placeholder of 1: Task 3 (`wip_is_counted_and_reserved_in_the_admission`). Forward-only states: Task 2 (`run_states_only_move_forward`) and Task 3 (`a_finished_run_is_published_then_done`). Cancel on label removal or ticket edit: Task 4. Kill switch and resume per D14, including the owner's kill switch when the policy fails: Task 4. Overview exit criteria against the fakes: refusals commented with the reason (`admission_refusals_are_commented_with_the_reason`), no second run from a duplicate, cancel or kill stops a queued or running run (`removing_the_label_cancels_the_live_run`, `removing_the_label_cancels_a_queued_run_before_it_starts`, `the_kill_switch_stops_queued_and_running_runs_and_holds`), states only move forward.
- **Placeholders.** None.
- **Type consistency.** `Dispatcher`, `Gateway`, `RunStart`, `RunRow`, `RunState`, `Received`, `Config` are spelled the same in every task; the code was compiled and tested in task order on top of sub-plans 3, 4, 6a, 2 and 5, and the plan text was replayed from scratch.
- **Review Focus.** Each of the five lines names its test and task above.

## Open items

- **Who may send the kill command, and how it proves it** (owner, 6b): `kill(login)` trusts the login its caller passes. Recommended: a CLI on the dispatcher host for the owner and backup, and a tracker comment or label from a tech lead mapped to a GitHub login through 6a's team mapping.
- **Naming the backup** (owner): the backup is `null` in `policy/people.json` and in the dispatcher config, so only the owner can resume today, and the owner is the only one whose kill switch works without the policy.
- **Re-running after an edit** (owner): an edit stops the run and returns the ticket to ready; a new label starts the next run (D8). Say if an edit should restart it automatically.
- **Bounces** (owner): counted as the ticket's runs that ended `blocked` until rework (sub-plan 7) counts rework requests.
