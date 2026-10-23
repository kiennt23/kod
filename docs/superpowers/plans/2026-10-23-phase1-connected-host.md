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
| 3. Strict host config and scaffold | Subagent + review | Owns local validation and composition |
| 4. Connected runner and trusted collection | Subagent + review | Owns process isolation and collection |
| 5. Dispatcher runtime bridge | Subagent + review | Owns registration and start/cancel races |
| 6. Approved artifacts and trusted source records | Subagent + review | Owns approval, evidence and audience boundaries |
| 7. Service startup, refresh and shutdown | Subagent + review | Owns restart, emergency kill and uncertain writes |

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

- [ ] Add `store::retire_active` and test it before the runtime uses it. Seed queued, running and publishing rows; call twice, assert only the first two become NeedsHuman and exactly two revoke plus two stop follow-ups exist. Publishing is untouched. Reuse the existing conditional retirement instead of adding another outbox format:

```rust
pub fn retire_active(c: &mut rusqlite::Connection, reason: &str) -> anyhow::Result<()> {
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let seqs = {
        let mut statement = tx.prepare("SELECT seq FROM runs WHERE state IN ('queued','running') ORDER BY seq")?;
        let rows = statement.query_map([], |r| r.get::<_,i64>(0))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        rows
    };
    for seq in seqs {
        crate::Dispatcher::end(&tx, seq, crate::RunState::NeedsHuman, reason,
            &[crate::RunState::Queued, crate::RunState::Running])?;
    }
    tx.commit()?;
    Ok(())
}
```

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

## Task 3: Add strict typed configuration and the host assembly scaffold

**Files:** root `Cargo.toml`; new `crates/kod-host/{Cargo.toml,src/{lib,main,config,deadlines}.rs,tests/config.rs}` and `config/connected.example.json`; modify `crates/kod-tracker/{Cargo.toml,src/{github,lib,strict_json}.rs}` and the 6b-a `crates/kod-tool-gateway/src/github/mod.rs` configuration derives. Depends on Tasks 1–2 and merged 6b-a. Task 7 owns the final service, not this scaffold.

**References:** `kod-llm-proxy/src/config.rs:99` (`parse_prices`), `:121` (`parse_runs`) and `:162` onward (`parse_paths`/`parse_limit`); `kod-llm-proxy/src/lib.rs:138` (`Limits`) and `:162` (`Config`); `kod-dispatcher/src/config.rs:55` (`Config::parse`); `kod-tracker/src/github.rs:99` (fixed-origin constructor); 6b-a Task 1 (`github::{Config,AppCredentials,RunPermit,GitHub::with_test_api}`). Do not claim the 6b-a implementation is already present on the base checkout.

- [ ] **Step 1 / Red:** add the workspace member and library skeleton, then these complete tests in `crates/kod-host/tests/config.rs`. `strict_json` is public so all trusted JSON loaders can reject duplicates before converting to `Value`; serde's `deny_unknown_fields` alone does not reject duplicate keys inside maps.

```rust
use kod_host::config::{FsPaths, strict_json};
#[test]
fn duplicates_unknown_and_missing_fields_are_errors() {
    assert!(strict_json::<serde_json::Value>(br#"{"model":{"x":1,"x":2}}"#).is_err());
    assert!(strict_json::<serde_json::Value>(br#"{"items":[{"x":1,"x":2}]}"#).is_err());
    assert!(strict_json::<FsPaths>(br#"{"dispatcher_db":"a","unknown":true}"#).is_err());
    assert!(strict_json::<FsPaths>(b"{}").is_err());
    assert!(strict_json::<serde_json::Value>(b"{} false").is_err());
    assert_eq!(strict_json::<serde_json::Value>(br#"{"a":[true,null,-2,1.25]}"#).unwrap()["a"][2], -2);
}
#[test]
fn default_deadlines_are_fixed() {
    use kod_host::deadlines::*;
    assert_eq!([MODEL_CANCEL, TERM_GRACE, ABSENCE, WORKER_TICK,
        STATUS_DEBOUNCE, RECONCILE, SOURCE].map(|d| d.as_millis()),
        [2_000, 5_000, 10_000, 250, 1_000, 30_000, 10_000]);
}
#[test]
fn secret_files_must_be_owner_only_regular_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("key");
    std::fs::write(&p, b"local test secret").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(kod_host::config::private_file(&p).is_ok());
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(kod_host::config::private_file(&p).is_err());
    let link = d.path().join("link"); symlink(&p, &link).unwrap();
    assert!(kod_host::config::private_file(&link).is_err());
}
```

Run `cargo test -p kod-host --test config`; expect missing exports before implementation, never record a passing baseline as the red result.

- [ ] **Step 2 / Implement:** use existing locked crates (`anyhow`, `serde`, `serde_json`, `tokio`, `axum`, `clap`, `reqwest`, `rusqlite`, `sha2`, `libc`; `tempfile` for tests). Add `tokio/test-util` only in dev-dependencies. Add `pub mod config; pub mod deadlines;` in `src/lib.rs`. In `src/config.rs`, reuse the existing duplicate-aware parser: make `strict_json::parse` public in kod-tracker and re-export it as `parse_json_unique` from its lib.rs. Then implement the host's typed conversion as follows; errors contain field names, never secret values.

```rust
use anyhow::{Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use std::{path::{Path, PathBuf}, os::unix::fs::MetadataExt};
pub fn strict_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value = kod_tracker::parse_json_unique(std::str::from_utf8(bytes)?)?;
    Ok(serde_json::from_value(value)?)
}
pub fn private_file(p: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(p)?;
    ensure!(m.is_file() && !m.file_type().is_symlink(), "secret is not a regular file");
    // SAFETY: geteuid has no arguments or preconditions.
    ensure!(m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "secret must be owner-only");
    Ok(())
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsPaths {
    pub dispatcher_db: PathBuf, pub audit_db: PathBuf, pub policy_dir: PathBuf,
    pub run_root: PathBuf, pub mirror: PathBuf, pub approval_manifest: PathBuf,
    pub credential_dir: PathBuf,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsGithub {
    pub product: kod_tool_gateway::github::Config,
    pub app: kod_tool_gateway::github::AppCredentials,
    pub tracker: kod_tracker::github::Config,
    pub tracker_repository_id: u64, pub tracker_installation_id: u64,
    pub mapping: PathBuf, pub webhook_secret_file: PathBuf,
    pub owner_key_file: PathBuf,
    #[serde(deserialize_with = "required_nullable")]
    pub backup_key_file: Option<PathBuf>,
    pub agent_name: String, pub agent_email: String,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsModel {
    pub endpoint_name: String, pub upstream: String, pub api_key_file: PathBuf,
    pub models: Vec<String>, pub selected_model: String, pub prices: PathBuf,
    pub allowed_paths: String, pub max_tokens: u64, pub max_body: usize,
    pub max_requests: u64, pub max_concurrent: usize,
    pub run_max_tokens: u64, pub run_max_cost_micro_usd: u64,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsNetworking {
    pub ingress_listen: std::net::SocketAddr, pub gateway_listen: std::net::SocketAddr,
    pub accepted_hosts: Vec<String>, pub sandbox_network: String, pub sandbox_gateway_url: String,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsSandbox {
    pub agent_image: String, pub collector_image: String, pub language: String,
    pub memory: String, pub cpus: f64, pub disk: String,
    pub max_turns: u32, pub wall_seconds: u64,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FsConfig {
    pub paths: FsPaths, pub github: FsGithub, pub model: FsModel,
    pub networking: FsNetworking, pub sandbox: FsSandbox, pub dispatcher_config: PathBuf,
}
fn required_nullable<'de,D,T>(d: D) -> std::result::Result<Option<T>,D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> { Option::<T>::deserialize(d) }
```

The two 6b-a provider structs need `#[derive(Clone, Deserialize)]` and `#[serde(deny_unknown_fields)]` if 6b-a has not added them. `AppCredentials.private_key` remains a path; do not serialize/debug PEM content. `FsConfig` is the file format, not the validated runtime object. The required-nullable deserializer makes an omitted `backup_key_file` fail while explicit null is accepted; pin both cases in tests. No optional acceptance field is introduced here; 6b-c adds its strict typed schema explicitly.

Implement `FsConfig::load(path: &Path) -> Result<Self>` as bounded read (1 MiB), `strict_json`, then `validate_local(&self) -> Result<()>`. The latter uses `Config::load(dispatcher_config)`; rejects product/dispatcher repo mismatch; validates positive tracker IDs, bot identity and App configuration through 6b-a's extracted pure `Config::validate` helper, shared with its constructor; validates every positive cap with existing `parse_limit` and `MAX_COST_MICRO_USD`; uses `parse_paths`, `parse_prices` (duplicate-check file first), and a synthetic `parse_runs` entry to validate budget/classes shape. Require selected model in `models`, nonempty unique models, HTTPS upstream without credentials/query/fragment, and exact image suffix `@sha256:` plus 64 lowercase hex characters. Validate finite positive CPU, `parse_size(disk)>0`, and memory greater than `2*disk + HOME_SIZE` with checked arithmetic. Validate wall/max-turns nonzero. `check` loads policy/mapping/artifact syntax but makes zero network calls, creates no DB, takes no lease lock, instantiates no provider, and never calls `verify_repository`.

All configured paths are absolute; canonicalize existing paths and reject symlink components before use. For absent DB leaves validate the canonical owner-only parent; refuse DB equality after canonicalization and a DB below agent-visible directories. Require owner UID and mode 0700 for run/credential roots, owner-only regular files for manifest, provider/CLI/webhook secrets, and private parent directories preventing replacement. Read through `OpenOptionsExt::custom_flags(libc::O_NOFOLLOW)`, then validate descriptor metadata before reading; `private_file` is the preliminary diagnostic, not a race-safe read substitute. Run subdirectories use `create_dir` and mode 0700; no reuse. Keep `agent_out` and `collect_out` disjoint, never mount root/credential/policy/DB paths into the sandbox.

In `src/deadlines.rs` define `pub const` `Duration::from_millis` values corresponding exactly to the test. Defaults are not user-configurable. Inject clocks/durations only through internal/test constructors.

- [ ] **Step 3 / Fixture transport:** `[features] default=[]; connected-test-support=["kod-tool-gateway/test-support","kod-tracker/test-support"]`. In tracker add `[features] default=[]; test-support=[]` and the following complete associated method in `impl GitHub`; production `new` retains its existing fixed origin even in a feature-enabled binary.

```rust
#[cfg(any(test, feature = "test-support"))]
pub fn with_test_api(config: Config, mapping: Mapping, token: &str, secret: &str, api: &str) -> Result<Self> {
    let u = reqwest::Url::parse(api)?;
    let ip = u.host_str().and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok());
    ensure!(u.scheme() == "http" && ip.is_some_and(|ip| ip.is_loopback())
        && u.port().is_some() && u.username().is_empty() && u.password().is_none()
        && u.path() == "/" && u.query().is_none() && u.fragment().is_none(), "test API must be literal loopback");
    Self::with_api(config, mapping, token, secret, api.trim_end_matches('/'))
}
```

Expose host `#[cfg(feature="connected-test-support")] pub mod test_support` only when a caller exists; its constructor is `provider_adapters(c: &FsConfig, product_api: &str, tracker_api: &str, clock: github::TestClock, tracker_token: &str, webhook_secret: &str) -> Result<(Arc<github::GitHub>, Arc<kod_tracker::github::GitHub>)>`. Its entire wiring is `GitHub::with_test_api(c.github.product.clone(), c.github.app.clone(), &c.paths.credential_dir, product_api, clock)` and tracker `with_test_api(c.github.tracker.clone(), Mapping::load(&c.github.mapping)?, tracker_token, webhook_secret, tracker_api)` (`Mapping::load(&Path)` is public at `kod-tracker/src/mapping.rs:25`). Both objects are real adapters. Use 6b-a's RSA test key and provider router fixture only if it is exported; otherwise copy that test-only fixture into `kod-host/tests/common/provider.rs`, not a nonexistent `Fixture` import. No production origin override, mock dispatcher, or fake model registry.

- [ ] **Step 4 / Acceptance / QA:** add full valid config fixtures in `tests/config.rs` from the exact struct fields above using temp paths, mode 0600 secrets and mode 0700 directories; seed policy from `../../policy` and mapping from `../../registry/trackers/github.json` (relative to the host crate). Assert nested duplicate/unknown/missing fields, missing optional-null key, bad digest, zero budget, symlink ancestry, readable-by-group key and same DB paths all fail with zero HTTP requests and no DB created. Run `cargo test -p kod-host --features connected-test-support --test config`, `cargo test -p kod-tracker --features test-support`, and `cargo check -p kod-host --no-default-features`. `kod-host check --config config/connected.example.json` must exit nonzero with a missing owner-value/path diagnostic. Until Task 7, `serve` returns a clear unavailable error before effects; never make a scaffold falsely report serving.
- [ ] **Step 5 / Commit:** `Validate connected host configuration` — Task 3 files only. Save red/green commands and feature-off check in `.omo/evidence/phase1-connected/host-config.log`.

## Task 4: Add a connected runner and trusted collection boundary

**Files:** new `crates/kod-host/src/{runner,workspace}.rs`, `tests/{runner,workspace}.rs`; new narrow `crates/kod-eval/src/{connected,export}.rs` exported from `lib.rs` and `src/bin/kod-collect-export.rs`; change `images/sandbox/{entrypoint.sh,Dockerfile}` only to add/build the connected collector-export mode; preserve offline `agent`, `collect`, `score` behavior. Depends on Task 3. No scoring, per-case proxy, provider key, `.git` history or arbitrary egress.

**References:** `kod-eval/src/run.rs:901` (`launch`), `:716` (`collect`), `:752` (`process_gate`); `sandbox.rs:40` (`run_args`); `workspace.rs:41` (`extract`) and `:63` (`prepare`); `runner.rs:88` (`env`) and `:118` (`end_state`); `images/sandbox/entrypoint.sh:95` (networkless clean-base collect); `kod-tool-gateway/src/forge.rs:13` (`Change`) and `:21` (`Collected`). Existing `launch` returns `Exited` for nonzero container exits other than 125 and has a 60-second timeout grace: do not use it unchanged for connected execution.

- [ ] **Step 1 / Red:** add these tests in `tests/workspace.rs`; they require no unspecified harness or Docker. Also add a nonzero container-exit test around the new connected process driver; a fake `collect_ok` cannot override its failure.

```rust
use kod_host::workspace::supported_patch;
#[test]
fn unsupported_patch_intent_is_not_silently_lost() {
    for body in ["old mode 100644\nnew mode 100755\n", "rename from x\nrename to y\n",
        "copy from x\ncopy to y\n", "new file mode 120000\n", "new file mode 160000\n",
        "GIT binary patch\n", "Binary files a/x and b/x differ\n"] {
        assert!(supported_patch(format!("diff --git a/x b/x\n{body}").as_bytes()).is_err());
    }
    assert!(supported_patch(b"diff --git a/x b/x\nindex aaa..bbb 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n").is_ok());
    assert!(supported_patch(b"diff --git a/x b/x\nindex aaa..bbb 100755\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").is_ok());
    assert!(supported_patch(&[0xff]).is_err());
    assert!(supported_patch(b"\0").is_err());
}
#[test]
fn process_evidence_and_collection_cannot_be_forged_by_outcome() {
    use kod_host::runner::ready_to_collect;
    assert!(ready_to_collect(true, Some("0"), Some("0")).is_ok());
    assert!(ready_to_collect(false, Some("0"), Some("0")).is_err());
    assert!(ready_to_collect(true, Some("0"), None).is_err());
    assert!(ready_to_collect(true, Some("0"), Some("1")).is_err());
    assert!(ready_to_collect(true, Some("1"), Some("0")).is_err());
}
```

Run `cargo test -p kod-host --test workspace`; expect missing symbols. Implement in `src/workspace.rs` and `src/runner.rs` respectively:

```rust
pub fn supported_patch(bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::ensure;
    ensure!(bytes.len() <= kod_tool_gateway::MAX_PUBLISH_BYTES * 4, "patch too large");
    let text = std::str::from_utf8(bytes)?;
    ensure!(!text.contains('\0'), "binary patch");
    let mut headers = true;
    for line in text.lines() {
        if line.starts_with("diff --git ") { headers = true; continue; }
        if line.starts_with("@@ ") { headers = false; continue; }
        if !headers { continue; }
        ensure!(!["old mode ", "new mode ", "rename from ", "rename to ",
            "copy from ", "copy to ", "GIT binary patch", "Binary files "]
            .iter().any(|prefix| line.starts_with(prefix)), "unsupported patch header");
        if let Some(mode) = line.strip_prefix("new file mode ") {
            ensure!(mode == "100644", "unsupported new file mode");
        }
        if let Some(mode) = line.strip_prefix("deleted file mode ") {
            ensure!(matches!(mode, "100644" | "100755"), "unsupported deleted file mode");
        }
        if line.starts_with("index ") {
            if let Some(mode) = line.split_whitespace().nth(2) {
                ensure!(matches!(mode, "100644" | "100755"), "unsupported indexed mode");
            }
        }
    }
    Ok(())
}
pub fn ready_to_collect(container_success: bool, exit: Option<&str>, procs: Option<&str>) -> anyhow::Result<()> {
    anyhow::ensure!(container_success && exit.map(str::trim) == Some("0"), "agent did not exit successfully");
    kod_eval::run::process_gate(procs).map_err(anyhow::Error::msg)
}
```

- [ ] **Step 2 / Connected process API:** put these exact host-owned types and complete spawn-gate implementation in `kod-eval/src/connected.rs`. They are independent of offline `Settings`/score. `SpawnGate` is shared with Runtime Task 5. It is a process-local one-way admission gate, never the durable stop flag. The stopped callback still reads the dispatcher row immediately before every spawn. Run-specific cancellation uses an `AtomicBool`; the dispatcher row remains authoritative.

```rust
use anyhow::{Result, ensure};
use std::{path::PathBuf, process::{Child, Command}, sync::Mutex};
#[derive(Default)]
pub struct SpawnGate(Mutex<bool>);
impl SpawnGate {
    pub fn close(&self) { *self.0.lock().unwrap_or_else(|e| e.into_inner()) = true; }
    pub fn spawn(&self, command: &mut Command, stopped: impl FnOnce() -> Result<bool>) -> Result<Child> {
        let closed = self.0.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(!*closed && !stopped()?, "launch admission closed");
        Ok(command.spawn()?)
    }
}
pub struct ConnectedSpec {
    pub run: String, pub base: String, pub mirror: PathBuf, pub dir: PathBuf,
    pub agent_image: String, pub collector_image: String, pub network: String,
    pub model: String, pub max_tokens: u64, pub budget: crate::case::Budget,
    pub prompt: String, pub model_url: String, pub mcp_url: String,
}
pub struct ProcessExit { pub success: bool, pub stopped: bool, pub timed_out: bool }
```

Implement `connected::agent_args(spec: &ConnectedSpec, host_ids: (u32,u32)) -> Result<Vec<String>>` by extracting the existing `sandbox::run_args` common argument builder to accept `&Budget` (retain its existing public wrapper for offline callers). Construct mounts `seed -> /seed:ro`, `in -> /in:ro`, `agent_out -> /out`, tmpfs `/work,/tmp`, read-only root, current capability/pid limits, and an internal Docker network validated by activation. Append exactly `spec.agent_image`, `agent`, and `Runner::KodLoop{model,max_tokens}.command` after env `ANTHROPIC_BASE_URL=spec.model_url` and `KOD_MCP_URL=spec.mcp_url`. No shell command interpolation. Extract the small KodLoop argument builder to accept max_turns if needed; do not fabricate a whole EvalCase with dummy test fields. Seed with `workspace::extract`, not `.git` history; prompt is host-rendered admitted issue text. `kod-loop`'s existing execution tool applies its own safe process handling; do not replace it with an unchecked patch execution path.

Implement `connected::launch(args: &[String], name: &str, wall: Duration, gate: &SpawnGate, stopped: &dyn Fn() -> Result<bool>) -> Result<ProcessExit>`: spawn under `gate`, poll `Child::try_wait` and the stopped callback every at most 50 ms, preserve `status.success()`, and on wall/cancel call `stop_owned` below. A stopped/error path never collects. Do not use the global offline stop file or its 60-second grace. Keep stdout/stderr bounded and drain them or route to host-owned files; never wait on a full pipe.

`connected::stop_owned(run: &str, names: &[String], term: Duration, total: Duration) -> Result<()>` verifies each extant name's `kod.run` label matches `run`, sends TERM once, repeats inspect during the 5-second grace, sends KILL after 5 seconds, and reports success only after all names are absent by 10 seconds. Names are `kod-agent-run-N`, `kod-collect-run-N`, `kod-export-run-N`, constructed only after strict positive `store::seq_of` validation. Docker command execution itself needs the remaining deadline: spawn child, poll/reap, kill a stuck Docker CLI at deadline, and return error. A Docker daemon error is not absence; only a successful `docker ps -aq --filter name=^/NAME$` with empty output proves absence, followed by joined local launch handles to prevent a late create. Creation is serialized by the spawn gate and per-run cancel cell; if a delayed Docker create appears before its launch CLI exits, keep stopping it. Per-run stop must not close the global gate.

- [ ] **Step 3 / Trusted collector:** expose `connected::collect(spec, gate, stopped) -> Result<PathBuf>` with the same gate/callback parameters as launch. Derive all paths from `spec.dir`, never agent output. Fresh `collect_out` and clean `base_tree` come from `spec.base` in the trusted mirror; reuse the existing collect mounts and `collect_memory`, network `none`, collector digest, and `collect` command. Require successful container exit, regular `collect_ok`, and bounded regular `diff.patch`; reject symlinks even if a marker exists. `ready_to_collect` must run first and Docker confirms the agent container absent; no sandbox output directly becomes a publishable `Collected`.

Validate the raw collector patch with `supported_patch` **before** applying. Add `collect-export` to the trusted collector image: mounts clean base `/in:ro`, validated patch `/patch:ro`, output `/out`, tmpfs `/tmp,/base,/wt`, network none, no agent archive. Copy base into tmpfs, initialize Git using the existing collector's no-config/no-filter/no-hook settings, create a base commit, and `git apply --index --check /patch/diff.patch` then `git apply --index /patch/diff.patch`. Require the patch to apply to this exact base. Export `git diff --cached --raw -z --no-renames HEAD` and per-file blobs from the index, **not** agent-controlled symlink traversal. Output filenames are host-assigned decimal indices `0,1,2`, with a JSON manifest `{version:1,base,files:[{path,mode,blob:0}|{path,deleted:true}]}`; the base value is passed from the host, not read from agent content. New file mode must be 100644; existing 100644/100755 mode must match the base; refuse any other mode. Fail on duplicate/traversing/absolute/.git paths, invalid UTF-8/NUL text, >1000 files or >8 MiB aggregate text. Use the Rust exporter implemented below; its executable is included in the digest-pinned collector image. Do not add a host-side patch-to-Forge parser.

Implement `workspace::CollectedWorkspace { entries: Mutex<HashMap<String, forge::Collected>> }` with `Default`, `pub(crate) fn insert(&self, run: &str, collected: Collected) -> Result<()>` insert-or-compare semantics and existing `Workspace::collected` returning error when absent. `runner::run(spec, gate, stopped, workspace) -> Result<RunState>` calls launch → outcome mapping → process gate → collect → supported_patch → export → bounded output read → insert. Only after insert may it return Done. The runtime never restores this memory map after restart; old executions are retired. Map `EndState::{Blocked,NeedsHuman,OverBudget}` directly, but accept Done only through the complete trusted sequence. Record honest collection evidence, never claim project tests ran unless host-owned results prove it.

The export implementation is owned here, not a future unspecified helper. Put this core in `kod-eval/src/export.rs`; `kod-collect-export` parses exactly `--repo PATH --out PATH --base SHA` with existing clap, invokes `export_index`, and exits nonzero on error. The connected shell mode performs the clean-base initialize/check/apply steps above then calls this binary; it writes `export_ok` only after the binary succeeds. Dockerfile's existing Rust stage builds `-p kod-loop -p kod-eval --bin kod-loop --bin kod-collect-export`, then copies both executables. Keep the exporter binary outside the agent's command tool API.

```rust
use anyhow::{Result, Context, ensure};
use std::{path::Path, process::{Command, Stdio}, io::Read};
const BYTES: usize = 8 << 20;
fn git_bytes(repo: &Path, args: &[&str], cap: usize) -> Result<Vec<u8>> {
    let mut child = Command::new("git").arg("-C").arg(repo)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
        .args(args).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let mut bytes = Vec::new();
    let read = child.stdout.take().context("missing git stdout")?
        .take((cap + 1) as u64).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > cap { let _ = child.kill(); }
    let status = child.wait()?;
    read?; ensure!(bytes.len() <= cap && status.success(), "bounded git export failed");
    Ok(bytes)
}
pub fn export_index(repo: &Path, out: &Path, base: &str) -> Result<()> {
    ensure!(base.len() == 40 && base.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid export base");
    let raw = git_bytes(repo, &["diff", "--cached", "--raw", "-z", "--no-abbrev", "--no-renames", "HEAD"], BYTES)?;
    let mut fields = raw.split(|b| *b == 0); let mut files = Vec::new(); let mut total = 0;
    let mut seen = std::collections::HashSet::new();
    while let Some(header) = fields.next() {
        if header.is_empty() { break; }
        ensure!(files.len() < 1000, "too many export files");
        let header = std::str::from_utf8(header)?;
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(parts.len() == 5, "invalid raw change");
        let old = parts[0].strip_prefix(':').context("missing mode prefix")?;
        let new = parts[1]; let status = parts[4];
        ensure!(matches!(status, "A"|"M"|"D"), "unsupported change kind");
        let path = std::str::from_utf8(fields.next().context("missing path")?)?;
        ensure!(!path.is_empty() && path.split('/').all(|p|
            !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git")), "invalid export path");
        ensure!(seen.insert(path.to_owned()), "duplicate export path");
        match status {
            "A" => ensure!(old == "000000" && new == "100644", "unsupported added mode"),
            "M" => ensure!(matches!(old,"100644"|"100755") && old == new, "changed file mode"),
            "D" => ensure!(matches!(old,"100644"|"100755") && new == "000000", "unsupported deleted mode"),
            _ => unreachable!(),
        }
        if status == "D" { files.push(serde_json::json!({"path":path,"deleted":true})); continue; }
        let oid = parts[3];
        ensure!(oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid export blob id");
        let blob = git_bytes(repo, &["cat-file", "blob", oid], BYTES - total)?;
        let text = std::str::from_utf8(&blob)?;
        ensure!(!text.contains('\0'), "binary export"); total += blob.len();
        let id = files.len();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(out.join(id.to_string()))?;
        f.write_all(&blob)?; f.sync_all()?;
        files.push(serde_json::json!({"path":path,"mode":new,"blob":id}));
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"base":base,"files":files}))?;
    std::fs::write(out.join("manifest.json"), bytes)?;
    Ok(())
}
```

Use a fresh empty `/out` and a standard Git directory in `/wt/.git` for `collect-export`; the old collect mode's `GIT_DIR=/tmp/collect.git` is deliberately not inherited. Host manifest reads are strict duplicate-aware JSON and no-follow regular file reads, verify `version/base`, sequential blob IDs and paths/modes/aggregate limits again, then construct `forge::Change`. An unchanged executable remains mode 100755 in the raw index; the existing Forge adapter preserves its base mode rather than defaulting it to 100644. Add one non-Docker test for `export_index`: initialize a temp repo with executable `keep`, text `delete`, stage editing `keep`, deleting `delete`, and adding `new`; assert manifest paths/modes and exact exported bytes. Include two different contents in `foo` and `0:foo`; assert each exported manifest entry resolves its own exact bytes, proving filenames cannot be interpreted as index-stage syntax. A second test stages a symlink and asserts error with no trusted completion marker. These exercise real Git, not Docker.

- [ ] **Step 4 / Acceptance / QA:** add `tests/runner.rs::closed_spawn_gate_starts_nothing` using `Command::new("sh").args(["-c","exit 0"])`, call `gate.close()`, assert spawn error; add a callback returning true and assert the same. Connected process tests inject an executable under the test directory that implements only the Docker CLI calls used here and records argv; use barriers/files to simulate delayed creation, nonzero exit and daemon hang. Test 5/10-second boundary with injected clock and assert TERM then KILL then error if still present; no Docker in this plan. `tests/workspace.rs` supplies raw supported add/delete/executable-content patches and unsupported modes, binary, rename, wrong-base and forged marker outputs; assert failures leave `CollectedWorkspace::collected(run)` an error. Provider tests in Task 5 additionally assert zero Forge writes. Container isolation and actual exporter execution are re-proved in 6b-c, never claimed from an argv fake.
- [ ] Run `cargo test -p kod-host --features connected-test-support --test runner --test workspace`, `cargo test -p kod-eval -p kod-loop`; retain offline behavior tests. Commit `Collect connected runs through the trusted sandbox boundary`; evidence `.omo/evidence/phase1-connected/host-runner.log`.

## Task 5: Implement the dispatcher Runtime bridge and cancellation races

**Files:** new `crates/kod-host/src/{runtime,binding}.rs`, `tests/runtime.rs`, `tests/common/{mod,provider}.rs`; additive Task 1 store helpers if missing. Depends on Tasks 1–4; Task 6 supplies the artifact resolver, Task 7 opens admission only after resolver wiring. No third DB, second durable lifecycle column, resumed execution, static model registrations, or duplicate cleanup outbox.

**References:** `kod-dispatcher/src/lib.rs:138` (all six Gateway methods), `:155` (`check_absent_run`), `:371` (`open` recovery), `:797` (`finished` immediately publishes Done); `store.rs:122` (`stopped`); `kod-tool-gateway/src/lib.rs:48` (`Run`), `:86` (`Adapters`), `:217`/`:259` (`start_run`/`end_run`), `:591` (`published`); `kod-dispatcher/tests/review_edges.rs:1` (barriers), `tests/common/mod.rs:188` (real dispatcher fixture); Task 2 dynamic `RunRegistry` and durable reservation sink.

- [ ] **Step 1 / Red:** add `tests/runtime.rs` with these self-contained tests. No implicit `Harness::connected()` or private cross-crate test module. The live bridge tests below extend the concrete existing fixture construction, not just a mock of `Gateway`.

```rust
#[test]
fn stopped_reader_uses_the_durable_row_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("dispatcher.db");
    let c = kod_dispatcher::store::open(&path).unwrap();
    c.execute("INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#7','v1','running')", []).unwrap();
    let stopped = kod_host::runtime::stopped_reader(path);
    assert!(!stopped("run-1").unwrap());
    c.execute("UPDATE runs SET state='canceled' WHERE seq=1", []).unwrap();
    assert!(stopped("run-1").unwrap());
    assert!(stopped("run-99").unwrap());
    assert!(stopped("run-01").unwrap());
    drop(c); std::fs::remove_file(dir.path().join("dispatcher.db")).unwrap();
    assert!(stopped("run-1").is_err());
}
#[test]
fn background_start_returns_before_agent_completion() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let join = kod_host::runtime::background(move || {
        entered_tx.send(()).unwrap(); release_rx.recv().unwrap(); done_tx.send(()).unwrap();
    }).unwrap();
    entered_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    assert!(done_rx.try_recv().is_err());
    release_tx.send(()).unwrap(); join.join().unwrap();
    done_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
}
```

The SQL spelling is `canceled`, matching `RunState::Canceled.name()` at `kod-dispatcher/src/lib.rs:97`. Run `cargo test -p kod-host --test runtime` red.

- [ ] **Step 2 / Implement:** the following code belongs in `src/runtime.rs`. `background` owns one OS thread for blocking Docker/collector work; `Gateway::start` returns after spawning it. Do not call `Handle::block_on` from an async worker or hold dispatcher/database mutexes while waiting for an agent. Production `RunRegistry::revoke` must be a local nonblocking cancellation signal; network credential cleanup is separate.

```rust
use anyhow::Result;
use std::{path::PathBuf, sync::{Arc, Weak}, thread::JoinHandle};
pub fn stopped_reader(path: PathBuf) -> kod_tool_gateway::Stopped {
    Arc::new(move |run| {
        let c = rusqlite::Connection::open_with_flags(&path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        c.busy_timeout(std::time::Duration::from_millis(50))?;
        kod_dispatcher::store::stopped(&c, run)
    })
}
pub fn background(work: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new().name("kod-connected-run".into()).spawn(work)?)
}
pub fn notify_finished(dispatcher: &Weak<kod_dispatcher::Dispatcher>, run: &str,
    outcome: kod_dispatcher::RunState, reason: &str) -> Result<()> {
    use anyhow::Context;
    dispatcher.upgrade().context("dispatcher detached")?.finished(run, outcome, reason)
}
```

Define strict `binding::Binding` (Serialize/Deserialize/PartialEq, `deny_unknown_fields`) with `version:u32`, `run:String`, `ticket:String`, `revision:String`, `repo:String`, `base:String`, `started_at:i64`, `data_classes:Vec<String>`, `pins:BTreeMap<String,String>`, `linked_tickets:Vec<String>`, `max_tokens:u64`, `max_cost_micro_usd:u64`, `agent_image:String`, `collector_image:String`, `agent_out:PathBuf`, `collect_out:PathBuf`, `model_token_hash:String`, `mcp_token_hash:String`. Exact positive run ID, base SHA, repository and output-root ancestry are validated before `store::bind_once`. Add `pub fn permit(&self) -> github::RunPermit` returning `{run:self.run.clone(),ticket:self.ticket.clone(),repo:self.repo.clone(),base:self.base.clone()}`. Never persist raw bearer tokens, and never regenerate/replace a binding for an old ID.

Use `pub type ResolveAdmission = Arc<dyn Fn(&RunStart) -> Result<Admitted> + Send + Sync>` and `pub struct Admitted { pub binding: Binding, pub prompt: String, pub model_token: String, pub mcp_token: String }` as the exact Task 6 integration boundary. The resolver uses the issue revision already admitted by dispatcher, resolves exact approved pins, reads only dispatcher-owned trusted rework links, resolves immutable mirror base and host time, and allocates fresh random opaque tokens via `kod_eval::run::token`. Task 6 must recheck the issue revision before returning, never trust mutable ticket links for rework. Task 5 tests construct `Admitted` explicitly from constants/temporary directories; production remains closed until the Task 6 resolver is installed.

Put this complete binding fixture in `tests/common/mod.rs`; every field is supplied and token hashes really match. Unit tests may supply the fake tracker ticket IDs, but the live-provider fixture uses native `acme/app#7`. The runtime must validate/reject differing start identity, not silently rewrite this fixture.

```rust
pub fn admitted(root: &std::path::Path, start: &kod_dispatcher::RunStart) -> kod_host::runtime::Admitted {
    use sha2::{Digest, Sha256};
    let model_token = "a".repeat(64); let mcp_token = "b".repeat(64);
    let hash = |s: &str| Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>();
    let dir = root.join(&start.id);
    kod_host::runtime::Admitted {
        binding: kod_host::binding::Binding {
            version: 1, run: start.id.clone(), ticket: start.ticket.clone(),
            revision: start.revision.clone(), repo: start.repo.clone(), base: "c".repeat(40),
            started_at: 1_800_000_000, data_classes: vec!["placeholder".into()],
            pins: std::collections::BTreeMap::from([("design".into(),"design-v3".into())]),
            linked_tickets: vec![], max_tokens: 10_000, max_cost_micro_usd: 1_000_000,
            agent_image: format!("agent@sha256:{}", "d".repeat(64)),
            collector_image: format!("collector@sha256:{}", "e".repeat(64)),
            agent_out: dir.join("agent_out"), collect_out: dir.join("collect_out"),
            model_token_hash: hash(&model_token), mcp_token_hash: hash(&mcp_token),
        },
        prompt: "Implement the admitted acceptance criteria.".into(), model_token, mcp_token,
    }
}
```

Define `Runtime` with `db:PathBuf`, `tool:Arc<kod_tool_gateway::Gateway>`, `model:RunRegistry`, `github:Arc<github::GitHub>`, `workspace:Arc<CollectedWorkspace>`, `gate:Arc<SpawnGate>`, `dispatcher:OnceLock<Weak<Dispatcher>>`, `resolve:ResolveAdmission`, validated sandbox/network/model configuration, and a mutex map of `run -> Arc<RunSlot>`. No lock in this map may be held during network calls, thread joins, runner execution or `finished`. Constructor signature is `Runtime::new(db: PathBuf, tool: Arc<kod_tool_gateway::Gateway>, model: RunRegistry, github: Arc<github::GitHub>, workspace: Arc<CollectedWorkspace>, gate: Arc<SpawnGate>, resolve: ResolveAdmission, sandbox: FsSandbox, networking: FsNetworking, model_config: FsModel, execute: Execute) -> Arc<Self>`; it only stores fields and initializes empty OnceLock/map, never starts work. Use `Arc::new_cyclic` for the runtime weak self needed by background closures. Public `attach(&self, Weak<Dispatcher>) -> Result<()>` inserts once and refuses replacement. Tests call `attach` after real `Dispatcher::open`; `find_published` must work before attachment.

Implement all six `kod_dispatcher::Gateway` methods with these exact operations and error semantics:

| Method | Required implementation ordering |
|---|---|
| `start(&RunStart)` | Require attached dispatcher; read stopped row; claim per-run start ownership once before generating credentials. Identical in-process successful retries return Ok without resolving/registering again; in-flight retries await only bounded setup, failed/tombstoned slots refuse. Obtain admission, verify run/ticket/revision/repo match; atomically insert-or-compare binding in existing DB **before** registration/launch. An existing binding after process restart is an error, never resume. Register `github.register_run(binding.permit())`; `model.register(model_token, RunSpec{id,ticket,repo,data_classes,budget})`; `tool.start_run(mcp_token, Run{id,ticket,repo,base,data_classes,linked_tickets,started_at})`. Task 6 adds immutable admitted pins to Run. Recheck stopped; on any failure invoke all revocation paths and return error. Spawn background work; its very last spawn checks share Task 4 gate and durable stopped reader. Save handle before returning. |
| `revoke(run)` | Set existing per-run local cancel flag first; attempt `model.revoke(run)` before waiting on broker/network. Call `tool.retire_run_access(run)` and schedule 6b-a `Broker::revoke` for any lease surviving registration failure with `poll_cleanup`; attempt every action even if one fails, aggregate sanitized errors. Missing run is Ok. No stop wait here. |
| `stop(run)` | Set cancel, validate DB binding identity or return Ok for a never-bound ID; stop only its exact three names with Task 4 deadlines; join launch handle only within remaining deadline. Return error if absence/revocation cannot be confirmed; dispatcher retains existing pending stop follow-up. Never stop another run or close global gate. |
| `stop_all()` | Snapshot live handles and durable bound IDs, invoke `revoke_all`, attempt stop on every ID even after an error, aggregate errors. Dispatcher global kill already closes durable admission; host shutdown additionally calls `quiesce` first. Do not permanently close SpawnGate for a signed kill if signed resume in the same process is supported. |
| `publish(run)` | Require valid immutable binding and `workspace.collected(run)`; `tool.publish(run).map(|p|p.pr).map_err(anyhow::Error::msg)`. Never translate a missing trusted collection into empty changes or rebuild collection from sandbox files. |
| `find_published(start)` | Load binding from DB, validate all four start identity fields, `github.register_run(binding.permit())`, then `tool.published(&start.repo,&start.ticket,&start.id).map_err(anyhow::Error::msg)`. Missing binding fails closed. Never execute or republish; 6b-a validates exact draft PR identity. |

The background closure maps any runner failure to NeedsHuman; on success it calls `notify_finished` with explicit Done/Blocked/NeedsHuman/OverBudget. Done is reachable only after `CollectedWorkspace::insert`. If `finished` fails, retain the failure for the worker's existing cleanup/recovery path, revoke locally and do not retry publication from the thread. Durable row remains the stop authority; completed terminal rows cannot be reopened.

Implement `Runtime::revoke_all()->Result<()>` by signaling all model cancellations, clearing MCP registrations with `tool.retire_all_access`, and scheduling 6b-a broker `revoke_all`, attempting all even on error. `Runtime::recover_local()->Result<()>` invokes those paths and `github.recover()`, enumerates immutable bound IDs from `run_bindings` (not all Docker containers on the machine), calls `model.revoke(id)` for each to install old-ID tombstones even when the new registry is empty, and confirms their owned names absent. It must attempt container cleanup even when remote revocation returns pending, then poll/reap credential cleanup until confirmed before allowing readiness. It creates no execution or spend ledger and does not report host ready. Task 7 calls it before `Dispatcher::open`, which retires queued/running rows and resolves publishing with this bridge. Unknown owned container/lease identities fail readiness and require cleanup by validated ownership, never broad label-blind deletion.

Implement `Runtime::quiesce()->Result<()>` as `gate.close()` first, then an additive Task 1 helper `store::retire_active(&mut Connection, reason: &str)->Result<()>`: IMMEDIATE transaction, for each queued/running row update to NeedsHuman and insert existing revoke/stop follow-ups through `Dispatcher::end`'s conditional state transition (repeat calls see no active rows and enqueue nothing); commit together; never rewrite publishing/terminal rows. If helper is not yet public, extract existing dispatcher retirement logic in Task 1 rather than duplicate SQL strings. Gate close shares the mutex held through **actual** process spawn, so a queue worker already inside `start` cannot launch after shutdown enumeration. It is intentionally one-way until process restart; signed runtime resume uses dispatcher policy and does not call quiesce.

- [ ] **Step 2b / Concrete start and cancellation implementation:** add these bodies to `src/runtime.rs`. Use local retirement and scheduled remote cleanup as in the ordering table. Task 2's exact local registry signatures are `revoke(&self, &str)->Result<()>` and `revoke_all(&self)->Result<()>`; they signal/drop local forwarding without awaiting a provider. Add `tool::Gateway::retire_run_access(&self, run:&str)` with body `self.lock().retain(|_, live| live.run.id != run);` and `retire_all_access(&self)` with body `self.lock().clear();`. Existing `end_run/end_all` call those methods before their existing broker RPC, preserving compatibility. Add `crates/kod-tool-gateway/src/lib.rs` to Task 5 ownership.

Use fields `weak:Weak<Runtime>`, `slots:Mutex<HashMap<String,Arc<RunSlot>>>`, `global_cleanup:Mutex<Option<JoinHandle<Result<()>>>>`, and `execute:Execute` on Runtime. The `new` constructor includes final parameter `execute:Execute`, initializes the fields with `Arc::new_cyclic`, `Mutex::default`, and stores the callback. `Execute` is a private host assembly seam, not a configuration/plugin hook: production captures validated FsSandbox/FsModel/FsNetworking plus mirror, builds Task 4's `ConnectedSpec` from `Admitted.binding`, and invokes the exact `runner::run(&ConnectedSpec,&SpawnGate,&dyn Fn()->Result<bool>,&CollectedWorkspace)->Result<RunState>` pipeline. Tokens appear only in its model/MCP URLs; tests may inject a blocked runner. No new durable work queue is introduced.

```rust
use std::{collections::HashMap, sync::{Mutex, atomic::{AtomicBool, Ordering}}, time::Instant};
use anyhow::{Context, ensure};
use kod_dispatcher::{RunStart, RunState};
use kod_tool_gateway::{Stopped, broker::Broker, forge::Workspace};
use kod_eval::connected::SpawnGate;
pub type Execute = Arc<dyn Fn(&Admitted, &SpawnGate, &Stopped, &CollectedWorkspace)
    -> Result<RunState> + Send + Sync>;
#[derive(Default)]
struct RunSlot {
    canceled: AtomicBool, execution_done: AtomicBool,
    cancel_at: Mutex<Option<Instant>>, setup: Mutex<Option<std::result::Result<(),String>>>,
    worker: Mutex<Option<JoinHandle<()>>>, cleanup: Mutex<Option<JoinHandle<Result<()>>>>,
}
impl RunSlot {
    fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        self.cancel_at.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Instant::now);
    }
    fn run_if_live<T>(&self, run: &str, stopped: &Stopped, f: impl FnOnce() -> Result<T>) -> Result<T> {
        ensure!(!self.canceled.load(Ordering::SeqCst) && !stopped(run)?, "run canceled before spawn"); f()
    }
}
fn poll_cleanup(cell: &Mutex<Option<JoinHandle<Result<()>>>>,
    work: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    let mut slot = cell.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|j| j.is_finished()) {
        return slot.take().expect("finished cleanup").join()
            .map_err(|_| anyhow::anyhow!("credential cleanup panicked"))?;
    }
    if slot.is_none() { *slot = Some(std::thread::Builder::new().name("kod-revoke".into()).spawn(work)?); }
    anyhow::bail!("credential cleanup pending")
}
impl Runtime {
    pub fn new(db: PathBuf, tool: Arc<kod_tool_gateway::Gateway>, model: kod_llm_proxy::RunRegistry,
        github: Arc<kod_tool_gateway::github::GitHub>, workspace: Arc<CollectedWorkspace>,
        gate: Arc<SpawnGate>, resolve: ResolveAdmission, sandbox: FsSandbox,
        networking: FsNetworking, model_config: FsModel, execute: Execute) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self { db, tool, model, github, workspace, gate, resolve,
            sandbox, networking, model_config, execute, weak: weak.clone(),
            dispatcher: std::sync::OnceLock::new(), slots: Mutex::default(), global_cleanup: Mutex::default() })
    }
    pub fn attach(&self, dispatcher: Weak<kod_dispatcher::Dispatcher>) -> Result<()> {
        ensure!(dispatcher.upgrade().is_some(), "dispatcher detached");
        self.dispatcher.set(dispatcher).map_err(|_| anyhow::anyhow!("dispatcher already attached"))
    }
    fn slot(&self, run: &str) -> Arc<RunSlot> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).entry(run.into()).or_default().clone()
    }
    fn bound(&self, run: &str) -> Result<bool> {
        let Some(seq) = kod_dispatcher::store::seq_of(run) else { return Ok(false); };
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM run_bindings WHERE run_seq=?1)", [seq], |r| r.get(0))?)
    }
    fn start_registered(&self, start: &RunStart) -> Result<()> {
        let dispatcher = self.dispatcher.get().context("dispatcher detached")?.clone();
        let slot = self.slot(&start.id);
        let mut setup = slot.setup.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = setup.as_ref() { return result.clone().map_err(anyhow::Error::msg); }
        let stopped = stopped_reader(self.db.clone());
        let result = (|| -> Result<()> {
            slot.run_if_live(&start.id, &stopped, || Ok(()))?;
            ensure!(!self.bound(&start.id)?, "old execution cannot resume");
            let admitted = (self.resolve)(start)?; let b = &admitted.binding;
            ensure!((b.run.as_str(),b.ticket.as_str(),b.revision.as_str(),b.repo.as_str()) ==
                (start.id.as_str(),start.ticket.as_str(),start.revision.as_str(),start.repo.as_str()), "admission identity changed");
            let c = kod_dispatcher::store::open(&self.db)?;
            kod_dispatcher::store::bind_once(&c, &start.id, &serde_json::to_value(b)?)?; drop(c);
            self.github.register_run(kod_tool_gateway::github::RunPermit {
                run:b.run.clone(),ticket:b.ticket.clone(),repo:b.repo.clone(),base:b.base.clone() })?;
            self.model.register(&admitted.model_token, kod_llm_proxy::config::RunSpec {
                id:b.run.clone(),ticket:b.ticket.clone(),repo:b.repo.clone(),data_classes:b.data_classes.clone(),
                budget:kod_llm_proxy::config::Budget { max_tokens:b.max_tokens,max_cost_micro_usd:b.max_cost_micro_usd } })?;
            self.tool.start_run(&admitted.mcp_token, kod_tool_gateway::Run {
                id:b.run.clone(),ticket:b.ticket.clone(),repo:b.repo.clone(),base:b.base.clone(),
                data_classes:b.data_classes.clone(),linked_tickets:b.linked_tickets.clone(),started_at:b.started_at })?;
            slot.run_if_live(&start.id, &stopped, || Ok(()))?;
            let runtime = self.weak.upgrade().context("runtime detached")?;
            let worker_slot = slot.clone(); let id = start.id.clone();
            let canceled = worker_slot.clone(); let durable = stopped.clone();
            let live: Stopped = Arc::new(move |run| Ok(canceled.canceled.load(Ordering::SeqCst) || durable(run)?));
            let join = background(move || {
                let result = worker_slot.run_if_live(&id, &live,
                    || (runtime.execute)(&admitted, &runtime.gate, &live, &runtime.workspace));
                let result = result.and_then(|state| {
                    if state == RunState::Done { runtime.workspace.collected(&id)?; } Ok(state)
                });
                worker_slot.execution_done.store(true, Ordering::SeqCst);
                let (state,reason) = match result {
                    Ok(state) => (state,"connected runner completed".to_owned()),
                    Err(e) => (RunState::NeedsHuman,kod_tool_gateway::scrub(&format!("{e:#}"))),
                };
                if notify_finished(&dispatcher,&id,state,&reason).is_err() {
                    let _ = runtime.revoke_registered(&id);
                }
            })?;
            *slot.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(join); Ok(())
        })();
        *setup = Some(result.as_ref().map(|_| ()).map_err(|e| kod_tool_gateway::scrub(&format!("{e:#}"))));
        drop(setup);
        if result.is_err() { let _ = self.revoke_registered(&start.id); }
        result
    }
    fn revoke_registered(&self, run: &str) -> Result<()> {
        let slot = self.slot(run); slot.cancel();
        let local = self.model.revoke(run); self.tool.retire_run_access(run);
        let bound = self.bound(run); if matches!(bound,Ok(false)) { return local; }
        let github = self.github.clone(); let id = run.to_owned();
        let remote = poll_cleanup(&slot.cleanup,move || github.revoke(&id));
        local.and(bound.map(|_| ())).and(remote)
    }
    pub fn revoke_all(&self) -> Result<()> {
        for slot in self.slots.lock().unwrap_or_else(|e| e.into_inner()).values() { slot.cancel(); }
        let local = self.model.revoke_all(); self.tool.retire_all_access();
        let github = self.github.clone();
        let remote = poll_cleanup(&self.global_cleanup,move || github.revoke_all()); local.and(remote)
    }
    pub fn quiesce(&self) -> Result<()> {
        self.gate.close();
        let mut c = kod_dispatcher::store::open(&self.db)?;
        kod_dispatcher::store::retire_active(&mut c,"host shutting down")
    }
}
```

`Gateway::start`'s body is `self.start_registered(run)`; `Gateway::revoke`'s body is `self.revoke_registered(run)`. These helpers are private; the trait is the sole admission API. Task 6 adds its admitted-pin field to the shown `Run` literal when it adds that field to the type. The resolver validates the full binding/hash/path schema before this code persists it. `RunSlot::setup` serializes only setup, not agent execution; revocation never acquires it. `stop` starts its 10-second total budget at `slot.cancel_at`, not after remote cleanup, and still attempts containers when revoke returns pending. A finished runner sets `execution_done` before `Dispatcher::finished`, whose Done branch synchronously invokes stop follow-ups: stop must not join its own thread. Once `execution_done` is true, no execution/collection/spawn remains; confirm owned containers absent, defer handle reaping, and let the completion callback return. Failed/pending broker jobs are retried only through existing durable follow-ups, with at most one in-memory worker for each run/global cleanup cell.

Add these exact unit tests inside `runtime.rs` (same private helpers as production). They require no invented provider harness. Retain the full real-provider/dispatcher matrix in Step 4 to prove adapter integration.

```rust
#[cfg(test)] mod bridge_tests {
    use super::*;
    #[test]
    fn cancel_before_spawn_never_creates_a_child() {
        let dir = tempfile::tempdir().unwrap(); let db = dir.path().join("dispatcher.db");
        let c = kod_dispatcher::store::open(&db).unwrap();
        c.execute("INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#7','v1','running')",[]).unwrap();
        let slot = Arc::new(RunSlot::default()); let gate = Arc::new(SpawnGate::default());
        let (entered,ready) = std::sync::mpsc::channel(); let (release,wait) = std::sync::mpsc::channel();
        let worker_slot = slot.clone(); let marker = dir.path().join("spawned"); let output = marker.clone();
        let stopped = stopped_reader(db);
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap(); wait.recv().unwrap();
            worker_slot.run_if_live("run-1",&stopped, || {
                let mut command = std::process::Command::new("touch"); command.arg(output);
                let mut child = gate.spawn(&mut command, || stopped("run-1"))?;
                ensure!(child.wait()?.success(),"child failed"); Ok(())
            })
        });
        ready.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        slot.cancel(); c.execute("UPDATE runs SET state='canceled' WHERE seq=1",[]).unwrap();
        release.send(()).unwrap(); assert!(worker.join().unwrap().is_err()); assert!(!marker.exists());
    }
    #[test]
    fn silent_remote_revocation_does_not_block_the_stop_caller() {
        let cell = Mutex::new(None); let (release,wait) = std::sync::mpsc::channel();
        let mut process = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let began = Instant::now();
        assert!(poll_cleanup(&cell,move || { wait.recv()?; Ok(()) }).is_err());
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
        // The same caller is free to issue stop while the remote worker remains blocked.
        assert!(!cell.lock().unwrap().as_ref().unwrap().is_finished());
        process.kill().unwrap(); assert!(!process.wait().unwrap().success());
        assert!(began.elapsed() < std::time::Duration::from_secs(1)); release.send(()).unwrap();
        let worker = cell.lock().unwrap().take().unwrap(); worker.join().unwrap().unwrap();
    }
}
```

Run `cargo test -p kod-host --lib runtime::bridge_tests` red before adding the bodies, then green. The remote-worker test proves the synchronous bridge caller returns and can stop a real local child; Step 4 additionally uses Task 4's fake Docker driver to assert TERM/KILL argv while a loopback revocation HTTP handler stays silent, at 5/10 seconds from `cancel_at`. Do not substitute the local-child assertion for that process-driver integration scenario.

- [ ] **Step 3 / Concrete integration fixture:** `tests/common/mod.rs` directly constructs `TempDir`, `store::open`, policy `Policy::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"))`, `AuditLog::open`, `let (fake, _seed) = kod_tracker::fake::sandbox(); let tracker = Arc::new(fake);` as used by dispatcher `tests/common/mod.rs:243`, real tool `Gateway::new(policy,audit,Adapters{tracker,docs:Arc::new(FakeDocs::default()),stopped:stopped_reader(db.clone()),broker:github.clone(),forge:github.clone(),workspace},Identity{name:"kod-agent",email:"agent@kod.invalid"})`, dynamic model `app_with_registry` with local model server and Task 1 reservation sink, then `Runtime::new` and real `Dispatcher::open`. Fake tracker/docs are limited to lifecycle unit scenarios; provider integration uses Task 3's real tracker constructor and 6b-a loopback API fixture. Explicitly keep the provider server, RSA key file, audit DB and TempDir alive in the fixture struct. Do not import another crate's private `tests/common` module.

- [ ] **Step 4 / Acceptance / QA:** `cargo test -p kod-host --features connected-test-support --test runtime`. Tests `start_returns_while_model_is_blocked`, `start_cancel_before_registration`, `start_cancel_after_registration_before_spawn`, `shutdown_closes_launch_gate`, `stop_timeout_stays_pending`, `done_requires_trusted_collection`, `recovery_does_not_resume`, `same_id_cannot_change_binding`, `absent_run_contract` use `Barrier`/channels, no timing sleeps. Specifically invoke `kod_dispatcher::check_absent_run(runtime.as_ref(), "run-999")` twice and assert no provider mint/Forge mutation. Hold a model response open, cancel, advance injected time 2 seconds, assert both queued and streaming requests finish; then hold container absence false, advance to 5 and 10 seconds, assert TERM/KILL ordering and pending follow-up in reopened DB. A forged marker, unsupported raw patch, wrong base, missing process gate or nonzero collector exit each yields zero Forge writes and no Done. Set DB stopped between every registration/spawn barrier and prove tokens denied. Reopen both existing DBs, recover old publishing with exact PR versus no PR, assert no `docker run`/model request or spend reset.
- [ ] Run `cargo test -p kod-dispatcher --test review_edges --test gateway_contract`, then `cargo test -p kod-host --features connected-test-support --test runtime --test workspace --test config` and `cargo test -p kod-eval -p kod-loop`. Commit `Bridge dispatcher lifecycle to connected execution`; evidence `.omo/evidence/phase1-connected/host-runtime.log`.

Keep an atomic cleanup-debt flag on Runtime: set it before scheduling any revocation and on scheduling/join errors; clear it only after every outstanding run/global revocation and stop obligation is confirmed. `cleanup_pending()` reads this flag without network/SQLite access. Reaping a failed worker never clears the flag; the existing durable follow-up schedules its retry. Add a test that a finished failed cleanup remains admission-blocking after its JoinHandle is removed.

## Task 6: Pin approved snapshots and produce authoritative gate/status records

Implement Task 5's production `ResolveAdmission` here, supplying every Binding field and checking the admitted issue revision again before returning. Bind Task 4's runner to Runtime's `Execute` callback during assembly; tests must cover both production connections.

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

## Task 7: Mount routes, reconcile writes and prove safe startup/shutdown

**Files:** add `kod-host/src/{service,refresh,tracker_credentials}.rs`, `tests/{service,refresh}.rs`; add a refreshable credential-provider constructor in `kod-tracker/src/github.rs`, retaining static-token `GitHub::new` compatibility; narrowly expose the existing App-JWT signer from `kod-tool-gateway/src/github/auth.rs` through `AppCredentials`. Read current tracker status/rework uncertainty tests and `kod-status::refresh` audit ordering.

Also modify `kod-status/src/lib.rs` for the writer hook, `kod-dispatcher/src/lib.rs` for maintenance/admission controls, and their focused regression tests. Keep existing wrapper methods intact so offline callers need no changes.

- [ ] Write a test that posts a status comment, loses the provider response, reconstructs the service, then refreshes twice. Assert exactly one POST, ownership by configured bot ID plus marker, and preserved human text. An absent/duplicate owned comment with unresolved intent blocks another create. A definite 4xx rejection can clear uncertainty; token renewal retains the adapter's instance guard. Rework uncertain creates also require reconciliation, not blind retry.
- [ ] Keep tracker-service credentials separate from 6b-a publish credentials. Reuse its RS256 signer as `AppCredentials::jwt(now: i64) -> anyhow::Result<Secret>`; do not request Issues permission through the publish Broker. The host tracker provider requests only the configured tracker repository ID with `issues:write` and implicit `metadata:read`, validates installation/App/bot/repository identity and renews before expiry. Persist its lease in a distinct owner-only tracker subdirectory; publisher leases remain under their own 6b-a lock. Reconcile/revoke both directories before startup admission and on host shutdown. A per-run stop revokes model/MCP/publish capabilities; it need not revoke the shared host-only tracker token used for cleanup comments. Add tracker repository/installation identity fields explicitly when tracker and product repositories differ; never silently reuse the product token for another repository.
- [ ] Persist the status write guard in `tracker_write_intents` before calling audited `kod_status::refresh`. Serialize the complete load/render/write cycle with snapshot activation. After restart, consult the operation guard and owned remote comments before allowing a create, independent of whether a failed audit-outcome append completed. Recompute status on startup and at 30 s intervals, so a dropped notification does not need a new queue. Do not store the rendered workflow as a new source of truth.
- [ ] Compose existing dispatcher ingress and gateway routers on separate bindings: only signed `/webhook` and `/command` may be exposed through public ingress; `/r/{token}/mcp` and model routes stay on the sandbox gateway network. Blocking adapters use `spawn_blocking` with provider read timeouts; kill handling cannot share a task or lock with a stuck model or status call. Keep `work_once`/follow-ups in their existing priority order.
- [ ] Before opening admission, acquire singleton ownership, reconcile orphan containers and broker leases, tombstone old registrations, run dispatcher restart recovery and confirm pending cleanup. Any unresolved cleanup keeps readiness false. Keep signed owner/backup kill usable during runtime audit/policy outage. Invalid startup config refuses activation; safety cleanup is attempted from validated durable identities before exit, without accepting arbitrary unauthenticated cleanup requests.
- [ ] Test source timeout/no stale fallback, second process refusal, old token denial, startup publishing with/without PR, source outage during kill, SIGTERM during publish, and failed revoke/stop. Readiness cannot report success while cleanup is pending. Run `cargo test -p kod-host --features connected-test-support --test service --test refresh` red then green. Then `cargo test --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`. Commit `Serve and recover the connected runtime`; evidence `host-service.log`.

### Step 1: Write failing ordering and route-isolation tests

Place the following tests in `src/service.rs`'s unit-test module, with `tower::ServiceExt` and the existing Axum/Tokio dev dependencies. These exercise the exact production helpers below, not a separate mock service implementation. They do not require an App key, Docker or future fixture methods.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::{Request, StatusCode}, routing::post};
    use std::cell::RefCell;
    use tower::ServiceExt;

    #[test]
    fn failed_recovery_does_not_open_dispatcher_or_enable_admission() {
        let events = RefCell::new(Vec::new());
        let result = recover_then_open(
            || { events.borrow_mut().push("recover"); anyhow::bail!("revoke pending") },
            || { events.borrow_mut().push("open"); Ok(()) },
        );
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["recover"]);
    }

    #[test]
    fn shutdown_attempts_stop_even_when_quiesce_and_revoke_fail() {
        let events = RefCell::new(Vec::new());
        let result = cleanup_all(
            || { events.borrow_mut().push("quiesce"); anyhow::bail!("store unavailable") },
            || { events.borrow_mut().push("revoke"); anyhow::bail!("provider unavailable") },
            || { events.borrow_mut().push("stop"); Ok(()) },
        );
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["quiesce", "revoke", "stop"]);
    }

    #[test]
    fn second_host_cannot_own_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let first = instance_lock(dir.path()).unwrap();
        assert!(instance_lock(dir.path()).is_err());
        drop(first);
        assert!(instance_lock(dir.path()).is_ok());
    }

    #[tokio::test]
    async fn model_and_tools_are_not_mounted_on_public_ingress() {
        let control = axum::Router::new()
            .route("/webhook", post(|| async { StatusCode::ACCEPTED }))
            .route("/command", post(|| async { StatusCode::FORBIDDEN }));
        let tools = axum::Router::new()
            .route("/r/{token}/mcp", post(|| async { StatusCode::NO_CONTENT }));
        let model = axum::Router::new().fallback(|| async { StatusCode::UNAUTHORIZED });
        let (ingress, internal) = split_routes(control, tools, model);
        for path in ["/r/known/mcp", "/r/known/v1/messages"] {
            let request = Request::post(path).body(Body::empty()).unwrap();
            assert_eq!(ingress.clone().oneshot(request).await.unwrap().status(), StatusCode::NOT_FOUND);
        }
        let request = Request::post("/r/known/mcp").body(Body::empty()).unwrap();
        assert_eq!(internal.clone().oneshot(request).await.unwrap().status(), StatusCode::NO_CONTENT);
        let request = Request::post("/command").body(Body::empty()).unwrap();
        assert_eq!(ingress.oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

}
```

Run `cargo test -p kod-host --lib service::tests` **before** adding the helpers: expect missing symbols, then ordering assertions to fail if cleanup is short-circuited. Step 5 also exercises `recover_and_attach` with the actual runtime; a flag assertion alone does not prove startup.

### Step 2: Implement the production ordering, mounting and worker boundaries

Put these complete helpers in `src/service.rs`. No new HTTP registration/shutdown endpoint or pluggable service framework is introduced; the closures keep the existing concrete dispatcher constructor ordered and make side effects testable.

```rust
use anyhow::{Result, bail};
use axum::Router;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync::watch;

fn recover_then_open<T>(recover: impl FnOnce() -> Result<()>, open: impl FnOnce() -> Result<T>) -> Result<T> {
    recover()?;
    open()
}

fn cleanup_all(
    quiesce: impl FnOnce() -> Result<()>,
    revoke: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<()> {
    // Evaluate every phase in order even if an earlier phase failed.
    let outcomes = [quiesce(), revoke(), stop()];
    let errors: Vec<String> = outcomes.into_iter().filter_map(Result::err)
        .map(|e| kod_tool_gateway::scrub(&format!("{e:#}"))).collect();
    if !errors.is_empty() { bail!("cleanup remains pending: {}", errors.join("; ")); }
    Ok(())
}

fn split_routes(control: Router, tools: Router, model: Router) -> (Router, Router) {
    (control, tools.fallback_service(model))
}

fn instance_lock(private_root: &std::path::Path) -> Result<std::fs::File> {
    use anyhow::{Context, ensure};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let root = std::fs::symlink_metadata(private_root)?;
    ensure!(root.is_dir() && !root.file_type().is_symlink()
        && root.permissions().mode() & 0o077 == 0, "host root must be private");
    let path = private_root.join("host.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(m) => ensure!(m.is_file() && !m.file_type().is_symlink(), "invalid host lock file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    let file = std::fs::OpenOptions::new().read(true).write(true)
        .create(true).truncate(false).mode(0o600).open(path)?;
    ensure!(file.metadata()?.permissions().mode() & 0o077 == 0, "host lock must be private");
    file.try_lock().context("another host owns this runtime")?;
    Ok(file)
}

pub(crate) async fn recover_and_attach(
    runtime: Arc<crate::runtime::Runtime>,
    open_dispatcher: impl FnOnce() -> Result<kod_dispatcher::Dispatcher> + Send + 'static,
) -> Result<Arc<kod_dispatcher::Dispatcher>> {
    tokio::task::spawn_blocking(move || {
        let dispatcher = Arc::new(recover_then_open(
            || runtime.recover_local(), open_dispatcher,
        )?);
        runtime.attach(Arc::downgrade(&dispatcher))?;
        Ok(dispatcher)
    }).await?
}

pub(crate) async fn dispatch_worker(
    dispatcher: Arc<kod_dispatcher::Dispatcher>,
    ready: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() { return Ok(()); }
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            _ = tick.tick() => {
                let d = dispatcher.clone();
                let allow_admission = ready.load(Ordering::Acquire);
                let result = tokio::task::spawn_blocking(move ||
                    d.work_once_with_admission(allow_admission)).await;
                let error = match result {
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    ready.store(false, Ordering::Release);
                    eprintln!("maintenance retry: {}", kod_tool_gateway::scrub(&error));
                }
            }
        }
    }
}
```

Add `Dispatcher::work_once_with_admission(bool)` as a compatibility-preserving variant of `work_once`: with false, select priority-0 cancellation/edit/command deliveries only, drain existing follow-ups and do not admit or start queued runs. `work_once` and `work_at` retain their current true/default behavior for other callers. **Do not block all `/webhook` requests while unready:** verified cancellation events must still be durably queued. Admission is closed in the worker, not by hiding the emergency control surface.

Because readiness can change after the worker captures the bool, also install a host-supplied `with_admission_guard(Arc<dyn Fn() -> bool + Send + Sync>)` on Dispatcher. The default is true for existing callers; the host's closure reads readiness atomically. Recheck this guard inside the admission transaction and before queued-start claim. Runtime Task 5's one-way spawn gate serializes the final process launch with shutdown; the readiness guard can only remove permission, never replace the durable stopped check. Seed a label while false, plus a cancellation on a running run: maintenance cancels/revokes/stops, leaves the label pending and starts zero runs. Flip true only after sources/recovery pass; the label may then be reconsidered against fresh inputs.

The host guard additionally checks `!runtime.cleanup_pending()`: remote revocation/stop debt must inhibit admission even if a prior source-health check set ready true. Capture a Weak Runtime in the closure to avoid an ownership cycle. Cleanup code never holds the in-memory cleanup mutex during RPC or SQLite access, so this guard cannot block a dispatcher transaction on the network. Only successful recovery/revalidation clears readiness; signed resume alone never overrides pending cleanup or revives old execution.

The concrete assembly passes `kod_dispatcher::router(dispatcher.clone())`, `kod_tool_gateway::router(tools.clone(), accepted_hosts)` and the router from `kod_llm_proxy::app_with_registry` to `split_routes`. Bind its two results to the separately validated ingress/internal addresses. Do not merge the internal result back into ingress. Open no listeners until recovery/attach succeeds; set ready true only after **both** binds, required source validation and worker installation succeed. Retain both listener task handles and singleton file lock for the process lifetime. A failed second bind invokes cleanup and never leaves the first listener accepting work.

Call `instance_lock` only on the owner/ancestor-validated resolved host root from Task 3; the sandbox has no access to that directory. Retain its returned File in the running service struct, not a temporary expression. Unit tests use TempDir's private directory; add symlink/public-mode refusal cases. The lock helper does not replace Task 3's ownership/ancestor validation.

On shutdown set ready false, signal worker cancellation, then invoke `cleanup_all(|| runtime.quiesce(), || runtime.revoke_all(), || Gateway::stop_all(runtime.as_ref()))` in a blocking task. Attempt all phases even if any fails. Keep the existing durable follow-ups and lease files until confirmation; nonempty errors produce an unsuccessful shutdown result. Do not treat `JoinHandle::abort` of a `spawn_blocking` call as proof it stopped: close the Runtime spawn gate first, join tracked container workers where possible, and retain/recover unresolved work on the next start. Cancellation remains bounded by the model/container deadlines from Tasks 2/4.

`Runtime::revoke_all` here performs prompt local invalidation and schedules/polls remote cleanup; it must not synchronously wait for a 30-second GitHub RPC before the stop phase. Pending remote revocation returns an error while the stop phase still runs. Its one in-memory in-flight handle per run only coordinates work; the existing dispatcher follow-up and owner-only lease are the durable retry obligations. Pin the full cancel-to-absence deadline, not a fresh 10-second timer started after a slow revoke. A timed-out or detached blocking RPC is not confirmation of revoked credentials.

### Step 3: Write the failing durable-status test, then implement its guard

Add `tests/refresh.rs` over a temporary dispatcher store. The HTTP lost-response test from the old summary is still required, but first test the exact SQLite guard without any unspecified provider fixture. The code belongs in `src/refresh.rs`; the test can be a unit module in that file to access it.

```rust
#[test]
fn uncertain_status_survives_reopen_and_blocks_a_second_create() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.db");
    let mut c = kod_dispatcher::store::open(&path).unwrap();
    let calls = AtomicUsize::new(0);
    let first = guarded_status_write(&mut c, "acme/app#1", 41, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(WriteFailure::uncertain("response lost"))
    });
    assert!(first.is_err());
    drop(c);
    let mut c = kod_dispatcher::store::open(&path).unwrap();
    assert!(guarded_status_write(&mut c, "acme/app#1", 42, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
```

Run `cargo test -p kod-host --lib refresh::tests::uncertain_status_survives_reopen_and_blocks_a_second_create -- --exact` red. Add the following complete guard and supporting types, then run it green:

```rust
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(crate) struct WriteFailure { not_applied: bool, message: String }
impl WriteFailure {
    pub(crate) fn uncertain(message: &str) -> Self {
        Self { not_applied: false, message: kod_tool_gateway::scrub(message) }
    }
    pub(crate) fn rejected(message: &str) -> Self {
        Self { not_applied: true, message: kod_tool_gateway::scrub(message) }
    }
}

fn guarded_status_write(
    c: &mut Connection, story: &str, audit_intent: u64,
    apply: impl FnOnce() -> std::result::Result<(), WriteFailure>,
) -> Result<()> {
    let intent = i64::try_from(audit_intent)?;
    let operation = format!("status:{story}:{intent}");
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending: Option<String> = tx.query_row(
        "SELECT operation_id FROM tracker_write_intents WHERE story=?1 AND kind='status' AND outcome IS NULL",
        [story], |r| r.get(0),
    ).optional()?;
    if pending.is_some() { bail!("status write unresolved; reconcile before retry"); }
    tx.execute(
        "INSERT INTO tracker_write_intents(operation_id,story,kind,audit_intent) VALUES(?1,?2,'status',?3)",
        params![operation, story, intent],
    )?;
    tx.commit()?; // Do not hold a DB transaction during HTTP.
    match apply() {
        Ok(()) => {
            c.execute("UPDATE tracker_write_intents SET outcome='posted' WHERE operation_id=?1", [&operation])?;
            Ok(())
        }
        Err(failure) => {
            if failure.not_applied {
                c.execute("UPDATE tracker_write_intents SET outcome='not_applied' WHERE operation_id=?1", [&operation])?;
            }
            bail!("{}", failure.message)
        }
    }
}
```

Add the partial unique index in Task 1's migration: `CREATE UNIQUE INDEX one_uncertain_status_per_story ON tracker_write_intents(story) WHERE kind='status' AND outcome IS NULL`. Keep one per-story OS lock across load/render/reconcile/write and snapshot activation; the index is a second line of defense, not permission to release that lock during HTTP.

The guard must run **at** the audited status write, not before arbitrary source reads. Add `kod_status::refresh_with_writer(source, tracker, audit, story, writer)` where `writer: FnOnce(&str, &str, u64) -> Result<()>` receives story, rendered text and the already-appended audit intent. Preserve `refresh` as a wrapper calling `tracker.status_comment` with that hook, so existing callers/tests stay valid. The host hook calls `guarded_status_write`; a failed DB/audit intent prevents the HTTP call. After a crash between the audit intent and hook, no guard means no write occurred; after guard commit, unresolved means reconcile even if outcome append failed.

Refactor the existing `kod-status/src/lib.rs::refresh` with this implementation; its current audit record names and error redaction remain the same:

```rust
pub fn refresh(
    source: &dyn Source, tracker: &dyn Tracker, audit: &AuditLog, story: &str,
) -> Result<View> {
    refresh_with_writer(source, tracker, audit, story,
        |key, text, _intent| tracker.status_comment(key, text))
}

pub fn refresh_with_writer(
    source: &dyn Source, tracker: &dyn Tracker, audit: &AuditLog, story: &str,
    writer: impl FnOnce(&str, &str, u64) -> Result<()>,
) -> Result<View> {
    let view = load(source, story)?;
    let issue = tracker.get_issue(story)?;
    if issue.key != view.story { bail!("tracker returned another story"); }
    let text = view.render();
    let intent = audit.append(RUN_AUDIT, &json!({
        "kind":"workflow_status_comment_intent", "tracker":tracker.name(), "story":story, "text":text
    }))?;
    match writer(story, &text, intent) {
        Ok(()) => {
            audit.append(RUN_AUDIT, &json!({
                "kind":"workflow_status_comment_posted", "tracker":tracker.name(), "story":story, "intent":intent
            })).context("comment posted but recording outcome failed; reconcile before retry")?;
        }
        Err(error) => {
            let reason = redact(&format!("{error:#}"));
            let failure = anyhow::anyhow!(reason.clone());
            if let Err(outcome) = audit.append(RUN_AUDIT, &json!({
                "kind":"workflow_status_comment_failed", "tracker":tracker.name(),
                "story":story, "intent":intent, "error":reason
            })) {
                return Err(failure.context(format!("recording failed comment outcome: {}", redact(&format!("{outcome:#}")))));
            }
            return Err(failure);
        }
    }
    Ok(view)
}
```

Before wrapping/scrubbing the provider failure, the host must retain the actual write disposition. Expose a small typed `github::WriteOutcome { NotApplied, Uncertain }` classifier from kod-tracker, defaulting untagged errors to Uncertain. Tag status validation/preflight failures and definite 4xx rejection as NotApplied; transport/5xx/malformed successful writes remain Uncertain. Classify inside the host hook before `kod_status` renders error text. Never infer certainty by matching an error message. Add rejected-then-success test with two calls and uncertain-then-retry test with one call.

Use the typed outcome itself as an anyhow context marker in `kod-tracker/src/github.rs`, replacing the private `UncertainWrite` marker and adding NotApplied context around preflight/definite rejections. At this boundary the implementation is:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome { NotApplied, Uncertain }
impl std::fmt::Display for WriteOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotApplied => "GitHub write was not applied",
            Self::Uncertain => "GitHub write outcome unknown; reconcile before retry",
        })
    }
}
impl std::error::Error for WriteOutcome {}
pub fn write_outcome(error: &anyhow::Error) -> WriteOutcome {
    error.downcast_ref::<WriteOutcome>().copied().unwrap_or(WriteOutcome::Uncertain)
}
```

In host `refresh.rs`, add the real adapter conversion and invoke the writer hook as follows; keep `Connection` local to the blocking worker and the story lock held through this call:

```rust
impl WriteFailure {
    fn from_tracker(error: anyhow::Error) -> Self {
        let message = format!("{error:#}");
        match kod_tracker::github::write_outcome(&error) {
            kod_tracker::github::WriteOutcome::NotApplied => Self::rejected(&message),
            kod_tracker::github::WriteOutcome::Uncertain => Self::uncertain(&message),
        }
    }
}
// In the blocking refresh worker, after opening `c` and taking the story lock:
let view = kod_status::refresh_with_writer(source, tracker, audit, story, |key, text, intent| {
    guarded_status_write(&mut c, key, intent, || {
        tracker.status_comment(key, text).map_err(WriteFailure::from_tracker)
    })
})?;
```

Add a read-only `GitHub::owned_status_comment(key) -> Result<Option<OwnedStatusComment>>`, returning only the bound issue's unique configured-bot-ID/marker match (`OwnedStatusComment { id, body }`); reuse existing pagination/ownership validation. On restart, read the unresolved row and remote owned comment under the story lock. Exactly one owned comment permits recording `remote_id`/`outcome='reconciled'` and subsequent updates to that ID. Zero or duplicate/unknown comments stay unresolved and make zero POSTs; do not clear uncertainty because a list is empty. A known-definite rejection may be marked NotApplied and retried. Do not reconstruct a writer to bypass this check during credential renewal.

### Step 4: Supply the real credential/refresh wiring and shutdown behavior

Read-only refresh calls run on blocking workers with a 10-second provider deadline. The 1-second debounce and 30-second reconciliation timers only request refresh; each refresh loads fresh source records. A required-source failure closes admission but does not kill the emergency ingress or stop cleanup processing. Resume readiness only after all pending cleanup/source health checks succeed; do not clear the dispatcher's persisted kill switch.

Use separate tracker and publisher credential directories and the shared signer described above; add tests verifying exact repository IDs and `issues:write` versus publish permission requests, token renewal before expiry without replacing the GitHub instance, wrong bot/installation refusal and retained lease on revoke failure. The owner/backup command keys are installed on the dispatcher before its router is bound. Source/model/tracker errors cannot be promoted into an unauthenticated host stop endpoint.

### Step 5: Run the integration regressions and commit

Complete `tests/service.rs` and `tests/refresh.rs` with real dispatcher/router instances, the local GitHub provider and the Runtime from Task 5. Their fixture creates owner-only files and the existing fake/scripted remote providers; it does not replace dispatcher, model/tool gateway, guard or Runtime behavior. Use counters and barriers at these exact boundaries:

| Test | Injected failure/barrier | Required observation |
| --- | --- | --- |
| `startup_pending_cleanup_keeps_admission_closed` | Broker revoke returns 503; one owned container is still present | No new run/model/PR, no accepting listeners/readiness; cleanup records retained; retry succeeds only after both confirmed |
| `publishing_restart_uses_only_bound_pr` | Crash after push, after PR create, and failed PR lookup in three cases | Done only for confirmed bound PR; otherwise NeedsHuman; no new execution or PR POST |
| `unready_worker_still_processes_cancel_and_cleanup` | Ready false plus queued label/cancel and failed tracker | Label remains pending; canceled run's model/MCP credentials denied; revoke/stop still attempted |
| `owner_kill_remains_live_during_dependency_outage` | Silent model stream, failed policy load and runtime audit-write failure | Existing signed owner command stops; bad signature fails; no command waits on source refresh |
| `lost_status_response_after_restart_posts_once` | Provider commits comment then disconnects before response/outcome | Reopen same DB, rotate token, refresh concurrently; one POST total; human comment preserved |
| `empty_remote_status_does_not_clear_unknown_create` | Same lost response followed by hidden remote comment | Guard remains pending and POST count stays one |
| `rejected_status_allows_retry` | 403/422 before provider mutation | Guard becomes NotApplied; next call can create once; no unknown-outcome classification |
| `shutdown_reports_failed_stop_and_retains_followup` | Container fails disappearance check at 10 s | Nonzero shutdown, pending stop row/lease remains; no claim that aborting a blocking task stopped it |

Run unit tests first: `cargo test -p kod-host --lib service::tests` and `cargo test -p kod-host --lib refresh::tests`. Then run `cargo test -p kod-host --features connected-test-support --test service --test refresh`; zero executed tests is failure. Full final checks: `cargo test --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`. Commit `Serve and recover the connected runtime`; retain red/green logs and ordered provider/DB observations in ignored `host-service.log`/`host-refresh.log`. Actual Docker absence and live GitHub acceptance still belong to 6b-c.

## Exit criteria and handoff

This plan is independently mergeable when the production composition binary, real adapters and non-Docker failure/race tests pass, old crate tests still pass, a whole-branch review approves and the documented defaults are pinned. It does not require actual deployment secrets for local provider tests. Record commands and limitations in the PR.

6b-c then proves real containers and the live GitHub/model path. Missing owner prerequisites remain BLOCKED; do not label 6b complete or rewrite the phase-0 baseline status. The next docs PR updates component commands and the roadmap only to the level actually demonstrated.
