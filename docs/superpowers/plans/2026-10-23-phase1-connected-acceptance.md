# Phase 1.6b-c: connected acceptance and operating runbook

**Goal:** Prove the composed host with deterministic real-container tests, then record actual live conformance 2/4/7, cancellation, restart and kill results before enabling the connected pilot.
**Architecture:** Extend `kod-host` from plan B with one acceptance entry point. Exercise the production composition and inspect authoritative external outcomes; only the remote GitHub/model providers are simulated in Docker tests. Live acceptance uses real GitHub, the configured paid model and separate sandbox/gate hosts.
**Dependencies:** Merge `2026-10-23-phase1-connected-github.md` (A), then `2026-10-23-phase1-connected-host.md` (B). This is a separate PR, requires neither upstream drafting (9) nor a new persistence service, and leaves A/B independently usable through `check`/`serve`.
**Tech stack:** Existing Rust workspace, Tokio, axum, reqwest, serde_json, tempfile, SQLite and Docker. Reuse installed dependencies and the toolchain in `rust-toolchain.toml`.

## Global Constraints

- No third database: inspect B's existing dispatcher SQLite operational records and separate audit log. Evidence files are reports, never an alternative operational store.
- Real `kod-loop`, dispatcher, policy, audit, model/tool gateways, GitHub adapters, runner and collector execute in deterministic acceptance. Never replace them with fake `Gateway`, `Forge`, collector output or an agent's claimed success.
- Docker tests have no production credentials, paid provider traffic or GitHub writes. Their provider fixtures bind local test endpoints and cannot satisfy live acceptance.
- Live writes are limited to the explicitly authorized disposable repository. Never provision an App/repository, weaken policy, merge a PR, enable auto-merge or alter production rules as an acceptance side effect.
- Missing authorization, caps, credentials, approved inputs, host isolation or gate infrastructure means `BLOCKED`, exit 2, before paid calls or writes. A failed scenario means `FAIL`, exit 1. Only every required scenario passing means exit 0.
- Keep raw local evidence under ignored `.omo/evidence/phase1-connected/`, mode 0700, files 0600. Only curated, sanitized **actual live results** may enter `docs/evidence/<date>-6b-acceptance.json`; no fixture success, credentials, private source text or raw logs there.
- Preserve all current safety rules, no stale approval cache for gates, no interrupted execution restart, no refund of uncertain model reservations, no budget reset under an old run ID.
- CI remains `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`, and the existing cargo-deny action (`.github/workflows/ci.yml:6`).
- Use one logical change and passing checks per commit, plain repository-style messages. Do not change the historical plans' embedded code examples.

## Decisions

1. `kod-host accept --config FILE --evidence DIR --mode docker|live` is the sole acceptance entry point; `check` stays read-only. No implicit default mode. Acceptance uses B's loaded configuration and production service wiring. Docker provider overrides require B's default-off `connected-test-support` feature, which enables loopback-only fixture constructors in the real GitHub adapters; the ordinary production build refuses Docker mode. All Docker test invocations below include `--features connected-test-support` before `--test`; no runtime arbitrary-origin override is added to production `serve`.
2. Add a strict optional `acceptance` section: `disposable_repo`, `authorization_ref`, `expires_at`, `max_requests`, `max_tokens`, `max_cost_micro_usd`, `scenario_deadline_secs`. All are required when present. Live caps are positive and no higher than the configured model limits/budget; total run-suite consumption cannot exceed them. Authorization is owner-provided evidence covering this exact repository, provider/model, caps and drills, not a string invented by the executor. `authorization_ref` names an owner-only JSON file with `authorized_by`, `recorded_at`, `expires_at`, `repo`, `endpoint`, `model`, `max_requests`, `max_tokens`, `max_cost_micro_usd`, and `actions`; validate identity against configured owner/backup and require actions `issue`, `draft_pr`, `checks`, `comment`, `cancel`, `restart`, `kill`, `cleanup`. Its creation/approval is an owner prerequisite; file existence alone does not authorize the executor to invent its contents.
3. The report records mode, schema version, deployed commit, image digests, repository/App identity, authorization reference, configured/actual spend, and every named scenario's status. A missing observation is never converted to a pass. Record measured durations, issue/PR URLs, exact heads, audit sequence references and cleanup outcomes. Keep one suite-wide reservation ledger in the acceptance process, in addition to per-run gateway budgets; stop scheduling before the next worst-case reservation exceeds any authorized cap and conservatively charge uncertain requests.
4. Pinned timing: queued/streaming LLM revocation within 2 s; sandbox SIGTERM grace 5 s then SIGKILL; confirmed absence within 10 s; queue tick 250 ms; status debounce 1 s and reconciliation 30 s; source-read deadline 10 s. Use Tokio paused time for simulated timer boundaries; real Docker stop has a measured 10 s outer bound.
5. All interrupted executions end on restart. An interrupted `publishing` run becomes `done` only when the real PR is confirmed; otherwise `needs_human`. Lookup uncertainty never triggers a second publish or execution resume (`docs/dispatcher-runbook.md:48`).
6. Before upstream drafting (9), the harness owner exports already-approved immutable documents. Kien is the configured owner (`policy/people.json:2`); a configured backup may operate the host but cannot approve an artifact unless separately named as its approver. Operators never infer approval from file presence, issue prose or their operating role.
7. Implementation may merge with **live acceptance pending**. Neither 6b nor connected-pilot cutover is complete until all live rows pass and whole-branch review approves. Existing offline baseline requirements stay separate (`docs/phase1-runbook.md:73`).

## Affected users and acceptance gaps

| ID | Ideal state and present gap | Reason |
| --- | --- | --- |
| IS-1 / GAP-1 | Maintainers have deterministic proof of the actual ticket-to-container-to-draft-PR path; current component tests do not join that path. | Catch composition failures before deployment. |
| IS-2 / GAP-2 | Operators can prove revoked access, stopped sandboxes and conservative restart recovery; component mocks cannot prove Docker cleanup. | Prevent execution or credential use after cancellation. |
| IS-3 / GAP-3 | Named artifact approvers retain authority while operators can activate/refresh/roll back snapshots safely; no connected procedure exists. | Keep inputs current without fabricating approval. |
| IS-4 / GAP-4 | Owners can distinguish real live acceptance from blocked prerequisites and simulated evidence. | Prevent unsupported pilot cutover and unapproved spend. |

## Owner prerequisites

- Supply the disposable GitHub repository/base and recorded permission for issue/branch/draft-PR/check/comment writes and destructive test cleanup in that repository; do not reuse production tickets.
- Install the scoped Apps and supply secret-file references, installation/App/bot IDs, webhook secret, reachable signed webhook ingress and owner/backup command keys. Confirm reviewer topology and merge-queue support; preserve the owner-authored approval conflict until resolved (`docs/gates-runbook.md:192`).
- Supply the authorized model key, endpoint, pinned model/prices, cleared data classes, per-run and whole-suite positive request/token/cost caps, expiry and authorization record. No subscription credential in a sandbox.
- Name each artifact's actual approver and provide recorded sign-off for its exact source revision, approved requirements/design/threat-model/API text and classification. Snapshot export belongs to the harness owner; no drafting/Approvals API integration is added here.
- Provide digest-pinned sandbox/collector/gate images, sandbox network with verified egress restrictions, host resource limits, separate gate host/daemon inventory, package-only preparation egress and existing App-bound gate configuration.
- Supply host installation/configuration, ownership permissions, dispatcher/audit paths, checkpoint/recovery access and evidence visibility/retention choices. Missing items remain enumerated `BLOCKED`; the executor does not invent values.

## Execution

Per ADR 0001 (`docs/adr/0001-per-task-execution-mode.md:25`); safety tasks receive a fresh implementation/review pair and the branch receives one final review.

| Task | Mode | Depends on | Reason |
| --- | --- | --- | --- |
| 1. Acceptance entry point and truthful report | Subagent + review | A, B | Authorization, spend and evidence boundaries |
| 2. Deterministic connected scenarios | Subagent + review | 1 | Actual isolation and publication proof |
| 3. Cancel/restart/kill scenarios | Subagent + review | 1 | Revocation, cleanup and recovery guarantees |
| 4. Connected operating runbook | Native | 1; final names from 2/3 | Concrete commands and owner responsibilities |
| 5. Actual live acceptance and curated evidence | Subagent + review | 2, 3, 4; owner inputs | Deployed conformance and cutover decision |

Tasks 2/3 can run in parallel with disjoint test files; task 1 owns common harness/report files, subsequent edits coordinate ownership. Task 4 can draft in parallel. Critical path: 1 → 2/3 → 5. No broadening of A/B's implementation scope to repair acceptance symptoms; regressions get a focused fix and failing test in their owning code.

## Review Focus

- `missing_live_authorization_makes_zero_calls`: no paid request or GitHub mutation before complete prerequisites.
- `connected_conformance_2_4_7`: MCP endpoint used, planted secret denied and audited, exact collected diff published with story/agent trailers.
- `cancel_revokes_queued_and_streaming_requests`, `kill_confirms_container_absence`: no post-revocation forwards and no unconfirmed successful stop.
- `restart_never_resumes_and_reconciles_publish`: all interrupted executions terminal, existing PR reused, missing/uncertain PR never retried as a publish.
- `approval_refresh_invalidates_before_current_changes`: failed invalidation prevents activation; no stale pass after new approval or source failure.
- `only_actual_live_report_is_exportable`: raw messages, secrets and mock results cannot enter checked-in evidence.

## Task 1: Acceptance entry point, prerequisites and evidence contract

**Files:** Create `crates/kod-host/src/acceptance.rs`, `crates/kod-host/tests/acceptance.rs`; modify `crates/kod-host/src/{lib,main,config}.rs`, `crates/kod-host/tests/cli.rs`.
**References:** B's `Config`/composition entry points; `crates/kod-tool-gateway/src/lib.rs:129` secret/URL scrubbing; `crates/kod-tool-gateway/tests/publish.rs:193` credential-leak assertions; `docs/phase1-runbook.md:73` cutover distinction.
**Closes:** GAP-4. Do not add another command daemon, generic workflow engine or mock internal runtime.

- [ ] **Red:** Add command-line tests using `env!("CARGO_BIN_EXE_kod-host")`, plus pure report/prerequisite tests. New symbols below are this task's contract, not claims of existing APIs.

```rust
#[test]
fn missing_live_authorization_makes_zero_calls() {
    let report = preflight_report(Mode::Live, &missing_authorization_fixture());
    assert_eq!(report.exit_code(), 2);
    assert!(report.blocked.contains(&"authorization".to_owned()));
    assert_eq!(report.external_writes, 0);
    assert_eq!(report.model_requests, 0);
}
```

Run `cargo test -p kod-host --features connected-test-support --test acceptance missing_live_authorization_makes_zero_calls`; expect unresolved acceptance API, then the assertion to fail until implemented. Use an HTTP fixture counter in the CLI case to prove zero network mutations, rather than trusting report counters alone.

- [ ] **Green:** Implement strict config parsing and the bounded report types; reuse existing production preflight/ownership checks. Load/verify the explicit authorization record and all prerequisites before starting scenarios. Required live rows are registered before execution; exceptions mark affected rows failed and remaining rows blocked, then run cleanup.

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode { Docker, Live }
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Pass, Fail, Blocked }
pub struct Scenario { pub name: String, pub outcome: Outcome, pub observations: Vec<String> }
pub struct Report {
    pub mode: Mode,
    pub scenarios: Vec<Scenario>,
    pub blocked: Vec<String>,
    pub external_writes: u64,
    pub model_requests: u64,
}
impl Report {
    pub fn exit_code(&self) -> i32 {
        if self.scenarios.iter().any(|s| s.outcome == Outcome::Fail) { return 1; }
        if !self.blocked.is_empty() || self.scenarios.is_empty()
            || self.scenarios.iter().any(|s| s.outcome == Outcome::Blocked) { return 2; }
        0
    }
}
```

Add the deployed identity/digest/cap/observation fields from Decision 3 to this report. Implement `pub async fn run(config: &Config, evidence: &Path, mode: Mode) -> anyhow::Result<Report>` by composing production preflight, registered scenario execution and cleanup; do not leave a stub. Compare observed rows to the required scenario set before evaluating exit status so a missing row cannot pass. Serialize via an explicit allowlist schema; observations contain codes/counts/approved public references, never arbitrary provider errors. Atomically write `report.json`; reject symlink destinations and existing run directories to avoid overwriting evidence. Docker mode rejects non-test provider targets and real secret references. `Report::curated_live()` refuses Docker/partial reports.

- [ ] **Verify:** `cargo test -p kod-host --features connected-test-support --test acceptance --test cli`. Include missing/expired authorization, wrong repo/model, zero/overflow caps, duplicate keys, unsupported mode, malformed provider error containing a planted token, partial report, existing evidence directory and evidence-write failure. Include two simultaneous requests competing for the last suite allowance: at most one forwards. Expected: exits 2/1 as appropriate, no writes/calls when blocked, no secrets in output. Save `.omo/evidence/phase1-connected/task-1-contract.txt`.
- [ ] **Commit:** `git add crates/kod-host && git commit -m 'Add connected acceptance command and evidence contract'` after focused checks pass.

## Task 2: Deterministic Docker conformance, current inputs and publication

**Files:** Create `crates/kod-host/tests/{connected_docker.rs,connected_http.rs,common/connected.rs}`, `crates/kod-host/tests/fixtures/connected/{AGENTS.md,src/lib.rs,Cargo.toml}`; modify `.github/workflows/ci.yml`.
**References:** `crates/kod-eval/tests/kod_loop.rs:10` scripted model selection; `crates/kod-eval/tests/e2e/mod.rs:95` fixture repo; `crates/kod-tool-gateway/tests/publish.rs:18` publish assertions; `crates/kod-gates/tests/checks.rs:1` gate fixtures; `docs/kod-harness-design.md:365` checklist; `.github/workflows/ci.yml:15` Docker job.
**Closes:** GAP-1 and GAP-4. Mock only upstream GitHub/model HTTP; use real policy/audit/router/Docker/collector and compare provider-observed repository objects, not fixture-inserted success records.

- [ ] **Red:** Implement a test-owned `ConnectedFixture` which starts remote HTTP fixtures and launches the actual `accept --mode docker` binary with generated config. Provide valid signed GitHub deliveries and loopback-only provider endpoints. These new helpers belong to `common/connected.rs`.

```rust
#[tokio::test]
async fn connected_conformance_2_4_7() {
    let h = ConnectedFixture::docker().await;
    let report = h.accept("conformance-2-4-7").await;
    assert!(report.passed("mcp-tool-call"));
    assert!(report.passed("planted-secret-denied-and-audited"));
    assert!(report.passed("draft-pr-story-agent-trailers"));
    h.assert_remote_tree_matches_collected_change().await;
    h.assert_no_secret_in_diff_logs_or_audit().await;
}
```

Run `KOD_DOCKER_TESTS=1 cargo test -p kod-host --features connected-test-support --test connected_docker connected_conformance_2_4_7 -- --nocapture`; expect missing helper/scenario assertions before implementing. Fixtures drive an MCP read, attempted secret read, allowed edit and `finish`; host publication follows the real collector.

- [ ] **Green:** Add cases for valid/doubled signed deliveries, invalid signature, unauthorized delegation, unlisted egress, protected-path change, wrong/ended MCP/model token, audit outage, over-budget request and forged sandbox metadata. Assert exact branch/head/tree, draft flag, story ID and agent trailers, provider mutation counts and ordered audit intent/result. Verify collector cannot accept `.git` tampering, symlink escape or fabricated diff.

```rust
let before = h.current_approved_revision().await;
h.fail_gate_invalidation().await;
assert!(h.activate_newly_approved_snapshot().await.is_err());
assert_eq!(h.current_approved_revision().await, before);
h.restore_gate_updates().await;
h.activate_newly_approved_snapshot().await.unwrap();
h.assert_old_pr_and_candidate_inputs_pending_or_failed().await;
```

Also exercise changed head, new approved revision, unavailable/current-source read timeout and lost status-comment response. Status keeps one bot-owned comment after reconciliation; gates fail admission on source error and never consult a cached success. Execute real isolated gate containers on an isolated test daemon; do not label a shared developer daemon as production gate-only. Test-only daemon inventory is explicitly recorded as such.

- [ ] **Verify:** Run `cargo test -p kod-host --features connected-test-support --test connected_http`; build sandbox/collector images according to B and `evals/VERSIONS.md`; run `KOD_DOCKER_TESTS=1 cargo test -p kod-host --features connected-test-support --test connected_docker -- --nocapture`. Require report counts `executed > 0`, every required Docker scenario present and zero skipped. Without opt-in print `SKIPPED docker disabled`, never a passing acceptance report. Add the exact invocation and required image builds to CI. Save `.omo/evidence/phase1-connected/task-2-docker.json` and `task-2-http.txt`.
- [ ] **Commit:** `git add crates/kod-host/tests .github/workflows/ci.yml && git commit -m 'Exercise connected host conformance with real Docker'`.

## Task 3: Cancellation, crash recovery and kill drills

**Files:** Create `crates/kod-host/tests/{connected_lifecycle.rs,connected_timing.rs}`; extend only task 1's acceptance scenario dispatch with lifecycle cases.
**References:** `crates/kod-dispatcher/tests/cancel.rs:29` retrying failed stops; `crates/kod-tool-gateway/tests/publish.rs:211` cancel/publish ordering; `crates/kod-tool-gateway/tests/publish.rs:266` published lookup; `docs/dispatcher-runbook.md:28` signed command and recovery contract; B's runtime/recovery/timing APIs.
**Closes:** GAP-2. Do not use wall-clock sleep for simulated timing or mock Docker to claim container disappearance.

- [ ] **Red:** Test timers with B's injectable durations and Tokio `test-util`; use barriers for provider request admission, streaming, push and PR response. Define helper `LifecycleFixture` in this test file over `common::connected::ConnectedFixture`.

```rust
#[tokio::test(start_paused = true)]
async fn cancel_revokes_queued_and_streaming_requests() {
    let h = LifecycleFixture::with_blocked_stream_and_queued_request().await;
    h.cancel().await;
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    assert!(h.stream_ended().await);
    assert_eq!(h.forwarded_request_count(), 1);
    assert!(h.old_run_tokens_refused().await);
}
```

Run `cargo test -p kod-host --features connected-test-support --test connected_timing`; expect failing cancellation/deadline assertions before wiring scenarios. Include 250 ms worker wake, 1 s debounce, 30 s reconciliation and 10 s source timeout exact boundaries; assert no early action and action at the boundary.

- [ ] **Green:** Drive actual signed cancel, label removal and global kill while queued/running/collecting/publishing. Hold a real container ignoring SIGTERM; measure SIGKILL/absence, rejecting an unconfirmed stop. Kill the host process at barriers, restart against the same dispatcher/audit files, and inspect provider calls, Docker inventory and ended tokens.

```rust
let started = std::time::Instant::now();
h.kill_via_signed_owner_command().await;
h.wait_for_owned_container_absence().await;
assert!(started.elapsed() <= std::time::Duration::from_secs(10));
h.assert_no_running_containers_or_usable_tokens().await;
h.restart_same_database().await;
h.assert_interrupted_runs_terminal_without_new_model_calls().await;
```

Use independent runs for crash-before-push, lost-push-response, lost-PR-response, absent PR and failed PR lookup. Confirmed existing PR means `done`; absent/uncertain means `needs_human`; repeated restarts never add another PR or restart execution. Verify owner kill during policy/audit/tracker outage; pending cleanup survives until confirmed. Resume requires fresh authorized signature and does not revive old runs.

- [ ] **Verify:** `cargo test -p kod-host --features connected-test-support --test connected_timing`; `KOD_DOCKER_TESTS=1 cargo test -p kod-host --features connected-test-support --test connected_lifecycle -- --nocapture`. Save `.omo/evidence/phase1-connected/task-3-timing.txt` and `task-3-lifecycle.json`. Expected every case terminal, zero post-stop forwarding, zero abandoned containers, measured Docker stop ≤10 s; cleanup uncertainty fails and preserves retry intent.
- [ ] **Commit:** `git add crates/kod-host && git commit -m 'Verify connected cancellation and crash recovery'`.

## Task 4: Executable connected runbook and approval snapshot operations

**Files:** Create `docs/connected-runbook.md`; modify `README.md`, `docs/{phase1,dispatcher,tool-gateway,llm-gateway,gates,status,github-tracker}-runbook.md`, `docs/superpowers/plans/2026-10-14-phase1-overview.md`; create `crates/kod-host/tests/runbook.rs` to validate embedded examples.
**References:** `docs/gates-runbook.md:135` invalidate-before-replace ordering; `docs/status-runbook.md:59` serialized refresh; `docs/kod-harness-design.md:259` approval snapshot authority; `policy/people.json:2` owner; B's `snapshot activate` command.
**Closes:** GAP-3 and GAP-4. No Google Workspace drafting/approval capture or guessed approver identity.

- [ ] **Red:** `cargo test -p kod-host --features connected-test-support --test runbook`. Parse marked JSON examples with production `Config`/manifest parsers; run `kod-host check` against temporary owner-only fixture paths, never the real deployment. Assert each documented command is recognized by the actual parser and each internal Markdown link resolves. Expect absent runbook/examples to fail.
- [ ] **Green:** Write commands using these explicit operator-provided environment variables: `CONNECTED_CONFIG`, `CANDIDATE_CONFIG`, `SNAPSHOT_STAGE`, `APPROVAL_EXPORT`, `EVIDENCE_DIR`. Describe their provenance and permissions before invocation.

```sh
umask 077
install -d -m 700 "$SNAPSHOT_STAGE"
install -m 600 "$APPROVAL_EXPORT/approved.txt" "$SNAPSHOT_STAGE/approved.txt"
sha256sum "$SNAPSHOT_STAGE/approved.txt"
kod-host check --config "$CANDIDATE_CONFIG"
kod-host snapshot activate --config "$CONNECTED_CONFIG" --manifest "$SNAPSHOT_STAGE/manifest.json"
kod-host check --config "$CONNECTED_CONFIG"
kod-host serve --config "$CONNECTED_CONFIG"
```

The owner creates `manifest.json` as `{ "version": 1, "artifacts": [{ "link": "brief-link", "revision": "brief-rev-3", "sha256": "<64 hex from exported bytes>", "class": "<approved class>", "path": "<immutable text path relative to manifest directory>", "approved_by": "<named artifact approver>", "approved_at": "<recorded approval timestamp>", "approval_ref": "<recorded sign-off reference>" }] }`. The runbook's machine-tested example uses concrete fixture values; angle-bracket descriptions here describe provenance, never executable JSON values. Required links include all story requirements/design/threat-model/API inputs, not just this one illustrated row. `CANDIDATE_CONFIG` differs only in its staged approval-manifest reference.

Document the exact sequence: obtain newly recorded named-approver sign-off → export that revision → verify hash/class/provenance and owner-only files → validate staged config → activate through B's command. Activation takes the shared `.gates.lock` and story refresh lock, persists affected PR/candidate invalidations, then atomically swaps the immutable manifest and schedules reconciliation. Never `cp` over an active manifest. An unapproved edit is not a refresh; admission/gates cannot treat it as approved current content.

Rollback: stop admission with the existing signed `/command` kill request (`docs/dispatcher-runbook.md:40`); confirm containers/tokens ended; retain dispatcher/audit/evidence; select the previously retained immutable snapshot only if its approval remains valid and a new recorded rollback approval authorizes making it current; validate and reactivate through the same command. If approval was revoked/superseded and rollback is not approved, remain stopped. Restore the reviewed binary/config without downgrading database schema, run `check`, start `serve`, confirm recovery, then signed owner/backup resume. A backup can operate these steps but cannot grant the missing approval.

- [ ] **Verify:** `cargo test -p kod-host --features connected-test-support --test runbook`; execute the documented staging/activate/rollback sequence against the Docker fixture. Seed invalid provenance and gate-update failure: activation fails, previous manifest remains current, admission cannot use staged inputs. Update historical “library only” language with actual host commands; label 6b `implemented; live acceptance pending`, and preserve 6c's merged status. Save `.omo/evidence/phase1-connected/task-4-runbook.txt`.
- [ ] **Commit:** `git add README.md docs crates/kod-host/tests/runbook.rs && git commit -m 'Document connected host and approved snapshot operations'`.

## Task 5: Actual live acceptance and curated result

**Files:** Modify `docs/connected-runbook.md` with verified deployment commands; create `docs/evidence/<actual-UTC-date>-6b-acceptance.json` only after actual live execution; update the overview's 6b status only if all required live rows pass.
**References:** Tasks 1–4 contracts; `docs/kod-harness-design.md:369` items 2/4/7; `docs/gates-runbook.md:171` App-bound activation requirements; `docs/phase1-runbook.md:73` connected cutover.
**Closes:** GAP-4, and live proofs of GAP-1/2/3. Do not substitute mocked responses, manual assertions or unavailable inputs for a passing observation.

- [ ] **Red:** Extend `only_actual_live_report_is_exportable` in `tests/acceptance.rs` with fixture/Docker/partial/secret-bearing evidence; `cargo test -p kod-host --features connected-test-support --test acceptance only_actual_live_report_is_exportable` must reject each. This tests the export guard, not the live deployment.
- [ ] **Green:** Once owner inputs and concrete authorization exist, execute the commands below. `accept` creates/labels disposable issues and observes real signed webhook delivery; no direct call into dispatcher admission may stand in for that webhook. Actual model output must call MCP and produce the accepted change; failure to do so is a recorded failure within the suite cap.

```sh
kod-host check --config "$CONNECTED_CONFIG"
kod-host accept --config "$CONNECTED_CONFIG" --evidence "$EVIDENCE_DIR" --mode live
jq -e '.mode == "live" and .outcome == "pass" and (.scenarios | all(.outcome == "pass"))' "$EVIDENCE_DIR/report.json"
```

Required live scenarios: (1) actual tool call through MCP; (2) planted nonsecret sentinel denied as a secret and audited; (3) collected change opens a real draft PR with story ID, agent trailers and matching tree/head; (4) real head-bound gates and one current status comment; (5) newly recorded approval invalidates old PR/candidate checks before new current inputs; (6) cancel queued/streaming work and real container stop; (7) crash/restart with confirmed/absent PR outcomes and ended old access; (8) owner kill during failing dependencies, denied unauthorized command, explicit resume without old-run resurrection. Exercise an authorized real merge-queue candidate only where supported and owner-authorized; never remove that requirement or merge automatically to manufacture success. Missing gate/reviewer/queue prerequisites block acceptance.

Record immutable commit/image digests, App/bot identity, authorized limits and measured consumption, issue/PR URLs, exact SHAs, each scenario observation/result, audit verification references and measured shutdowns. Export through the allowlisted `Report::curated_live()` path to `EVIDENCE_DIR/curated-live.json`, inspect it for private fields, then copy only that artifact to the dated documentation path. Preserve failures locally, record missing prerequisites and leave status pending; never create a checked-in passing example in advance.

- [ ] **Verify:** `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test --all`; existing cargo-deny workflow; tasks 2/3 Docker commands; actual `accept --mode live` exit 0. One whole-branch review checks all evidence and owner constraints. Save `.omo/evidence/phase1-connected/task-5-live-review.txt`.
- [ ] **Commit:** After redaction review, `git add docs/evidence docs/connected-runbook.md docs/superpowers/plans/2026-10-14-phase1-overview.md && git commit -m 'Record actual connected acceptance results'`. If blocked, no passing evidence/complete status commit; implementation can merge with the blocked reasons documented.

## Independent exit criteria

| Ideal state | Proof | Evidence |
| --- | --- | --- |
| IS-1 | Tasks 2 and 5: real composed Docker suite and actual live items 2/4/7 | `task-2-docker.json`; dated curated live report |
| IS-2 | Tasks 3 and 5: deadlines, ended access, terminal restart states, confirmed Docker absence | `task-3-lifecycle.json`; dated curated live report |
| IS-3 | Tasks 2/4/5: named approval, failed activation preservation, current-input invalidation and rollback | `task-4-runbook.txt`; live approval scenario |
| IS-4 | Tasks 1/5: zero calls when blocked, export guard, all actual live rows pass | `task-1-contract.txt`; dated curated live report |

Implementation exit: task 1–4 code/docs/tests are complete, Docker scenarios actually ran, standard CI and one whole-branch review pass; explicitly retain `live acceptance pending` if task 5 is blocked. Full 6b exit: all task 5 live rows pass with sanitized evidence and review approval. This plan neither claims the separate offline baseline complete nor enables connected cutover merely because implementation merged.
