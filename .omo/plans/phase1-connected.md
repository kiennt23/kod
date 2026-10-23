# phase1-connected - Work Plan

## TL;DR (For humans)
<!-- Fill this LAST, after the detailed plan below is written, so it summarizes the REAL plan. -->
<!-- Plain English for a non-engineer: NO file paths, NO todo numbers, NO wave/agent/tool names. -->

**Who this is for and what changes for them:** Engineers can delegate a real GitHub issue and inspect a draft PR and accurate status. Operators can stop the work, revoke access, and recover after a restart.

**What you'll get:** A runnable connected host, the missing production adapters, and automated safety checks. A separate live drill proves the deployed ticket-to-PR path before connected acceptance is declared complete.

**Why this approach:** Compose the already-built libraries and retain their policy, audit and isolation controls. Keep real deployment evidence separate from local simulations.

**What it will NOT do:** Draft Google Docs, introduce a scheduler or UI, merge PRs, or activate a live deployment without its required inputs and authorization.

**Effort:** XL
<!-- Effort is exactly ONE band, never hours/days. Quick = single edit, minutes of agent work; Short = one focused change, a few files; Medium = multi-file feature in one session; Large = several waves, one long session; XL = multi-session or architectural work. A written duration is rewritten to a band. -->
**Risk:** High - cancellation, credentials, collection and publication cross existing component boundaries.
**Decisions to sanity-check:** One host/process for the connected runtime, immutable approved document snapshots, and interrupted runs terminated rather than automatically resumed.

Your next move: start implementation, or request a high-accuracy review of this plan first. Full execution detail follows below.

---

> TL;DR (machine): XL / High / connected host, real adapters, lifecycle recovery, deterministic tests and live acceptance.

## Scope
### Affected user and ideal state
<!-- From the draft ledger: who this touches and how they use the result, then one IS row per property with its reason, then one GAP row per difference with its reason. Every todo below closes a GAP row; ## Success criteria proves every IS row. -->
**Affected user:** the delegating engineer and trusted-host operator. Today they can exercise isolated libraries and offline evals; after execution they can delegate a real GitHub issue, inspect its draft PR/status, and stop or recover a run without granting the sandbox host credentials.

| Row | Statement | Reason |
| --- | --- | --- |
| IS-1 / GAP-1 | One authenticated, current issue admits one run; a composed service is absent. | Duplicate or stale webhooks must not create duplicate work. |
| IS-2 / GAP-2 | Model/tool access is scoped, priced and revocable; the model registry currently loads only at startup. | Cancellation must cover queued and in-flight work, not just new requests. |
| IS-3 / GAP-3 | The sandbox has gateway-only egress and no host credentials; the runner currently assumes offline cases/per-case proxies. | Connected operation must retain phase-0 isolation. |
| IS-4 / GAP-4 | Trusted collection produces one deterministic draft PR; real broker, forge and workspace adapters are absent. | Retries must not duplicate PRs or trust agent-written success files. |
| IS-5 / GAP-5 | Status and gates use actual source records and recover comments; the source producer and durable uncertain-write coordination are absent. | A plausible status rendering is not evidence of current approvals. |
| IS-6 / GAP-6 | Cancel, kill and restart retire execution and credentials; the production dispatcher Gateway is absent. | No stranded sandbox or publish token may outlive its run unnoticed. |
| IS-7 / GAP-7 | Real-host positive/negative acceptance has recorded evidence; current provider fixtures cannot prove deployment. | Code completion and connected acceptance must remain distinguishable. |

### Must have

- Base: merged `origin/main` at `9cbae9a` or its verified descendant. Preserve unrelated work in `/Users/k/exploration/ai/kod`; execute on a new task-owned branch/worktree.
- One composition crate/binary `crates/kod-host` (`kod-host`) using existing Axum, Tokio, SQLite, policy, audit, dispatcher, gateways and sandbox helpers. It is assembly, not a new domain service. Gate scanners remain on their separate trusted host/daemon.
- Single host, one dispatcher process, existing policy WIP limit (placeholder 1 remains a refusal-safe development value). Existing model endpoint/class allowlists and prices are reused; no guessed production clearance or spend caps.
- Required nonsecret `connected.json`: lower-case tracker/product repo, base ref, dispatcher config path, policy/audit/dispatcher/host-state paths, run root/mirror path, installed App IDs/installation IDs/bot ID, committer identity, bind addresses/accepted hosts, public webhook URL, sandbox/collector image digests, resource/time limits, endpoint/model/class/price/budget paths, approved-artifact manifest, gate evidence/source paths and CLI-key file references. Reject unknown/duplicate keys, missing inputs, invalid repos, unpinned images, nonpositive budgets and writable secret/approval files. Use existing parsers for existing files. Secrets are referenced by owner-only files, never embedded in checked-in examples or logs.
- Command surface: `kod-host check --config FILE` is read-only validation (no model calls or external writes); `kod-host serve --config FILE` starts composed routes and workers; `kod-host accept --config FILE --evidence DIR` runs the explicitly selected disposable-repo live acceptance. Preserve existing `/webhook`, `/command`, `/r/{token}/mcp`, `/r/{token}/v1/messages` routes. Model/MCP listeners are reachable only from the sandbox gateway network; public ingress exposes signed webhook/command routes only.
- Approved document reads use owner-provisioned immutable host snapshots with logical link, exact revision ID, content SHA-256 and class; never follow a URL supplied by a ticket. `Issue.approved_versions` remains the required logical-link list expected by the existing gates. Admission resolves each link to a revision and pins it; MCP reads require that admitted link/revision, while gates/status resolve latest approved revisions independently. This supplies the existing `Docs` trait without implementing drafting or Google Workspace approval capture. The manifest is authoritative only after operator approval; do not infer sign-off from an issue body or file existence.
- Persist operational run bindings and cleanup/status refresh intents in one host SQLite journal, distinct from dispatcher states and audit records. Do not mirror their state machines. Required bindings: run ID, issue revision, repo/base, start time, admitted classes/budgets/pins, collector paths, container IDs, token hashes and cleanup references. Store recoverable short-lived credentials only in an owner-only host directory, never the audit DB or sandbox. Delete after confirmed revocation/expiry; redact all evidence.
- No running run is resumed after a host restart. Before admission, revoke outstanding tokens, stop and confirm disappearance of owned containers, invalidate old model/tool registrations, reconcile dispatcher runs using forward-only terminal states, and recover `publishing` through the existing PR lookup. Existing PR found means Done; no confirmed PR means NeedsHuman, even if a branch was pushed. Never blindly replay publication on startup. Keep dispatcher delivery dedup history. Do not reset a budget under an old run ID; tombstone ended registrations. Persist conservative request reservations before forwarding and settle after confirmed usage; mark interrupted requests uncertain/consumed rather than refunding them. This avoids pretending the in-memory LLM ledger is restart-durable.
- GitHub App tokens are repository/permission scoped, not natively `agent/*` scoped. Enforce branch prefix, repo and exact run branch in the host Forge; withhold credentials from agents and apply repository protection to other refs. No bypass/administration/workflows permission. Do not claim token cryptography enforces a branch namespace. Use the minimum JWT/signing machinery available on the host (OpenSSL RS256 with the key file, no shell interpolation); validate prerequisite availability in `check`.

### Must NOT have (guardrails, anti-slop, scope boundaries)

- No new queue, status database, cloud control plane, generalized adapter framework, multi-host scheduler, UI, auto-merge or weakening of policy/audit/collector controls.
- No Google Docs draft/edit/approval workflow (sub-plan 9), arbitrary external document fetch, PAT production fallback, automatic repository/App provisioning, fabricated baseline or live-acceptance success.
- No silent support for binary changes, symlinks, gitlinks, executable-bit changes or renames that the text-only `Change` contract cannot represent. Refuse them before any forge write; retain their collected evidence.
- No unchecked patch parsing on the host, mounting the Docker socket or repository history/credentials into a sandbox, shell construction from ticket content, or treating PR prose as gate evidence.

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: TDD using existing Rust tests, local Axum scripted HTTP providers and Docker suites. Add a failing safety scenario before each trust-boundary fix. Test code that calls provider HTTP must assert side-effect counts and state, not merely an error string.
- Evidence root: `.omo/evidence/phase1-connected/`; each task writes its command transcript/JSON assertions there. No credentials/raw tokens/private prompts in artifacts. Record commit, image digests, provider identities and skip reasons.
- Standard checks: `cargo test --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`. Run these once on the final implementation tree; rerun affected checks after meaningful fixes.
- `KOD_DOCKER_TESTS=1 cargo test -p kod-host --test connected_docker -- --nocapture` must launch real sandbox/collector containers with a scripted model and actual MCP/LLM/dispatcher routers. Test must report execution counts; opt-in not set is SKIPPED, never PASS.
- Live acceptance is separate: `kod-host accept --config FILE --evidence DIR` may run only with supplied disposable repository, configured App, approved test documents, installed images/host networking and an explicit model-spend cap. Missing inputs produce BLOCKED and nonzero exit; do not substitute mock results. A test fixture with the same shape cannot satisfy IS-7.

## Execution strategy
### Parallel execution waves
> Target 5-8 todos per wave. Fewer than 3 (except the final) means you under-split.

Wave 1: tasks 1-4 establish composition/config, model lifecycle, runner/collector and document snapshots. Tasks 2-4 can proceed after the minimal crate/config contract in 1.
Wave 2: tasks 5-8 implement credentials, forge, dispatcher bridge and source records; 6 follows 5, 7 follows 2-5, and 8 follows 4/6/7. Preserve these dependencies rather than pretending the wave is fully parallel.
Wave 3: tasks 9-12 connect durable refresh, service lifecycle, deterministic end-to-end checks, deployment/docs and live acceptance. Task 12 cannot pass on offline checks alone.

Execution modes follow the existing ADR's distinction: Native for 1 and documentation portions of 12; Subagent + review for 2-11 and live security acceptance in 12, because these own safety guarantees. ADR 0001 remains Proposed; these are explicit modes for this plan, not a claim that its adoption status changed. Before spawning implementers, allocate file ownership and tell them not to revert others' work. Only one whole-branch gate reviewer is needed at the end; F1/F4 can be root audits and F3 is root-owned manual QA.

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| 1 | none | 2-12 | none |
| 2 | 1 | 7,10,11 | 3,4,5 |
| 3 | 1 | 7,11 | 2,4,5 |
| 4 | 1 | 7,8 | 2,3,5 |
| 5 | 1 | 6,7,9 | 2,3,4 |
| 6 | 5 | 8,10,11 | 7 |
| 7 | 2,3,4,5 | 8,10,11 | 6 |
| 8 | 4,6,7 | 9,10,11 | none |
| 9 | 5,8 | 10,11 | none |
| 10 | 2,6,7,8,9 | 11,12 | none |
| 11 | 3,6,7,8,9,10 | 12 | none |
| 12 | 11 | F1-F4 | none |

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->
- [ ] 1. Add the composition entrypoint and strict deployment contract
  Recommended task executor category: unspecified-high - a small workspace crate plus validation using existing parsers.
  What to do / Must NOT do: create `crates/kod-host/{Cargo.toml,src/lib.rs,src/main.rs,src/config.rs,tests/config.rs}` and register the crate in root Cargo.toml. Implement the stated command/config contract, owner-only secret references and read-only preflight. Add `config/connected.example.json` with inert placeholders that refuse activation. No fake adapters in `serve` and no new public HTTP control route for registration.
  Closes: GAP-1, GAP-7.
  Parallelization: Wave 1 | Blocked by: none | Blocks: all.
  References: root Cargo.toml; `crates/kod-dispatcher/src/config.rs::Config`; `crates/kod-llm-proxy/src/config.rs::{RunSpec,parse_prices,parse_runs}`; `crates/kod-llm-proxy/src/main.rs`; `docs/dispatcher-runbook.md`; `docs/github-tracker-runbook.md`.
  Acceptance criteria: `cargo test -p kod-host --test config`; valid fixture produces a redacted resolved configuration with zero outbound calls; duplicate keys, missing caps, bad image digest and permissive key file are rejected before binding listeners.
  QA: happy `cargo run -p kod-host -- check --config crates/kod-host/tests/fixtures/connected-valid.json` against an inert local fixture created by this task; failure run the config integration tests and assert zero mock GitHub/model calls. Evidence `.omo/evidence/phase1-connected/task-1-config.log`.
  Commit: Y | Add connected host configuration and preflight.

- [ ] 2. Add dynamic, cancelable model-run registrations
  Recommended task executor category: deep-low - preserves budget/concurrency invariants across live lifecycle changes.
  What to do / Must NOT do: expose a cloneable lifecycle handle alongside the LLM router in `crates/kod-llm-proxy/src/lib.rs`, preserving `app(Config)` for offline callers. Register a validated token/RunSpec exactly once; identical retries reuse the ledger, differing retries fail; ended IDs cannot reopen. Revoke one/all runs, reject queued requests and abort active upstream forwarding/streaming without dropping required audit/reservation settlement. Host restart terminates runs rather than resetting usage. No HTTP registration endpoint.
  Closes: GAP-2, GAP-6.
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: 7,10,11.
  References: `crates/kod-llm-proxy/src/lib.rs::{Config,Run,AppState,app,forward,route_input}`; `src/budget.rs`; `src/config.rs`; `tests/gateway.rs`; `crates/kod-loop/tests/proxy.rs`.
  Acceptance criteria: `cargo test -p kod-llm-proxy`; concurrent re-registration cannot double the cap; revoked queued and streaming requests cease within the configured cancellation deadline and no new upstream request is made. Add a host-supplied reservation journal hook before forwarding, so failed persistence prevents a request and a crash cannot refund uncertain spend; no extra LLM database. Keep all existing budget/audit/routing tests.
  QA: happy dynamic registration through a real local HTTP model; failure revoke while a queued call and open SSE response exist, assert cancellation and conservative spend settlement. Evidence `.omo/evidence/phase1-connected/task-2-model.log`.
  Commit: Y | Add dynamic model run lifecycle and revocation.

- [ ] 3. Expose the connected sandbox and trusted collector seam
  Recommended task executor category: deep-low - reuses isolation controls while separating offline scoring assumptions.
  What to do / Must NOT do: extract the smallest reusable runner/collector API from `kod-eval`, consumed by `crates/kod-host/src/runner.rs`. Resolve base SHA from the configured repo/ref once and extract a history-free seed. Start background execution, inject both gateway URLs and an opaque per-run token, enforce wall/resource/egress limits, and provide confirmed per-run stop. Require process gate plus networkless clean-base collection and `collect_ok`. Implement `Workspace::collected` in `src/workspace.rs` from a host-owned immutable manifest. Apply patch interpretation in an isolated clean workspace/collector, validate paths and shape, and reject unsupported binary/mode/symlink/gitlink changes before publish. Do not call the complete eval scoring harness or accept sandbox-authored check evidence.
  Closes: GAP-3, GAP-4, GAP-6.
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: 7,11.
  References: `crates/kod-eval/src/run.rs::{run_case,collect,process_gate,launch,stop}`; `src/{sandbox,workspace,runner}.rs`; `images/sandbox/entrypoint.sh` (locate actual entrypoint if named differently); `crates/kod-loop/src/main.rs::setup`; `kod-tool-gateway/src/forge.rs::{Workspace,Change,Collected}`; `kod-eval/tests/{kod_loop,sandbox,docker,kill_switch,gateway_down}.rs`.
  Acceptance criteria: `cargo test -p kod-eval -p kod-host`; fixture collection recreates exact supported text additions/deletions at the pinned base. No credentials, `.git`, host mounts or Docker socket in agent container. Repeated absent-run stop passes `check_absent_run` semantics.
  QA: happy real Docker scripted run and collection; failure process-gate violation, tampered collector path, symlink/binary/mode patch, timeout and unrelated run stop. Execute via `KOD_DOCKER_TESTS=1 cargo test -p kod-host --test connected_docker collector -- --nocapture`. Evidence `.omo/evidence/phase1-connected/task-3-collector.log`.
  Commit: Y | Reuse sandbox execution and trusted collection for connected runs.

- [ ] 4. Read only exact approved artifact snapshots
  Recommended task executor category: unspecified-high - one concrete Docs adapter with strict provenance checks.
  What to do / Must NOT do: implement `Docs` and the existing gate `Artifacts` interface from one owner-managed manifest in `crates/kod-host/src/artifacts.rs`. Bind ID/revision/SHA/class/content path, restrict files to the trusted snapshot root, reject symlinks and mutable revision collisions, and capture an admitted pin set per run. Enforce the pin set in tool-gateway `read_doc`; class permission alone must not allow a newer or unapproved revision. Add the narrow Run/publish evidence pin field needed; update every constructor/test. Drafting and remote URL fetching remain absent.
  Closes: GAP-4, GAP-5.
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: 7,8.
  References: `crates/kod-tool-gateway/src/docs.rs::{Doc,Docs}`; `src/lib.rs::{Run,read_doc}`; `crates/kod-gates/src/records.rs`; `crates/kod-status/src/records.rs::{Artifact,Snapshot}`; policy doc-read rules; `docs/gates-runbook.md`.
  Acceptance criteria: `cargo test -p kod-host --test artifacts` and `cargo test -p kod-tool-gateway`; exact admitted revision is returned with its class, while changed hash, unknown revision, external URL, cross-class path, symlink and unapproved artifact are refused. Approval manifest updates invalidate affected gates before new records become current.
  QA: happy pinned document through actual MCP router; failure swap content or revision after admission and assert refusal/no arbitrary HTTP fetch. Evidence `.omo/evidence/phase1-connected/task-4-artifacts.log`.
  Commit: Y | Bind connected document reads to approved snapshots.

- [ ] 5. Implement App credential minting, renewal and durable revocation
  Recommended task executor category: deep-low - protects host-only credentials and crash cleanup.
  What to do / Must NOT do: implement `crates/kod-host/src/github/auth.rs` and a real gateway Broker. Sign bounded-lifetime App JWTs, mint explicit repository/permission-scoped installation tokens, track every minted credential by run/profile, and durably record cleanup before using it. Use owner-only recoverable token files plus journal references for restart revocation; never raw token data in audit/logs. Tracker/read/status and publish have separate token lifetimes. Add a provider hook to `kod-tracker` for per-request current authorization while preserving `GitHub::new`; token renewal must not reconstruct away uncertain-create guards. Reads may refresh an expired token once; non-idempotent writes require reconciliation. Revoke all run tokens, including cleanup tokens, and surface failures as pending cleanup, not successful revocation.
  Closes: GAP-4, GAP-6.
  Parallelization: Wave 2 | Blocked by: 1 | Blocks: 6,7,9.
  References: `kod-tool-gateway/src/broker.rs`; `src/lib.rs::{publish,published,end_run,end_all}`; `kod-tracker/src/github.rs::{new,request,status_comment}`; `docs/github-tracker-runbook.md`; official GitHub App/token references below.
  Acceptance criteria: `cargo test -p kod-host --test github_auth`; mocked issuer verifies exact repository IDs/permissions, JWT claims and no overbroad defaults; repeated revoke and simulated restart retire all minted credentials; revoke failures remain visible and admission stays inhibited until cleanup is reconciled. Correct the overbroad native branch-confinement promise in `broker.rs` documentation and runbooks; verify the configured repository protections as an activation prerequisite, with no App bypass permission.
  QA: happy mint/use/revoke against local GitHub-shaped server; failure crash after mint, expiry during tracker read, unknown write response and failed revoke. Assert secret absence in formatted errors, journal and audit evidence. Evidence `.omo/evidence/phase1-connected/task-5-auth.log`.
  Commit: Y | Add scoped GitHub App credentials and recovery.

- [ ] 6. Implement the real deterministic GitHub publishing Forge
  Recommended task executor category: deep-low - provider idempotency and branch confinement share one correctness boundary.
  What to do / Must NOT do: implement `crates/kod-host/src/github/forge.rs` against existing Forge trait using Git data API blobs/tree/commit, ref create/update/delete, PR lookup and draft creation. Pin parent SHA, tree, author+committer identity/date and message; retry creates the same commit SHA. Check configured repo and exact `agent/<ticket>-<run>` ref before every branch operation, refuse unrelated existing refs/PRs, never force-update a divergent branch or default branch. Find only the matching open PR by owner/head/base/repo/draft before attempting creation, and reconcile an uncertain create before retrying. Retry cleanup safely when PR already exists or token was revoked. No live merges.
  Closes: GAP-4, GAP-6.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 8,10,11.
  References: `kod-tool-gateway/src/forge.rs::{Forge,Commit,Change,check}`; `tests/publish.rs`; `kod-gates/src/github.rs` for provider headers/error hygiene (different trait, do not substitute it); official Git commits/refs/PR APIs.
  Acceptance criteria: `cargo test -p kod-host --test github_forge`; run the existing forge/broker contract over local HTTP, asserting request bodies and side effects. Lost PR response creates one PR, repeated commit gives identical SHA, foreign/default/divergent ref and revoked token operations fail. Content changes preserve the parent regular-file mode; new files are 100644, unsupported mode changes are refused.
  QA: happy contract and text patch round trip; failure lost reply after ref push/PR creation, pagination hiding an existing PR, 403/422/5xx and cancellation after push. Evidence `.omo/evidence/phase1-connected/task-6-forge.log`.
  Commit: Y | Publish connected changes through the GitHub Forge.

- [ ] 7. Implement the production dispatcher-to-runtime bridge
  Recommended task executor category: deep-low - idempotent lifecycle must survive start/cancel/publish races.
  What to do / Must NOT do: implement dispatcher `Gateway` in `crates/kod-host/src/runtime.rs`. Persist RunStart -> resolved base/start time/classes/budgets/pins/trusted linked-rework snapshot before registration; retries must reuse identical bindings. Register MCP and LLM access, recheck dispatcher stop state, then start sandbox in a background task. Report a terminal outcome only from host-validated process/collector results; `finished(Done)` publishes immediately and must not be called prematurely. Map needs-human/blocked/over-budget/failure explicitly. Revoke both access paths before waiting for confirmed stop; every absent-run operation is idempotent. Startup leaves unfinished executions terminal and reconciles publishing via `find_published`, never resumes a spend ledger. Persist credential-cleanup failures even if a publish succeeded. Keep block/cancel semantics distinct: workflow state is intentionally excluded from issue revision to avoid reacting to the agent's own transitions.
  Closes: GAP-1, GAP-2, GAP-3, GAP-4, GAP-6.
  Parallelization: Wave 2 | Blocked by: 2,3,4,5 | Blocks: 8,10,11.
  References: `kod-dispatcher/src/lib.rs::{Gateway,RunStart,start_queued,finished,revision}`; `src/{edits,followup,store}.rs`; `tests/{gateway_contract,review_edges}.rs`; `kod-tool-gateway/src/lib.rs::{Run,start_run,end_run,end_all,publish,published}`.
  Acceptance criteria: `cargo test -p kod-host --test runtime`; all six Gateway methods satisfy retry semantics and `check_absent_run`. A canceled registration cannot launch; a late registration is revoked; failed stop is not reported as gone; incomplete collection never calls publish.
  QA: happy scripted completion to local Forge; failure barriers cancel before/during start, during collection and after push; restart at queued/running/publishing and assert forward-only terminal outcome, no live old tokens/containers and no duplicate PR. Evidence `.omo/evidence/phase1-connected/task-7-runtime.log`.
  Commit: Y | Connect dispatcher runs to sandbox and gateway lifecycles.

- [ ] 8. Produce authoritative gate and workflow source records
  Recommended task executor category: deep-low - current approval/head identity and audience are trust boundaries.
  What to do / Must NOT do: implement `crates/kod-host/src/records.rs`. Produce immutable gate publish Evidence from trusted run/base/pins/policy/runner identities, never from PR body; read fresh tracker/rework and GitHub PR/head/check records into existing status Snapshot/HostRecords. Bind every effective check to head SHA, intended app/generation and required source freshness. Use existing gates event ordering/lock to invalidate affected checks before swapping approval records. Write snapshots atomically and debounce only after durable event receipt, with periodic reconciliation for dropped webhooks. Expose no partial/error view and stop new admission when required ingestion/approval state is stale. Respect tracker comment audience/data-class bounds.
  Closes: GAP-5, GAP-7.
  Parallelization: Wave 2 | Blocked by: 4,6,7 | Blocks: 9,10,11.
  References: `kod-status/src/{records,lib}.rs::{Snapshot,HostRecords,load,refresh}`; `kod-gates/src/{github,records,lib}.rs`; `docs/{status,gates}-runbook.md`; existing status audit/workflow tests and gate event-order tests.
  Acceptance criteria: `cargo test -p kod-host --test records`; fresh real-shaped sources yield identical CLI/comment inputs; stale head, wrong app, superseded approval and failed read never produce a green/current claim. A snapshot read concurrent with replace sees a whole old/new snapshot, never partial JSON. Persist publish bindings before remote actions; recover missing immutable Evidence from those bindings and validated PR/head responses only. Missing bindings leave gates/status unverified even if a PR exists.
  QA: happy publish-to-record-to-CLI with local provider; failure change approved revision/head during refresh, forge PR prose, duplicate check generations and unavailable source, then assert invalidation precedes publication and admission is refused when required. Evidence `.omo/evidence/phase1-connected/task-8-records.log`.
  Commit: Y | Feed gates and workflow status from trusted connected sources.

- [ ] 9. Reconcile uncertain tracker writes and serialize status refreshes durably
  Recommended task executor category: unspecified-high - uses existing audit intents and journal without inventing a status store.
  What to do / Must NOT do: implement `crates/kod-host/src/refresh.rs` with one per-story cross-process lock and durable pending refresh/reconciliation intents. Use `kod_status::refresh`; after unconfirmed status create or adapter/token replacement, inspect existing audit intent/outcome plus owned remote comments before allowing another create. Missing/multiple ambiguous comments remain needs-human, not blind retry. A definite 4xx rejection allows retry. Rework creates and other non-idempotent writes also require remote reconciliation; never turn a delivery retry into duplicate child creation. Preserve original intent IDs and do not treat audit outage as permission to write.
  Closes: GAP-5, GAP-6.
  Parallelization: Wave 3 | Blocked by: 5,8 | Blocks: 10,11.
  References: `kod-status/src/lib.rs::refresh`; `kod-status/tests/audit.rs`; `kod-tracker/src/github.rs::{create_linked,status_comment}`; `github_tests.rs`; audit read/verification interfaces; `docs/github-tracker-runbook.md`.
  Acceptance criteria: `cargo test -p kod-host --test refresh`; two workers/processes and restart after committed-but-lost comment response produce exactly one bot-owned comment and preserve human comments. Missing audit, uncertain remote absence and duplicate owned comments block creation.
  QA: happy create then update after token renewal; failure kill worker after provider commit/before local outcome, reconstruct writer and run two refreshes, asserting one POST. Evidence `.omo/evidence/phase1-connected/task-9-refresh.log`.
  Commit: Y | Recover tracker writes and serialize connected status refresh.

- [ ] 10. Mount routes and run the fail-closed service lifecycle
  Recommended task executor category: deep-low - startup/shutdown ordering enforces cancellation and host isolation.
  What to do / Must NOT do: complete `kod-host serve` in `src/service.rs`: load validated inputs, acquire single-host ownership, reconcile tokens/containers/publishing before enabling admission, mount existing dispatcher/LLM/MCP routes, and run queue/followup/source/status workers. Use spawn_blocking for blocking adapters/SQLite; never block kill handling on an agent/model call. Separate ingress binding from sandbox-only bindings; no public model/tool forwarding. On shutdown inhibit admissions, revoke, stop-confirm, persist unresolved cleanup and flush audit. Preserve owner/backup signed-command kill during runtime audit/policy outages; restart recovery must execute safety cleanup before refusing to activate invalid config/policy. Do not reinterpret a failed stop as successful shutdown.
  Closes: GAP-1, GAP-2, GAP-6.
  Parallelization: Wave 3 | Blocked by: 2,6,7,8,9 | Blocks: 11,12.
  References: existing `kod-dispatcher::router`, `command` signature/replay checks, `work_once`; `kod-tool-gateway::router`; `kod-llm-proxy::app`; `kod-eval::run::stop`; dispatcher/gateway runbooks; `policy/compliance.json` placeholder restrictions.
  Acceptance criteria: `cargo test -p kod-host --test service`; health reports not-ready until reconciled; only designated routes/listeners are reachable; unauthenticated commands and unknown MCP/model tokens fail; owner kill works with runtime policy/audit failure; a second instance refuses ownership.
  QA: happy binary start/readiness/shutdown using local providers; failure startup orphan container, nonresponsive model request, DB outage, invalid policy, stale ingestion and SIGTERM during publish. Evidence `.omo/evidence/phase1-connected/task-10-service.log`.
  Commit: Y | Serve and recover the connected host process.

- [ ] 11. Prove the composed flow in deterministic Docker acceptance
  Recommended task executor category: unspecified-high - connects real components rather than adding more mocks of their contracts.
  What to do / Must NOT do: add `crates/kod-host/tests/{connected_docker,connected_http}.rs` plus small fixture repo/model scripts. Run actual dispatcher, policy/audit, GitHub adapter, tool/model gateways, sandboxed kod-loop and collector; mock only remote GitHub/model HTTP. Verify provider side effects and exact final branch/content/trailers, not agent self-report. Include conformance items 2,4,7 as named in design, egress/secret/protected-path denial and old-token denial. Keep tests opt-in when Docker unavailable, with explicit skip status.
  Closes: GAP-1 through GAP-7, deterministic portion.
  Parallelization: Wave 3 | Blocked by: 3,6,7,8,9,10 | Blocks: 12.
  References: `kod-eval/tests/{docker,kod_loop,kill_switch,gateway_down}.rs`; `kod-loop/tests/{mcp,proxy}.rs`; `kod-dispatcher/tests/review_edges.rs`; `kod-tool-gateway/tests/publish.rs`; `docs/kod-harness-design.md` conformance list.
  Acceptance criteria: standard workspace checks plus `KOD_DOCKER_TESTS=1 cargo test -p kod-host --test connected_docker -- --nocapture`; assert exactly one run/PR on duplicate webhook, final draft=true with story/agent trailers and host evidence, one status comment, no secret/egress/protected-path escape, budget refusal, lost-response restart, stale approval, cancel and owner kill with policy/audit outage.
  QA: happy real-container ticket-to-PR; failure each safety scenario above with container absence/token denial assertions and provider call counts. Evidence `.omo/evidence/phase1-connected/task-11-connected-docker.log` and redacted JSON scenario results.
  Commit: Y | Verify connected runs and safety controls end to end.

- [ ] 12. Document activation, provision inputs and record real-host acceptance
  Recommended task executor category: unspecified-high - operational proof uses the assembled surface and exact deployed identities.
  What to do / Must NOT do: add `docs/connected-runbook.md`, update README/overview/design and dispatcher/tool/model/gates/status/tracker runbooks to actual commands and responsibility boundaries. Add a superseding note to the historical 6a plan for stateless parsing/body-hash dedup/no signed GitHub timestamp; preserve its historical code listings. Set 6c merged and 6b implemented-awaiting-live until actual acceptance. Document App permissions, token limitations, webhook routes, approved snapshot provenance, gate-host separation, backup/kill keys, budgets, checkpoint account and rollback (stop admissions, revoke, stop-confirm, preserve DB/audit, restore binary). Implement `accept` so evidence manifest names every conformance check and marks missing credentials/resources BLOCKED. Once operator-supplied disposable repo/App/host/model caps exist, run a real ticket -> signed webhook -> Docker loop -> GitHub draft PR, effective gates/status, cancel/restart and live kill drill, collecting redacted URLs/SHAs/audit checks/container/token proofs. No claimed completion while any live row is blocked; no new App/repo/rules provisioning or paid call without explicit session authorization for the concrete target/caps. Live writes stay in the named test repo and never merge.
  Closes: GAP-7 and documentation gaps for GAP-1 through GAP-6.
  Parallelization: Wave 3 | Blocked by: 11 | Blocks: F1-F4.
  References: `docs/superpowers/plans/2026-10-14-phase1-overview.md` 6b exit; all component runbooks; `.github/pull_request_template.md`; `docs/adr/0001-per-task-execution-mode.md`.
  Acceptance criteria: `kod-host check --config FILE`; then `kod-host accept --config FILE --evidence .omo/evidence/phase1-connected/live`. Evidence reports actual deployed commit/images, repository/App bot, issue/PR/head, exact cost cap and no raw secrets. Missing input returns nonzero BLOCKED and leaves 6b acceptance incomplete. A live complete manifest plus F1-F4 approvals is required for 6b complete.
  QA: happy actual live path and kill/resume/restart drill; failure missing credential/budget, wrong installation/bot identity, unauthorized command and unavailable gate source all fail before unsafe action. Evidence `.omo/evidence/phase1-connected/task-12-live.json`, `live/` transcripts and `task-12-docs.log` link checks.
  Commit: Y | Document connected operations and record acceptance evidence (sanitized references only, no secrets/private payloads).

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit
  Root compares every todo/IS row against actual diffs, commands and artifacts; code-only and live acceptance statuses remain separate. Evidence `.omo/evidence/phase1-connected/f1-compliance.md`.
- [ ] F2. Code quality review
  One fresh whole-branch gate reviewer audits security/correctness/compatibility and actual evidence; fix blockers and rerun affected checks. Evidence `.omo/evidence/phase1-connected/f2-review.md`.
- [ ] F3. Real manual QA
  Root executes the binary and connected scenarios, reusing current-tree artifacts where valid; independently checks GitHub draft/head, DB/audit chain, sandbox absence and denied revoked tokens. Evidence `.omo/evidence/phase1-connected/f3-qa.md`.
- [ ] F4. Ideal-state fidelity
  Root maps all IS rows 1:1 to evidence below, records unmet live prerequisites as BLOCKED, and presents the reviewable result. No phase completion claim or main merge without explicit authorization. Evidence `.omo/evidence/phase1-connected/f4-fidelity.md`.

## Commit strategy

Each todo is one independently buildable behavior plus its tests; split only when needed to keep commits reviewable. Preserve existing commit style, never push to main, and run relevant checks before each commit. No commits/push/PR creation are authorized by this plan-writing turn. On execution, use a fresh task-owned worktree; PR delivery follows explicit user instructions or `$ulw-execute --make-pr` authorization. Documentation alone needs link/diff validation rather than repeating the complete Rust suite.

Primary provider contracts: [GitHub App installation tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app), [Git commits](https://docs.github.com/en/rest/git/commits), [Git refs](https://docs.github.com/en/rest/git/refs), [pull requests](https://docs.github.com/en/rest/pulls/pulls). Verify endpoint permissions/version at implementation time. Repository/permission scoping is documented by GitHub; branch-prefix confinement is an explicit host responsibility in this plan, not an inferred GitHub capability.

## Success criteria
> One row per IS row. The plan is complete only when every IS row has a delivering todo and a proving QA scenario; F4 checks the delivered behavior against these rows 1:1, and a shortfall becomes new `- [ ] N.` rows, never a note.
| IS | Delivering todo(s) | Proving QA scenario | Evidence |
| --- | --- | --- | --- |
| IS-1 | 1,7,10,11,12 | Signed duplicate/current-ticket admission | task-7-runtime.log, task-11-connected-docker.log, task-12-live.json |
| IS-2 | 2,5,7,10,11 | Register/revoke queued+streaming model and MCP; budget race | task-2-model.log, task-5-auth.log, task-11-connected-docker.log |
| IS-3 | 3,10,11,12 | Sandbox egress, credentials, protected-path and collector tests | task-3-collector.log, task-11-connected-docker.log, task-12-live.json |
| IS-4 | 3,5,6,7,11,12 | Deterministic commit and lost-response PR recovery | task-6-forge.log, task-7-runtime.log, task-12-live.json |
| IS-5 | 4,8,9,11,12 | Pin/head invalidation, atomic records and uncertain comment restart | task-4-artifacts.log, task-8-records.log, task-9-refresh.log, task-12-live.json |
| IS-6 | 2,5,7,9,10,11,12 | Start/publish cancellation barriers and kill/restart cleanup | task-7-runtime.log, task-10-service.log, task-12-live.json |
| IS-7 | 11,12,F1-F4 | Real-host manifest with conformance 2/4/7 and kill drill | task-12-live.json, f1-compliance.md, f2-review.md, f3-qa.md, f4-fidelity.md |

All evidence filenames in this table are relative to `.omo/evidence/phase1-connected/`. The plan is ready to execute when reviewed for structural completeness; 6b itself is complete only when real-host acceptance and all final verification rows pass. Phase-0 baseline comparison remains a separate outstanding phase-exit requirement.
