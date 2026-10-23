# Phase 1.6b-a Connected GitHub Broker and Forge Implementation Plan

> **For agentic workers:** use the per-task mode in **Execution**; finish with one whole-branch reviewer. This is independently mergeable production adapter work, followed by `2026-10-23-phase1-connected-host.md` and `2026-10-23-phase1-connected-acceptance.md`. It does not require `kod-host`.

**Goal:** Implement the real GitHub App broker and publishing Forge behind the existing gateway traits, with durable revocation, deterministic commits and recoverable draft-PR creation. Local HTTP contracts establish the standalone merge boundary; the later plans establish connected host and live acceptance.

**Architecture:** One `Arc<github::GitHub>` implements both existing `Broker` and `Forge`, so a credential is bound to a registered run, repository and exact branch. `github/mod.rs` owns configuration, transport and shared state; `auth.rs` owns signing, issuance and lease recovery; `forge.rs` owns Git data, refs and PRs. No host crate, service binary, scheduler or new database.

## Scope
### Affected user and ideal state
**Affected user:** the trusted-host operator currently has fake-only publish credentials and Forge; after this plan they can assemble real adapters, revoke outstanding leases after restart, and publish ordinary text changes safely. The delegating engineer receives one correctly attributed draft PR on retry. Live connected operation remains the acceptance plan's responsibility.

| Row | Statement | Reason |
|---|---|---|
| IS-1 | Credentials remain host-only, repository-scoped and recoverably revoked. | A stopped/restarted publisher must not silently retain usable credentials. |
| GAP-1 | Only `Broker` and `FakeGitHub` exist (`src/broker.rs:30`, `src/forge.rs:251`). | The current contract has no production issuer or restart recovery. |
| IS-2 | Only the registered run's exact repository/branch is writable. | Installation-token permissions do not encode an `agent/*` boundary. |
| GAP-2 | The fake checks the repository; production branch/rules activation is absent (`src/forge.rs:237`). | Real tokens require trusted mediation and repository protection. |
| IS-3 | Supported text edits preserve parent contents/modes and yield deterministic commits. | Retrying must not change the patch or the SHA. |
| GAP-3 | `Change` contains only path/optional UTF-8 text; fake commits hash debug text (`src/forge.rs:13`, `:278`). | A real Git data implementation and an explicit metadata boundary are missing. |
| IS-4 | Lost replies and restart produce at most one draft PR; existing callers still work. | Ambiguity must not cause duplicate or unrelated publication. |
| GAP-4 | The existing contract exercises an in-memory Forge only (`src/forge.rs:84`, `tests/publish.rs:278`). | HTTP semantics, pagination and durable uncertain writes are unproven. |

Paths abbreviated as `src/...` in this table are under `crates/kod-tool-gateway/`.

### Must have
- Preserve `Broker`, `Forge`, `Change`, `Commit`, `Secret`, fake implementations and existing gateway construction. Add the concrete adapter and public registration/recovery methods.
- Owner-only durable lease files with exclusive ownership, atomic replacement and directory synchronization; fail closed on corrupt state. Known token cleanup must survive process restart.
- Normal UTF-8 additions, modifications and deletions; existing regular executable files retain `100755`, existing regular files retain `100644`, additions use `100644`.

### Must NOT have
- No production PAT fallback, native branch-scoped-token claim, App/repository/rules provisioning, model calls, auto-merge, `kod-host` dependency or third database.
- No binary/symlink/gitlink/rename/mode-change support hidden behind `Change`. The runtime plan must reject unsupported raw diff metadata before constructing `Change`; the Forge independently rejects unsafe paths, remote entry types and binary blobs before writes.

## Global Constraints
- Work from `cd "$(git rev-parse --show-toplevel)"`; resolve all paths from that repository, never a user's checkout path. Rust 1.99.0/edition 2024 as recorded in `rust-toolchain.toml:1` and the crate manifest.
- Tests use loopback HTTP and a temporary Git repository only. Generate an ephemeral RSA test key with argument-array `openssl genpkey`; never use a developer's credentials. Production uses OpenSSL RS256 through `Command`, stdin/stdout pipes and a checked key file, with no shell interpolation. Validate `git`/`openssl` availability and capture no private-key material in errors.
- Promote existing reqwest 0.12 to normal dependencies with `blocking,json,rustls-tls` and no default features; add direct existing-lockfile `serde` derive, `base64` 0.22 and `chrono` 0.4 dependencies. Update `Cargo.lock` only as required; keep `deny.toml` unchanged.
- Add `/.omo/evidence/` to `.gitignore`; command transcripts go under `.omo/evidence/phase1-connected/`. No raw credentials, payload dumps or transcripts are committed. The later acceptance plan alone commits curated, sanitized acceptance manifests.
- Add default-off Cargo feature `test-support`. Keep fixture implementation private, but expose a feature-gated loopback-only constructor and clock for dependent host tests; normal builds/CLI have no endpoint override. Production HTTPS origin is fixed to `https://api.github.com`; no redirect following, 10-second connect/30-second total deadline, bounded JSON response, sensitive Authorization header, fixed GitHub API version `2026-03-10`, status-only sanitized provider errors.
- Finish each task's failing test, smallest implementation, green test and commit before dependent tasks. Every listed test must exist and execute; a filtered command running zero tests fails verification.

## Decisions
| | Decision | Why |
|---|---|---|
| D1 | Use one shared concrete adapter implementing both existing traits. | A token can be checked against the same durable lease/permit in every Forge method without changing trait callers. |
| D2 | A permit contains run, native tracker ticket, repository and pinned base; branch is exactly `agent/{ticket}-{run}`. | Matches gateway `src/lib.rs:504,597`; GitHub tickets contain `owner/repo#N` (`kod-tracker/src/github.rs:183`). Do not sanitize into a different branch. |
| D3 | Issue one-repository tokens with `contents:write`, `pull_requests:write`, implicit metadata read; reject other granted permissions. | GitHub's [installation-token API](https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app) scopes repositories and permissions, not branches. |
| D4 | Persist a pending issuance marker before POST, then the received secret before returning it. | A process can die between issuance and saving the response; that unknown secret cannot be revoked by token. Keep admission blocked until its maximum one-hour lifetime plus 60-second skew allowance has elapsed; inject time in tests. Never report that interval as successful revocation. |
| D5 | Activation requires an active repository guard ruleset for non-agent branches, with no App/role-wide bypass. | [Rulesets](https://docs.github.com/en/rest/repos/rules) provide defense outside the adapter. Do not create or repair rules remotely. |
| D6 | Preserve the existing `Change` contract; preflight all changes before any Git write. | It cannot carry original rename/type/mode intent (`src/forge.rs:11-16`). Raw-diff rejection belongs to the collector; unsupported remote entries still fail here. |
| D7 | Use deterministic Git trees/commits, create-or-confirm refs, and a durable PR intent before POST. | Git object retries are content-addressed; PR creation is not. An unresolved PR intent forbids another create, even when one list response is empty. |
| D8 | Keep nonsecret per-run commit/PR intent in the lease record after revoking tokens. | Restart must preserve ambiguous publication knowledge; this is an owner-only file, not another database or dispatcher state machine. |

## Execution
| Task | Mode | Depends on | Why |
|---|---|---|---|
| 1. Adapter contract and local provider | Native | none | Establish one compatible public seam and working fixture. |
| 2. App issuance and durable recovery | Subagent + review | 1 | Credentials, persistence and crash windows require focused review. |
| 3. Deterministic text commits | Subagent + review | 2 | Preflight and Git tree correctness share credential validation. |
| 4. Exact refs and recoverable draft PRs | Subagent + review | 3 | Reconciliation depends on the recorded commit identity. |
| 5. Gateway conformance and operating contract | Native | 4 | Prove the adapter fits existing callers and document activation. |

## Review Focus
- An unknown, expired, revoked or other-run credential cannot write; inactive/overbroad protection blocks activation, with no repository write during verification.
- An issuance or PR response lost across restart retains explicit uncertainty; retries never invent success, discard cleanup debt or duplicate a PR.
- Commit preflight checks every changed path, parent directory and existing blob before the first mutation. Recursive-tree truncation must fail closed.
- Real issue keys with slashes, executable regular files and text deletions work; unrelated/default refs, symlinks, gitlinks, NUL-containing text and divergent refs fail without writes.
- Review sanitized errors at the adapter boundary, because `Gateway::publish` can record a broker error without possessing the newly minted token (`src/lib.rs:476-489`).

## Verification strategy
Offline verification runs without production credentials. The optional live broker contract requires the owner prerequisites below before agent-executed verification.
- Test decision: TDD with Rust unit/integration tests, a local Axum GitHub provider and real temporary Git object hashes; existing publish and MCP tests stay intact.
- QA policy: every task runs happy and adversarial scenarios with exact HTTP request/mutation counts, disk state and final Git/PR state assertions.
- Evidence: `.omo/evidence/phase1-connected/a-task-N.log`; set `set -o pipefail` before piping commands into `tee`. Outside an existing goal loop this is the evidence directory; under a loop also record its attempt directory in the transcript.

Tasks run in dependency order 1 through 5 because they share adapter/fixture files. Do not add nominal parallel waves that conflict over these files.

## TDD tasks
### Task 1: Define the public adapter and an executable local provider fixture
**Files:** create `crates/kod-tool-gateway/src/github/{mod,auth,forge,tests}.rs`; modify `crates/kod-tool-gateway/{Cargo.toml,src/lib.rs}`, `Cargo.lock`, `.gitignore`. Closes GAP-1, GAP-2. Wave 1; blocks 2; no dependencies. Do not add a host dependency or change existing trait signatures.

**References:** `src/broker.rs:9-35` (`Secret`, broker contract); `src/forge.rs:42-77` (`Commit`, Forge); `crates/kod-tracker/src/github.rs:153-172` (HTTP construction); `crates/kod-tracker/src/github_tests.rs:1-88` (Axum fixture); all unqualified `src/` paths refer to `crates/kod-tool-gateway/`.

- [ ] **Step 1: Write failing tests** in `github/tests.rs`, declare `#[cfg(test)] mod tests;`, and expose `pub mod github;`. The test module's `Fixture::start()` binds `127.0.0.1:0` on a dedicated Tokio thread; Drop stops/joins it. `Fixture::adapter()` uses a new temporary lease directory and an injected clock; its generated test RSA key never leaves that temp directory. Implement fixture counters `requests(method,path)`, `mutations()`, and private accessors for lease permissions. No external service is contacted.
```rust
#[test]
fn constructor_rejects_untrusted_inputs_before_http() {
    let f = Fixture::start();
    let mut config = f.config();
    config.repo = "acme/../other".into();
    assert!(f.adapter_with(config).is_err());
    assert_eq!(f.mutations(), 0);
    assert_eq!(f.total_requests(), 0);
}
```
- [ ] **Step 2: Run red:** `cargo test -p kod-tool-gateway --lib github::tests::constructor_rejects_untrusted_inputs_before_http -- --exact`; expect compilation failure for the missing adapter/fixture API, then an assertion failure with a permissive constructor stub.
- [ ] **Step 3: Implement** these public types/methods in `github/mod.rs`; fields shown are the entire caller-owned configuration, not suggestions. `new` is local-only. `recover` performs cleanup; `verify_repository` mints/revokes a probe token and checks remote identity/protection, so it must never be called by a command promising read-only checks.
```rust
pub struct Config {
    pub repo: String, pub repository_id: u64, pub installation_id: u64,
    pub base_branch: String, pub guard_ruleset_id: u64, pub bot_user_id: u64,
}
pub struct AppCredentials { pub app_id: u64, pub private_key: std::path::PathBuf }
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunPermit { pub run: String, pub ticket: String, pub repo: String, pub base: String }
// Implement on GitHub (which implements Broker + Forge):
// new(Config, AppCredentials, impl AsRef<Path>) -> anyhow::Result<Self>
// register_run(&self, RunPermit) -> anyhow::Result<()>
// recover(&self) -> anyhow::Result<()>
// verify_repository(&self) -> anyhow::Result<()>
#[cfg(any(test, feature = "test-support"))]
pub type TestClock = std::sync::Arc<dyn Fn() -> i64 + Send + Sync>;
// Under the same cfg, implement this associated method on GitHub:
// with_test_api(Config, AppCredentials, impl AsRef<Path>, &str, TestClock)
//     -> anyhow::Result<Self>
```
Validate positive IDs, canonical `owner/repo`, 40-character lower-case SHA, nonempty/control-free identities and exact Git ref validity with `Command::new("git").args(["check-ref-format", &format!("refs/heads/{branch}")])`. Tickets retain slashes and `#`; reject invalid constructed refs. Reject a default branch equal to any registered branch. Identical registration is idempotent; changed run bindings fail. `with_test_api` accepts only an HTTP literal loopback IP, explicit port, empty credentials and no query/fragment/non-root path; no DNS hostname override. Dependent host test-support features enable it explicitly; `GitHub::new` remains fixed-origin even when the feature is compiled. `Arc<GitHub>` coerces directly to both existing trait objects.
- [ ] **Step 4 / Acceptance / QA:** rerun the exact red command green (one test); also run `cargo test -p kod-tool-gateway --lib github::tests::registration -- --nocapture` with tests `registration_native_issue_key` and `registration_conflict`. Assert `agent/acme/app#7-run-1` is unchanged; conflict and malformed config make zero HTTP requests. Record `.omo/evidence/phase1-connected/a-task-1.log`.
- [ ] **Step 5 / Commit:** `Define the connected GitHub adapter contract` — only Task 1 files; the intermediate adapter's unimplemented network operations return explicit errors and all existing tests pass.

### Task 2: Issue App tokens and retain revocation obligations across restart
**Files:** modify `src/github/{mod,auth,tests}.rs`, `src/broker.rs` documentation. Closes GAP-1, GAP-2. Wave 2; depends 1; blocks 3. Do not add SQL, change the Broker trait, expose token serialization publicly, or silently treat failed cleanup as success.

**References:** `src/broker.rs:27-35` (correct the branch-scoping promise); `src/lib.rs:476-489,550-577` (publish/revoke and fresh cancellation cleanup token); [JWT claims/signing](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app); [mint](https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app); [revoke](https://docs.github.com/en/rest/apps/installations#revoke-an-installation-access-token).

- [ ] **Step 1: Write failing tests** with the fixture verifying RSA signature, `iss == app_id`, `iat == now-60`, `exp == now+540`, exact repository ID and permission request; record no raw JWT in assertion output.
```rust
#[test]
fn auth_restart_revokes_every_known_lease() {
    let f = Fixture::start(); let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let first = a.mint("run-1", "acme/app").unwrap();
    let second = a.mint("run-1", "acme/app").unwrap();
    assert_eq!(f.known_leases(), 2);
    drop(a); let restarted = f.reopen_adapter();
    restarted.recover().unwrap();
    assert_eq!(f.requests("DELETE", "/installation/token"), 2);
    assert!(!f.token_is_live(first.expose())); assert!(!f.token_is_live(second.expose()));
    restarted.revoke("never-started").unwrap(); assert_eq!(f.known_leases(), 0);
}
```
- [ ] **Step 2: Run red:** `cargo test -p kod-tool-gateway --lib github::tests::auth_ -- --nocapture`; expect mint/recovery stubs to fail the restart test and each security assertion.
- [ ] **Step 3: Implement** RS256 using an owner-only, non-symlink regular key file in an owner-only parent. Use `Command::new("openssl").args(["dgst","-sha256","-sign"]).arg(&key_path)` with piped stdin containing the encoded header/claims and piped stdout for the signature; verify stable inode/metadata around opening/signing. Never place key bytes in argv. Encode header/claims/signature with URL-safe unpadded base64. Check process status; discard stderr content from public errors.
Lease directory: require current-user ownership and mode `0700`, ancestor trust/no symlink traversal, and a lifetime OS file lock (`File::try_lock`) so two adapter processes cannot own it. Files use hashed run names, `0600`, same-directory temporary creation, `sync_all`, rename and directory `sync_all`; inspect existing entries without following symlinks. One private serde lease record stores its immutable permit, known tokens/expiry, pending issuance deadlines, expected commit and PR intent. Keep secrets out of `Debug`. Under an operation mutex, fsync pending before issuance, fsync known token before returning `Secret`, then clear pending. If persistence fails after receiving a token, attempt immediate revoke; retain pending uncertainty until cleanup is confirmed or expiry passes. Never retry an uncertain mint automatically.
`mint` validates permit/repo, signs App JWT and posts `{"repository_ids":[id],"permissions":{"contents":"write","pull_requests":"write"}}`. Validate actual response permissions, expiry and returned repository scope (or inspect all `/installation/repositories` pages when omitted); reject widened results and revoke. Before exposing a token, read the repository and configured guard ruleset: IDs/name/default branch must match; guard `target=branch`, `enforcement=active`, includes exactly `["~ALL"]`, excludes exactly `["refs/heads/agent/*","refs/heads/agent/**/*"]`, contains creation/update/deletion restrictions, and allows only explicitly listed human-team bypasses. Reject App, broad-role and unrecognized bypasses. This narrow supported guard shape intentionally avoids a general rules engine. Revalidate each mint; failed verification revokes the probe/publish token. Rulesets must already exist and support these endpoints.
`revoke(run)` attempts every known token with `DELETE /installation/token`; remove only on 204, authenticated 401 confirming unusability, or validated expiry. Keep 403/5xx/transport errors pending and return a fixed error. `revoke_all` and `recover` attempt all records, aggregate failures without secrets, and refuse readiness while pending issuance has not expired. Recovery never re-exposes old tokens. Preserve run permit and PR intent after revocation for later reconciliation/cleanup mint; an absent run is a successful no-op. A host must keep the clock synchronized and fail on rollback, never use negative/overflowed expiration arithmetic.
- [ ] **Step 4 / Acceptance / QA:** rerun the red command green; include `auth_failed_revoke_retains_lease`, `auth_lost_mint_reply_blocks_restart`, `auth_lease_write_failure`, `auth_disabled_rules_refuses_token`, `auth_widened_scope_revoked`, `auth_second_owner_denied`, and `auth_errors_redacted`. Inject clock advance 3661 seconds instead of sleeping. Assert every refusal causes zero Git/PR writes, a failed revoke remains on disk, recovery revisits all tokens, and `Secret`/errors omit planted credentials. Evidence `.omo/evidence/phase1-connected/a-task-2.log`.
- [ ] **Step 5 / Commit:** `Persist and revoke GitHub App leases` — Task 2 files; no trait signature changes.

### Task 3: Create deterministic Git commits after complete text preflight
**Files:** modify `src/github/{mod,forge,tests}.rs`. Closes GAP-3. Wave 3; depends 2; blocks 4. Do not shell out to authenticated Git, silently translate unsupported metadata, or alter `Change`/`Commit`.

**References:** `src/forge.rs:11-52` (input boundary); `src/lib.rs:171-189` (existing size/duplicate checks); [trees](https://docs.github.com/en/rest/git/trees), [blobs](https://docs.github.com/en/rest/git/blobs), [commits](https://docs.github.com/en/rest/git/commits#create-a-commit).

- [ ] **Step 1: Write failing tests**. The fixture's object store must compute real Git hashes using an ephemeral bare repository and argument-array `git hash-object`, `git mktree`, `git commit-tree`; do not repeat the fake's debug-text hash. Seed a nested text file, an executable text file, a deletion and an untouched neighbor. `text_commit()` updates/adds/deletes those supported files.
```rust
#[test]
fn commit_is_repeatable_and_preserves_regular_modes() {
    let f = Fixture::start(); let a = f.adapter(); a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap(); let c = f.text_commit();
    let first = a.commit(&t, &c).unwrap(); let second = a.commit(&t, &c).unwrap();
    assert_eq!(first, second); assert_eq!(f.unique_commit_objects(), 1);
    assert_eq!(f.mode(&first, "bin/check"), "100755");
    assert_eq!(f.contents(&first, "new.txt"), Some("new\n".into()));
    assert_eq!(f.contents(&first, "old.txt"), None);
    assert_eq!(f.contents(&first, "untouched.txt"), Some("keep\n".into()));
}
```
- [ ] **Step 2: Run red:** `cargo test -p kod-tool-gateway --lib github::tests::commit_ -- --nocapture`; expect unimplemented commit errors, not false-green empty selection.
- [ ] **Step 3: Implement** token→lease→permit validation, exact repository/base matching and the existing 1000-file/8-MiB limits before HTTP writes. Validate identity, date range and message before writes too. Validate path components: reject empty/absolute, `.`/`..`, `.git` case-insensitively, backslash, controls, duplicate paths and file/directory prefix conflicts; reject NULs and empty change sets. GET pinned commit then its recursive tree; reject truncated/malformed tree and unsafe ancestors. GET/decode every touched existing blob, including deletions, reject invalid UTF-8/NULs and unsupported types/modes. Existing leaves must be blobs with `100644`/`100755`; additions cannot overwrite a directory or descend through a symlink/gitlink. Deleting a nonexistent path is a preflight error. Complete the entire read/preflight pass before the first POST.
Build entries in lexicographic path order, retaining base tree and existing modes. Content may be supplied directly to Create Tree (no redundant blob POST is needed). Deletion entries use `sha:null`; never encode deletion as empty content. Use the same `author` and `committer`, exact message/parent SHA, and `chrono::DateTime::from_timestamp(c.date,0)` formatted RFC3339 UTC for both dates. Do not send a signature or current time. Persist the resulting SHA in the run lease; a changed input producing another SHA for that run is refused before branch mutation.
```rust
let entry = match &change.content {
    Some(text) => serde_json::json!({"path":change.path,"mode":mode,"type":"blob","content":text}),
    None => serde_json::json!({"path":change.path,"mode":mode,"type":"blob","sha":null}),
};
let person = serde_json::json!({"name":c.committer.name,"email":c.committer.email,"date":date});
let payload = serde_json::json!({"tree":tree_sha,"parents":[c.base],"message":c.message,
    "author":person,"committer":person});
```
- [ ] **Step 4 / Acceptance / QA:** rerun the red command green. `commit_preflight_rejects_unsafe_tail` puts a valid file first and each unsupported file last; assert exactly zero Git mutations, including tree/commit POSTs. Cover symlink/gitlink/file-directory collisions, binary deletion/modification, invalid UTF-8 remote blob, duplicate/NUL/path traversal, unknown base and truncated tree. Assert two tree/commit request sequences yield one tree/commit object identity and unchanged untouched content. Evidence `.omo/evidence/phase1-connected/a-task-3.log`.
- [ ] **Step 5 / Commit:** `Create deterministic GitHub text commits` — Task 3 files; tests explicitly document that raw rename/mode intent is rejected upstream before `Change` exists.

### Task 4: Mediate exact refs and reconcile draft PR creation
**Files:** modify `src/github/{mod,auth,forge,tests}.rs`. Closes GAP-2, GAP-4. Wave 4; depends 3; blocks 5. Never force-push, overwrite divergent refs, mutate a PR belonging to another author or blindly replay an uncertain PR POST.

**References:** `src/forge.rs:58-135` (methods and contract); `src/lib.rs:554-580,591-615` (cancel cleanup and restart lookup); [Git refs](https://docs.github.com/en/rest/git/refs); [PR list/create](https://docs.github.com/en/rest/pulls/pulls).

- [ ] **Step 1: Write failing tests**; extend `Fixture` to commit the PR then close the response stream, and to hide it on one subsequent list call. `publish_once(&GitHub,&Fixture)` mints, commits, sets branch, finds/opens the draft, and revokes even on error, matching gateway ordering.
```rust
#[test]
fn pr_lost_reply_survives_restart_without_second_post() {
    let f = Fixture::start(); let a = f.adapter(); a.register_run(f.permit()).unwrap();
    f.lose_next_pr_reply(); f.hide_pr_lookup_after_next_create();
    assert!(publish_once(&a, &f).is_err()); drop(a);
    let b = f.reopen_adapter(); b.recover().unwrap(); b.register_run(f.permit()).unwrap();
    assert!(publish_once(&b, &f).is_ok());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
    assert_eq!(f.pr_count(), 1); assert!(f.only_pr_is_draft());
}
```
- [ ] **Step 2: Run red:** `cargo test -p kod-tool-gateway --lib github::tests::pr_ -- --nocapture` and `cargo test -p kod-tool-gateway --lib github::tests::refs_ -- --nocapture`; expect the missing ref/PR implementations to fail.
- [ ] **Step 3: Implement** validation in every Forge entry point, before HTTP: token is known/live, repository and exact branch equal the permit, no tag/default/other-run target, SHA equals the recorded commit. Use encoded path segments/query parameters rather than raw URL interpolation. `set_branch`: GET exact ref; equal SHA succeeds, absent creates, any different SHA fails. A lost create reply/422 must be reconciled by exact GET; never use matching-prefix results or force update. `delete_branch`: GET and verify recorded expected SHA before DELETE; 404 is success; after lost DELETE reply confirm absence. Ref mutation APIs lack compare-and-swap: document the remaining trusted-owner race rather than claiming atomic foreign-ref protection; uncertain/conflicting state stays blocked for inspection.
`find_pr`: list all pages with exact owner:branch/base filters, then validate `state=open`, `draft=true`, `head.repo.id`, `head.ref`, `base.repo.id`, `base.ref`, and `user.id == bot_user_id`; reject multiple or mismatched candidates. Validate HTTPS GitHub PR URL/repository/number and head SHA against stored commit when available. Follow only same-origin bounded pagination; inconsistent pages, 403, rate limits or malformed responses are errors, never absence. Recovery may recover the recorded SHA from the owned PR only when a durable permit/PR intent already exists; never accept an unbound PR.
`open_draft_pr` first calls `find_pr`, serializes per-run creates, fsyncs `{expected_sha,branch,base,state:"uncertain"}` before POST, and sends `draft:true`, `maintainer_can_modify:false`, exact title/body and configured base branch. Save validated result on success. A transport/5xx/malformed-success response stays uncertain; reconcile by list, and if absent return `GitHub PR creation outcome unknown; reconcile before retry` without POST. A definite validation rejection may clear intent only when the provider unambiguously created nothing; duplicate-PR 422 reconciles first. Keep intent after token revocation/restart. The adapter never closes, edits or merges a PR as compensation.
- [ ] **Step 4 / Acceptance / QA:** both red commands green; `pr_paginated_existing` finds page-two owned PR with zero POST; `pr_hidden_uncertain_blocks_retry` repeats empty lookup with one total POST; `pr_ambiguous_candidates` and `pr_foreign_author` produce no mutations. `refs_wrong_run`, `refs_divergent`, `refs_lost_create_reply`, `refs_delete_twice` assert exact final refs and create/delete request counts; denied targets make zero HTTP requests. Evidence `.omo/evidence/phase1-connected/a-task-4.log`.
- [ ] **Step 5 / Commit:** `Reconcile GitHub refs and draft PRs` — Task 4 files.

### Task 5: Prove compatibility and document activation/recovery
**Files:** modify `crates/kod-tool-gateway/src/github/tests.rs`, the private fixture module owned by that file (if Task 1 extracted it), and `docs/tool-gateway-runbook.md`; add `crates/kod-tool-gateway/tests/github_api.rs`. Keep all code blocks below in their stated target modules; consolidate duplicate imports when inserting them. Narrow adapter fixes belong in `src/github/{mod,auth,forge}.rs` only when a test below demonstrates the defect. Closes GAP-1 through GAP-4. Wave 5; depends 4; blocks final verification. Keep the Task 1 public API and existing traits unchanged.

**References:** `crates/kod-tool-gateway/src/forge.rs:84-135` (unchanged contract), `src/lib.rs:194-209,476-580` (composition, publish and cleanup), `tests/common/mod.rs:87-157` (real gateway construction), `tests/publish.rs:336-347` (cancellation during ref publication), `src/forge.rs:358-375` (existing workspace fake), `crates/kod-tracker/src/fake.rs:265-284` (seeded tracker). Paths without a crate prefix refer to `crates/kod-tool-gateway/`. [Installation-token revocation](https://docs.github.com/en/rest/apps/installations#revoke-an-installation-access-token) supplies the optional live assertion.

- [ ] **Step 1: Add these failing tests** to `src/github/tests.rs`. Reuse Task 1's `Fixture::start()`, `adapter()`, `permit()`, `config()`, `adapter_with(Config) -> anyhow::Result<GitHub>`, and Task 2's `reopen_adapter()`. The additional fixture methods and Gateway composition are implemented in Step 3, below; no mock `Forge` stands in for `GitHub`.
```rust
use super::{AppCredentials, Config, GitHub, RunPermit};
use crate::{Adapters, Gateway, Run, broker::Broker, docs::FakeDocs};
use crate::forge::{self, Change, Collected, FakeWorkspace, Identity};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc};
use std::time::Duration;

#[test]
fn contract_real_http() {
    let f = Fixture::start(); let a = f.adapter();
    let base = f.permit().base;
    a.register_run(RunPermit { run: "contract-run".into(), ticket: "CONTRACT-1".into(),
        repo: "acme/app".into(), base: base.clone() }).unwrap();
    forge::check(&a, &a, "acme/app", &base).unwrap();
    assert!(!f.branch_exists("agent/CONTRACT-1-contract-run"));
    assert_eq!(f.pr_count(), 1);
    assert_eq!(f.credential_counts(), (2, 0));
    assert_eq!(f.requests("DELETE", "/installation/token"), 2);
}

#[test]
fn contract_gateway_cancel_after_ref() {
    let f = Fixture::start(); let a = Arc::new(f.adapter());
    let dir = tempfile::tempdir().unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let gw = gateway(&f, a.clone(), dir.path(), stopped.clone());
    let (created, release) = f.pause_after_ref();
    let worker = std::thread::spawn(move || gw.publish("run-1"));
    let reached = created.recv_timeout(Duration::from_secs(10));
    // Release even on a failed assertion; never strand the fixture thread.
    stopped.store(true, Ordering::SeqCst);
    let _ = release.send(());
    let result = worker.join().unwrap();
    reached.expect("publish never reached the committed remote ref");
    assert!(result.unwrap_err().contains("the run was canceled during publish"));
    assert!(!f.branch_exists("agent/FAKE-1-run-1"));
    assert_eq!(f.pr_count(), 0);
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 0);
    assert_eq!(f.credential_counts(), (2, 0)); // Publish + fresh cleanup token.
    assert_eq!(f.requests("DELETE", "/installation/token"), 2);
    assert_eq!(f.requests("DELETE", "/repos/acme/app/git/refs/heads/agent/FAKE-1-run-1"), 1);
}

#[test]
fn contract_activation_failure_and_restart_cleanup() {
    let f = Fixture::start(); let mut wrong = f.config();
    wrong.base_branch = "wrong-default".into();
    let refused = f.adapter_with(wrong).unwrap();
    assert!(refused.verify_repository().is_err());
    assert_eq!(f.credential_counts(), (1, 0));
    assert_eq!(f.mutations(), 0); // Git/PR writes; issuance/revocation are not Git mutations.
    drop(refused);
    let a = f.adapter(); a.register_run(f.permit()).unwrap();
    let token = a.mint("run-1", "acme/app").unwrap();
    drop(a);
    let restarted = f.reopen_adapter(); restarted.recover().unwrap();
    assert!(!f.token_is_live(token.expose()));
    assert_eq!(f.credential_counts(), (2, 0));
    restarted.verify_repository().unwrap();
    assert_eq!(f.credential_counts(), (3, 0));
}
```
- [ ] **Step 2 / red:** `cargo test -p kod-tool-gateway --lib github::tests::contract_ -- --nocapture`. First expect missing fixture-helper compilation errors; after adding helpers, record any real contract/activation/cleanup assertion failure. If Tasks 1–4 already satisfy an assertion, retain it as regression coverage; do not manufacture a production defect or weaken `forge::check` to force red.

- [ ] **Step 3: Implement the test boundary and production assembly checks.** These fields extend the private HTTP fixture, never the production adapter. Bind the following storage to the fixture's actual route state: `Fixture.contract: Arc<Mutex<ContractRemote>>`, initialized with `Default::default()` in `Fixture::start()` and cloned into the Axum state. Move the fixture's existing equivalent refs, PRs, and token liveness into this single canonical store; do not maintain a second observational mirror. Keep Task 1–4's JWT, scope, Git-object and failure-injection validation intact. `refs` uses branch names without `refs/heads/`; request counters use percent-decoded paths.
```rust
#[derive(Default)]
struct ContractRemote {
    refs: std::collections::BTreeMap<String, String>,
    prs: Vec<serde_json::Value>,
    issued: std::collections::HashSet<String>,
    live: std::collections::HashSet<String>,
    after_ref: Option<Box<dyn FnOnce() + Send>>,
}
impl Fixture {
    fn branch_exists(&self, branch: &str) -> bool {
        self.contract.lock().unwrap().refs.contains_key(branch)
    }
    fn credential_counts(&self) -> (usize, usize) {
        let s = self.contract.lock().unwrap(); (s.issued.len(), s.live.len())
    }
    fn pr_count(&self) -> usize { self.contract.lock().unwrap().prs.len() }
    fn token_is_live(&self, token: &str) -> bool {
        self.contract.lock().unwrap().live.contains(token)
    }
    fn pause_after_ref(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (made_tx, made_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        self.contract.lock().unwrap().after_ref = Some(Box::new(move || {
            made_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(10)).expect("ref barrier timed out");
        }));
        (made_rx, resume_tx)
    }
}
// Call after validation in the fixture POST /repos/acme/app/git/refs handler,
// before its successful response. This mutates the SAME map GET/DELETE read.
async fn fixture_create_ref(s: Arc<Mutex<ContractRemote>>, branch: String, sha: String) {
    let hook = {
        let mut remote = s.lock().unwrap();
        remote.refs.insert(branch, sha);
        remote.after_ref.take()
    };
    if let Some(hook) = hook { tokio::task::spawn_blocking(hook).await.unwrap(); }
}
```
The existing fixture ref GET reads `remote.refs.get(branch)` and DELETE executes `remote.refs.remove(branch)`; PR POST appends the validated draft object to `remote.prs` and list reads that same vector. Successful token issuance executes `remote.issued.insert(token.clone()); remote.live.insert(token.clone());`; successful token DELETE executes `remote.live.remove(token)`. Authentication checks `remote.live.contains(token)` together with existing expiry/scope checks. Preserve failure-injection ordering: failed revocation leaves `live` intact; lost issuance/PR replies update provider state before dropping the response. These are the actual handler state transitions, not merely metrics. Remove duplicate helper definitions from earlier tasks when their names already exist.

Implement the concrete gateway helper in the same unit-test module:
```rust
fn gateway(f: &Fixture, github: Arc<GitHub>, dir: &std::path::Path,
           stopped: Arc<AtomicBool>) -> Arc<Gateway> {
    let (tracker, _) = kod_tracker::fake::sandbox();
    let mut issue = tracker.issue("FAKE-1").unwrap(); issue.workflow_state = "Doing".into();
    tracker.add_issue("FAKE-1", issue);
    let workspace = Arc::new(FakeWorkspace::default());
    workspace.set("run-1", Collected { changes: vec![Change {
        path: "contract.txt".into(), content: Some("contract\n".into()) }],
        test_evidence: "fixture collector: passed".into() });
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(kod_policy::Policy::load(&root.join("policy")).unwrap());
    let audit = Arc::new(kod_audit::AuditLog::open(&dir.join("audit.db")).unwrap());
    let base = f.permit().base;
    github.register_run(RunPermit { run: "run-1".into(), ticket: "FAKE-1".into(),
        repo: "acme/app".into(), base: base.clone() }).unwrap();
    let adapters = Adapters { tracker: Arc::new(tracker), docs: Arc::new(FakeDocs::default()),
        stopped: Arc::new(move |_| Ok(stopped.load(Ordering::SeqCst))),
        broker: github.clone(), forge: github, workspace };
    let gw = Arc::new(Gateway::new(policy, audit, adapters, Identity {
        name: "kod-agent".into(), email: "agent@kod.invalid".into() }).unwrap());
    gw.start_run(&"a".repeat(64), Run { id: "run-1".into(), ticket: "FAKE-1".into(),
        repo: "acme/app".into(), base, data_classes: vec!["placeholder".into()],
        linked_tickets: vec![], started_at: 1_700_000_000 }).unwrap();
    gw
}
```
Add the **entire** `tests/github_api.rs` below. These tests compile constructor bodies and trait coercions without reading a developer key or contacting GitHub. Keep the negative endpoint runtime assertion in the private fixture suite with a valid generated key; compile-only coverage must not claim it proves runtime validation.
```rust
use kod_tool_gateway::{broker::Broker, forge::Forge,
    github::{AppCredentials, Config, GitHub, RunPermit}};
use std::{path::Path, sync::Arc};
fn production(c: Config, k: AppCredentials, leases: &Path, p: RunPermit)
    -> anyhow::Result<(Arc<dyn Broker>, Arc<dyn Forge>)> {
    let a = Arc::new(GitHub::new(c, k, leases)?);
    a.recover()?; a.verify_repository()?; a.register_run(p)?;
    Ok((a.clone(), a))
}
#[test]
fn public_api_compiles() { let _constructor = production; }
#[cfg(feature = "test-support")]
fn loopback(c: Config, k: AppCredentials, leases: &Path)
    -> anyhow::Result<(Arc<dyn Broker>, Arc<dyn Forge>)> {
    let a = Arc::new(GitHub::with_test_api(c, k, leases, "http://127.0.0.1:12345",
        Arc::new(|| 1_700_000_000))?);
    Ok((a.clone(), a))
}
#[cfg(feature = "test-support")]
#[test]
fn test_support_api_compiles() { let _constructor = loopback; }
```
Add the following private unit test and helper; bind `Fixture.app_id: u64` and `Fixture.key_path: PathBuf` to Task 1's already generated fixture App identity and RSA key. These are private fixture fields, not new production configuration or public test APIs.
```rust
impl Fixture {
    fn app_credentials(&self) -> AppCredentials {
        AppCredentials { app_id: self.app_id, private_key: self.key_path.clone() }
    }
}
#[test]
fn test_api_rejects_remote_origins() {
    let f = Fixture::start();
    for origin in ["https://api.github.com", "http://example.com:80", "http://192.0.2.1:80"] {
        let leases = tempfile::tempdir().unwrap();
        assert!(GitHub::with_test_api(f.config(), f.app_credentials(), leases.path(),
            origin, Arc::new(|| 1_700_000_000)).is_err());
    }
    assert_eq!(f.total_requests(), 0);
}
```

Add this ignored **real** broker test to the private unit module. The owner JSON has the exact flat fields declared here; `repo`, IDs, base, guard ruleset and key must describe the explicitly authorized installation. No helper performs hidden provisioning. The total network budget is one mint/protection verification sequence plus two bounded GETs and one revoke sequence; there is no polling or retry loop.
```rust
#[test]
#[ignore = "requires explicit owner authorization and a protected disposable repository"]
fn live_broker_contract() -> anyhow::Result<()> {
    anyhow::ensure!(std::env::var("KOD_LIVE_BROKER").as_deref() == Ok("1"),
        "set KOD_LIVE_BROKER=1 only with owner authorization");
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Owner { repo: String, repository_id: u64, installation_id: u64,
        base_branch: String, guard_ruleset_id: u64, bot_user_id: u64,
        app_id: u64, private_key: std::path::PathBuf, leases: std::path::PathBuf,
        base: String }
    let path = std::env::var("KOD_GITHUB_BROKER_CONFIG")
        .map_err(|_| anyhow::anyhow!("KOD_GITHUB_BROKER_CONFIG is required"))?;
    let o: Owner = serde_json::from_slice(&std::fs::read(path)?)?;
    let repo = o.repo.clone();
    let a = GitHub::new(Config { repo: o.repo, repository_id: o.repository_id,
        installation_id: o.installation_id, base_branch: o.base_branch,
        guard_ruleset_id: o.guard_ruleset_id, bot_user_id: o.bot_user_id },
        AppCredentials { app_id: o.app_id, private_key: o.private_key }, &o.leases)?;
    a.recover()?;
    a.register_run(RunPermit { run: "live-broker-contract".into(),
        ticket: "BROKER-1".into(), repo: repo.clone(), base: o.base })?;
    let client = reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(30)).build()?;
    let token = a.mint("live-broker-contract", &repo)?;
    let get = || -> anyhow::Result<reqwest::StatusCode> {
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))?;
        auth.set_sensitive(true);
        Ok(client.get(format!("https://api.github.com/repos/{repo}"))
            .header(reqwest::header::AUTHORIZATION, auth).header("User-Agent", "kod-broker-contract")
            .header("X-GitHub-Api-Version", "2026-03-10").send()?.status())
    };
    let before = get(); // Revoke even if the access request fails.
    let revoked = a.revoke("live-broker-contract");
    revoked.map_err(|_| anyhow::anyhow!("live revocation failed; retain lease and run recovery"))?;
    anyhow::ensure!(before? == reqwest::StatusCode::OK, "live repository access failed");
    anyhow::ensure!(get()? == reqwest::StatusCode::UNAUTHORIZED, "revoked token still usable");
    Ok(())
}
```
Update the runbook with the compile-checked `production` sequence above and the Gateway `Adapters` assembly shown in `gateway`, replacing fixture inputs with caller-owned tracker/docs/workspace/stopped inputs. State exactly: recover before admission; verification mints/revokes a token and is not a read-only check; register each immutable permit before `start_run`; both trait objects share one `Arc<GitHub>`. Retain leases across restart, recover known tokens, block on unknown issuance until validated expiry, and preserve uncertain PR intent for reconciliation; never delete the lease directory to bypass recovery. Describe the supported active `~ALL` guard with exactly the two agent-ref exclusions, no App/role bypass, regular UTF-8 metadata limits, and the remaining non-atomic trusted-owner ref race. Replace obsolete fake-only and branch-scoped-token claims in the introduction/provider table.

- [ ] **Step 4 / acceptance / QA:** rerun the exact Step 2 command: three `contract_` tests execute and pass; cancellation observes the branch creation barrier, removes the branch, creates zero PRs, and revokes both credentials. Run `cargo test -p kod-tool-gateway --test publish --test mcp --test github_api` (one public API test), then `cargo test -p kod-tool-gateway --features test-support --test github_api` (two public API tests). Run `cargo test -p kod-tool-gateway --lib github::tests::test_api_rejects_remote_origins -- --exact` (one test, three refused inputs, zero HTTP requests). Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --all`, `cargo deny check`, and `git diff --check` once on the final branch. Capture `.omo/evidence/phase1-connected/a-task-5.log` and `a-final-checks.log`; zero executed tests is failure. Only when owner-authorized, run `KOD_LIVE_BROKER=1 KOD_GITHUB_BROKER_CONFIG="$CONFIG_FILE" cargo test -p kod-tool-gateway --lib github::tests::live_broker_contract -- --ignored --exact --nocapture`; missing config/flag must fail explicitly. The ignored test is not merge acceptance and never substitutes for connected acceptance.
- [ ] **Step 5 / commit:** `Verify the connected GitHub publish contract` — Task 5 files and test-proven adapter fixes only; retain the existing plain-message commit style. Do not commit secrets, leases or generated evidence. No live repository/model operation is required for merge.

## Owner prerequisites for the optional live broker contract
The owner supplies a disposable repository identity, existing GitHub App/installation and bot user IDs, owner-only RSA key path, exact base ref, active supported guard ruleset, synchronized clock and isolated lease directory. App installation permissions must be contents/write and pull_requests/write with no administration/workflows grant; permissions or protection outside this shape block activation. No implementation task provisions these resources. Once explicitly authorized for that named installation, execute `KOD_LIVE_BROKER=1 KOD_GITHUB_BROKER_CONFIG="$CONFIG_FILE" cargo test -p kod-tool-gateway --lib github::tests::live_broker_contract -- --ignored --exact --nocapture`; unset/missing prerequisites must yield a failing explanatory result. This proves only live broker mint/use/revoke, not connected ticket-to-PR acceptance.

## Whole-branch review and acceptance
Root executes F1/F3/F4; one whole-branch reviewer executes F2. These checks may run concurrently after Task 5; all must pass and findings require fixes plus affected reruns. Present the results for the caller's explicit okay before declaring the implementation accepted; no second review panel.
- [ ] F1. Plan compliance: verify every task/test/commit and API consumer against this plan.
- [ ] F2. Code quality: one reviewer audits security boundaries, lease failure/crash behavior, provider reconciliation and regression risk.
- [ ] F3. Real manual QA: execute the local HTTP contract and cancellation scenarios, inspect the resulting Git tree, refs, PRs and owner-only lease files; save sanitized results.
- [ ] F4. Ideal-state fidelity: map every IS row to the evidence below; unresolved safety findings become tasks, not caveats claiming success.

## Commit strategy
One logical task per passing commit using the repository's plain-message style, no WIP commits. Preserve repository convention of plain commit messages without trailers (`2026-10-16-phase1-tool-gateway.md`, Global Constraints); reference `docs/superpowers/plans/2026-10-23-phase1-connected-github.md` in the PR description. Only the plan document is produced while planning; the implementation executor owns the commits above.

## Success criteria
| IS | Delivering tasks | Proving QA scenario | Evidence |
|---|---|---|---|
| IS-1 | 1,2,5 | `auth_restart_revokes_every_known_lease`, `auth_lost_mint_reply_blocks_restart`, `auth_failed_revoke_retains_lease` | `a-task-2.log` |
| IS-2 | 1,2,4,5 | `registration_native_issue_key`, `auth_disabled_rules_refuses_token`, `refs_wrong_run`, `refs_divergent` | `a-task-1.log`, `a-task-2.log`, `a-task-4.log` |
| IS-3 | 3,5 | `commit_is_repeatable_and_preserves_regular_modes`, `commit_preflight_rejects_unsafe_tail` | `a-task-3.log` |
| IS-4 | 4,5 | `pr_lost_reply_survives_restart_without_second_post`, `pr_hidden_uncertain_blocks_retry`, `contract_real_http`, `contract_gateway_cancel_after_ref` | `a-task-4.log`, `a-task-5.log` |

All evidence paths above are under `.omo/evidence/phase1-connected/`. Standalone exit: exported adapter is usable without `kod-host`; all existing and new offline checks pass; every IS row has captured evidence; one reviewer approves; credential/rules/recovery instructions are complete. This plan may merge independently. Optional live broker execution may remain explicitly unrun; overall 6b connected acceptance remains incomplete until the later acceptance plan records its real deployment evidence.
