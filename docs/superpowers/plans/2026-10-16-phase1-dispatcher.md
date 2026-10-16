# Phase 1.6 Dispatcher (`kod-dispatcher`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development; *Native* tasks use superpowers:executing-plans. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `kod-dispatcher` crate that:
- takes tracker webhooks, checks their signature and dedups them by the signed body;
- queues them in a SQLite table with per-delivery retry, backoff and a dead letter, and lets cancels jump the queue;
- admits a ticket atomically against the WIP limit (placeholder 1) or comments why not;
- starts and ends runs through a gateway, with states that only move forward;
- holds the one durable cancel flag that the tool gateway checks;
- cancels a run on label removal or a ticket edit without depending on the policy, the audit log or the tracker;
- holds the kill switch and `resume` per the policy plan's D14, reachable only from an authenticated identity; `resume` only from a signed CLI request.

**Architecture:** A library. `Dispatcher` holds the dispatcher config (repo, owner, backup), the policy (or the error it failed to load with), the audit log, the tracker adapter, a `Gateway` trait object, the owner's and backup's CLI keys and one SQLite connection.
- **Intake and work.** `receive` verifies a webhook and stores it keyed by the SHA-256 of its signed body. `work_at(now)` handles the next due delivery: cancels first and never behind another delivery, a ticket's labels in order, a failure retried with backoff and dead-lettered after five attempts. It then drains the follow-up queue (revokes, then stops, which are never given up, then the tracker) and starts queued runs.
- **State before effects.** Every state change is committed in one transaction together with its delivery's `done` mark and the follow-ups it causes (stop the sandbox, move and comment the ticket). The follow-ups run after the commit and are retried on failure, stops first, so a tracker outage never holds up a stop.
- **Rules in the database.** A unique index enforces one live run per ticket and triggers enforce forward-only states.
- **Entry points.** `stopped(run)` is the tool gateway's cancel check. `finished` is how the 6b runner ends a run. Signed tracker comments (`/kod kill`, `/kod cancel`) and `signed_command` (an HMAC-signed CLI request) are the only ways to reach the kill switch and a human's cancel; `resume` is reached only through `signed_command`. `router` serves `POST /webhook` and `POST /command`.

Tests use the 6a fake tracker (wrapped to inject faults) and a fake gateway that records starts, stops and publish calls.

**Tech Stack:** Rust 1.99.0 (edition 2024). No new crates: rusqlite 0.40 (bundled, as kod-audit), axum 0.8, tokio, serde_json, sha2 0.10, anyhow; tower 0.5 in tests. Crates from this phase: kod-policy, kod-audit (PR #14 / `feat/phase1-audit`), kod-tracker (the 6a plan, extended here).

**Spec:** `docs/kod-harness-design.md` (the PR flow, Run identity and lifecycle, Rework and staleness, Ownership and the kill switch, the action matrix rows for admit, cancel and resume, Workflow status), row 6 of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/superpowers/plans/2026-10-15-phase1-policy.md` (D14 and the `delegate`, `cancel` and `resume` inputs), `docs/superpowers/plans/2026-10-15-phase1-audit.md`, `docs/superpowers/plans/2026-10-15-phase1-tracker-contract.md`, `docs/superpowers/plans/2026-10-16-phase1-tool-gateway.md` (the `stopped` check and host-only publish), `docs/adr/0001-per-task-execution-mode.md`.

## Decisions

| | Decision | Why |
|---|---|---|
| D1 | SQLite (rusqlite 0.40, bundled), one database file owned by the dispatcher, `synchronous = FULL`, a 2 s busy timeout. | The same engine and settings as kod-audit. The dispatcher is one process (the design), so a server database buys nothing in phase 1. Postgres is the upgrade if the dispatcher ever runs as more than one process. |
| D2 | Dedup is the dispatcher's alone. A delivery's key is the SHA-256 of its signed body. `receive` stores a verified delivery with `INSERT OR IGNORE`, and a stored key is a `Duplicate`, even after a restart. The tracker adapter keeps no record of IDs; the delivery ID it reports must come from the signed payload. | A replay can't be dressed up as new with an unsigned header. A failed store write never spends an ID, so the tracker's retry is queued. |
| D3 | Deliveries are retried per delivery: a failed handling sets `next_at` with exponential backoff (2 s, 4 s, ... 256 s), and after five attempts the delivery is dead (`done = 2`) and the ticket gets a comment saying so. Label removals, edits and commands go first and bypass older admissions, including those for the same ticket. A cancellation retires older pending labels for its ticket in the same transaction; admission rechecks that its delivery is pending before inserting a run. | A delivery that keeps failing can't block every ticket behind it. A cancel must not wait behind a failing admission, and retrying that older label must not restart canceled work. |
| D4 | A delivery is marked done in the same transaction as the state change it causes. External effects (revoke, stop, stop all, transition, comment) are durable follow-ups: revokes first, then stops, then tracker calls, retried with capped backoff. Tracker calls dead-letter after five attempts. Revoke and stop calls retry indefinitely; at attempt five a structured `stop_overdue` event goes to host stderr before the best-effort audit write, plus a comment when a run has a ticket. This alerts even for `stop_all`, which has no ticket, while audit is down. A new ticket transition supersedes older pending transitions. | An unconfirmed sandbox must not be forgotten. The alarm must not depend on the audit log, and delayed tracker updates must not undo a cancellation. |
| D5 | Admission does the WIP count, the live-run check, the kill-switch check, the `delegate` decision and the run insert in one `IMMEDIATE` transaction. The WIP count is every live run (`queued`, `running`, `publishing`). Bounces are the ticket's runs that ended `blocked`; a failed publish ends `needs_human`, so it isn't counted as one. | The design's atomic admission: two deliveries can't both take the last WIP slot. A publish failure is Kod's, not a bounce of the work. |
| D6 | The database enforces the run rules: a `CHECK` on the eight state names, a unique index allowing one live run per ticket, a trigger refusing any state move that isn't forward (`queued` 0, `running` 1, `publishing` 2, ended 3), and a trigger refusing a change to a run's ticket or revision. | Forward-only holds even against a bug in the code above it. |
| D7 | The run's row is the one durable cancel flag. `Dispatcher::stopped(run)` is true unless the run is `queued`, `running` or `publishing`; an unknown run is stopped. 6b passes it to the tool gateway as `Adapters::stopped`, which checks it on every call and inside publish. A cancel commits the row before any external call. | One source of truth that survives a crash and that the gateway reads at the moment it acts. |
| D8 | The revision of a ticket is the first 8 bytes of the SHA-256 of its title, body, acceptance criteria, requirement ID, risk class and approved document versions. Workflow state and links are left out. | The 6a tracker has no revision field; these are the run's inputs. The run itself moves the state and adds links, which must not look like an edit. |
| D9 | kod-tracker gains `TaskEvent::Unlabeled { issue, by }` and `TaskEvent::Edited { issue }`. The fake parses `label_removed` (trigger label only) and `issue_edited`, and drops its in-memory replay set. `contract::check` and its negative fakes cover the new events. | 6a's events cover labelling and comments only, and the overview's cancel needs these two. An adapter that drops them must fail the contract. |
| D10 | A label removal or an edit that changes the revision cancels the live run with no policy decision. The run row, the delivery mark and the follow-ups commit in one transaction, then follow-ups run with revokes and stops first, then the best-effort `cancel` audit record. An edit returns the ticket to ready at its new revision; a new run needs a new label. An edit that doesn't change the revision does nothing. | Stopping is always safe. A run's own cancel must not depend on the policy, the audit log or the tracker being up. |
| D11 | Humans reach the dispatcher only from an authenticated identity: a signed tracker comment whose first line is `/kod kill` or `/kod cancel` (the author is taken from the signed payload), or `signed_command`, a JSON request `{command, by, at}` HMAC-signed with that login's CLI key, at most 300 s old and used once. If replay storage fails, a verified owner or backup may still stop everything with a signed CLI kill; that request reports the storage error. Resume and other identities stay fail closed. `/kod resume` in a comment is refused with a comment saying resume is accepted only as a signed CLI request. A login string the caller passes in is never trusted. | The review's S13: stopping is safe from any authenticated channel; restarting needs the owner's or backup's own key, so it never rests on a tracker login matching a GitHub login. Emergency stopping must still work without the store. |
| D12 | The kill switch: the owner or backup named in the dispatcher's own config is honoured without the policy and without a working audit log (the audit record is best effort); anyone else goes through the policy's `cancel` `scope: "all"` rule with a recorded decision. One transaction sets `halted`, cancels every live run, captures the kill epoch and queues each run's revoke, stop and tracker follow-ups. Each run's credential revoke is attempted before the blocking `Gateway::stop_all`; failures remain queued and do not suppress shutdown. `stop_all` is always attempted even if the transaction failed. A failed global stop is retried with the captured epoch; resume advances it, invalidating old retries. Global stops and resume share a mutex in the single dispatcher instance, including the retry's epoch check and adapter call. The error names every part that didn't complete. | Policy plan D14: the owner's or backup's kill switch must not depend on the policy or audit log. Stopping must not depend on the store or on a successful revoke. A hung shutdown must not delay the initial revokes. |
| D13 | `resume` comes only from the owner or backup, as a signed CLI request, through the policy's `resume` rule with a recorded decision; it fails while the policy is unavailable. It also refuses promptly while any global stop is in flight; retry with a fresh signed request after the stop returns. While halted, admission refuses every ticket and queued runs aren't started. `/kod cancel` cancels the ticket's live run for the owner, the backup, or anyone the policy's `cancel` `scope: "run"` rule allows (a tech lead). | D14: restart only by the owner or backup. Restarting must not overlap a global stop that could reach new runs. The dispatcher remains one instance in one process (D1). |
| D14 | Run outcomes: `finished(run, outcome, reason)` takes an ended state for a `running` run. `Done` moves through `publishing`, calls `Gateway::publish(run_id)` and ends `done` (ticket to in review) or `needs_human` if publish fails. Other endings are recorded as given; `canceled` comes only from the cancel paths. A run whose start fails ends `needs_human`. Every ending queues a revoke (the run's token and forge credentials end at once) and a stop. A cancel that lands after the gateway's last check inside publish, so the draft PR opens anyway, is recorded in the audit log (`canceled_after_publish`) and said on the ticket with the PR: close it, do not merge it. An `IMMEDIATE` transaction checks halted and claims `queued` to `running` before the external start; only the worker that changed the row starts it, and a cancel during that start still queues a stop. | The design's lifecycle. Concurrent workers must not start a run twice or stop another worker's successful start. Publish is refused for a canceled run, because a canceled run can't reach `publishing`, and the gateway checks `stopped` again inside publish. |
| D15 | Crash recovery: on open, every run left in `publishing` is resolved with `Gateway::find_published` (the branch's PR, found by `agent/<ticket>-<run>`). It ends `done` if there is a PR and `needs_human` otherwise. Before calling publish, the same transaction that enters `publishing` queues a dormant `check_publish` follow-up. When publish returns, its ending transaction activates the check, carrying the PR URL if known; on open, all pending checks are activated. For a canceled run, the check uses the known URL or `find_published` after a lost response. Lookup errors retry with capped backoff without a dead-letter limit. The warning and check completion commit together, then the audit record is attempted. | A crash must not leave a live run holding WIP or lose the warning for a canceled draft PR. The dormant check cannot run before the in-process publish returns; restart treats that process as gone. |
| D16 | Ticket moves: admitted → in progress; done → in review; canceled → ready; blocked, needs_human, over_budget → blocked. Every ending is commented with the run ID, the state and the reason. | The 6a adapter's five states. |
| D17 | A library and a router, no binary. `Config` is `{repo, owner, backup}` JSON with exact keys, logins lower-cased and the backup different from the owner, checked at load. CLI keys are given with `with_cli_key(login, key)`. | 6b wires the process, the real adapters and the key storage. |
| D18 | If cancellation wins while `Gateway::start` is still registering a token, the post-start check queues revoke again before stop. Cleanup of a never-started run succeeds: every gateway adapter must pass `check_absent_run` with a dedicated unused ID, including repeated revoke and stop. 6b must run this shared contract against its real gateway; the fake and negative adapters exercise it here. | The first cancel can precede token registration. Queued runs may also be canceled before any sandbox exists; their cleanup must finish rather than retry forever. |

How 6b wires the runner and publish: the runner starts the sandbox for `Gateway::start`. When the agent loop exits, the kod-eval collector writes `collect_out/diff.patch` and the check results next to the sandbox, never inside it. The runner then calls `finished(run, outcome, reason)`. For `Done`, `finished` calls `Gateway::publish(run_id)`, which the tool gateway's `publish` implements by reading the change through its `Workspace` adapter from that collector output.

Out of scope here: the real tracker and gateway adapters and the process (6b); the remaining admission checks that need real documents (approved and current versions; 6b with the docs adapter); runs left `running` by a crash (6b, which owns the sandboxes); the bounce limit beyond counting `blocked` runs (rework is sub-plan 7).

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Unlabel and edit events in kod-tracker | Subagent + review | Changes what every tracker adapter must parse, and moves dedup out of the adapter; extends the contract and its negative fakes |
| 2. Config and the store | Subagent + review | Owns forward-only states, one live run per ticket, the cancel flag and config validation at load |
| 3. Admission, runs and the webhook route | Subagent + review | Owns signature, dedup, retry and dead letter, atomic WIP admission, crash recovery and refusal comments |
| 4. Cancel, the kill switch and resume | Subagent + review | Owns D14: the kill switch without policy, authenticated commands, cancels that never wait on the tracker |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged. No new crates.
- Fail closed:
  - an unverified webhook or CLI request is refused;
  - a policy that failed to load refuses admission, a tech lead's kill switch, cancel and resume;
  - a decision that can't be recorded refuses the action.
- The exceptions are stops: the owner's or backup's kill switch (D12) and a run's own cancel on label removal or edit (D10).
- Config is validated at load; an unknown, missing or malformed key is an error naming the file.
- No error text reaches a comment, a reply or the store without kod-audit's `redact`.
- No network: fakes only (the 6a fake tracker and a fake gateway).
- Code against kod-audit's plan API (`open`, `record_decision`, `append`, `RUN_AUDIT`, `redact::redact`) and kod-tracker's plan API.
- Commit messages are plain, with no trailers.

## Review Focus

- Issue #18: a token registered after cancellation is revoked (`cancellation_during_start_revokes_a_token_registered_after_cancel`, Task 4); a global-stop alarm reaches host stderr with audit writes rejected (`overdue_global_stop_alert_does_not_depend_on_audit`, Task 4); absent-run cleanup passes the shared gateway contract and rejects adapters that error on either operation (`gateway_contract`, Task 3).
- A blocked global stop must not delay revocation or allow resume to admit work it could stop: `kill_revokes_before_waiting_for_global_stop`, `a_kill_finishing_after_resume_does_not_stop_new_runs`, `resume_cannot_cross_an_inflight_stop_all_retry` (Task 4, `review_edges`).
- A canceled publication with a lost response must still be reconciled, including after a crash and more than five failed forge lookups: `a_canceled_publish_with_a_lost_response_still_marks_the_pr`, `a_canceled_publish_is_reconciled_after_a_crash_and_a_forge_outage` (Task 4, `review_edges`).
- A replay must never start a second run: after a restart (`a_duplicate_delivery_never_starts_a_second_run`), or under a new unsigned delivery ID (`a_replay_under_a_new_unsigned_delivery_id_is_a_duplicate`) (Task 3).
- A store write that failed must not spend the delivery: `a_delivery_whose_write_failed_is_queued_on_the_retry` (Task 3).
- One failing delivery must not block others, and must end as a dead letter said on the ticket: `a_failing_delivery_backs_off_without_holding_up_other_tickets` (Task 3); nor hold up a cancel for its own ticket: `a_cancel_does_not_wait_behind_a_failing_event_for_its_ticket` (Task 4).
- A cancel that lands after publish's last check must be recorded and said with the PR: `a_cancel_in_the_last_publish_window_marks_the_pr` (Task 3).
- Two workers must not start one queued run twice: `concurrent_workers_start_a_queued_run_once` (Task 3).
- A pending label must not hold up its ticket's cancel, and an old transition retried later must not undo a cancel: `pending_label_does_not_block_the_same_tickets_cancel`, `retrying_an_old_transition_does_not_undo_a_cancel` (Task 4).
- The owner's CLI kill must stop the sandboxes while the dispatcher's database is locked: `owner_kill_stops_sandboxes_while_dispatcher_db_is_locked` (Task 4).
- A run left `publishing` by a crash must be resolved on restart: `a_run_left_publishing_by_a_crash_is_finished_on_restart`, `a_run_left_publishing_with_no_pr_needs_a_human_on_restart` (Task 3).
- A tracker failure must never stop a cancel's stop, and the ticket must catch up: `a_tracker_failure_never_stops_the_stop` (Task 4).
- A stop must never be given up, the credential must go at once, and the alarm must be raised once: `a_stop_is_never_given_up_and_raises_the_alarm_once` (Task 4).
- A stop-all retry must not outlive a resume: `a_stop_all_retry_does_not_outlive_a_resume` (Task 4).
- A label removal must cancel with the policy and audit log down: `a_label_removal_needs_neither_the_policy_nor_the_audit_log` (Task 4).
- The owner's kill switch must work when the policy fails to load, when the audit log and gateway fail, and when the store fails: `the_owners_kill_switch_works_when_the_policy_fails_to_load`, `the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail`, `the_kill_switch_still_stops_the_sandboxes_when_the_store_fails` (Task 4).
- A forged, stale, replayed or unknown-key CLI request must be refused: `a_signed_cli_request_is_checked_before_it_acts`; a comment can kill but never resume: `a_comment_can_kill_but_never_resume` (Task 4).
- An adapter that drops the new events must fail the contract: `the_contract_fails_an_adapter_with_any_one_flaw` (Task 1).

## File Structure

```
Cargo.toml                                   modify: add crates/kod-dispatcher to members (Task 2)
crates/kod-tracker/
  src/lib.rs                                 modify: TaskEvent::{Unlabeled, Edited}, the Delivery ID rule (Task 1)
  src/fake.rs                                modify: parse label_removed and issue_edited, no replay set (Task 1)
  src/contract.rs                            modify: the new events (Task 1)
  tests/fake.rs                              modify (Task 1)
  tests/contract.rs                          modify: the new flaws (Task 1)
crates/kod-dispatcher/
  Cargo.toml                                 create (Task 2)
  src/config.rs                              create: Config, parse, load (Task 2)
  src/store.rs                               create: the schema, open, stopped (Task 2), modify (Task 3)
  src/lib.rs                                 create (Task 2), replace (Task 3), modify (Task 4)
  src/command.rs                             create: CLI request signatures (Task 4)
  tests/config.rs                            create (Task 2)
  tests/store.rs                             create (Task 2)
  tests/common/mod.rs                        create: FakeGateway, the faulty tracker and the harness (Task 3), modify (Task 4)
  tests/admission.rs                         create (Task 3)
  tests/gateway_contract.rs                  create (Task 3): never-started cleanup contract and negative adapters
  tests/cancel.rs                            create (Task 4)
  tests/review_edges.rs                      create (Task 4): shutdown ordering, concurrent resume and canceled-publish recovery
```

Execution order: this plan runs after sub-plans 3 (policy), 4 (audit), 6a (tracker), 2 (LLM gateway) and 5 (tool gateway). It doesn't use 5's code; its diffs apply on top of it.

---

### Task 1: Unlabel and edit events in kod-tracker

**Files:**
- Modify: `crates/kod-tracker/src/lib.rs`, `src/fake.rs`, `src/contract.rs`
- Test: `crates/kod-tracker/tests/fake.rs`, `tests/contract.rs`

**Interfaces:**
- Consumes: 6a's `TaskEvent`, `Delivery`, `FakeTracker::{webhook, parse_webhook}`, `FAKE_MAPPING` (trigger label `kod`), `contract::{check, Sandbox}`.
- Produces: `TaskEvent::Unlabeled { issue: String, by: String }` and `TaskEvent::Edited { issue: String }`; the fake maps `{"type": "label_removed", "label": "kod", "issue", "user"}` and `{"type": "issue_edited", "issue"}` to them and keeps no record of delivery IDs; `Sandbox` gains `untrigger`, `untrigger_event`, `other_unlabel`, `edited`, `edited_event`; `contract::check` requires the new events and that a redelivery parses the same.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-tracker/tests/fake.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/tests/fake.rs b/crates/kod-tracker/tests/fake.rs
index 91885ee..8bfd83a 100644
--- a/crates/kod-tracker/tests/fake.rs
+++ b/crates/kod-tracker/tests/fake.rs
@@ -89,16 +89,57 @@ fn the_signature_is_hmac_sha256() {
     assert_eq!(mac[..4], [0x60, 0xe4, 0x31, 0x59]);
 }
 
-/// A request that fails the signature check doesn't use up its delivery ID,
-/// so a forger can't block the real delivery; the ID is spent once the real
-/// one is accepted, and a signed request with no ID is refused.
+/// The fake keeps no delivery IDs: a redelivery parses again, for the
+/// dispatcher to refuse after its own write. A signed request with no ID is
+/// refused.
 #[test]
-fn delivery_ids_are_spent_only_by_signed_requests() {
+fn the_fake_spends_no_delivery_ids() {
     let (t, s) = sandbox();
     assert!(t.parse_webhook(&s.tampered).is_err());
-    assert!(t.parse_webhook(&s.trigger).unwrap().is_some());
-    let e = t.parse_webhook(&s.trigger).unwrap_err().to_string();
-    assert!(e.contains("already accepted"), "{e}");
+    assert_eq!(
+        t.parse_webhook(&s.trigger).unwrap(),
+        Some(s.trigger_event.clone())
+    );
+    assert_eq!(t.parse_webhook(&s.trigger).unwrap(), Some(s.trigger_event));
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

Apply to `crates/kod-tracker/tests/contract.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/tests/contract.rs b/crates/kod-tracker/tests/contract.rs
index 2ef0750..6f5b67b 100644
--- a/crates/kod-tracker/tests/contract.rs
+++ b/crates/kod-tracker/tests/contract.rs
@@ -1,11 +1,11 @@
 use anyhow::Result;
 use kod_tracker::{
-    Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook,
+    Delivery, Issue, Rework, ReworkKind, State, TaskEvent, Tracker, Webhook,
     contract::check,
     fake::{FakeTracker, SIGNATURE, sandbox},
 };
 use std::{
-    collections::HashMap,
+    collections::HashSet,
     panic::{AssertUnwindSafe, catch_unwind},
     sync::Mutex,
 };
@@ -29,8 +29,14 @@ enum Flaw {
     IgnoresSignatures,
     /// Accepts any request that has a signature header, whatever its value.
     ChecksPresenceOnly,
-    /// Keeps no delivery IDs, so a recorded webhook can be sent again.
-    AcceptsReplays,
+    /// Refuses a delivery ID it has seen, before the dispatcher stored it.
+    SpendsIds,
+    /// Reports the removal of any label, not only the trigger.
+    UnlabelsAnyLabel,
+    /// Reads no edit events.
+    IgnoresEdits,
+    /// Loses who removed the label.
+    DropsUnlabeledBy,
     EveryoneIsAMember,
     TransitionDoesNothing,
     MissingIssueIsOk,
@@ -39,8 +45,8 @@ enum Flaw {
 }
 
 /// The fake with one flaw, to show the contract catches it. `seen` holds
-/// what AcceptsReplays answered, by body.
-struct Broken(FakeTracker, Flaw, Mutex<HashMap<Vec<u8>, Option<Delivery>>>);
+/// the bodies SpendsIds accepted.
+struct Broken(FakeTracker, Flaw, Mutex<HashSet<Vec<u8>>>);
 
 impl Tracker for Broken {
     fn name(&self) -> &str {
@@ -89,18 +95,37 @@ impl Tracker for Broken {
             Flaw::ChecksPresenceOnly if request.header(SIGNATURE).is_some() => {
                 request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
             }
-            Flaw::AcceptsReplays => {
-                let mut seen = self.2.lock().unwrap();
-                if let Some(d) = seen.get(&request.body) {
-                    return Ok(d.clone());
+            Flaw::SpendsIds => {
+                if !self.2.lock().unwrap().insert(request.body.clone()) {
+                    anyhow::bail!("a replay");
+                }
+            }
+            Flaw::UnlabelsAnyLabel => {
+                let mut v: serde_json::Value = serde_json::from_slice(&request.body)?;
+                if v["type"] == "label_removed" {
+                    v["label"] = "kod".into();
+                    request = self.0.webhook(&v);
                 }
-                let d = self.0.parse_webhook(&request)?;
-                seen.insert(request.body.clone(), d.clone());
-                return Ok(d);
             }
             _ => {}
         }
-        self.0.parse_webhook(&request)
+        let d = self.0.parse_webhook(&request)?;
+        Ok(match (self.1, d) {
+            (
+                Flaw::IgnoresEdits,
+                Some(Delivery {
+                    event: TaskEvent::Edited { .. },
+                    ..
+                }),
+            ) => None,
+            (Flaw::DropsUnlabeledBy, Some(mut d)) => {
+                if let TaskEvent::Unlabeled { by, .. } = &mut d.event {
+                    by.clear();
+                }
+                Some(d)
+            }
+            (_, d) => d,
+        })
     }
 }
 
@@ -109,7 +134,10 @@ fn the_contract_fails_an_adapter_with_any_one_flaw() {
     for flaw in [
         Flaw::IgnoresSignatures,
         Flaw::ChecksPresenceOnly,
-        Flaw::AcceptsReplays,
+        Flaw::SpendsIds,
+        Flaw::UnlabelsAnyLabel,
+        Flaw::IgnoresEdits,
+        Flaw::DropsUnlabeledBy,
         Flaw::EveryoneIsAMember,
         Flaw::TransitionDoesNothing,
         Flaw::MissingIssueIsOk,
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-tracker`
Expected: FAIL to compile: no variant named `Unlabeled` found for enum `TaskEvent`; no variant named `Edited` found for enum `TaskEvent`.

- [ ] **Step 3: Add the events and extend the contract**

Apply to `crates/kod-tracker/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/lib.rs b/crates/kod-tracker/src/lib.rs
index d38c7b1..ec346ea 100644
--- a/crates/kod-tracker/src/lib.rs
+++ b/crates/kod-tracker/src/lib.rs
@@ -93,11 +93,18 @@ pub enum TaskEvent {
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
 
-/// A verified task event and the tracker's ID for its delivery. The
-/// dispatcher keeps the IDs it has acted on, so a redelivery is not a second
-/// instruction.
+/// A verified task event and the tracker's ID for its delivery. The ID must
+/// come from the signed payload, never from an unsigned header. The adapter
+/// keeps no record of IDs: the dispatcher dedups in its own store, after
+/// the write that queues the event, so a failed write never spends an ID.
 #[derive(Clone, Debug, PartialEq)]
 pub struct Delivery {
     pub id: String,
````

Apply to `crates/kod-tracker/src/fake.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/fake.rs b/crates/kod-tracker/src/fake.rs
index c27b322..0445bb5 100644
--- a/crates/kod-tracker/src/fake.rs
+++ b/crates/kod-tracker/src/fake.rs
@@ -75,8 +75,6 @@ struct Inner {
     prs: Vec<(String, String)>,
     members: HashSet<String>,
     created: u64,
-    /// Delivery IDs of the webhooks accepted so far.
-    deliveries: HashSet<String>,
 }
 
 pub struct FakeTracker {
@@ -246,11 +244,6 @@ impl Tracker for FakeTracker {
                 .with_context(|| format!("fake: the webhook has no {k}"))
         };
         let id = text("delivery").context("fake: the webhook has no delivery ID")?;
-        // Spent only now, after the signature: a forged request can't use up
-        // the ID of the real one.
-        if !self.lock().deliveries.insert(id.clone()) {
-            bail!("fake: delivery {id} was already accepted (a replay)");
-        }
         let event = match text("type")?.as_str() {
             "label_added" if text("label")? == self.mapping.trigger_label => {
                 Some(TaskEvent::Labeled {
@@ -258,6 +251,15 @@ impl Tracker for FakeTracker {
                     by: text("user")?,
                 })
             }
+            "label_removed" if text("label")? == self.mapping.trigger_label => {
+                Some(TaskEvent::Unlabeled {
+                    issue: text("issue")?,
+                    by: text("user")?,
+                })
+            }
+            "issue_edited" => Some(TaskEvent::Edited {
+                issue: text("issue")?,
+            }),
             "comment" => Some(TaskEvent::Commented {
                 issue: text("issue")?,
                 author: text("user")?,
@@ -346,6 +348,26 @@ pub fn sandbox() -> (FakeTracker, Sandbox) {
         ignored: t.webhook(
             &json!({"delivery": "d-3", "type": "label_added", "issue": "FAKE-1", "label": "frontend", "user": "dev"}),
         ),
+        untrigger: t.webhook(
+            &json!({"delivery": "d-4", "type": "label_removed", "issue": "FAKE-1", "label": "kod", "user": "po"}),
+        ),
+        untrigger_event: Delivery {
+            id: "d-4".into(),
+            event: TaskEvent::Unlabeled {
+                issue: "FAKE-1".into(),
+                by: "po".into(),
+            },
+        },
+        other_unlabel: t.webhook(
+            &json!({"delivery": "d-5", "type": "label_removed", "issue": "FAKE-1", "label": "frontend", "user": "dev"}),
+        ),
+        edited: t.webhook(&json!({"delivery": "d-6", "type": "issue_edited", "issue": "FAKE-1"})),
+        edited_event: Delivery {
+            id: "d-6".into(),
+            event: TaskEvent::Edited {
+                issue: "FAKE-1".into(),
+            },
+        },
     };
     (t, s)
 }
````

Apply to `crates/kod-tracker/src/contract.rs` with `git apply`:

````diff
diff --git a/crates/kod-tracker/src/contract.rs b/crates/kod-tracker/src/contract.rs
index 495eb45..56a13b8 100644
--- a/crates/kod-tracker/src/contract.rs
+++ b/crates/kod-tracker/src/contract.rs
@@ -32,6 +32,14 @@ pub struct Sandbox {
     pub comment_event: Delivery,
     /// A recorded, signed webhook that is not a task event.
     pub ignored: Webhook,
+    /// A recorded webhook removing the trigger label, and its delivery.
+    pub untrigger: Webhook,
+    pub untrigger_event: Delivery,
+    /// A recorded webhook removing some other label: not a task event.
+    pub other_unlabel: Webhook,
+    /// A recorded webhook for an edit of the story's fields, and its delivery.
+    pub edited: Webhook,
+    pub edited_event: Delivery,
 }
 
 pub fn check(t: &dyn Tracker, s: &Sandbox) {
@@ -123,8 +131,7 @@ pub fn check(t: &dyn Tracker, s: &Sandbox) {
     );
 
     let parse = |w: &Webhook| t.parse_webhook(w);
-    // Forgeries first, while the trigger's delivery ID is unspent, so only a
-    // signature check can refuse them; they are errors, never a quiet None.
+    // Forgeries are errors, never a quiet None.
     assert!(
         parse(&s.tampered).is_err(),
         "a changed body passed the signature check"
@@ -154,12 +161,25 @@ pub fn check(t: &dyn Tracker, s: &Sandbox) {
         Some(s.comment_event.clone())
     );
     assert_eq!(parse(&s.ignored).expect("ignored webhook"), None);
-    assert!(
-        parse(&s.trigger).is_err(),
-        "a replayed trigger was accepted"
+    assert_eq!(
+        parse(&s.untrigger).expect("untrigger webhook"),
+        Some(s.untrigger_event.clone())
     );
-    assert!(
-        parse(&s.comment).is_err(),
-        "a replayed comment was accepted"
+    assert_eq!(
+        parse(&s.other_unlabel).expect("other unlabel webhook"),
+        None,
+        "removing a label other than the trigger is not a task event"
+    );
+    assert_eq!(
+        parse(&s.edited).expect("edited webhook"),
+        Some(s.edited_event.clone())
+    );
+    // The adapter keeps no state: a redelivery parses the same, and the
+    // dispatcher's store, not the adapter, refuses it as a duplicate. An
+    // adapter that spent IDs would lose an event whose first write failed.
+    assert_eq!(
+        parse(&s.trigger).expect("redelivered trigger"),
+        Some(s.trigger_event.clone()),
+        "a redelivery parsed differently"
     );
 }
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-tracker fake 6, contract 3; nothing else changes), clean.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-tracker
git commit -m "kod-tracker: unlabel and edit events, dedup left to the dispatcher"
```

### Task 2: Config and the store

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/kod-dispatcher/Cargo.toml`, `src/lib.rs`, `src/config.rs`, `src/store.rs`
- Test: `crates/kod-dispatcher/tests/config.rs`, `tests/store.rs`

**Interfaces:**
- Consumes: nothing from this plan.
- Produces: `Config { repo: String, owner: String, backup: Option<String> }` with `Config::{parse(&str), load(&Path)} -> anyhow::Result<Config>` and `may_always_stop(&str) -> bool`; `store::{open(&Path) -> anyhow::Result<rusqlite::Connection>, stopped(&Connection, &str) -> anyhow::Result<bool>, BUSY_TIMEOUT, LIVE}`; tables `deliveries(id, delivery, ticket, event, priority, attempts, next_at, error, done)`, `followups(id, kind, target, arg, attempts, next_at, error, done)`, `runs(seq, ticket, revision, state, reason)`, `control(id, halted, by)`.

- [ ] **Step 1: Write the failing tests**

Apply to `Cargo.toml` with `git apply`:

````diff
diff --git a/Cargo.toml b/Cargo.toml
index 3d38611..77b815a 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,6 +1,7 @@
 [workspace]
 members = [
     "crates/kod-audit",
+    "crates/kod-dispatcher",
     "crates/kod-eval",
     "crates/kod-llm-proxy",
     "crates/kod-loop",
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
//! The dispatcher's SQLite store: the delivery queue, the runs, the
//! follow-up calls, and the kill switch. The database itself keeps run states
//! moving forward and allows one live run per ticket. A run's state is also
//! its durable cancel flag: the gateway reads it before every call.
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::{path::Path, time::Duration};

/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS deliveries (
    id TEXT PRIMARY KEY,
    delivery TEXT NOT NULL,
    ticket TEXT NOT NULL,
    event TEXT NOT NULL,
    priority INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1, 2))
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
CREATE TABLE IF NOT EXISTS followups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('revoke', 'stop', 'stop_all', 'transition', 'comment', 'check_publish')),
    target TEXT NOT NULL,
    arg TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at INTEGER NOT NULL DEFAULT 0,
    error TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1, 2))
);
CREATE TABLE IF NOT EXISTS control (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    halted INTEGER NOT NULL,
    by TEXT NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 0
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

/// Whether the run must stop: it has ended, or there is no such run. The
/// gateway's `Stopped` check reads this on its own connection to the store.
pub fn stopped(c: &Connection, run_id: &str) -> Result<bool> {
    let Some(seq) = run_id
        .strip_prefix("run-")
        .and_then(|n| n.parse::<i64>().ok())
    else {
        return Ok(true);
    };
    let live: Option<bool> = c
        .query_row(
            &format!("SELECT state IN {LIVE} FROM runs WHERE seq = ?1"),
            [seq],
            |r| r.get(0),
        )
        .optional()?;
    Ok(live != Some(true))
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
- Modify: `crates/kod-dispatcher/src/lib.rs` (replace), `src/store.rs`
- Test: `crates/kod-dispatcher/tests/common/mod.rs`, `tests/admission.rs`, `tests/gateway_contract.rs`

**Interfaces:**
- Consumes: Task 1's events; Task 2's `Config`, `store::{open, LIVE}`; `kod_policy::Policy::{load, decide}` with the policy plan's `delegate` input (`ticket.{repo, risk_class, acceptance_criteria, bounces}`, `wip_in_progress`); `kod_audit::AuditLog::{open, record_decision}`; `kod_tracker::{Tracker, Issue, State, TaskEvent, Webhook}`.
- Produces: `RunState` (`ALL`, `name`), `RunRow { id, ticket, revision, state, reason }`, `RunStart { id, ticket, revision, repo }`, `trait Gateway: Send + Sync { start(&RunStart), revoke(&str), stop(&str), stop_all(), publish(&str) -> Result<String>, find_published(&RunStart) -> Result<Option<String>> }`, `Received { Queued, Duplicate, Ignored, Refused(String) }`, `MAX_ATTEMPTS`, `run_id(i64) -> String`, `revision(&Issue) -> String`, `Dispatcher::{open(Config, &Path, anyhow::Result<Arc<Policy>>, Arc<AuditLog>, Arc<dyn Tracker>, Arc<dyn Gateway>) -> Result<Dispatcher>, receive(&Webhook) -> Result<Received>, work_once() -> Result<bool>, work_at(i64) -> Result<bool>, finished(&str, RunState, &str) -> Result<()>, stopped(&str) -> Result<bool>, is_halted() -> Result<bool>, runs() -> Result<Vec<RunRow>>}`, `router(Arc<Dispatcher>) -> axum::Router` (202 queued, 200 duplicate or ignored, 401 refused, 500 on a store error). Test helpers: `FakeGateway`, `Flaky` (the fake tracker with faults), `harness()`, `harness_with(..)`, `label`, `unlabel`, `edited`, `Harness::{deliver, running, queued, sql, count, reopen, states, workflow, last_comment}`, `NOW`.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-dispatcher/tests/common/mod.rs`:

````rust
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
use std::sync::atomic::Ordering;
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
    assert_eq!(h.d.receive(&w).unwrap(), Received::Duplicate);
    while h.d.work_once().unwrap() {}
    // After a restart too.
    let again = h.reopen();
    assert_eq!(again.receive(&w).unwrap(), Received::Duplicate);
    while again.work_once().unwrap() {}
    assert_eq!(h.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    assert_eq!(h.states().len(), 1);
}

#[test]
fn a_replay_under_a_new_unsigned_delivery_id_is_a_duplicate() {
    let h = harness();
    h.flaky.header_ids.store(true, Ordering::SeqCst);
    let mut w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    w.headers.push(("x-delivery".into(), "h-1".into()));
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
    w.headers[1].1 = "h-2".into();
    assert_eq!(h.d.receive(&w).unwrap(), Received::Duplicate);
}

#[test]
fn a_delivery_whose_write_failed_is_queued_on_the_retry() {
    let h = harness();
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(h.d.receive(&w).is_err(), "the store is busy");
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
}

#[test]
fn a_failing_delivery_backs_off_without_holding_up_other_tickets() {
    let h = harness();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    assert!(h.d.work_at(NOW).unwrap());
    assert!(
        h.d.work_at(NOW).unwrap(),
        "FAKE-2 is not stuck behind FAKE-1"
    );
    assert!(!h.d.work_at(NOW).unwrap(), "FAKE-1 waits for its backoff");
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    assert_eq!(h.d.runs().unwrap()[0].ticket, "FAKE-2");
    // Retried with growing backoff, then a dead letter, said on the ticket.
    let mut t = NOW;
    for _ in 1..kod_dispatcher::MAX_ATTEMPTS {
        t += 1000;
        assert!(h.d.work_at(t).unwrap());
    }
    assert!(!h.d.work_at(t + 1000).unwrap());
    assert_eq!(
        h.count("SELECT count(*) FROM deliveries WHERE done = 2 AND error LIKE '%timed out%'"),
        1
    );
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("gave up on an event for this ticket after 5 attempts"),
        "{c}"
    );
}

#[test]
fn a_tracker_that_is_down_never_holds_up_a_start_and_is_retried() {
    let h = harness();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    assert!(h.d.work_at(NOW).unwrap());
    assert_eq!(h.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    assert_eq!(h.workflow("FAKE-1"), "Backlog");
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Backlog", "still backing off");
    h.d.work_at(NOW + 2).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Doing");
}

#[test]
fn concurrent_admissions_respect_the_wip_limit() {
    let h = harness();
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| h.d.work_once().unwrap());
        }
    });
    while h.d.work_once().unwrap() {}
    assert_eq!(h.states().len(), 1, "{:?}", h.states());
    let refused = [h.last_comment("FAKE-1"), h.last_comment("FAKE-2")];
    assert!(
        refused.iter().any(|c| c.contains("WIP limit reached (1)")),
        "{refused:?}"
    );
}

#[test]
fn a_run_left_publishing_by_a_crash_is_finished_on_restart() {
    let h = harness();
    h.running();
    h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
    *h.gw.published.lock().unwrap() = Ok(Some("https://github.example/acme/app/pull/7".into()));
    h.reopen();
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert!(
        h.gw.calls()
            .contains(&"find_published run-1 FAKE-1".to_owned())
    );
    assert!(h.last_comment("FAKE-1").contains("pull/7"));
    assert_eq!(h.workflow("FAKE-1"), "Review");
}

#[test]
fn a_run_left_publishing_with_no_pr_needs_a_human_on_restart() {
    for answer in [Ok(None), Err("fake github: down".to_owned())] {
        let h = harness();
        h.running();
        h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
        *h.gw.published.lock().unwrap() = answer;
        h.reopen();
        assert_eq!(h.states(), [("run-1".to_owned(), "needs_human")]);
        assert!(h.d.stopped("run-1").unwrap());
        assert!(h.gw.calls().contains(&"stop run-1".to_owned()));
    }
}

#[test]
fn stopped_is_true_for_ended_and_unknown_runs() {
    let h = harness();
    h.running();
    assert!(!h.d.stopped("run-1").unwrap());
    assert!(h.d.stopped("run-2").unwrap());
    assert!(h.d.stopped("FAKE-1").unwrap());
    h.d.finished("run-1", RunState::Blocked, "stuck").unwrap();
    assert!(h.d.stopped("run-1").unwrap());
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
    assert_eq!(h.gw.calls()[1..], ["publish run-1", "stop run-1"]);
    assert_eq!(h.gw.revoked(), ["run-1"]);
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert_eq!(h.workflow("FAKE-1"), "Review");
    assert!(
        h.last_comment("FAKE-1")
            .contains("published https://github.example")
    );
    // Ended is final: no second finish, no second publish.
    assert!(h.d.finished("run-1", RunState::Done, "again").is_err());
    assert!(h.d.finished("run-1", RunState::Blocked, "again").is_err());
    assert_eq!(h.gw.calls().len(), 3);
}

#[test]
fn a_cancel_in_the_last_publish_window_marks_the_pr() {
    let h = harness();
    h.running();
    // The run is canceled after the gateway's last check, and the PR opens.
    let db = h.db.clone();
    *h.gw.on_publish.lock().unwrap() = Some(Box::new(move || {
        kod_dispatcher::store::open(&db)
            .unwrap()
            .execute("UPDATE runs SET state = 'canceled' WHERE seq = 1", [])
            .unwrap();
    }));
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("https://github.example/acme/app/pull/1 is not Kod's work: close it"),
        "{c}"
    );
    let audit = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = audit
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%canceled_after_publish%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_failed_publish_or_start_ends_the_run_and_says_why() {
    let h = harness();
    h.running();
    h.gw.fail_publish.store(true, Ordering::SeqCst);
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    // Not blocked: a failed publish is not a bounce of the work.
    assert_eq!(h.states(), [("run-1".to_owned(), "needs_human")]);
    assert_eq!(h.workflow("FAKE-1"), "Waiting");
    assert!(h.last_comment("FAKE-1").contains("publish failed"));
    h.gw.fail_start.store(true, Ordering::SeqCst);
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "needs_human"));
    assert!(h.last_comment("FAKE-2").contains("the run did not start"));
    // A half-started sandbox is stopped.
    assert_eq!(h.gw.calls().last().unwrap(), "stop run-2");
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
    assert_eq!(post(&w), 200, "a duplicate");
    let mut forged = label(&h.tracker, "d-2", "FAKE-1", "dev");
    forged.headers[0].1 = "0".repeat(64);
    assert_eq!(post(&forged), 401);
    let other = h.tracker.webhook(&serde_json::json!({"delivery": "d-3", "type": "label_added", "issue": "FAKE-1", "label": "frontend", "user": "dev"}));
    assert_eq!(post(&other), 200);
}

use kod_dispatcher::{Dispatcher, Gateway, RunStart};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

struct BlockingGateway {
    inner: Arc<FakeGateway>,
    entered: mpsc::Sender<()>,
    release: (Mutex<bool>, Condvar),
}

impl Gateway for BlockingGateway {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)?;
        let first = self
            .inner
            .calls()
            .iter()
            .filter(|c| c.starts_with("start "))
            .count()
            == 1;
        if first {
            self.entered.send(()).unwrap();
            let (lock, ready) = &self.release;
            drop(
                ready
                    .wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
        }
        Ok(())
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.inner.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.inner.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        self.inner.publish(run)
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(run)
    }
}

#[test]
fn concurrent_workers_start_a_queued_run_once() {
    let h = harness();
    let (entered, rx) = mpsc::channel();
    let gateway = Arc::new(BlockingGateway {
        inner: h.gw.clone(),
        entered,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = Arc::new(
        Dispatcher::open(
            kod_dispatcher::Config::parse(CONFIG).unwrap(),
            &h.db,
            kod_policy::Policy::load(&policy_dir()).map(Arc::new),
            Arc::new(kod_audit::AuditLog::open(&h.audit).unwrap()),
            h.flaky.clone(),
            gateway.clone(),
        )
        .unwrap(),
    );
    h.queued("FAKE-1");
    let first_d = d.clone();
    let first = std::thread::spawn(move || first_d.work_once());
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = d.work_once();
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    first.join().unwrap().unwrap();
    second.unwrap();
    assert_eq!(
        h.gw.calls()
            .iter()
            .filter(|c| c.starts_with("start "))
            .count(),
        1
    );
    assert!(!h.gw.calls().contains(&"stop run-1".to_owned()));
}
````

Create `crates/kod-dispatcher/tests/gateway_contract.rs`:

````rust
mod common;

use common::FakeGateway;
use kod_dispatcher::{Gateway, RunStart, check_absent_run};

#[test]
fn the_fake_accepts_revoke_and_stop_for_a_never_started_run() {
    let gateway = FakeGateway::default();
    check_absent_run(&gateway, "never-started").unwrap();
    assert_eq!(gateway.revoked(), ["never-started", "never-started"]);
    assert_eq!(
        gateway.calls(),
        ["stop never-started", "stop never-started"]
    );
}

struct RejectsMissing(bool);

impl Gateway for RejectsMissing {
    fn start(&self, _: &RunStart) -> anyhow::Result<()> {
        unreachable!()
    }
    fn revoke(&self, _: &str) -> anyhow::Result<()> {
        if self.0 {
            anyhow::bail!("unknown run");
        }
        Ok(())
    }
    fn stop(&self, _: &str) -> anyhow::Result<()> {
        if !self.0 {
            anyhow::bail!("unknown run");
        }
        Ok(())
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        unreachable!()
    }
    fn publish(&self, _: &str) -> anyhow::Result<String> {
        unreachable!()
    }
    fn find_published(&self, _: &RunStart) -> anyhow::Result<Option<String>> {
        unreachable!()
    }
}

#[test]
fn the_contract_refuses_an_adapter_that_errors_on_either_absent_operation() {
    for rejects_revoke in [true, false] {
        let result = check_absent_run(&RejectsMissing(rejects_revoke), "never-started");
        assert!(
            result.is_err(),
            "accepted an adapter that rejects an absent run"
        );
    }
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-dispatcher --test admission --test gateway_contract`
Expected: FAIL to compile: unresolved imports `kod_dispatcher::Dispatcher`, `kod_dispatcher::Gateway`, `kod_dispatcher::RunStart`.

- [ ] **Step 3: Implement admission, runs and the route**

Replace `crates/kod-dispatcher/src/lib.rs`:

````rust
//! Kod's dispatcher: tracker webhooks in, runs out. A verified delivery is
//! queued in a SQLite table, admitted or refused (with the reason commented
//! on the ticket), and started through the tool gateway. Runs move forward
//! only; a label removal, a ticket edit or the kill switch cancels them.
//!
//! Every outside call that follows a state change (stopping a sandbox,
//! moving or commenting on the ticket) is written as a follow-up in the same
//! transaction as the change, then made and retried until it succeeds. Stops
//! go first, and a tracker that is down never holds up a stop.
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Router,
    body::Bytes,
    extract::State as Shared,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use kod_audit::{AuditLog, RUN_AUDIT, redact::redact};
use kod_policy::Policy;
use kod_tracker::{Issue, State, TaskEvent, Tracker, Webhook};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use store::LIVE;

pub mod config;
pub mod store;

pub use config::Config;

/// Attempts before a delivery or follow-up becomes a dead letter.
pub const MAX_ATTEMPTS: i64 = 5;

/// Seconds before attempt `n + 1`: 2, 4, 8, ... up to about 4 minutes.
fn backoff(attempts: i64) -> i64 {
    1 << attempts.clamp(1, 8)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// An error as it may be stored or shown: secrets redacted.
fn scrub(e: &anyhow::Error) -> String {
    redact(&format!("{e:#}"))
}

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

/// The tool gateway as the dispatcher sees it. Every method must be safe to
/// repeat. The gateway also reads the run's state from the store (see
/// `store::stopped`) before each call it serves and twice inside publish, so
/// a run canceled here is refused there even if `stop` has not arrived yet.
///
/// How sub-plan 6b wires it: the runner calls `Dispatcher::finished` on the
/// host after the sandbox exits and kod-eval's collector has written
/// `collect_out` next to it, outside the sandbox. `publish` takes only the
/// run ID; the gateway reads the change from that collector output, never
/// from the sandbox.
pub trait Gateway: Send + Sync {
    fn start(&self, run: &RunStart) -> Result<()>;
    /// Ends the run's tool-gateway token and revokes its forge credentials
    /// at once, without waiting for the sandbox. A never-started run is Ok.
    fn revoke(&self, run_id: &str) -> Result<()>;
    /// Stops the run's sandbox. Ok only once confirmed gone; a never-started run is already gone.
    fn stop(&self, run_id: &str) -> Result<()>;
    /// The kill switch: stop every sandbox and revoke every agent identity.
    fn stop_all(&self) -> Result<()>;
    /// Publishes the run's collected change; returns the draft PR's URL.
    fn publish(&self, run_id: &str) -> Result<String>;
    /// After a restart: the draft PR a publish of this run opened, if any.
    fn find_published(&self, run: &RunStart) -> Result<Option<String>>;
}

/// Adapter contract: the caller supplies a run ID that was never started.
/// Revoking and stopping that absent run must succeed, including on retry.
pub fn check_absent_run(gateway: &dyn Gateway, run_id: &str) -> Result<()> {
    for _ in 0..2 {
        gateway
            .revoke(run_id)
            .context("revoking a never-started run")?;
        gateway
            .stop(run_id)
            .context("stopping a never-started run")?;
    }
    Ok(())
}

/// What `receive` did with a webhook.
#[derive(Clone, Debug, PartialEq)]
pub enum Received {
    Queued,
    /// The same signed body was queued before.
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    hex(&Sha256::digest(format!("{fields:?}").as_bytes())[..8])
}

/// A queued event, as the store keeps it.
#[derive(Clone, Debug, PartialEq)]
enum Event {
    Labeled { issue: String, by: String },
    Unlabeled { issue: String, by: String },
    Edited { issue: String },
}

impl Event {
    fn from(event: &TaskEvent) -> Option<Event> {
        Some(match event {
            TaskEvent::Labeled { issue, by } => Event::Labeled {
                issue: issue.clone(),
                by: by.clone(),
            },
            TaskEvent::Unlabeled { issue, by } => Event::Unlabeled {
                issue: issue.clone(),
                by: by.clone(),
            },
            TaskEvent::Edited { issue } => Event::Edited {
                issue: issue.clone(),
            },
            // Comments are instructions for a live run: sub-plan 8.
            TaskEvent::Commented { .. } => return None,
        })
    }

    fn ticket(&self) -> &str {
        match self {
            Event::Labeled { issue, .. } | Event::Unlabeled { issue, .. } => issue,
            Event::Edited { issue } => issue,
        }
    }

    /// Lower goes first: anything that can stop a run jumps the queue.
    fn priority(&self) -> i64 {
        match self {
            Event::Labeled { .. } => 1,
            _ => 0,
        }
    }

    fn encode(&self) -> Value {
        match self {
            Event::Labeled { issue, by } => json!({"type": "labeled", "issue": issue, "by": by}),
            Event::Unlabeled { issue, by } => {
                json!({"type": "unlabeled", "issue": issue, "by": by})
            }
            Event::Edited { issue } => json!({"type": "edited", "issue": issue}),
        }
    }

    fn decode(v: &Value) -> Result<Event> {
        let text = |k: &str| {
            v[k].as_str()
                .map(str::to_owned)
                .with_context(|| format!("queued event has no {k}"))
        };
        Ok(match v["type"].as_str() {
            Some("labeled") => Event::Labeled {
                issue: text("issue")?,
                by: text("by")?,
            },
            Some("unlabeled") => Event::Unlabeled {
                issue: text("issue")?,
                by: text("by")?,
            },
            Some("edited") => Event::Edited {
                issue: text("issue")?,
            },
            _ => bail!("unknown queued event {v}"),
        })
    }
}

/// Queues a call to make once the transaction commits.
fn follow(c: &Connection, kind: &str, target: &str, arg: &str) -> Result<()> {
    if kind == "transition" {
        c.execute(
            "UPDATE followups SET done = 1 WHERE kind = 'transition' AND target = ?1 AND done = 0",
            [target],
        )?;
    }
    c.execute(
        "INSERT INTO followups (kind, target, arg) VALUES (?1, ?2, ?3)",
        params![kind, target, arg],
    )?;
    Ok(())
}

fn state_named(name: &str) -> Result<State> {
    State::ALL
        .into_iter()
        .find(|s| s.name() == name)
        .with_context(|| format!("unknown ticket state {name:?}"))
}

fn live_run(c: &Connection, ticket: &str) -> Result<Option<(i64, String)>> {
    Ok(c.query_row(
        &format!("SELECT seq, revision FROM runs WHERE ticket = ?1 AND state IN {LIVE}"),
        [ticket],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
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
    global_stop: Mutex<()>,
}

impl Dispatcher {
    /// Opens the store and finishes what a crash interrupted: a run left in
    /// `publishing` ends done if the gateway finds its draft PR, and
    /// needs_human otherwise. It is never left live.
    pub fn open(
        config: Config,
        db: &Path,
        policy: Result<Arc<Policy>>,
        audit: Arc<AuditLog>,
        tracker: Arc<dyn Tracker>,
        gateway: Arc<dyn Gateway>,
    ) -> Result<Dispatcher> {
        let d = Dispatcher {
            config,
            policy: policy.map_err(|e| format!("{e:#}")),
            audit,
            tracker,
            gateway,
            db: Mutex::new(store::open(db)?),
            global_stop: Mutex::new(()),
        };
        d.recover()?;
        Ok(d)
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recover(&self) -> Result<()> {
        let publishing: Vec<RunStart> = {
            let c = self.db();
            let mut s =
                c.prepare("SELECT seq, ticket, revision FROM runs WHERE state = 'publishing'")?;
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
        for run in publishing {
            let (to, why) = match self.gateway.find_published(&run) {
                Ok(Some(pr)) => (
                    RunState::Done,
                    format!("the dispatcher restarted during publish; the draft PR is {pr}"),
                ),
                Ok(None) => (
                    RunState::NeedsHuman,
                    "the dispatcher restarted during publish and found no draft PR".to_owned(),
                ),
                Err(e) => (
                    RunState::NeedsHuman,
                    format!(
                        "the dispatcher restarted during publish and could not check for a draft PR: {}",
                        scrub(&e)
                    ),
                ),
            };
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::end(&tx, seq_of(&run.id)?, to, &why, &[RunState::Publishing])?;
            tx.commit()?;
        }
        self.db().execute(
            "UPDATE followups SET next_at = 0 WHERE kind = 'check_publish' AND done = 0",
            [],
        )?;
        self.follow_up(now())?;
        self.follow_up(now())
    }

    /// Checks the webhook and queues its event. Errors are store errors only;
    /// a failed write spends nothing, so the tracker's retry is queued.
    pub fn receive(&self, webhook: &Webhook) -> Result<Received> {
        let delivery = match self.tracker.parse_webhook(webhook) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(Received::Ignored),
            Err(e) => return Ok(Received::Refused(scrub(&e))),
        };
        let Some(event) = Event::from(&delivery.event) else {
            return Ok(Received::Ignored);
        };
        let added = self.db().execute(
            "INSERT OR IGNORE INTO deliveries (id, delivery, ticket, event, priority)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                hex(&Sha256::digest(&webhook.body)),
                delivery.id,
                event.ticket(),
                event.encode().to_string(),
                event.priority()
            ],
        )?;
        Ok(if added == 1 {
            Received::Queued
        } else {
            Received::Duplicate
        })
    }

    /// `work_at` now.
    pub fn work_once(&self) -> Result<bool> {
        self.work_at(now())
    }

    /// Handles the next due delivery, makes the due follow-ups, then starts
    /// queued runs. Returns whether there was a delivery. Deliveries that can
    /// stop a run go first and never wait; a ticket's labels keep their
    /// order behind its earlier deliveries. One that
    /// fails is retried with backoff, without holding up other tickets, and
    /// after `MAX_ATTEMPTS` it is a dead letter, reported on its ticket.
    pub fn work_at(&self, now: i64) -> Result<bool> {
        let next: Option<(String, String, String, i64)> = self
            .db()
            .query_row(
                "SELECT id, ticket, event, attempts FROM deliveries d
                 WHERE done = 0 AND next_at <= ?1 AND (d.priority = 0 OR NOT EXISTS (
                     SELECT 1 FROM deliveries e
                     WHERE e.done = 0 AND e.ticket = d.ticket AND e.rowid < d.rowid))
                 ORDER BY priority, rowid LIMIT 1",
                [now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((id, ticket, event, attempts)) = &next
            && let Err(e) = self.handle(id, event)
        {
            let attempts = attempts + 1;
            let error = scrub(&e);
            let dead = attempts >= MAX_ATTEMPTS;
            let c = self.db();
            c.execute(
                "UPDATE deliveries SET attempts = ?2, next_at = ?3, error = ?4, done = ?5
                 WHERE id = ?1 AND done = 0",
                params![
                    id,
                    attempts,
                    now + backoff(attempts),
                    error,
                    if dead { 2 } else { 0 }
                ],
            )?;
            if dead {
                follow(
                    &c,
                    "comment",
                    ticket,
                    &format!(
                        "Kod gave up on an event for this ticket after {attempts} attempts: {error}."
                    ),
                )?;
            }
        }
        self.follow_up(now)?;
        self.start_queued()?;
        self.follow_up(now)?;
        Ok(next.is_some())
    }

    fn handle(&self, id: &str, event: &str) -> Result<()> {
        match Event::decode(&serde_json::from_str(event)?)? {
            Event::Labeled { issue, by } => self.admit(id, &issue, &by),
            Event::Unlabeled { .. } | Event::Edited { .. } => self.done(id),
        }
    }

    fn done(&self, delivery: &str) -> Result<()> {
        self.db()
            .execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        Ok(())
    }

    /// Makes the due follow-ups: revokes, then stops, then the tracker. One
    /// that fails is retried with backoff. A tracker call is a dead letter
    /// after `MAX_ATTEMPTS`; a revoke or a stop is never given up, and when
    /// it reaches `MAX_ATTEMPTS` Kod raises the alarm once, on the ticket and
    /// in the audit log. A stop of every sandbox from before the last
    /// `resume` is dropped: the kill's own runs have their own stops.
    fn follow_up(&self, now: i64) -> Result<()> {
        let due: Vec<(i64, String, String, String, i64)> = {
            let c = self.db();
            let mut s = c.prepare(
                "SELECT id, kind, target, arg, attempts FROM followups
                 WHERE done = 0 AND next_at <= ?1
                 ORDER BY CASE kind WHEN 'revoke' THEN 0 WHEN 'check_publish' THEN 2
                     WHEN 'transition' THEN 2 WHEN 'comment' THEN 2 ELSE 1 END, id",
            )?;
            s.query_map([now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (id, kind, target, arg, attempts) in due {
            let r = match kind.as_str() {
                "revoke" => self.gateway.revoke(&target),
                "stop" => self.gateway.stop(&target),
                "stop_all" => {
                    let _stop = self.global_stop.lock().unwrap_or_else(|e| e.into_inner());
                    if arg != Self::epoch(&self.db())?.to_string() {
                        self.db().execute(
                            "UPDATE followups SET done = 1, error = 'the kill switch was lifted' WHERE id = ?1",
                            [id],
                        )?;
                        continue;
                    }
                    self.gateway.stop_all()
                }
                "check_publish" => self.reconcile_publish(id, &target, &arg),
                "transition" => state_named(&arg).and_then(|s| self.tracker.transition(&target, s)),
                "comment" => self.tracker.comment(&target, &arg),
                k => Err(anyhow!("unknown follow-up {k}")),
            };
            let c = self.db();
            match r {
                Ok(()) => c.execute("UPDATE followups SET done = 1 WHERE id = ?1", [id])?,
                Err(e) => {
                    let attempts = attempts + 1;
                    let error = scrub(&e);
                    let stopping = matches!(kind.as_str(), "revoke" | "stop" | "stop_all");
                    let dead = !stopping && kind != "check_publish" && attempts >= MAX_ATTEMPTS;
                    c.execute(
                        "UPDATE followups SET attempts = ?2, next_at = ?3, error = ?4, done = ?5
                         WHERE id = ?1",
                        params![
                            id,
                            attempts,
                            now + backoff(attempts),
                            error,
                            if dead { 2 } else { 0 }
                        ],
                    )?;
                    if stopping && attempts == MAX_ATTEMPTS {
                        Self::alarm(&c, &self.audit, &kind, &target, attempts, &error)?;
                    }
                    0
                }
            };
        }
        Ok(())
    }

    /// A revoke or a stop has failed `MAX_ATTEMPTS` times: say so once on
    /// the run's ticket and in the audit log (best effort), and keep trying.
    fn alarm(
        c: &Connection,
        audit: &AuditLog,
        kind: &str,
        target: &str,
        attempts: i64,
        error: &str,
    ) -> Result<()> {
        let record = json!({"kind": "stop_overdue", "call": kind, "run": target, "attempts": attempts, "error": error});
        eprintln!("{record}");
        let _ = audit.append(RUN_AUDIT, &record);
        let ticket: Option<String> = match seq_of(target) {
            Ok(seq) => c
                .query_row("SELECT ticket FROM runs WHERE seq = ?1", [seq], |r| {
                    r.get(0)
                })
                .optional()?,
            Err(_) => None,
        };
        if let Some(ticket) = ticket {
            follow(
                c,
                "comment",
                &ticket,
                &format!(
                    "Kod has not confirmed the {kind} of {target} after {attempts} attempts and keeps trying: {error}."
                ),
            )?;
        }
        Ok(())
    }

    /// The kill switch's epoch: moved on by each `resume`.
    fn epoch(c: &Connection) -> Result<i64> {
        Ok(
            c.query_row("SELECT epoch FROM control WHERE id = 1", [], |r| r.get(0))
                .optional()?
                .unwrap_or(0),
        )
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
        let pending: bool = tx.query_row(
            "SELECT done = 0 FROM deliveries WHERE id = ?1",
            [delivery],
            |r| r.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        if Self::halted(&tx)? {
            reasons.push(
                "Kod is stopped by the kill switch until the owner or backup resumes it".to_owned(),
            );
        }
        if let Some((seq, _)) = live_run(&tx, ticket)? {
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
                // A bounce is a run that ended blocked: the work was sent
                // back. A failed publish ends needs_human and is not one.
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
                    reasons.push(format!("the decision could not be recorded: {}", scrub(&e)));
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
            follow(&tx, "transition", ticket, State::InProgress.name())?;
        } else {
            follow(
                &tx,
                "comment",
                ticket,
                &format!("Kod did not start this ticket: {}.", reasons.join("; ")),
            )?;
        }
        tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
        tx.commit()?;
        Ok(())
    }

    /// Starts queued runs. Each is checked again just before its start, and
    /// one canceled or halted while starting is stopped.
    fn start_queued(&self) -> Result<()> {
        let queued: Vec<RunStart> = {
            let c = self.db();
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
            let claimed = {
                let mut c = self.db();
                let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if Self::halted(&tx)? {
                    return Ok(());
                }
                let claimed = tx.execute(
                    "UPDATE runs SET state = 'running' WHERE seq = ?1 AND state = 'queued'",
                    [seq],
                )?;
                tx.commit()?;
                claimed == 1
            };
            if !claimed {
                continue;
            }
            let started = self.gateway.start(&run);
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match started {
                Ok(()) => {
                    if Self::halted(&tx)? || store::stopped(&tx, &run.id)? {
                        // Canceled while starting.
                        follow(&tx, "revoke", &run.id, "")?;
                        follow(&tx, "stop", &run.id, "")?;
                    }
                }
                Err(e) => {
                    Self::end(
                        &tx,
                        seq,
                        RunState::NeedsHuman,
                        &format!("the run did not start: {}", scrub(&e)),
                        &[RunState::Running],
                    )?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Ends the run if it is in one of `from`, and queues its stop, the
    /// ticket's move and a comment. Returns whether the run ended here.
    fn end(
        c: &Connection,
        seq: i64,
        to: RunState,
        reason: &str,
        from: &[RunState],
    ) -> Result<bool> {
        let from: Vec<String> = from.iter().map(|s| format!("'{}'", s.name())).collect();
        let ticket: Option<String> = c
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
        follow(c, "revoke", &run_id(seq), "")?;
        follow(c, "stop", &run_id(seq), "")?;
        follow(c, "transition", &ticket, state.name())?;
        follow(
            c,
            "comment",
            &ticket,
            &format!("Kod {} ended {}: {reason}.", run_id(seq), to.name()),
        )?;
        Ok(true)
    }

    /// Ends the run in its own transaction, then makes the follow-ups.
    fn end_now(&self, seq: i64, to: RunState, reason: &str, from: &[RunState]) -> Result<bool> {
        let ended = {
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ended = Self::end(&tx, seq, to, reason, from)?;
            tx.commit()?;
            ended
        };
        self.follow_up(now())?;
        Ok(ended)
    }

    /// The runner reports how a running run ended. `Done` publishes through
    /// the gateway first; a canceled run is never published, and a failed
    /// publish ends needs_human.
    pub fn finished(&self, run: &str, outcome: RunState, reason: &str) -> Result<()> {
        let seq = seq_of(run)?;
        match outcome {
            RunState::Done => {
                {
                    let mut c = self.db();
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let moved = tx.execute(
                        "UPDATE runs SET state = 'publishing' WHERE seq = ?1 AND state = 'running'",
                        [seq],
                    )?;
                    if moved == 0 {
                        bail!("{run} is not running");
                    }
                    tx.execute(
                        "INSERT INTO followups (kind, target, arg, next_at)
                         VALUES ('check_publish', ?1, '', ?2)",
                        params![run, i64::MAX],
                    )?;
                    tx.commit()?;
                }
                let published = self.gateway.publish(run);
                let (to, why) = match &published {
                    Ok(pr) => (RunState::Done, format!("{reason}; published {pr}")),
                    Err(e) => (
                        RunState::NeedsHuman,
                        format!("publish failed: {}", scrub(e)),
                    ),
                };
                {
                    let mut c = self.db();
                    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    Self::end(&tx, seq, to, &why, &[RunState::Publishing])?;
                    tx.execute(
                        "UPDATE followups SET next_at = 0, arg = ?2
                         WHERE kind = 'check_publish' AND target = ?1 AND done = 0",
                        params![run, published.as_ref().map_or("", String::as_str)],
                    )?;
                    tx.commit()?;
                }
                self.follow_up(now())?;
                self.follow_up(now())?;
                Ok(())
            }
            RunState::Blocked | RunState::NeedsHuman | RunState::OverBudget => {
                if !self.end_now(seq, outcome, reason, &[RunState::Running])? {
                    bail!("{run} is not running");
                }
                Ok(())
            }
            _ => bail!("{} is not how a run ends", outcome.name()),
        }
    }

    fn reconcile_publish(&self, followup: i64, run: &str, known_pr: &str) -> Result<()> {
        let (start, canceled) = self.db().query_row(
            "SELECT ticket, revision, state = 'canceled' FROM runs WHERE seq = ?1",
            [seq_of(run)?],
            |r| {
                Ok((
                    RunStart {
                        id: run.to_owned(),
                        ticket: r.get(0)?,
                        revision: r.get(1)?,
                        repo: self.config.repo.clone(),
                    },
                    r.get::<_, bool>(2)?,
                ))
            },
        )?;
        if !canceled {
            return Ok(());
        }
        let pr = if known_pr.is_empty() {
            self.gateway.find_published(&start)?
        } else {
            Some(known_pr.to_owned())
        };
        let Some(pr) = pr else {
            return Ok(());
        };
        let recorded = {
            let mut c = self.db();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let recorded = tx.execute(
                "UPDATE followups SET done = 1 WHERE id = ?1 AND done = 0",
                [followup],
            )? == 1;
            if recorded {
                follow(
                    &tx,
                    "comment",
                    &start.ticket,
                    &format!(
                        "Kod {run} was canceled while it published. Its draft PR {pr} is not Kod's work: close it, do not merge it."
                    ),
                )?;
            }
            tx.commit()?;
            recorded
        };
        if recorded {
            let _ = self.audit.append(
                RUN_AUDIT,
                &json!({"kind": "canceled_after_publish", "run": run, "pr": pr}),
            );
        }
        Ok(())
    }

    /// Whether the run must stop: what the gateway's `Stopped` check asks.
    pub fn stopped(&self, run_id: &str) -> Result<bool> {
        store::stopped(&self.db(), run_id)
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

/// The routes: `POST /webhook` answers 202 queued, 200 duplicate or
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

Apply to `crates/kod-dispatcher/src/store.rs` with `git apply`:

````diff
diff --git a/crates/kod-dispatcher/src/store.rs b/crates/kod-dispatcher/src/store.rs
index 4166e35..de740e7 100644
--- a/crates/kod-dispatcher/src/store.rs
+++ b/crates/kod-dispatcher/src/store.rs
@@ -2,6 +2,16 @@
 //! follow-up calls, and the kill switch. The database itself keeps run states
 //! moving forward and allows one live run per ticket. A run's state is also
 //! its durable cancel flag: the gateway reads it before every call.
+//!
+//! A follow-up that revokes or stops is never given up: it is retried with
+//! backoff until the gateway confirms it. A stop of every sandbox carries
+//! the kill switch's epoch, which `resume` moves on, so a retry left over
+//! from a kill never stops runs started after the resume.
+//!
+//! A delivery is keyed by a hash of its signed body, so a redelivery, or a
+//! replay with a different unsigned delivery header, is a duplicate. `done`
+//! is 0 while queued, 1 once its effect is committed, and 2 when it failed
+//! `MAX_ATTEMPTS` times (a dead letter, kept with its last error).
 use anyhow::{Context, Result};
 use rusqlite::{Connection, OptionalExtension};
 use std::{path::Path, time::Duration};
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-dispatcher && cargo clippy -p kod-dispatcher --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (admission 20, gateway_contract 2, config 3, store 2), clean. `concurrent_admissions_respect_the_wip_limit` and the restart tests take about 2 s together (the busy timeout).

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-dispatcher
git commit -m "kod-dispatcher: verified deliveries with retry, atomic admission, run endings and crash recovery"
```

### Task 4: Cancel, the kill switch and resume

**Files:**
- Modify: `crates/kod-dispatcher/src/lib.rs`
- Create: `crates/kod-dispatcher/src/command.rs`
- Test: `crates/kod-dispatcher/tests/common/mod.rs` (modify), `tests/cancel.rs`, `tests/review_edges.rs`

**Interfaces:**
- Consumes: Task 3's `Dispatcher`, `Gateway::{revoke, stop, stop_all}`, the harness; `Config::may_always_stop`; the policy plan's `cancel` input (`scope` `run` or `all`, `actor` `{kind: "human", login}` or `{kind: "label_removal"}`, `run.id`, `target.run`) and `resume` input (`scope: "all"`, `actor: {kind: "human", login}`); `kod_audit::{AuditLog::append, RUN_AUDIT}`.
- Produces: `/kod kill` and `/kod cancel` comment commands (`/kod resume` is in `COMMANDS` only to be refused); `Dispatcher::{with_cli_key(&str, &[u8]) -> Dispatcher, signed_command(&[u8], &str, i64) -> Result<String>}`; `command::{SIGNATURE, WINDOW, sign, verify}`; `POST /command` on the router (200, 403 refused, 500); `Unlabeled` and `Edited` deliveries handled by `work_once`. `kill` and `resume` are private: there is no entry point that takes a login from the caller.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-dispatcher/tests/common/mod.rs` with `git apply`:

````diff
diff --git a/crates/kod-dispatcher/tests/common/mod.rs b/crates/kod-dispatcher/tests/common/mod.rs
index daa763a..86a15d3 100644
--- a/crates/kod-dispatcher/tests/common/mod.rs
+++ b/crates/kod-dispatcher/tests/common/mod.rs
@@ -223,6 +223,7 @@ pub fn dispatcher(
         gw,
     )
     .unwrap();
+    let d = d.with_cli_key("kiennt23", OWNER_KEY);
     Arc::new(d)
 }
 
@@ -274,6 +275,17 @@ pub fn edited(t: &FakeTracker, delivery: &str, issue: &str) -> Webhook {
     t.webhook(&json!({"delivery": delivery, "type": "issue_edited", "issue": issue}))
 }
 
+pub fn said(t: &FakeTracker, delivery: &str, issue: &str, user: &str, body: &str) -> Webhook {
+    t.webhook(&json!({"delivery": delivery, "type": "comment", "issue": issue, "user": user, "body": body}))
+}
+
+/// A signed CLI request and its signature.
+pub fn cli(command: &str, by: &str, at: i64, key: &[u8]) -> (Vec<u8>, String) {
+    let body = json!({"command": command, "by": by, "at": at}).to_string();
+    let sig = kod_dispatcher::command::sign(key, body.as_bytes());
+    (body.into_bytes(), sig)
+}
+
 impl Harness {
     /// Receives the webhook and works the queue until it is empty.
     pub fn deliver(&self, w: &Webhook) {
@@ -335,6 +347,11 @@ impl Harness {
             Policy::load(&policy_dir()),
         )
     }
+
+    /// Delivers a signed comment from `user` on FAKE-1.
+    pub fn say(&self, delivery: &str, user: &str, body: &str) {
+        self.deliver(&said(&self.tracker, delivery, "FAKE-1", user, body));
+    }
 }
 
 /// Keeps `Tracker` in scope for tests that call it on the fake.
````

Create `crates/kod-dispatcher/tests/cancel.rs`:

````rust
mod common;

use common::*;
use kod_dispatcher::RunState;
use kod_policy::Policy;
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn a_tracker_failure_never_stops_the_stop() {
    let h = harness();
    h.running();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(
        h.count("SELECT done FROM deliveries WHERE delivery = 'd-2'"),
        1
    );
    // The ticket catches up once the tracker is back.
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(h.last_comment("FAKE-1").contains("removed by dev"));
}

#[test]
fn a_failed_stop_is_retried() {
    let h = harness();
    h.running();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    h.gw.fail_stop.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 1"),
        1
    );
}

#[test]
fn a_stop_is_never_given_up_and_raises_the_alarm_once() {
    let h = harness();
    h.running();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    // The credential goes at once, whatever the sandbox does.
    assert_eq!(h.gw.revoked(), ["run-1"]);
    for i in 1..=8 {
        h.d.work_at(NOW + 1000 * i).unwrap();
    }
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 0"),
        1
    );
    let alarms = h
        .tracker
        .comments("FAKE-1")
        .iter()
        .filter(|c| c.contains("has not confirmed the stop of run-1 after 5 attempts"))
        .count();
    assert_eq!(alarms, 1);
    let audit = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = audit
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%stop_overdue%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    h.gw.fail_stop.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 10_000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 1"),
        1
    );
}

#[test]
fn a_cancel_does_not_wait_behind_a_failing_event_for_its_ticket() {
    let h = harness();
    h.running();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.deliver(&edited(&h.tracker, "d-2", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    h.deliver(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
}

#[test]
fn a_cancel_jumps_the_queue() {
    let h = harness();
    h.running();
    // A label for another ticket is queued first, the cancel second.
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    h.d.receive(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0], ("run-1".to_owned(), "canceled"));
    assert_eq!(
        h.count("SELECT done FROM deliveries WHERE delivery = 'd-2'"),
        0
    );
}

#[test]
fn a_label_removal_needs_neither_the_policy_nor_the_audit_log() {
    let h = harness_with(|_| Err(anyhow::anyhow!("routing.json is missing")));
    h.queued("FAKE-1");
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("start")));
    lock.execute_batch("ROLLBACK").unwrap();
    // The cancel is recorded when the log takes it.
    let h = harness();
    h.running();
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%\"kind\":\"cancel\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_run_canceled_while_starting_is_stopped() {
    let h = harness();
    h.queued("FAKE-1");
    h.sql("UPDATE runs SET state = 'canceled' WHERE seq = 1");
    h.d.work_once().unwrap();
    assert!(h.gw.calls().is_empty(), "rechecked before the start");
}

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
    assert!(h.d.stopped("run-1").unwrap());
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
    h.say("c-1", "kiennt23", "/kod kill");
    assert_eq!(
        h.states(),
        [
            ("run-1".to_owned(), "canceled"),
            ("run-2".to_owned(), "canceled")
        ]
    );
    assert!(h.gw.calls().contains(&"stop_all".to_owned()));
    assert!(
        h.last_comment("FAKE-2")
            .contains("the kill switch, by kiennt23")
    );
    // Held across a restart: nothing new is admitted.
    let again = h.reopen();
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
    h.say("c-1", "KienNT23", "/kod kill");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls(), ["stop_all", "stop run-1"]);
    // Nobody else's does, and resume needs the policy.
    h.say("c-2", "lead", "/kod kill");
    assert!(
        h.last_comment("FAKE-1")
            .contains("Kod did not kill for lead")
    );
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("the policy is unavailable"), "{e}");
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn a_comment_can_kill_but_never_resume() {
    let h = harness();
    h.say("c-1", "kiennt23", "/kod kill");
    assert!(h.d.is_halted().unwrap());
    h.say("c-2", "kiennt23", "/kod resume");
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("resume is accepted only as a signed CLI request"),
        "{c}"
    );
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn a_stop_all_retry_does_not_outlive_a_resume() {
    let h = harness();
    h.running();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(h.d.signed_command(&body, &sig, NOW).is_err());
    let stop_alls = |h: &Harness| h.gw.calls().iter().filter(|c| *c == "stop_all").count();
    let tried = stop_alls(&h);
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW).unwrap();
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    // The kill's run is stopped by its own stop; the new run is not touched.
    assert_eq!(stop_alls(&h), tried);
    let calls = h.gw.calls();
    assert!(calls.contains(&"stop run-1".to_owned()));
    assert!(!calls.contains(&"stop run-2".to_owned()));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("did not confirm it stopped the runs"), "{e}");
    // The runs are canceled and new ones refused even so, and the stop is
    // tried again.
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.d.is_halted().unwrap());
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop_all' AND done = 1"),
        1
    );
}

#[test]
fn the_kill_switch_still_stops_the_sandboxes_when_the_store_fails() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("database is locked"), "{e}");
    lock.execute_batch("ROLLBACK").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW + 1, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW + 1).unwrap();
    assert!(h.d.is_halted().unwrap());
    assert!(h.gw.calls().contains(&"stop_all".to_owned()));
}

#[test]
fn a_tech_lead_uses_the_kill_switch_through_the_policy_and_cannot_resume() {
    let h = harness_with(|dir| {
        Policy::load(&policy_with_people(
            dir,
            json!({"owner": "kiennt23", "backup": null, "tech_leads": ["Lead"]}),
        ))
    });
    h.say("c-1", "mallory", "/kod kill");
    let c = h.last_comment("FAKE-1");
    assert!(c.contains("mallory may not use the kill switch"), "{c}");
    assert!(!h.d.is_halted().unwrap());
    h.running();
    h.say("c-2", "lead", "/kod kill");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    let (body, sig) = cli("resume", "lead", NOW, b"lead's guess");
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("lead has no CLI key"), "{e}");
    assert!(h.d.is_halted().unwrap());
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW).unwrap();
    assert!(!h.d.is_halted().unwrap());
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn a_tech_lead_cancels_one_run_through_the_policy() {
    let h = harness_with(|dir| {
        Policy::load(&policy_with_people(
            dir,
            json!({"owner": "kiennt23", "backup": null, "tech_leads": ["lead"]}),
        ))
    });
    h.running();
    h.say("c-1", "mallory", "/kod cancel");
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    h.say("c-2", "lead", "/kod cancel");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.last_comment("FAKE-1").contains("canceled by lead"));
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn other_comments_are_not_commands() {
    let h = harness();
    h.running();
    let w = said(&h.tracker, "c-1", "FAKE-1", "kiennt23", "please /kod kill");
    assert_eq!(h.d.receive(&w).unwrap(), kod_dispatcher::Received::Ignored);
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn a_signed_cli_request_is_checked_before_it_acts() {
    let h = harness();
    h.running();
    let try_it = |body: &[u8], sig: &str, now: i64| {
        format!("{:#}", h.d.signed_command(body, sig, now).unwrap_err())
    };
    let (body, _) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(try_it(&body, &"0".repeat(64), NOW).contains("the signature does not match"));
    let (body, sig) = cli("kill", "kiennt23", NOW, b"a guessed key");
    assert!(try_it(&body, &sig, NOW).contains("the signature does not match"));
    let (body, sig) = cli("kill", "mallory", NOW, OWNER_KEY);
    assert!(try_it(&body, &sig, NOW).contains("mallory has no CLI key"));
    let (body, sig) = cli("kill", "kiennt23", NOW - 301, OWNER_KEY);
    assert!(try_it(&body, &sig, NOW).contains("more than 300 s old"));
    assert!(!h.d.is_halted().unwrap());
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert_eq!(h.d.signed_command(&body, &sig, NOW).unwrap(), "killed");
    assert!(h.d.is_halted().unwrap());
    let (resume, rsig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    assert_eq!(h.d.signed_command(&resume, &rsig, NOW).unwrap(), "resumed");
    // A replay of the kill is refused.
    assert!(try_it(&body, &sig, NOW).contains("already used"));
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn the_command_route_answers_by_outcome() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let h = harness();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let post = |body: Vec<u8>, sig: &str| {
        let req = Request::post("/command")
            .header("x-kod-signature", sig)
            .body(Body::from(body))
            .unwrap();
        rt.block_on(kod_dispatcher::router(h.d.clone()).oneshot(req))
            .unwrap()
            .status()
            .as_u16()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (body, sig) = cli("kill", "kiennt23", now, OWNER_KEY);
    assert_eq!(post(body.clone(), "bad"), 403);
    assert_eq!(post(body.clone(), &sig), 200);
    assert_eq!(post(body, &sig), 403, "a replay");
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn owner_kill_stops_sandboxes_while_dispatcher_db_is_locked() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(h.d.signed_command(&body, &sig, NOW).is_err());
    assert!(
        h.gw.calls().contains(&"stop_all".to_owned()),
        "{:?}",
        h.gw.calls()
    );
}

#[test]
fn pending_label_does_not_block_the_same_tickets_cancel() {
    let h = harness();
    h.running();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    h.d.receive(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0].1, "canceled");
    *h.flaky.fail_issue.lock().unwrap() = None;
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.states().len(), 1);
}

#[test]
fn retrying_an_old_transition_does_not_undo_a_cancel() {
    let h = harness();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.running();
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.workflow("FAKE-1"), "Open");
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
}
````

Create `crates/kod-dispatcher/tests/review_edges.rs`:

````rust
mod common;

use common::*;
use kod_dispatcher::{Dispatcher, Gateway, RunStart, RunState};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

struct SlowKill {
    inner: Arc<FakeGateway>,
    first: AtomicBool,
    entered: mpsc::Sender<()>,
    release: (Mutex<bool>, Condvar),
}

impl Gateway for SlowKill {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.inner.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            let (lock, ready) = &self.release;
            drop(ready.wait_while(lock.lock().unwrap(), |v| !*v).unwrap());
            anyhow::bail!("stop_all timed out");
        }
        self.inner.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        self.inner.publish(run)
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(run)
    }
}

fn with_gateway(h: &Harness, gateway: Arc<dyn Gateway>) -> Arc<Dispatcher> {
    Arc::new(
        Dispatcher::open(
            kod_dispatcher::Config::parse(CONFIG).unwrap(),
            &h.db,
            kod_policy::Policy::load(&policy_dir()).map(Arc::new),
            Arc::new(kod_audit::AuditLog::open(&h.audit).unwrap()),
            h.flaky.clone(),
            gateway,
        )
        .unwrap()
        .with_cli_key("kiennt23", OWNER_KEY),
    )
}

#[test]
fn a_kill_finishing_after_resume_does_not_stop_new_runs() {
    let h = harness();
    h.running();
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    let kill_d = d.clone();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        kill_d.signed_command(&body, &sig, NOW)
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (body, sig) = cli("resume", "kiennt23", NOW + 1, OWNER_KEY);
    let resumed = d.signed_command(&body, &sig, NOW + 1);
    if let Err(e) = &resumed {
        assert!(
            e.to_string().contains("global stop is still in flight"),
            "{e:#}"
        );
        assert!(d.is_halted().unwrap());
    } else {
        d.receive(&label(&h.tracker, "new", "FAKE-2", "dev"))
            .unwrap();
        d.work_at(NOW + 1).unwrap();
    }
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    assert!(kill.join().unwrap().is_err());
    if resumed.is_err() {
        let (body, sig) = cli("resume", "kiennt23", NOW + 2, OWNER_KEY);
        d.signed_command(&body, &sig, NOW + 2).unwrap();
        d.receive(&label(&h.tracker, "new", "FAKE-2", "dev"))
            .unwrap();
        d.work_at(NOW + 2).unwrap();
    }
    let calls = h.gw.calls();
    let start = calls
        .iter()
        .position(|c| c.starts_with("start run-2 "))
        .unwrap();
    assert!(
        !calls[start + 1..].iter().any(|c| c == "stop_all"),
        "{calls:?}"
    );
}

#[test]
fn kill_revokes_before_waiting_for_global_stop() {
    let h = harness();
    h.running();
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let revoked = h.gw.revoked();
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    assert!(kill.join().unwrap().is_err());
    assert!(
        revoked.contains(&"run-1".to_owned()),
        "revoked when stop_all began: {revoked:?}"
    );
}

struct LostPublish(Arc<FakeGateway>);
impl Gateway for LostPublish {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.0.start(run)
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.0.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.0.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.0.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        let pr = self.0.publish(run)?;
        *self.0.published.lock().unwrap() = Ok(Some(pr));
        anyhow::bail!("the PR response was lost");
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.0.find_published(run)
    }
}

#[test]
fn a_canceled_publish_with_a_lost_response_still_marks_the_pr() {
    let h = harness();
    h.running();
    let d = with_gateway(&h, Arc::new(LostPublish(h.gw.clone())));
    let cancel_d = d.clone();
    let tracker = h.tracker.clone();
    *h.gw.on_publish.lock().unwrap() = Some(Box::new(move || {
        cancel_d
            .receive(&unlabel(&tracker, "cancel-publish", "FAKE-1", "dev"))
            .unwrap();
        cancel_d.work_at(NOW).unwrap();
    }));
    d.finished("run-1", RunState::Done, "tests pass").unwrap();
    let reopened = h.reopen();
    reopened.work_at(NOW + 1000).unwrap();
    assert!(h.gw.published.lock().unwrap().as_ref().unwrap().is_some());
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let marked: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%canceled_after_publish%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        marked, 1,
        "the canceled run's existing draft PR was never flagged"
    );
}

#[test]
fn a_canceled_publish_is_reconciled_after_a_crash_and_a_forge_outage() {
    let h = harness();
    h.running();
    h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
    h.sql(
        "INSERT INTO followups(kind,target,arg,next_at)
           VALUES('check_publish','run-1','',9223372036854775807)",
    );
    h.deliver(&unlabel(&h.tracker, "crash-cancel", "FAKE-1", "dev"));
    *h.gw.published.lock().unwrap() = Err("forge unavailable".into());
    let reopened = h.reopen();
    for attempt in 1..=7 {
        reopened.work_at(NOW + attempt * 1000).unwrap();
    }
    assert_eq!(
        h.count("SELECT done FROM followups WHERE kind = 'check_publish'"),
        0
    );
    *h.gw.published.lock().unwrap() = Ok(Some("https://github.example/acme/app/pull/9".into()));
    reopened.work_at(NOW + 10000).unwrap();
    assert!(
        h.last_comment("FAKE-1")
            .contains("pull/9 is not Kod's work: close it")
    );
    assert_eq!(
        h.count("SELECT done FROM followups WHERE kind = 'check_publish'"),
        1
    );
}

#[test]
fn resume_cannot_cross_an_inflight_stop_all_retry() {
    let h = harness();
    h.running();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    h.say("kill-before-retry", "kiennt23", "/kod kill");
    h.sql(&format!(
        "UPDATE followups SET next_at = {NOW} WHERE kind = 'stop_all'"
    ));
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    let retry_d = d.clone();
    let retry = std::thread::spawn(move || retry_d.work_at(NOW + 1000));
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (body, sig) = cli("resume", "kiennt23", NOW + 1, OWNER_KEY);
    let resumed = d.signed_command(&body, &sig, NOW + 1);
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    retry.join().unwrap().unwrap();
    assert!(
        resumed
            .unwrap_err()
            .to_string()
            .contains("global stop is still in flight")
    );
    let (body, sig) = cli("resume", "kiennt23", NOW + 2, OWNER_KEY);
    d.signed_command(&body, &sig, NOW + 2).unwrap();
    d.receive(&label(&h.tracker, "after-retry", "FAKE-2", "dev"))
        .unwrap();
    d.work_at(NOW + 2000).unwrap();
    assert_eq!(h.states()[1].1, "running");
}

struct LateStart {
    inner: Arc<FakeGateway>,
    cancel: Mutex<Option<Box<dyn Fn() + Send>>>,
    token_live: AtomicBool,
}

impl Gateway for LateStart {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)?;
        if let Some(cancel) = self.cancel.lock().unwrap().take() {
            cancel();
        }
        self.token_live.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)?;
        self.token_live.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.inner.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.inner.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        self.inner.publish(run)
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(run)
    }
}

#[test]
fn cancellation_during_start_revokes_a_token_registered_after_cancel() {
    let h = harness();
    h.queued("FAKE-1");
    let gateway = Arc::new(LateStart {
        inner: h.gw.clone(),
        cancel: Mutex::new(None),
        token_live: AtomicBool::new(false),
    });
    let d = with_gateway(&h, gateway.clone());
    let canceled = Arc::downgrade(&d);
    let tracker = h.tracker.clone();
    *gateway.cancel.lock().unwrap() = Some(Box::new(move || {
        let d = canceled.upgrade().unwrap();
        d.receive(&unlabel(&tracker, "cancel-start", "FAKE-1", "dev"))
            .unwrap();
        d.work_at(NOW).unwrap();
    }));
    d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0].1, "canceled");
    assert!(
        !gateway.token_live.load(Ordering::SeqCst),
        "the start registered a live token after the first revoke"
    );
}

#[test]
fn overdue_global_stop_alert_does_not_depend_on_audit() {
    const CHILD: &str = "KOD_TEST_STOP_ALARM_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let h = harness();
        h.running();
        rusqlite::Connection::open(&h.audit)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_alarm_audit BEFORE INSERT ON audit
             BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
            )
            .unwrap();
        h.gw.fail_stop_all.store(true, Ordering::SeqCst);
        h.say("alarm-kill", "kiennt23", "/kod kill");
        for attempt in 1..=8 {
            h.d.work_at(NOW + attempt * 1000).unwrap();
        }
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "overdue_global_stop_alert_does_not_depend_on_audit",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let alerts: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        alerts
            .iter()
            .filter(|e| e["kind"] == "stop_overdue" && e["call"] == "stop_all")
            .count(),
        1,
        "{alerts:?}"
    );
}
````

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p kod-dispatcher --test cancel --test review_edges`
Expected: FAIL to compile: no method named `with_cli_key` found for struct `Dispatcher`; no method named `signed_command` found for struct `Arc<Dispatcher>`; cannot find `command` in `kod_dispatcher`.

- [ ] **Step 3: Implement cancel, the commands, the kill switch and resume**

Create `crates/kod-dispatcher/src/command.rs`:

````rust
//! The signed CLI's requests: HMAC-SHA256 of the body under the signer's
//! key, hex in the `x-kod-signature` header.
use sha2::{Digest, Sha256};

pub const SIGNATURE: &str = "x-kod-signature";

/// How far, in seconds, a request's `at` may be from the dispatcher's clock.
pub const WINDOW: i64 = 300;

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
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

/// The signature the CLI sends for `body`.
pub fn sign(key: &[u8], body: &[u8]) -> String {
    hmac_sha256(key, body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compares in constant time.
pub fn verify(key: &[u8], body: &[u8], signature: &str) -> bool {
    let want = sign(key, body);
    want.len() == signature.len()
        && want
            .bytes()
            .zip(signature.bytes())
            .fold(0, |d, (a, b)| d | (a ^ b))
            == 0
}
````

Apply to `crates/kod-dispatcher/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-dispatcher/src/lib.rs b/crates/kod-dispatcher/src/lib.rs
--- a/crates/kod-dispatcher/src/lib.rs
+++ b/crates/kod-dispatcher/src/lib.rs
@@ -21,6 +21,7 @@ use kod_tracker::{Issue, State, TaskEvent, Tracker, Webhook};
 use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
 use serde_json::{Value, json};
 use sha2::{Digest, Sha256};
+use std::collections::HashMap;
 use std::{
     path::Path,
     sync::{Arc, Mutex, MutexGuard},
@@ -28,6 +29,7 @@ use std::{
 };
 use store::LIVE;
 
+pub mod command;
 pub mod config;
 pub mod store;
 
@@ -201,9 +203,32 @@ pub fn revision(i: &Issue) -> String {
 /// A queued event, as the store keeps it.
 #[derive(Clone, Debug, PartialEq)]
 enum Event {
-    Labeled { issue: String, by: String },
-    Unlabeled { issue: String, by: String },
-    Edited { issue: String },
+    Labeled {
+        issue: String,
+        by: String,
+    },
+    Unlabeled {
+        issue: String,
+        by: String,
+    },
+    Edited {
+        issue: String,
+    },
+    /// `/kod kill`, `/kod resume` or `/kod cancel` in a signed comment.
+    Command {
+        issue: String,
+        by: String,
+        command: String,
+    },
+}
+
+/// The commands a signed tracker comment can give.
+pub const COMMANDS: [&str; 3] = ["kill", "resume", "cancel"];
+
+fn command_in(body: &str) -> Option<String> {
+    let line = body.lines().next()?.trim();
+    let word = line.strip_prefix("/kod ")?.trim();
+    COMMANDS.contains(&word).then(|| word.to_owned())
 }
 
 impl Event {
@@ -220,8 +245,17 @@ impl Event {
             TaskEvent::Edited { issue } => Event::Edited {
                 issue: issue.clone(),
             },
-            // Comments are instructions for a live run: sub-plan 8.
-            TaskEvent::Commented { .. } => return None,
+            // The author is the tracker's, from the signed payload. Other
+            // comments are instructions for a live run: sub-plan 8.
+            TaskEvent::Commented {
+                issue,
+                author,
+                body,
+            } => Event::Command {
+                issue: issue.clone(),
+                by: author.trim().to_ascii_lowercase(),
+                command: command_in(body)?,
+            },
         })
     }
 
@@ -229,6 +263,7 @@ impl Event {
         match self {
             Event::Labeled { issue, .. } | Event::Unlabeled { issue, .. } => issue,
             Event::Edited { issue } => issue,
+            Event::Command { issue, .. } => issue,
         }
     }
 
@@ -246,6 +281,9 @@ impl Event {
             Event::Unlabeled { issue, by } => {
                 json!({"type": "unlabeled", "issue": issue, "by": by})
             }
+            Event::Command { issue, by, command } => {
+                json!({"type": "command", "issue": issue, "by": by, "command": command})
+            }
             Event::Edited { issue } => json!({"type": "edited", "issue": issue}),
         }
     }
@@ -268,6 +306,11 @@ impl Event {
             Some("edited") => Event::Edited {
                 issue: text("issue")?,
             },
+            Some("command") => Event::Command {
+                issue: text("issue")?,
+                by: text("by")?,
+                command: text("command")?,
+            },
             _ => bail!("unknown queued event {v}"),
         })
     }
@@ -314,6 +357,8 @@ pub struct Dispatcher {
     gateway: Arc<dyn Gateway>,
     db: Mutex<Connection>,
     global_stop: Mutex<()>,
+    /// The signed CLI's keys, by login.
+    cli_keys: HashMap<String, Vec<u8>>,
 }
 
 impl Dispatcher {
@@ -336,6 +381,7 @@ impl Dispatcher {
             gateway,
             db: Mutex::new(store::open(db)?),
             global_stop: Mutex::new(()),
+            cli_keys: HashMap::new(),
         };
         d.recover()?;
         Ok(d)
@@ -391,6 +437,13 @@ impl Dispatcher {
         self.follow_up(now())
     }
 
+    /// Adds a key for the signed CLI: `login` may send signed commands.
+    pub fn with_cli_key(mut self, login: &str, key: &[u8]) -> Dispatcher {
+        self.cli_keys
+            .insert(login.trim().to_ascii_lowercase(), key.to_vec());
+        self
+    }
+
     /// Checks the webhook and queues its event. Errors are store errors only;
     /// a failed write spends nothing, so the tracker's retry is queued.
     pub fn receive(&self, webhook: &Webhook) -> Result<Received> {
@@ -482,7 +535,9 @@ impl Dispatcher {
     fn handle(&self, id: &str, event: &str) -> Result<()> {
         match Event::decode(&serde_json::from_str(event)?)? {
             Event::Labeled { issue, by } => self.admit(id, &issue, &by),
-            Event::Unlabeled { .. } | Event::Edited { .. } => self.done(id),
+            Event::Unlabeled { issue, by } => self.unlabeled(id, &issue, &by),
+            Event::Edited { issue } => self.edited(id, &issue),
+            Event::Command { issue, by, command } => self.command(id, &issue, &by, &command),
         }
     }
 
@@ -940,6 +995,314 @@ impl Dispatcher {
         store::stopped(&self.db(), run_id)
     }
 
+    /// Cancels the ticket's live run, if any, and marks the delivery done, in
+    /// one transaction; the stop and the tracker calls follow it. Needs no
+    /// policy and no audit log: stopping is always safe. Returns the run.
+    fn cancel(
+        &self,
+        delivery: &str,
+        ticket: &str,
+        reason: &str,
+        by: &str,
+    ) -> Result<Option<String>> {
+        let ended = {
+            let mut c = self.db();
+            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
+            let live = [RunState::Queued, RunState::Running, RunState::Publishing];
+            let ended = match live_run(&tx, ticket)? {
+                Some((seq, _)) if Self::end(&tx, seq, RunState::Canceled, reason, &live)? => {
+                    Some(run_id(seq))
+                }
+                _ => None,
+            };
+            tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
+            // An older trigger must not restart work after this cancellation.
+            tx.execute(
+                "UPDATE deliveries SET done = 1 WHERE ticket = ?1 AND priority = 1 AND done = 0
+                 AND rowid < (SELECT rowid FROM deliveries WHERE id = ?2)",
+                params![ticket, delivery],
+            )?;
+            tx.commit()?;
+            ended
+        };
+        self.follow_up(now())?;
+        if let Some(run) = &ended {
+            // Recorded if the log takes it; the cancel never waits on it.
+            let _ = self.audit.append(
+                RUN_AUDIT,
+                &json!({"kind": "cancel", "run": run, "by": by, "reason": reason}),
+            );
+        }
+        Ok(ended)
+    }
+
+    fn unlabeled(&self, delivery: &str, ticket: &str, by: &str) -> Result<()> {
+        let reason = format!("the trigger label was removed by {by}");
+        self.cancel(delivery, ticket, &reason, by)?;
+        Ok(())
+    }
+
+    fn edited(&self, delivery: &str, ticket: &str) -> Result<()> {
+        let live = live_run(&self.db(), ticket)?;
+        let Some((_, old)) = live else {
+            return self.done(delivery);
+        };
+        let new = revision(&self.tracker.get_issue(ticket)?);
+        if new == old {
+            return self.done(delivery);
+        }
+        self.cancel(
+            delivery,
+            ticket,
+            &format!("the ticket changed (revision {old} to {new}); it is back in ready at the new revision"),
+            "a ticket edit",
+        )?;
+        Ok(())
+    }
+
+    /// Asks the policy whether a human may act, and records the decision.
+    fn human_may(&self, input: &Value) -> Result<()> {
+        let policy = self
+            .policy
+            .as_ref()
+            .map_err(|e| anyhow!("the policy is unavailable: {e}"))?;
+        let decision = policy.decide(input);
+        self.audit
+            .record_decision(input, &decision)
+            .context("the decision could not be recorded")?;
+        if !decision.allow {
+            bail!("{}", decision.reasons.join("; "));
+        }
+        Ok(())
+    }
+
+    /// A command from a signed comment. A refusal is commented on the ticket;
+    /// the delivery is done either way.
+    fn command(&self, delivery: &str, ticket: &str, by: &str, command: &str) -> Result<()> {
+        let r = match command {
+            "kill" => self.kill(by),
+            "resume" => Err(anyhow!("resume is accepted only as a signed CLI request")),
+            _ => self.cancel_by(delivery, ticket, by),
+        };
+        let mut c = self.db();
+        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
+        if let Err(e) = r {
+            follow(
+                &tx,
+                "comment",
+                ticket,
+                &format!("Kod did not {command} for {by}: {}.", scrub(&e)),
+            )?;
+        }
+        tx.execute("UPDATE deliveries SET done = 1 WHERE id = ?1", [delivery])?;
+        tx.commit()?;
+        drop(c);
+        self.follow_up(now())
+    }
+
+    /// `/kod cancel`: the owner or backup always; anyone else, such as a
+    /// tech lead, through the policy.
+    fn cancel_by(&self, delivery: &str, ticket: &str, by: &str) -> Result<()> {
+        let Some((seq, _)) = live_run(&self.db(), ticket)? else {
+            bail!("{ticket} has no live run");
+        };
+        if !self.config.may_always_stop(by) {
+            let run = run_id(seq);
+            self.human_may(&json!({
+                "action": "cancel",
+                "scope": "run",
+                "actor": {"kind": "human", "login": by},
+                "run": {"id": run},
+                "target": {"run": run},
+            }))?;
+        }
+        self.cancel(delivery, ticket, &format!("canceled by {by}"), by)?;
+        Ok(())
+    }
+
+    /// The kill switch: cancel every live run, stop every sandbox, and refuse
+    /// new runs until `resume`. `by` is an authenticated identity: the
+    /// author of a signed comment, or the signer of a CLI request. The
+    /// owner's or backup's works without the policy or the audit log;
+    /// anyone else's goes through both. The gateway's `stop_all` is called
+    /// even if the store fails.
+    fn kill(&self, by: &str) -> Result<()> {
+        if !self.config.may_always_stop(by) {
+            self.human_may(&json!({
+                "action": "cancel",
+                "scope": "all",
+                "actor": {"kind": "human", "login": by},
+            }))
+            .with_context(|| format!("{by} may not use the kill switch"))?;
+        }
+        let stop = self.global_stop.lock().unwrap_or_else(|e| e.into_inner());
+        let ended = (|| -> Result<(i64, Vec<String>)> {
+            let mut c = self.db();
+            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
+            tx.execute(
+                "INSERT INTO control (id, halted, by) VALUES (1, 1, ?1)
+                 ON CONFLICT (id) DO UPDATE SET halted = 1, by = ?1",
+                [by],
+            )?;
+            let ended: Vec<(i64, String)> = tx
+                .prepare(&format!(
+                    "UPDATE runs SET state = 'canceled', reason = ?1 WHERE state IN {LIVE} RETURNING seq, ticket"
+                ))?
+                .query_map([format!("the kill switch, by {by}")], |r| {
+                    Ok((r.get(0)?, r.get(1)?))
+                })?
+                .collect::<rusqlite::Result<_>>()?;
+            let runs: Vec<String> = ended.iter().map(|(seq, _)| run_id(*seq)).collect();
+            for (seq, ticket) in ended {
+                follow(&tx, "revoke", &run_id(seq), "")?;
+                follow(&tx, "stop", &run_id(seq), "")?;
+                follow(&tx, "transition", &ticket, State::Ready.name())?;
+                follow(
+                    &tx,
+                    "comment",
+                    &ticket,
+                    &format!(
+                        "Kod {} ended canceled: the kill switch, by {by}.",
+                        run_id(seq)
+                    ),
+                )?;
+            }
+            let epoch = Self::epoch(&tx)?;
+            tx.commit()?;
+            Ok((epoch, runs))
+        })();
+        let mut errors = vec![];
+        let epoch = match &ended {
+            Ok((epoch, _)) => Some(*epoch),
+            Err(_) => match Self::epoch(&self.db()) {
+                Ok(epoch) => Some(epoch),
+                Err(e) => {
+                    errors.push(format!("the kill epoch could not be read: {}", scrub(&e)));
+                    None
+                }
+            },
+        };
+        if let Ok((_, runs)) = &ended {
+            for run in runs {
+                if let Err(e) = self.gateway.revoke(run) {
+                    errors.push(format!(
+                        "the credential revoke for {run} failed: {}",
+                        scrub(&e)
+                    ));
+                }
+            }
+        }
+        let stopped = self.gateway.stop_all();
+        if self.config.may_always_stop(by) {
+            let _ = self
+                .audit
+                .append(RUN_AUDIT, &json!({"kind": "kill_switch", "by": by}));
+        }
+        if stopped.is_err()
+            && let Some(epoch) = epoch
+            && let Err(e) = follow(&self.db(), "stop_all", "", &epoch.to_string())
+        {
+            errors.push(format!(
+                "the global stop retry could not be queued: {}",
+                scrub(&e)
+            ));
+        }
+        drop(stop);
+        let _ = self.follow_up(now());
+        if let Err(e) = ended {
+            errors.push(format!("the store did not take it: {}", scrub(&e)));
+        }
+        if let Err(e) = stopped {
+            errors.push(format!(
+                "the gateway did not confirm it stopped the runs: {}",
+                scrub(&e)
+            ));
+        }
+        if !errors.is_empty() {
+            bail!("the kill switch is not complete: {}", errors.join("; "));
+        }
+        Ok(())
+    }
+
+    /// Lifts the kill switch: the owner or backup only, from a signed CLI
+    /// request (never a tracker comment), and through the policy. A stop of
+    /// every sandbox still waiting for a retry is dropped; the kill's runs
+    /// keep their own stops.
+    fn resume(&self, by: &str) -> Result<()> {
+        if !self.config.may_always_stop(by) {
+            bail!("only the owner or backup may resume");
+        }
+        let _stop = match self.global_stop.try_lock() {
+            Ok(guard) => guard,
+            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
+            Err(std::sync::TryLockError::WouldBlock) => {
+                bail!("a global stop is still in flight; retry resume after it returns")
+            }
+        };
+        self.human_may(&json!({
+            "action": "resume",
+            "scope": "all",
+            "actor": {"kind": "human", "login": by},
+        }))
+        .with_context(|| format!("{by} may not resume"))?;
+        self.db().execute(
+            "INSERT INTO control (id, halted, by) VALUES (1, 0, ?1)
+             ON CONFLICT (id) DO UPDATE SET halted = 0, by = ?1, epoch = epoch + 1",
+            [by],
+        )?;
+        Ok(())
+    }
+
+    /// A request from the signed CLI: a JSON body
+    /// `{"command": "kill" | "resume", "by": login, "at": unix seconds}`
+    /// signed with `by`'s key (`command::sign`). Refused unless the
+    /// signature matches, `at` is within `command::WINDOW` of `now`, and
+    /// the same body was never seen before.
+    pub fn signed_command(&self, body: &[u8], signature: &str, now: i64) -> Result<String> {
+        let v: Value = serde_json::from_slice(body).context("the request is not JSON")?;
+        let by = v["by"]
+            .as_str()
+            .context("the request has no by")?
+            .trim()
+            .to_ascii_lowercase();
+        let key = self
+            .cli_keys
+            .get(&by)
+            .with_context(|| format!("{by} has no CLI key"))?;
+        if !command::verify(key, body, signature) {
+            bail!("the signature does not match");
+        }
+        let at = v["at"].as_i64().context("the request has no at")?;
+        if now.abs_diff(at) > command::WINDOW as u64 {
+            bail!("the request is more than {} s old", command::WINDOW);
+        }
+        let fresh = self.db().execute(
+            "INSERT OR IGNORE INTO deliveries (id, delivery, ticket, event, priority, done)
+             VALUES (?1, 'cli', '', ?2, 0, 1)",
+            params![hex(&Sha256::digest(body)), v.to_string()],
+        );
+        let fresh = match fresh {
+            Ok(fresh) => fresh,
+            Err(e) if v["command"] == "kill" && self.config.may_always_stop(&by) => {
+                let stopped = self.kill(&by);
+                bail!(
+                    "the command could not be recorded: {}; emergency stop: {}",
+                    scrub(&e.into()),
+                    stopped.map_or_else(|e| scrub(&e), |()| "completed".to_owned())
+                );
+            }
+            Err(e) => return Err(e.into()),
+        };
+        if fresh == 0 {
+            bail!("the request was already used");
+        }
+        match v["command"].as_str() {
+            Some("kill") => self.kill(&by).map(|()| "killed".to_owned()),
+            Some("resume") => self.resume(&by).map(|()| "resumed".to_owned()),
+            _ => bail!("unknown command {}", v["command"]),
+        }
+    }
+
     pub fn is_halted(&self) -> Result<bool> {
         Self::halted(&self.db())
     }
@@ -975,9 +1338,12 @@ impl Dispatcher {
 
 /// The routes: `POST /webhook` answers 202 queued, 200 duplicate or
 /// ignored, 401 refused, 500 when the store fails (the tracker retries).
+/// `POST /command` takes a signed CLI request, its signature in
+/// `x-kod-signature`: 200 done, 403 refused or failed, with the reason.
 pub fn router(dispatcher: Arc<Dispatcher>) -> Router {
     Router::new()
         .route("/webhook", post(webhook))
+        .route("/command", post(cli))
         .with_state(dispatcher)
 }
 
@@ -1001,3 +1367,20 @@ async fn webhook(
         Ok(Err(_)) | Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "error".into()),
     }
 }
+
+async fn cli(
+    Shared(d): Shared<Arc<Dispatcher>>,
+    headers: HeaderMap,
+    body: Bytes,
+) -> (StatusCode, String) {
+    let signature = headers
+        .get(command::SIGNATURE)
+        .and_then(|v| v.to_str().ok())
+        .unwrap_or_default()
+        .to_owned();
+    match tokio::task::spawn_blocking(move || d.signed_command(&body, &signature, now())).await {
+        Ok(Ok(done)) => (StatusCode::OK, done),
+        Ok(Err(e)) => (StatusCode::FORBIDDEN, scrub(&e)),
+        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "error".into()),
+    }
+}
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS (kod-dispatcher cancel 24, admission 20, review_edges 7, gateway_contract 2, config 3, store 2; everything else unchanged), clean. The tests that lock the audit log or the store take about 2 s each (the busy timeout); the emergency CLI test can wait through three of them before reporting.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-dispatcher
git commit -m "kod-dispatcher: cancel on unlabel or edit, signed commands, the kill switch and resume"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc's run identity and lifecycle, rework and staleness, kill switch and action matrix sections, policy plan D14, the tool gateway plan and this plan, with these questions:
- Can any sequence of deliveries, replays, restarts and calls start two runs for one delivery, two live runs for one ticket, or more live runs than the WIP limit?
- Can a run's state move backward, or a canceled run reach `publishing`? Can a crash leave a run live forever?
- Does any external call (tracker, gateway) happen before the state change it follows is committed? Can a failure of one block a stop?
- Does the owner's or backup's kill switch depend on the policy, the audit log, the store or the gateway anywhere? Does a run's own cancel depend on the policy or the audit log? Does everyone else's kill switch depend on the policy?
- Is there any path that takes a human's login from an unsigned source, or resumes from anything but a signed CLI request?
- Can a stop or a revoke ever be given up, or a stale stop-all reach a run started after a resume?
- Is every refusal commented on the ticket with its reason, and every decision recorded before it takes effect?
- Do the policy inputs match the policy plan's `delegate`, `cancel` and `resume` contracts field for field?

## Self-review

- **Spec coverage.**
  - Webhook service with a DB table as its queue: Tasks 2 and 3 (SQLite, D1; retry and dead letter, D3).
  - Signature check and delivery dedup: Task 3 (`a_webhook_that_is_not_genuine_is_refused_and_never_queued`, `a_duplicate_delivery_never_starts_a_second_run`, `a_replay_under_a_new_unsigned_delivery_id_is_a_duplicate`).
  - Atomic WIP admission with the placeholder of 1: Task 3 (`wip_is_counted_and_reserved_in_the_admission`, `concurrent_admissions_respect_the_wip_limit`).
  - Forward-only states: Task 2 (`run_states_only_move_forward`) and Task 3 (`a_finished_run_is_published_then_done`).
  - Cancel on label removal or ticket edit: Task 4.
  - Kill switch and resume per D14, including the owner's kill switch when the policy fails: Task 4.
  - Overview exit criteria against the fakes:
    - refusals commented with the reason (`admission_refusals_are_commented_with_the_reason`);
    - no second run from a duplicate;
    - cancel or kill stops a queued or running run (`removing_the_label_cancels_the_live_run`, `removing_the_label_cancels_a_queued_run_before_it_starts`, `the_kill_switch_stops_queued_and_running_runs_and_holds`);
    - states only move forward.
- **Placeholders.** None.
- **Type consistency.** `Dispatcher`, `Gateway`, `RunStart`, `RunRow`, `RunState`, `Received`, `Config` and `Flaky` are spelled the same in every task. The code was compiled and tested in task order on top of sub-plans 3, 4, 6a, 2 and 5. Each task's tests were run against the previous task's code to see them fail. The plan text was replayed from scratch.
- **Review Focus.** Each line names its tests and task above.

## Open items

- **Who may stop and restart, and how they prove it** (owner, flagged as the default; D11):
  - Kill and a per-run cancel may come from any authenticated channel: a signed tracker comment or a signed CLI request.
  - Resume is accepted only as a signed CLI request from the owner or backup, through the policy.
  - A login the caller passes in is never trusted.
  - A comment's kill or cancel is checked against the policy and config by its tracker login. 6a's team mapping must keep tracker logins equal to GitHub logins, or 6b maps one to the other. Resume does not depend on this.
- **CLI key storage** (owner, 6b): where the owner's and backup's CLI keys live and how they rotate.
- **Naming the backup** (owner): the backup is `null` in `policy/people.json` and in the dispatcher config, so only the owner can resume today (with the owner's CLI key), and the owner is the only one whose kill switch works without the policy.
- **Re-running after an edit** (owner): an edit stops the run and returns the ticket to ready; a new label starts the next run (D10). Say if an edit should restart it automatically.
- **Bounces** (owner): counted as the ticket's runs that ended `blocked` until rework (sub-plan 7) counts rework requests. A failed publish ends `needs_human` and is not counted.
- **Rework requests across runs** (6b, sub-plan 7): `RunStart` carries no linked-ticket snapshot yet. The tool gateway's `Run::linked_tickets` starts empty in phase 1, and a run reaches only the rework requests it creates itself. Sub-plan 7 records the requests Kod creates so the next run's snapshot can include them.
- **The stop alarm** (owner, 6b): the alarm already emits structured JSON to host stderr independently of audit and tracker availability. The 6b service must preserve stderr in its host logs; routing that event to a pager is an operational choice.
- **Runs left `running` by a crash** (6b): the dispatcher resolves `publishing` runs on open; `running` runs need the sandbox state 6b owns.
