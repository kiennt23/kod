# Phase 1.6b-b: Connected host implementation plan

**Goal:** Compose the existing dispatcher, gateways, GitHub adapters and sandbox into one host process, with durable cleanup and real workflow sources. This plan can merge after [6b-a](2026-10-23-phase1-connected-github.md); Docker/live acceptance is the separate [6b-c plan](2026-10-23-phase1-connected-acceptance.md). Passing this plan does not complete 6b.

**Architecture:** `kod-host` is an assembly binary, not a new domain service. It mounts the existing routers, implements the dispatcher's `Gateway`, and supplies concrete `Workspace`, `Docs` and status-source adapters. The dispatcher database remains the only queue, run lifecycle and recovery store; the audit database remains append-only evidence. No third SQLite database. The overview's "no new components" means reuse these domain components; it does not prohibit the executable needed to compose their libraries.

**Base:** PR #30 merged at `9cbae9a`, then the merged 6b-a implementation. Resolve the active checkout with `git rev-parse --show-toplevel`; do not assume an absolute user path. Use a task-owned worktree and preserve unrelated work.

**Tech stack:** Existing Rust/Axum/Tokio/rusqlite/reqwest/policy/audit libraries. Reuse locked dependency versions. Add Tokio's `test-util` feature in dev-dependencies for deterministic deadlines. Approved document exports are local trusted inputs until sub-plan 9 supplies document tooling.

## Global Constraints

1. One host and dispatcher process, existing WIP policy. No scheduler, new queue, status database, UI or automatic merge. Gate scanners retain their separate host/daemon; no sandbox or shared-cache reuse for gates.
2. A run's dispatcher row is the sole durable stop flag. Extend its existing database with immutable execution bindings and request/write bookkeeping, never a second lifecycle state column. Keep the existing durable `revoke`, `stop`, `stop_all` and `check_publish` follow-ups and indefinite cleanup retries.
3. No execution resumes after restart. Retire queued/running executions, revoke their MCP/model/forge access and confirm owned containers gone before admission. `publishing` resolves Done only when the exact draft PR is found; otherwise NeedsHuman. Do not replay publication or reset the old run's spend.
4. The sandbox receives only opaque gateway tokens and history-free code. No GitHub/model key, host filesystem, `.git`, Docker socket or arbitrary egress. Base SHA, resource limits, immutable pins and run start time come from trusted host records.
5. Collect with the existing clean-base, networkless collector only after process quiescence. Require successful exit and `collect_ok`. Refuse binary files, symlinks, gitlinks, renames and file-mode changes from the raw patch before any Forge write. Preserve unchanged regular-file modes.
6. Keep exact admitted document revisions and classes. Logical links in `Issue.approved_versions` resolve through the approved manifest to revision IDs. MCP cannot silently read a newer revision; gates/status independently resolve current approvals. Issue URLs are identifiers, never fetch instructions.
7. Require audit intent before external writes. A failed audit, unknown source or pending uncertain create never permits a blind retry. Token renewal retains adapter identity and its uncertain-create guard; restart reconciliation uses durable write intents plus owned remote comments.
8. Runtime owner/backup kill remains authenticated and works during policy/audit outages. Revocation is initiated before waiting for stop. Failure to confirm stop or revoke remains pending and visible; tracker failure cannot delay it. Resume remains the existing signed CLI command through policy.
9. This PR builds code and deterministic non-Docker integration checks. It makes no live changes or paid model calls. Owner prerequisites below gate activation; 6b-c supplies the real-container and live exit evidence.

## Decisions and thresholds

| Decision | Implementation and pinned test |
| --- | --- |
| Model cancellation deadline | 2 seconds from local revocation to cancellation of queued work and dropping active upstream streams; `queued_and_streaming_calls_cancel_within_deadline`. No claim that a remote provider stops billing instantly. |
| Container stop | SIGTERM grace 5 seconds, then SIGKILL; confirm absence within 10 seconds total, otherwise return an error and retain the stop follow-up; `stop_timeout_stays_pending`. |
| Queue worker | Tick every 250 ms; `cancels_are_not_delayed_by_admission_tick`. Cancellation command handling is independent of this timer and of a blocked model call. |
| Status | Debounce 1 second after a committed change; reconcile all known stories every 30 seconds, including after startup; `missed_event_is_reconciled`. Sources are still read fresh for every gate decision. |
| Source reads | 10-second deadline, then refuse the affected admission/gate/status refresh; `source_timeout_never_becomes_current`. No stale-approved fallback. |
| Tests | Inject durations and clock internally. Pin production defaults in `default_deadlines_are_fixed`; use paused Tokio time for timing tests, barriers for races, and real Docker deadlines in 6b-c. |

## Why the existing stores are sufficient

`kod-dispatcher/src/store.rs` currently records ticket/revision/state and follow-up target/arg/retry. It does not record the admitted base SHA, immutable pins, collector location, a request's reserved spend or the identity of an uncertain status-comment create. Those facts are needed to recover, but none needs another run state machine.

Add these tables through an additive, transactional migration in the **existing dispatcher database**:

```sql
CREATE TABLE run_bindings (
    run_seq INTEGER PRIMARY KEY REFERENCES runs(seq),
    binding TEXT NOT NULL
);
CREATE TABLE model_requests (
    run_seq INTEGER NOT NULL REFERENCES runs(seq),
    request_id TEXT NOT NULL,
    reserved_tokens TEXT NOT NULL,
    reserved_cost TEXT NOT NULL,
    settled_usage TEXT,
    PRIMARY KEY (run_seq, request_id)
);
CREATE TABLE tracker_write_intents (
    operation_id TEXT PRIMARY KEY,
    story TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('status', 'rework')),
    audit_intent INTEGER,
    outcome TEXT,
    remote_id TEXT
);
```

`binding` is strict versioned JSON containing run identity, repo/base, start time, admitted classes/budget/pins/trusted rework links, image digests and host-owned output paths. It contains token hashes only. Reservation token/cost fields use validated decimal text to preserve existing unsigned ranges rather than truncating them to SQLite's signed integer. An unsettled model request remains charged at its reservation after a crash. `tracker_write_intents` is an operation outbox, not a cached workflow view. Use one unresolved status intent per story and serialize it with refresh/snapshot replacement.

Do not copy the existing follow-up queue into these tables. Cleanup continues through the existing follow-ups, whose `Gateway::revoke` enumerates the 6b-a broker's owner-only credential lease directory. Audit is a record of decisions/effects, not a mutable work queue. Atomicity is local to the dispatcher DB; no claim of a cross-database transaction: persist a pending write guard, append its audit intent, save the audit sequence, perform the call, then record outcome. A crash at any boundary requires reconciliation and never authorizes a repeat create.

## Owner prerequisites for activation

- Harness owner supplies the tracker/product repositories, GitHub App installation/key/bot ID, webhook secret, operator and backup CLI keys, committer identity and protected-ref rules. No PAT fallback for bot-owned comments.
- Owner supplies approved endpoint/model/data classes, prices and explicit per-run token/cost/resource/time caps. Placeholder policy values are not production clearance.
- Owner supplies image digests, trusted storage and network bindings, approved artifact exports and the separate gate-host integration. All are validated before admission; none is inferred from a ticket.
- Harness owner (the configured policy owner, currently Kien) creates and refreshes snapshots. The configured backup may perform the export/activation, but approval must already exist from the artifact's named approver. Operators do not create approval by exporting a file. The exact process is documented by 6b-c.

## Execution

| Task | Mode | Reason |
| --- | --- | --- |
| 1. Store migration and recovery bindings | Subagent + review | Owns ghost-run and durable spend guarantees |
| 2. Dynamic model lifecycle | Subagent + review | Owns cancellation and budget accounting |
| 3. Host config, runner and dispatcher bridge | Subagent + review | Owns isolation and start/cancel races |
| 4. Approved artifacts and trusted source records | Subagent + review | Owns approval, evidence and audience boundaries |
| 5. Service startup, refresh and shutdown | Subagent + review | Owns restart, emergency kill and uncertain writes |

Follow the existing per-task execution distinction in ADR 0001 (still Proposed), then one whole-branch review. Workers own named files and do not revert others. Each task includes its tests and must leave the workspace buildable.

## Review Focus

- Ghost run: a crash after registration or launch never leaves old execution or credentials usable.
- Spend: duplicate registration preserves the ledger; a failed reservation write prevents upstream calls; an interrupted request is not refunded.
- Collection: a forged success file, missing process gate, unsupported patch or wrong base cannot publish.
- Approvals: same link/new revision invalidates old pins; a live issue edit cannot expand a run's admitted docs/classes/rework scope.
- Recovery: an existing PR without immutable host bindings cannot acquire trusted gate evidence from its description.
- Kill: owner stop works while policy/audit/model/tracker is unavailable, and unconfirmed cleanup never reports success.

## Task 1: Extend dispatcher records and reuse its cleanup outbox

**Files:** modify `crates/kod-dispatcher/src/{store,lib}.rs`; add `src/bindings.rs` and `tests/connected_store.rs`. Read `src/followup.rs`, `tests/review_edges.rs`, and `tests/gateway_contract.rs` first.

- [ ] Write the failing migration/immutable-binding test. Add `store::bind_once(&Connection, run_id, &serde_json::Value)` and `store::binding(&Connection, run_id)` with these exact retry semantics:

```rust
use kod_dispatcher::store;
use serde_json::json;

#[test]
fn a_run_binding_is_immutable_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.db");
    let c = store::open(&path).unwrap();
    c.execute("INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#1','v1','running')", []).unwrap();
    let binding = json!({"version": 1, "base": "a".repeat(40)});
    store::bind_once(&c, "run-1", &binding).unwrap();
    store::bind_once(&c, "run-1", &binding).unwrap();
    assert!(store::bind_once(&c, "run-1", &json!({"version": 1, "base": "b".repeat(40)})).is_err());
    drop(c);
    let c = store::open(&path).unwrap();
    assert_eq!(store::binding(&c, "run-1").unwrap(), binding);
}
```

- [ ] Run `cargo test -p kod-dispatcher --test connected_store a_run_binding_is_immutable_across_reopen`; expect missing-API failure. Implement additive `CREATE TABLE IF NOT EXISTS` migration, `PRAGMA foreign_keys=ON`, strict `seq_of`, and insert-or-compare inside an IMMEDIATE transaction. `binding` returns an error for unknown IDs; no silent replacement. The low-level JSON helper checks object/version; the host validates the full binding shape before calling it.
- [ ] Add restart tests using the existing fake Gateway: a queued/running run becomes NeedsHuman with revoke+stop follow-ups in the same transaction. Keep publishing lookup semantics; an orphan branch with no confirmed PR remains NeedsHuman. Existing revokes/stops retry indefinitely, before tracker effects. Never run SQL that resets ended states or deletes dedup history.
- [ ] Add failed-reservation, unsettled-spend and uncertain-write-boundary tests against reopened DB files. Snapshot the old schema from current `store.rs` in `tests/fixtures/pre-connected.sql` and prove migration preserves runs, deliveries, control and pending cleanup. Do not edit its CHECK constraints when no new follow-up kind is required.
- [ ] Run `cargo test -p kod-dispatcher`; all old race and kill tests must remain green. Commit `Extend dispatcher records for connected recovery`. Save transcript under ignored `.omo/evidence/phase1-connected/host-store.log`.

## Task 2: Make model registrations dynamic and cancel queued/streaming calls

**Files:** modify `crates/kod-llm-proxy/src/{lib,budget,config}.rs`; add `src/lifecycle.rs`, `tests/lifecycle.rs`; reuse `tests/common/mod.rs` and `tests/gateway.rs`.

- [ ] Add public `RunRegistry` and `app_with_registry(Config) -> (Router, RunRegistry)`; keep `app(Config)` as a compatibility wrapper. `register(&self, token: &str, spec: RunSpec)` reuses an identical registration/ledger and rejects changed bindings, token collisions and tombstoned IDs. `revoke(&self, id: &str)` and `revoke_all` are idempotent. Define injectable cancellation signals and a host reservation-sink callback; no public HTTP registration route.
- [ ] Write red tests using the existing local model fixture and counters. Queue a second call behind a semaphore, hold the first response stream open, revoke, and assert both futures finish and no second upstream request occurs. Add a duplicate-registration test that spends near the cap before registering again and proves the cap cannot reset.
- [ ] Pin production limits in code and in a simple test:

```rust
pub const MODEL_CANCEL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

#[test]
fn default_model_cancellation_deadline_is_fixed() {
    assert_eq!(MODEL_CANCEL_DEADLINE.as_millis(), 2_000);
}
```

- [ ] Implement the cancellation check at token lookup, before/after permit acquisition, before forwarding and while streaming. Invoke the durable reservation sink before upstream dispatch, with a unique request ID; persist settlement only after confirmed usage. Failed/unknown settlement retains conservative charge. Use `tokio::select!` around pending upstream work and body polling; removing a map entry alone is insufficient because active calls hold `Arc<Run>`.

Use a Tokio watch channel with one irreversible false-to-true transition for each run. Wrap permit acquisition, request send and each stream poll with the same helper; it checks an already-canceled receiver before polling work:

```rust
use anyhow::{Result, bail};
use std::future::Future;
use tokio::sync::watch;

async fn until_revoked<T>(mut canceled: watch::Receiver<bool>, work: impl Future<Output = Result<T>>) -> Result<T> {
    if *canceled.borrow() { bail!("run revoked"); }
    tokio::select! {
        biased;
        _ = canceled.changed() => bail!("run revoked"),
        result = work => result,
    }
}

#[tokio::test(start_paused = true)]
async fn a_silent_upstream_is_canceled_without_waiting_for_data() {
    let (cancel, receiver) = watch::channel(false);
    let task = tokio::spawn(until_revoked(receiver, std::future::pending::<Result<()>>()));
    tokio::task::yield_now().await;
    cancel.send(true).unwrap();
    let result = tokio::time::timeout(MODEL_CANCEL_DEADLINE, task).await.unwrap().unwrap();
    assert!(result.is_err());
}
```

This unit test supplements the HTTP call-count/stream tests; it does not replace them or prove a provider stops billing. Settlement and required audit cleanup occur outside the dropped forwarding future.
- [ ] Run `cargo test -p kod-llm-proxy --test lifecycle` red then green, and `cargo test -p kod-llm-proxy`. Use a test clock to advance exactly 2 seconds for cancellation, not an unbounded sleep. Commit `Add dynamic model registration and cancellation`; evidence `host-model.log`.

## Task 3: Add strict host composition, the connected runner and runtime bridge

**Files:** root `Cargo.toml`; add `crates/kod-host/{Cargo.toml,src/{main,lib,config,deadlines,runner,workspace,runtime}.rs,tests/{config,runtime,workspace}.rs}`; extract narrow public helpers in `kod-eval/src/{run,sandbox,workspace}.rs` without changing offline behavior. Use the existing `kod-loop` binary, which already reads `KOD_MCP_URL`.

- [ ] Write config tests first. Commands are `kod-host check --config FILE`, `kod-host serve --config FILE`, and the bounded approval activation command specified in Task 4. `check` validates local inputs without model calls/external writes. `serve` uses only real adapters. The inert checked-in `config/connected.example.json` must refuse activation until its required values are supplied.
- [ ] Add a default-off `connected-test-support` Cargo feature enabling 6b-a's and kod-tracker's `test-support` features. It exposes a fixture constructor with loopback-only provider endpoints and injected clocks for integration tests. Add the equivalent gated fixture constructor to kod-tracker (its current private `with_api` cannot be used from another crate's tests). Normal `serve` has fixed GitHub origins and no arbitrary-origin config switch. The test build still uses real adapters; it changes only provider transport and fixtures. Document the explicit feature flag in test commands; 6b-c's Docker mode refuses execution in an ordinary production build.
- [ ] Config is strict JSON with sections `paths`, `github`, `model`, `networking`, `sandbox`, plus `dispatcher_config`. Required paths: `dispatcher_db`, `audit_db`, `policy_dir`, `run_root`, `mirror`, `approval_manifest`, `credential_dir`. GitHub section embeds 6b-a `github::Config` and `AppCredentials`, plus tracker repo, webhook secret file and agent identity; use its `RunPermit` for publish/recovery. Model config reuses existing endpoint/models/prices/limits/budget parsers. Networking supplies ingress/gateway listen addresses and accepted hosts. Sandbox supplies agent/collector digest-pinned images, language, memory/CPU/disk, max turns and wall seconds. Reject unknown/duplicate/missing keys, permissive secret paths, invalid caps and unpinned images. Plan 6b-c adds an optional `acceptance` section with stricter live-only authorization/caps; it is unnecessary for `serve`. `check` never calls 6b-a `verify_repository`, which mints a probe token; that check belongs to authorized activation.
- [ ] Keep all timing defaults in `src/deadlines.rs` and assert them in `default_deadlines_are_fixed`: model 2 s, term 5 s, absence 10 s, worker tick 250 ms, status debounce 1 s, reconciliation 30 s, source deadline 10 s. Tests inject a shorter clock internally; configuration cannot disable the safety deadline with zero/unbounded values.
- [ ] Before implementation, test the pure unsupported-change boundary with a proposed `workspace::supported_patch(&[u8]) -> anyhow::Result<()>` helper. It accepts supported UTF-8 regular-file addition/content/deletion patches and refuses unsupported headers **before applying** them in a separate clean collector workspace:

```rust
#[test]
fn unsupported_patch_intent_is_not_silently_lost() {
    for patch in [
        "diff --git a/x b/x\nold mode 100644\nnew mode 100755\n",
        "diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n",
        "diff --git a/x b/x\nnew file mode 120000\n",
        "diff --git a/x b/x\nGIT binary patch\n",
    ] {
        assert!(kod_host::workspace::supported_patch(patch.as_bytes()).is_err());
    }
}
```

- [ ] Reuse existing Docker arguments, network isolation, `process_gate`, clean-base collector and `collect_ok`. Do not parse arbitrary patch text into Forge changes on the host: validate shape, apply in the isolated collector workspace and read supported regular UTF-8 results through bounded host-owned output. Expose only this narrow run/collect API from `kod-eval`; do not invoke offline scoring or per-case model proxies. Test happy add/delete/mode preservation, wrong base, missing process gate, forged output and unsupported input with zero Forge calls.
- [ ] Implement all six synchronous dispatcher `Gateway` methods in `Runtime`; `start` saves one immutable binding, registers both gateways, rechecks stopped state, then launches background work. Inject `ANTHROPIC_BASE_URL` and `KOD_MCP_URL`, never keys. Pass a read-only dispatcher connection via `Adapters::stopped`. `finished(Done)` is allowed only after trusted collection, since it immediately publishes. Map Blocked/NeedsHuman/OverBudget explicitly. Invoke `check_absent_run` on the real bridge; repeated stop confirms the same container absent, never another run's container.
- [ ] Runtime cancellation revokes model/MCP/forge first, sends TERM, waits 5 s then KILL and confirms absent by 10 s. Keep per-run and global stop distinct. Test start/cancel barriers from `kod-dispatcher/tests/review_edges.rs` and prove failed stop remains a dispatcher follow-up. Run `cargo test -p kod-host --features connected-test-support --test config --test workspace --test runtime` red then green and `cargo test -p kod-eval -p kod-loop`. Commit `Compose the connected host and sandbox runtime`; evidence `host-runtime.log`.

## Task 4: Pin approved snapshots and produce authoritative gate/status records

**Files:** add `kod-host/src/{artifacts,records}.rs`, `tests/{artifacts,records}.rs`; update `kod-tool-gateway/src/lib.rs::Run/read_doc` and every test constructor that gains admitted pin fields. Read `kod-gates/src/{lib,records,github}.rs`, `kod-status/src/{records,lib}.rs` and their runbooks first.

- [ ] Specify the manifest and write red loader/authorization tests before adding the adapter:

```json
{"version":1,"artifacts":[{"link":"design","revision":"design-v3","sha256":"<64 lowercase hex>","class":"placeholder","path":"design-v3.txt","approved_by":"<named approver>","approved_at":"<UTC timestamp>","approval_ref":"<recorded sign-off ID>"}]}
```

The angle-bracket values above are required deployment values, not parser defaults. A test creates actual content, computes its SHA-256 and names a seeded approver in trusted policy data. Validate exact keys, unique link/revision, recorded approver, known classification, content hash and owner-only regular files below the snapshot root. Reject symlinks, path escape, empty sign-off reference and same-revision/different-content replacement. Stored approval references are operator-provisioned provenance until sub-plan 9, not cryptographic verification of Google Docs approval.

- [ ] Tests advance one logical link from v3 to v4. A run admitted at v3 can read only its immutable v3, while `inputs-current` compares its pins to current v4 and becomes stale. Removal/unknown links fail closed. Do not authorize reads using mutable `issue.approved_versions` alone; check the admitted set and data classes. Copying a revision string into issue metadata creates no approval.
- [ ] Implement `Docs` and gates `Artifacts` from this shared loader; add `kod-host snapshot activate --config FILE --manifest CANDIDATE`. Validate the staged directory with `check` first. Activation acquires existing gates lock and per-story refresh lock, invalidates affected checks through the existing gates event-order path, then atomically publishes the validated manifest/directory and schedules fresh source reads. Failure never leaves stale successful checks current. No direct `cp` over an active manifest and no source-document editing command.
- [ ] Produce gate `Evidence` from immutable pre-publish bindings and authoritative returned PR/repo/head, including run/story/team/pins/policy/runner versions. Recovery may repair missing evidence from those bindings and validated provider identity, never PR description. Missing bindings leave gates unverified. Build existing status `Snapshot` with fresh tracker/rework, current PR/check generation and approvals; write atomically, respect tracker audience, leave missing records unknown and fail on required source errors.
- [ ] Run `cargo test -p kod-host --features connected-test-support --test artifacts --test records` red then green, plus `cargo test -p kod-tool-gateway -p kod-gates -p kod-status`. Assert invalidation-before-replace, same-link revision change, stale-head/wrong-App check refusal and no fetched arbitrary URL. Commit `Bind connected reads and status to approved source records`; evidence `host-records.log`.

## Task 5: Mount routes, reconcile writes and prove safe startup/shutdown

**Files:** add `kod-host/src/{service,refresh,tracker_credentials}.rs`, `tests/{service,refresh}.rs`; add a refreshable credential-provider constructor in `kod-tracker/src/github.rs`, retaining static-token `GitHub::new` compatibility; narrowly expose the existing App-JWT signer from `kod-tool-gateway/src/github/auth.rs` through `AppCredentials`. Read current tracker status/rework uncertainty tests and `kod-status::refresh` audit ordering.

- [ ] Write a test that posts a status comment, loses the provider response, reconstructs the service, then refreshes twice. Assert exactly one POST, ownership by configured bot ID plus marker, and preserved human text. An absent/duplicate owned comment with unresolved intent blocks another create. A definite 4xx rejection can clear uncertainty; token renewal retains the adapter's instance guard. Rework uncertain creates also require reconciliation, not blind retry.
- [ ] Keep tracker-service credentials separate from 6b-a publish credentials. Reuse its RS256 signer as `AppCredentials::jwt(now: i64) -> anyhow::Result<Secret>`; do not request Issues permission through the publish Broker. The host tracker provider requests only the configured tracker repository ID with `issues:write` and implicit `metadata:read`, validates installation/App/bot/repository identity and renews before expiry. Persist its lease in a distinct owner-only tracker subdirectory; publisher leases remain under their own 6b-a lock. Reconcile/revoke both directories before startup admission and on host shutdown. A per-run stop revokes model/MCP/publish capabilities; it need not revoke the shared host-only tracker token used for cleanup comments. Add tracker repository/installation identity fields explicitly when tracker and product repositories differ; never silently reuse the product token for another repository.
- [ ] Persist the status write guard in `tracker_write_intents` before calling audited `kod_status::refresh`. Serialize the complete load/render/write cycle with snapshot activation. After restart, consult the operation guard and owned remote comments before allowing a create, independent of whether a failed audit-outcome append completed. Recompute status on startup and at 30 s intervals, so a dropped notification does not need a new queue. Do not store the rendered workflow as a new source of truth.
- [ ] Compose existing dispatcher ingress and gateway routers on separate bindings: only signed `/webhook` and `/command` may be exposed through public ingress; `/r/{token}/mcp` and model routes stay on the sandbox gateway network. Blocking adapters use `spawn_blocking` with provider read timeouts; kill handling cannot share a task or lock with a stuck model or status call. Keep `work_once`/follow-ups in their existing priority order.
- [ ] Before opening admission, acquire singleton ownership, reconcile orphan containers and broker leases, tombstone old registrations, run dispatcher restart recovery and confirm pending cleanup. Any unresolved cleanup keeps readiness false. Keep signed owner/backup kill usable during runtime audit/policy outage. Invalid startup config refuses activation; safety cleanup is attempted from validated durable identities before exit, without accepting arbitrary unauthenticated cleanup requests.
- [ ] Test source timeout/no stale fallback, second process refusal, old token denial, startup publishing with/without PR, source outage during kill, SIGTERM during publish, and failed revoke/stop. Readiness cannot report success while cleanup is pending. Run `cargo test -p kod-host --features connected-test-support --test service --test refresh` red then green. Then `cargo test --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`. Commit `Serve and recover the connected runtime`; evidence `host-service.log`.

## Exit criteria and handoff

This plan is independently mergeable when the production composition binary, real adapters and non-Docker failure/race tests pass, old crate tests still pass, a whole-branch review approves and the documented defaults are pinned. It does not require actual deployment secrets for local provider tests. Record commands and limitations in the PR.

6b-c then proves real containers and the live GitHub/model path. Missing owner prerequisites remain BLOCKED; do not label 6b complete or rewrite the phase-0 baseline status. The next docs PR updates component commands and the roadmap only to the level actually demonstrated.
