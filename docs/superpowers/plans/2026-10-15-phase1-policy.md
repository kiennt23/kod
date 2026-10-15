# Phase 1.3 Policy (`kod-policy`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development (a fresh implementer, then a fresh reviewer before the next task starts). *Native* tasks use superpowers:executing-plans in the orchestrating session. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `kod-policy`, a small Rust library that evaluates our Rego rules in-process with the regorus crate. It answers `decide(input) -> {allow, reasons, policy_version}` for every row of the spec's action and permission matrix, plus the delegation policy, requester approval, model routing and the compliance profile. Anything not listed is denied.

**Architecture:** A new workspace crate, `crates/kod-policy`, with one public type, `Policy`. `Policy::load(dir)` reads every `*.rego` and `*.json` file directly in a directory (the repo's `policy/`), hashes them into a version string and compiles them into one regorus `Engine`. `Policy::decide(&serde_json::Value) -> Decision` evaluates the single rule `data.kod.decision` and never returns an error: a bad input or an evaluation error is a deny that says why. The Rego is default deny by construction: a request is allowed only when a positive `grant` rule for its action holds and no `deny` rule fires. Sub-plans 2 (LLM gateway), 4 (audit), 5 (tool gateway), 6 (dispatcher) and 7 (required checks) call this crate; none of them exists yet.

**Tech Stack:** Rust 1.99.0 (edition 2024), regorus `=0.13.0` (Rego v1, no default features), sha2 0.10, serde_json, anyhow. Tests use tempfile.

**Spec:** `docs/kod-harness-design.md`, in particular "Trust boundaries → Action and permission matrix" (including the vendor-login subscription row), "Configurable policies" (compliance profile, model routing policy, delegation policy and risk classes, requester approval), "Subscription auth for vendor runners" (the credential exception, eligibility check and business plans), "Security model" and "Ownership". The phase decomposition is in `2026-10-14-phase1-overview.md` (sub-plan 3). Execution modes follow `docs/adr/0001-per-task-execution-mode.md`.

**Out of scope here** (later sub-plans, see the overview): the gateways that call `decide` (2, 5), the audit store that records the decision and its `policy_version` (4), the dispatcher and its atomic WIP admission (6), the GitHub required checks (7), adding `policy/` to the protected-paths check (7), and real values for the compliance profile, routing table, team settings and people.

## Decisions

These choices were left open by the spec and the overview. Each one is made here, with its reason.

| # | Decision | Why |
|---|---|---|
| D1 | A separate crate, `crates/kod-policy`, and a top-level `policy/` directory with the Rego and data files. Callers pass the directory to `Policy::load` at startup. Nothing is embedded in the binary. | The spec says policies are configuration in the harness repo, versioned and reviewed like code. Loading at runtime matches how kod-loop loads its prompt. The content hash (D4) says exactly which rules were used, whatever the build. |
| D2 | regorus pinned to `=0.13.0` with `default-features = false, features = ["std", "arc"]`. License: `MIT AND Apache-2.0 AND BSD-3-Clause`, all three on the `deny.toml` allow list. The only non-optional transitive crates it adds beyond what we already use are `vstd`, `verus_builtin`, `verus_builtin_macros` and `verus_state_machines_macros` (all MIT), `spin` (MIT), `num-bigint`, `num-traits`, `lazy_static` and `thiserror` (MIT OR Apache-2.0). CI's existing cargo-deny step checks this on every push. | An exact pin, because a minor release of regorus can change Rego semantics (0.13.0 changed rule path handling). `arc` makes the engine `Send`, so the gateways can share a `Policy` across threads. Without default features we drop regex, glob, time, http, net, jsonschema and yaml builtins: the rules need only core builtins, and a smaller dependency tree is less to audit. Upgrades go through the owner, after the tests pass. |
| D3 | The API is `Policy::load(&Path) -> anyhow::Result<Policy>`, `Policy::version(&self) -> &str` and `Policy::decide(&self, &serde_json::Value) -> Decision`, where `Decision { allow: bool, reasons: Vec<String>, policy_version: String }` (Serialize, for the audit record). The input is an untyped JSON object with an `"action"` field. `decide` never fails. | Four sub-plans will call this. Typed Rust input structs per action would repeat the Rego contract in a second place, and drift. The Rego checks every field it reads, and a missing or mistyped field is a deny. Returning a `Decision` instead of a `Result` means no caller can get the error path wrong: an error is a refusal with the error as its reason. |
| D4 | `policy_version` is `sha256:<64 hex>;regorus=<version>`. The hash covers the loaded files in file-name order, each as `name`, a NUL byte, its length as a u64 little-endian, then its bytes. Files that aren't loaded (anything other than `*.rego` and `*.json`, and subdirectories) aren't hashed. `REGORUS_VERSION` is a const kept equal to the pin by a test that reads `Cargo.lock`. `.gitattributes` has `policy/** text eol=lf`, so a CRLF checkout can't change the hash. Data files load as `data.config.<name>`, rules as `data.kod`, so a data file can never shadow a rule. | The audit log (sub-plan 4) records this with each decision, so an auditor can find the exact rules and engine. Hashing names and lengths means a rename or moving bytes between files changes the version. An engine bump with unchanged files is still a new version. |
| D5 | Default deny by construction. `allow` is true only when some `grant` body for the action holds and the `deny` set is empty. Grants state every condition positively. Every comparison of two input values first checks that one side is a string (`is_string`), so `null == null` and missing-equals-missing never match. Deny rules explain refusals and can veto. With no matching deny reason, the reason is `<action>: the request does not meet the rule for this action`. | The usual "allow unless denied" style lets a missing field silently pass a `!=` check (undefined in Rego). A grant that lists what must be true can't be satisfied by leaving fields out. |
| D6 | Each matrix row has one action name (table below). Two rows are enforced outside the policy engine: local commands (the sandbox) and sign and release (trusted CI runners). They are known actions that are always denied with the enforcer named. | Then every row has a policy answer and a test, and no caller can treat a policy allow as authority for something the policy doesn't enforce. |
| D7 | `delegate` (dispatcher admission) and `retention_delete` (audit record deletion) are matrix rows, added by this plan to the spec on the owner's decision. `retention_delete` is always denied while the profile is a placeholder. | Every operation the policy grants has a matrix row stating its scope, approval, enforcement, evidence and failure behavior. |
| D8 | Model routing: a run may use an endpoint only if it is allowed for **every** class the run read. An unknown class, an empty list or a missing list is refused. The placeholder table has one class, `placeholder`, with one endpoint, `placeholder`. | The spec says a run uses the strictest class of anything it read. Requiring the endpoint for every class is the same when the table is ordered strict-to-lenient, and never weaker when it isn't, and it needs no ranking of classes. The names say "not decided yet" so nobody mistakes them for real values (overview: one class and one endpoint until the routing values are set). |
| D9 | Publish clearance is one team-wide list, `publish.cleared_classes` (placeholder: `["placeholder"]`). The publish rule checks repo, branch `agent/<ticket>-<run>`, draft, a live run and the run's classes. Refusing publish when the audit write fails, and idempotency, stay with the gateway (sub-plan 5). | One team, one set of repos: a per-repo list is a config shape for a problem we don't have yet (add it when a second repo with different clearance appears). The gateway writes the audit record of a decision before acting on it, so the policy can't see whether that write worked. |
| D10 | Delegation: `risk_classes` ranks `low` 0, `medium` 1, `high` 2. The effective class is the highest of the ticket's class and every path rule it matches. Paths are normalised for matching: trimmed, lower-cased, `\` read as `/`, empty and `.` segments dropped. A rule matches at the repo root or below any directory (`src/auth/x` like `auth/x`). A path that isn't a string, is empty after normalising, or has a `..` segment is refused. Phase 1 delegates `low` and `medium`. `wip_in_progress` and `bounces` must be numbers `>= 0`. Path rules are config, not code: `default_path_rules` apply to every repo and `repo_path_rules.<repo>` add a repo's own rules (adding is always safe, since rules only raise). The delegate input names `ticket.repo`, and a ticket without one is refused; publish uses `run.repo`. The placeholder defaults, all `high`, each with a `covers` note, are the owner's list: `auth/`, `payments/`, `crypto/`, `pii/` (personal or regulated data), `migrations/`, `infra/`, `.github/` (CI), `.kod/` (the harness itself), `policy/`, `codeowners`. The real paths are set per repo in `repo_path_rules`. `wip_limit` is 1 (overview placeholder) and `bounce_limit` is 2 (spec default). **The ticket's paths are an early filter only.** `publish` carries the diff's `changed_paths`, and a changed path whose rule class isn't delegated refuses the publish, whatever the ticket said. The dispatcher passes its current WIP count and bounce count; making admission atomic is its job. | Path rules only raise the class, so a path written to dodge a rule must be raised or refused, not ignored. The ticket author picks the ticket's paths, so only the diff can enforce "a change that touches auth/ is high". The policy states the rule; the dispatcher (sub-plan 6) holds the lock. |
| D11 | Logins are GitHub logins, compared lower-cased on both sides (GitHub logins are case-insensitive). Callers map tracker ids to GitHub logins before asking (sub-plan 7, through 6a's `is_team_member` mapping). `requester_approval` is read per team from `teams.json` (`{"<team>": {"requester_approval": "..."}}`); a team with no setting gets `not_sole`. Only approvals with `"agent": false` and a string `login` count. Any other approval (an agent, or no flag) denies the merge outright. An unknown setting denies. `allowed`: at least one human approval. `not_sole`: at least one human approval from someone other than the requester. `forbidden`: at least one human approval, and the requester isn't among them. | The spec's three settings and default. An agent approval should never be on a PR at all, so its presence is a refusal, not just "doesn't count". |
| D12 | The compliance profile is `compliance.json`, `{"profiles": {"<name>": {...}}}`. The placeholder has one profile, `placeholder`, with no values. `retention_delete` is allowed only when every active profile has a numeric `retention_days.<record_type>` and the record is at least as old as the longest of them. Any missing value denies, and the reason names the profile and the key. | The spec: more than one profile can be active and the strictest wins; for retention that is the longest period. A missing value must refuse, so the placeholder refuses all deletion, as the overview asks for sub-plan 4. |
| D13 | Vendor-login requests (`subscription_model_request`): allowed only in `offline_spike` or `offline_eval`, for runners `claude-code` (plans `team`, `enterprise`), `codex` (`business`, `enterprise`) and `cursor` (`business`), with a seat whose `workspace` equals the approved workspace recorded for that runner and a string `account`, and only for the classes in `subscription.classes` (placeholder `["placeholder"]`). No workspace is recorded yet, so every such request is denied with "no approved workspace is recorded for runner X". `kod-loop` has no seat and is always refused. | The spec's credential exception (offline only until security accepts more), the business plans it names, and its eligibility check (valid credentials for the wrong workspace are refused). An unrecorded workspace is a missing value, so it denies. |
| D14 | `people.json` holds `owner` (`kiennt23`, the GitHub login), `backup` (`null` until named) and `tech_leads` (`[]`). `harness_change` needs a human approval from every named owner and backup (today the owner only). The kill switch (`cancel` with scope `all`) is the owner, backup or a tech lead. Restarting after it (`resume`, scope `all`) is the owner or backup only. Cancelling one run is the owner, backup, a tech lead, or a label removal of that run's ticket (`target.run` equals `run.id`). **The kill switch must not depend on the policy:** the dispatcher (sub-plan 6) honours the owner's kill switch without asking the policy when the policy fails to load or evaluate, since a fail-closed policy would otherwise block an emergency stop. | The owner's decision on PR 11: anyone who might see an incident first can stop everything, and only the accountable owner (or backup) restarts. Harness changes need both once the backup is named (overview). A label removal cancels its own run only. |
| D15 | Tests are Rust integration tests in the crate (`cargo test`), not `opa test`. Tests that need a non-placeholder value load a temp copy of `policy/` with one data file replaced (`common::policy_with`). | One toolchain in CI, and the tests exercise the same `Policy` API the callers use. The allow paths that placeholders block are still tested. |

**Action names** (D6, D7). The `input` fields are the contract each caller builds; Tasks 2 to 4 define them in full.

| Matrix row (spec) | Action | Called by |
|---|---|---|
| Local commands | `local_command` (always denied: the sandbox enforces it) | none |
| Model request | `model_request` | 2 |
| Vendor-login model request | `subscription_model_request` | 2, and kod-eval once subscription mode is built |
| Read tracker or docs | `read` | 5 |
| Write to the tracker | `tracker_write` | 5 |
| Write a draft doc | `draft_doc_write` | 5 (doc tools land in 9) |
| Publish to GitHub | `publish` | 5 |
| Approve and merge a PR | `merge` | 7 (`requester-check`) |
| Change the harness, policy or CI config | `harness_change` | 7 (`protected-paths`) |
| Cancel a run or pull the kill switch | `cancel` | 6 |
| Sign and release | `sign_release` (always denied: trusted CI enforces it) | none |
| Restart after the kill switch | `resume` | 6 |
| Admit a ticket for delegation | `delegate` | 6 |
| Delete audit records at the end of retention | `retention_delete` | 4 |

## Execution

The modes follow [ADR 0001](../../adr/0001-per-task-execution-mode.md): a task that owns a safety guarantee or a Review Focus test runs as Subagent + review, and every other task runs Native. Every task here owns one, so every task is Subagent + review. The plan ends with one whole-branch review.

| Task | Mode | Why |
|---|---|---|
| 1. Crate, loader, version and the default-deny core | Subagent + review | Owns default deny, fail-closed evaluation and the policy version (Review Focus 1 and 4). |
| 2. Agent operations: routing, vendor login, reads, writes, publish | Subagent + review | Owns the routing refusals, the subscription exception and the publish scope (Review Focus 1, 3 and 6). |
| 3. Human operations: merge, harness change, cancel | Subagent + review | Owns separation of duties: agents never approve, the requester rule, the kill switch and harness changes (Review Focus 5 and 7). |
| 4. Delegation, compliance profile and matrix coverage | Subagent + review | Owns the risk-class rules, path raising on tickets and diffs, and missing-value denial (Review Focus 2 and 3). |

## Global Constraints

- Toolchain: Rust 1.99.0 from `rust-toolchain.toml`, edition 2024, `publish = false` on every crate.
- CI must pass: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`, and cargo-deny with the existing `deny.toml` (licenses MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, CDLA-Permissive-2.0). `deny.toml` is not changed.
- New dependencies are limited to: `regorus = { version = "=0.13.0", default-features = false, features = ["std", "arc"] }` and `sha2 = "0.10"` in kod-policy. `anyhow`, `serde`, `serde_json` and the dev-dependency `tempfile` are already in the workspace.
- Rego is v1 syntax (regorus's default: `if`, `contains`, `in`, `every`). Every Rego file is `package kod`. The Rust API reads only `data.kod.decision`.
- Data lives under `data.config`: a data file `policy/<name>.json` is loaded as `data.config.<name>`. Rules live under `data.kod`, so no data file can shadow a rule.
- Default deny: a request is allowed only through a `grant` body for its action, with an empty `deny` set. Every comparison between two input values guards one side with `is_string` (or `is_number`).
- Rule names that define a single value are defined once. Where a value has fallbacks, use `else := ... if { ... }` chains: regorus doesn't support an `else` without a body.
- No real values are invented. Unknown values are named `placeholder`, or left out so that the rule reading them denies.
- Commit messages are plain, with no Co-Authored-By or other trailers. Every change goes through a PR.

## Review Focus

1. **A missing, null or wrongly typed field** (an input that isn't an object, `null` ticket ids on both sides, `canceled` left out or `"false"` as a string, `data_classes` as a string) must deny, never allow. Pinned by Task 1 `input_that_is_not_an_object_is_denied` and `a_missing_or_unknown_action_is_denied`, and Task 2 `null_ids_never_match`, `publish_needs_an_explicit_live_run` and `an_unknown_or_missing_data_class_is_refused`.
2. **A path written to dodge a path rule, or a diff that touches a high-risk path the ticket never named** (`./auth/x`, `Auth/x`, `auth\x`, ` auth/x`, `src/auth/x`, `src/../auth/x`, a path that isn't a string, `paths` or `changed_paths` given as a string or left out) must not lower the risk class: the ticket is raised or refused, and the publish is refused. Pinned by Task 4 `path_tricks_cannot_dodge_a_path_rule` and `publish_refuses_a_diff_that_touches_a_high_risk_path`.
3. **A placeholder or missing configuration value** (no recorded seat workspace, a `null` workspace sent by the caller, a compliance profile without the key, no active profile) must deny with a reason that names what is missing, and must never match a `null` in the input. Pinned by Task 2 `subscription_is_refused_until_a_workspace_is_recorded` and `a_null_or_wrong_workspace_never_matches`, and Task 4 `retention_delete_is_refused_while_the_profile_is_a_placeholder` and `a_missing_value_in_any_active_profile_denies`.
4. **A broken policy directory or an evaluation error** (empty or missing directory, Rego that doesn't parse, invalid JSON, no `decision` rule, a decision of the wrong shape, a builtin error at request time) must fail at load or deny with a `policy error` reason, never allow. Pinned by Task 1 `a_broken_policy_fails_to_load` and `evaluation_errors_deny`, and Task 4 `a_malformed_ticket_is_a_policy_error_deny`.
5. **An agent approval, or the requester as the only approver**, must block a merge under the matching setting, and an unknown setting must deny. Requester and approver logins that differ only in case are the same person. Pinned by Task 3 `agent_approvals_block_the_merge`, `not_sole_is_the_default_and_needs_another_human`, `forbidden_refuses_any_requester_approval`, `an_unknown_requester_setting_is_refused` and `logins_compare_case_insensitively`.
6. **Publish outside its own scope** (another repo, a branch other than `agent/<ticket>-<run>`, a non-draft PR, a canceled run, `null` repos on both sides) must be refused. Pinned by Task 2 `publish_anywhere_else_is_refused`, `publish_needs_an_explicit_live_run` and `null_ids_never_match`.
7. **The kill switch and harness changes**: the owner, backup or a tech lead pulls the kill switch, only the owner or backup restarts, a label removal cancels only its own run, and a harness change needs every named owner and backup. The dispatcher honours the owner's kill switch even when the policy fails (D14). Pinned by Task 3 `who_may_cancel_a_run_or_pull_the_kill_switch`, `only_the_owner_or_backup_restarts_after_the_kill_switch` and `a_harness_change_needs_the_owner_and_the_backup_once_named`.

---

## File Structure

```
Cargo.toml                                modify: add crates/kod-policy to members
.gitattributes                            create: policy/** text eol=lf
crates/kod-policy/
  Cargo.toml                              create
  src/lib.rs                              create: Policy, Decision
  tests/common/mod.rs                     create: policy(), policy_with(), copy_policy(), allowed*(), denied*(), set(), unset()
  tests/api.rs                            create: loader, version, default deny, errors
  tests/agent_ops.rs                      create: model_request, subscription_model_request, read, tracker_write, draft_doc_write, publish
  tests/human_ops.rs                      create: merge, harness_change, cancel
  tests/delegation.rs                     create: delegate, retention_delete, matrix coverage
policy/
  kod.rego                                create: decision, default deny, known and external actions
  agent.rego                              create: agent rows (Task 2)
  humans.rego                             create: merge, harness_change, cancel (Task 3)
  delegation.rego  compliance.rego        create (Task 4)
  routing.json  subscription.json  publish.json             create (Task 2)
  people.json  teams.json                                   create (Task 3)
  delegation.json  compliance.json                          create (Task 4)
docs/superpowers/plans/2026-10-14-phase1-overview.md        modify (Task 4): link this plan from row 3
```

---

### Task 1: Crate, loader, version and the default-deny core

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/kod-policy/Cargo.toml`, `crates/kod-policy/src/lib.rs`, `policy/kod.rego`, `.gitattributes`
- Test: `crates/kod-policy/tests/common/mod.rs`, `crates/kod-policy/tests/api.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `kod_policy::Decision { pub allow: bool, pub reasons: Vec<String>, pub policy_version: String }`, deriving Debug, Clone, PartialEq, Eq and Serialize.
  - `kod_policy::Policy` (Send + Sync) with `pub fn load(dir: &Path) -> anyhow::Result<Policy>`, `pub fn version(&self) -> &str` and `pub fn decide(&self, input: &serde_json::Value) -> Decision`.
  - Rego in `package kod`: `decision`, `allow`, `reasons`, `default grant := false`, `deny` (a set of strings), `known_actions`, `external`. Later tasks add `grant if { input.action == "<name>"; ... }` bodies and `deny contains ...` rules.
  - Test helpers in `tests/common/mod.rs`: `policy_dir() -> PathBuf`, `policy() -> &'static Policy`, `copy_policy() -> tempfile::TempDir`, `policy_with(file: &str, contents: Value) -> Policy`, `decide(Value) -> Decision`, `allowed(Value)`, `denied(Value, &str)`, `allowed_by(&Policy, Value)`, `denied_by(&Policy, Value, &str)`, `set(Value, &str, Value) -> Value`, `unset(Value, &str) -> Value`.

- [ ] **Step 1: Create the crate skeleton**

`Cargo.toml` (workspace root):

```toml
[workspace]
members = ["crates/kod-eval", "crates/kod-llm-proxy", "crates/kod-loop", "crates/kod-policy"]
resolver = "3"
```

`crates/kod-policy/Cargo.toml`:

```toml
[package]
name = "kod-policy"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
# Exact pin: a regorus minor release can change Rego semantics. Upgrades go
# through the owner after the policy tests pass.
regorus = { version = "=0.13.0", default-features = false, features = ["std", "arc"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"

[dev-dependencies]
tempfile = "3"
```

`crates/kod-policy/src/lib.rs` (a stub so the tests compile and fail):

```rust
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub allow: bool,
    pub reasons: Vec<String>,
    pub policy_version: String,
}

pub struct Policy;

impl Policy {
    pub fn load(_dir: &Path) -> anyhow::Result<Policy> {
        anyhow::bail!("not implemented")
    }

    pub fn version(&self) -> &str {
        ""
    }

    pub fn decide(&self, _input: &serde_json::Value) -> Decision {
        unimplemented!()
    }
}
```

- [ ] **Step 2: Write the test helpers**

`crates/kod-policy/tests/common/mod.rs`:

```rust
#![allow(dead_code)] // each test file uses a different subset

use std::path::PathBuf;
use std::sync::OnceLock;

use kod_policy::{Decision, Policy};
use serde_json::Value;

pub fn policy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

/// The repo's policy, loaded once.
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| Policy::load(&policy_dir()).expect("the repo policy loads"))
}

/// A temp copy of `policy/`.
pub fn copy_policy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
    }
    dir
}

/// The repo's policy with one data file replaced, for values that are
/// placeholders in the repo.
pub fn policy_with(file: &str, contents: Value) -> Policy {
    let dir = copy_policy();
    std::fs::write(dir.path().join(file), contents.to_string()).unwrap();
    Policy::load(dir.path()).unwrap()
}

pub fn decide(input: Value) -> Decision {
    policy().decide(&input)
}

#[track_caller]
pub fn allowed_by(policy: &Policy, input: Value) {
    let d = policy.decide(&input);
    assert!(d.allow, "expected allow for {input}: {d:?}");
    assert!(d.reasons.is_empty(), "{d:?}");
}

#[track_caller]
pub fn denied_by(policy: &Policy, input: Value, reason: &str) {
    let d = policy.decide(&input);
    assert!(!d.allow, "expected deny for {input}: {d:?}");
    assert!(
        d.reasons.iter().any(|r| r.contains(reason)),
        "expected a reason containing {reason:?} for {input}: {d:?}"
    );
}

#[track_caller]
pub fn allowed(input: Value) {
    allowed_by(policy(), input)
}

#[track_caller]
pub fn denied(input: Value, reason: &str) {
    denied_by(policy(), input, reason)
}

/// `input` with the value at a JSON pointer replaced.
pub fn set(mut input: Value, pointer: &str, value: Value) -> Value {
    *input.pointer_mut(pointer).expect(pointer) = value;
    input
}

/// `input` with the key at a JSON pointer removed.
pub fn unset(mut input: Value, pointer: &str) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    input
        .pointer_mut(parent)
        .expect(parent)
        .as_object_mut()
        .unwrap()
        .remove(key);
    input
}
```

- [ ] **Step 3: Write the failing tests**

`crates/kod-policy/tests/api.rs`:

```rust
mod common;

use std::fs;

use common::{copy_policy, decide, denied, denied_by, policy};
use kod_policy::{Policy, REGORUS_VERSION};
use serde_json::json;

#[test]
fn the_repo_policy_loads_with_a_content_hash_version() {
    let v = policy().version();
    let (hash, engine) = v.split_once(';').unwrap();
    assert!(hash.starts_with("sha256:") && hash.len() == 7 + 64, "{v}");
    assert_eq!(engine, format!("regorus={REGORUS_VERSION}"));
    assert_eq!(decide(json!({})).policy_version, v);
}

#[test]
fn the_version_follows_the_content() {
    let dir = copy_policy();
    let first = Policy::load(dir.path()).unwrap().version().to_string();
    assert_eq!(Policy::load(dir.path()).unwrap().version(), first);

    fs::write(dir.path().join("README.md"), "not loaded").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/extra.rego"), "package kod\n").unwrap();
    assert_eq!(
        Policy::load(dir.path()).unwrap().version(),
        first,
        "files that aren't loaded don't change the version"
    );

    fs::rename(
        dir.path().join("teams.json"),
        dir.path().join("teams2.json"),
    )
    .unwrap();
    assert_ne!(
        Policy::load(dir.path()).unwrap().version(),
        first,
        "a rename changes it"
    );
    fs::rename(
        dir.path().join("teams2.json"),
        dir.path().join("teams.json"),
    )
    .unwrap();

    let rego = dir.path().join("kod.rego");
    let text = fs::read_to_string(&rego).unwrap();
    fs::write(&rego, format!("{text}\n# changed\n")).unwrap();
    assert_ne!(Policy::load(dir.path()).unwrap().version(), first);
}

#[test]
fn a_broken_policy_fails_to_load() {
    let empty = tempfile::tempdir().unwrap();
    assert!(Policy::load(empty.path()).is_err(), "no rego files");
    assert!(
        Policy::load(&empty.path().join("missing")).is_err(),
        "no directory"
    );

    for text in [
        "package kod\nthis is not rego\n",
        "package kod\nother := 1\n",
        "package kod\ndecision := {\"allow\": \"yes\"}\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("kod.rego"), text).unwrap();
        assert!(Policy::load(dir.path()).is_err(), "{text}");
    }

    let dir = copy_policy();
    fs::write(dir.path().join("broken.json"), "{not json").unwrap();
    assert!(Policy::load(dir.path()).is_err(), "invalid JSON");
}

#[test]
fn input_that_is_not_an_object_is_denied() {
    for input in [
        json!(null),
        json!("publish"),
        json!([{"action": "publish"}]),
        json!(1),
    ] {
        let d = policy().decide(&input);
        assert!(!d.allow, "{d:?}");
        assert!(
            d.reasons
                .iter()
                .any(|r| r.contains("input must be a JSON object")),
            "{d:?}"
        );
    }
}

#[test]
fn a_missing_or_unknown_action_is_denied() {
    denied(json!({}), "unknown action");
    denied(json!({"action": "delete_repo"}), "unknown action");
    denied(json!({"action": 5}), "unknown action");
    denied(json!({"action": ["publish"]}), "unknown action");
}

#[test]
fn external_rows_are_never_granted() {
    denied(
        json!({"action": "local_command", "actor": {"kind": "agent"}}),
        "local_command is enforced by the sandbox",
    );
    denied(
        json!({"action": "sign_release", "actor": {"kind": "trusted_ci"}}),
        "sign_release is enforced by trusted CI runners",
    );
}

#[test]
fn evaluation_errors_deny() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("kod.rego"),
        r#"package kod

decision := {"allow": true, "reasons": []} if not input.x

decision := {"allow": count(input.x) > 0, "reasons": []} if input.x
"#,
    )
    .unwrap();
    let probe = Policy::load(dir.path()).unwrap();
    assert!(
        probe.decide(&json!({})).allow,
        "the probe allows when it evaluates"
    );

    // count(5) is a builtin error at request time.
    let d = probe.decide(&json!({"x": 5}));
    assert!(!d.allow, "{d:?}");
    assert!(d.reasons[0].starts_with("policy error"), "{d:?}");
    assert_eq!(d.policy_version, probe.version());
}

#[test]
fn policy_is_send_and_sync() {
    fn shareable<T: Send + Sync>() {}
    shareable::<Policy>();
}

#[test]
fn data_files_cannot_override_rules() {
    let dir = copy_policy();
    fs::write(dir.path().join("allow.json"), "true").unwrap();
    fs::write(
        dir.path().join("decision.json"),
        r#"{"allow": true, "reasons": []}"#,
    )
    .unwrap();
    fs::write(dir.path().join("grant.json"), "true").unwrap();
    let p = Policy::load(dir.path()).unwrap();
    denied_by(&p, json!({"action": "delete_repo"}), "unknown action");
}

#[test]
fn the_regorus_version_matches_cargo_lock() {
    let lock =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let pinned = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"regorus\"\n"))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = ")))
        .unwrap();
    assert_eq!(pinned, format!("\"{REGORUS_VERSION}\""));
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p kod-policy --test api`
Expected: FAIL. Every test that loads a policy panics with "not implemented" (`policy_is_send_and_sync` passes).

- [ ] **Step 5: Write the core Rego**

`policy/kod.rego`:

```rego
# Kod policy: the core. Default deny.
#
# The Rust API (crates/kod-policy) reads only data.kod.decision. A request
# is allowed when a grant rule for its action holds and no deny rule fires.
# Grant rules state every condition positively, so a missing or null field
# never allows anything. Deny rules explain refusals and can veto a grant.
# A data file <name>.json in this directory is data.config.<name>, so a
# data file can never shadow a rule.
package kod

decision := {"allow": allow, "reasons": reasons}

default allow := false

default grant := false

allow if {
	grant
	count(deny) == 0
}

reasons := sort(deny) if {
	count(deny) > 0
} else := [] if {
	allow
} else := [sprintf("%v: the request does not meet the rule for this action", [input.action])] if {
	true
}

# Every action the policy knows: one per row of the action and permission
# matrix.
known_actions := {
	"local_command",
	"model_request",
	"subscription_model_request",
	"read",
	"tracker_write",
	"draft_doc_write",
	"publish",
	"merge",
	"harness_change",
	"cancel",
	"resume",
	"sign_release",
	"delegate",
	"retention_delete",
}

known_action if input.action in known_actions

deny contains "unknown action" if not known_action

# Matrix rows enforced outside the policy engine. The policy never grants
# them, so no caller can treat an allow as authority for them.
external := {
	"local_command": "the sandbox",
	"sign_release": "trusted CI runners",
}

deny contains sprintf("%v is enforced by %v, not by policy", [input.action, by]) if {
	by := external[input.action]
}
```

- [ ] **Step 6: Implement the loader and `decide`**

`.gitattributes` (repo root), so the policy hash doesn't depend on the checkout's line endings:

```
policy/** text eol=lf
```

`crates/kod-policy/src/lib.rs`:

```rust
//! Kod's policy engine: the Rego rules and data files in one directory,
//! evaluated in-process with regorus. Callers act only on `Decision::allow`
//! and record the whole decision, `policy_version` included.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use regorus::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The one rule every decision reads.
const DECISION_RULE: &str = "data.kod.decision";

/// The pinned regorus version, part of `policy_version` so an engine bump
/// with unchanged files is still a new version. A test keeps it equal to
/// the version in Cargo.lock.
pub const REGORUS_VERSION: &str = "0.13.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub allow: bool,
    pub reasons: Vec<String>,
    pub policy_version: String,
}

#[derive(Deserialize)]
struct Raw {
    allow: bool,
    reasons: Vec<String>,
}

pub struct Policy {
    // ponytail: one engine behind a Mutex serializes decisions; clone the
    // engine per thread if lock contention ever shows up.
    engine: Mutex<Engine>,
    version: String,
}

impl Policy {
    /// Loads every `*.rego` and `*.json` file directly in `dir`. A data file
    /// `<name>.json` becomes `data.config.<name>`. Fails if there is no Rego, a
    /// file doesn't parse, or the rules don't yield a decision.
    pub fn load(dir: &Path) -> Result<Policy> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if path.is_file() && matches!(ext, Some("rego" | "json")) {
                files.push(path);
            }
        }
        files.sort();
        if !files
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "rego"))
        {
            bail!("no .rego files in {}", dir.display());
        }

        let mut engine = Engine::new();
        let mut data = Map::new();
        let mut hash = Sha256::new();
        for path in &files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .with_context(|| format!("non-UTF-8 file name: {}", path.display()))?;
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            hash.update(name.as_bytes());
            hash.update([0]);
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
            if let Some(stem) = name.strip_suffix(".json") {
                let value: Value =
                    serde_json::from_str(&text).with_context(|| format!("{name}: invalid JSON"))?;
                data.insert(stem.to_string(), value);
            } else {
                engine
                    .add_policy(name.to_string(), text)
                    .map_err(|e| anyhow!("{name}: {e}"))?;
            }
        }
        let data = Value::Object(Map::from_iter([(
            "config".to_string(),
            Value::Object(data),
        )]));
        engine
            .add_data_json(&data.to_string())
            .map_err(|e| anyhow!("data: {e}"))?;

        let policy = Policy {
            engine: Mutex::new(engine),
            version: format!("sha256:{:x};regorus={REGORUS_VERSION}", hash.finalize()),
        };
        // A broken rule set fails here, at startup, not as a deny on every request.
        policy
            .eval(&Value::Object(Map::new()))
            .context("the rules don't yield a decision")?;
        Ok(policy)
    }

    /// `sha256:<hex>;regorus=<version>`: the hash covers the loaded files'
    /// names and contents.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Decides one request. Never fails: an input that isn't a JSON object,
    /// or any evaluation error, is a deny whose reason says why.
    pub fn decide(&self, input: &Value) -> Decision {
        let (allow, reasons) = match self.eval(input) {
            Ok(raw) => (raw.allow, raw.reasons),
            Err(e) => (false, vec![format!("policy error: {e:#}")]),
        };
        Decision {
            allow,
            reasons,
            policy_version: self.version.clone(),
        }
    }

    fn eval(&self, input: &Value) -> Result<Raw> {
        if !input.is_object() {
            bail!("input must be a JSON object");
        }
        let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        engine
            .set_input_json(&input.to_string())
            .map_err(|e| anyhow!("input: {e}"))?;
        let value = engine
            .eval_rule(DECISION_RULE.to_string())
            .map_err(|e| anyhow!("{e}"))?;
        let json = value.to_json_str().map_err(|e| anyhow!("{e}"))?;
        serde_json::from_str(&json).with_context(|| format!("not a decision: {json}"))
    }
}
```

If `policy_is_send_and_sync` fails to compile, `regorus::Engine` isn't `Send` even with the `arc` feature. Stop and report it to the owner; don't work around it with `unsafe impl`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p kod-policy --test api`
Expected: PASS (10 tests).

- [ ] **Step 8: Check licenses and lints**

Run: `cargo deny check licenses && cargo clippy -p kod-policy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all three exit 0. `cargo deny` lists no rejected license. If `cargo-deny` isn't installed locally, CI's cargo-deny step is the check; read its log on the PR.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock .gitattributes crates/kod-policy policy/kod.rego
git commit -m "kod-policy: loader, content-hash version and default-deny core"
```

---

### Task 2: Agent operations: routing, vendor login, reads, writes, publish

**Files:**
- Create: `policy/agent.rego`, `policy/routing.json`, `policy/subscription.json`, `policy/publish.json`
- Test: `crates/kod-policy/tests/agent_ops.rs`

**Interfaces:**
- Consumes: Task 1's `Policy`, `kod.rego` (`grant`, `deny`), and the helpers in `tests/common/mod.rs`.
- Produces, as input contracts for sub-plans 2 and 5 (every request also has `"actor": {"kind": "agent"}`):
  - `model_request`: `{"run": {"data_classes": [..], "auth_mode": "api_key"|"cloud"}, "endpoint": "<name>"}`. kod-loop uses an API key or a cloud provider; `subscription` is the separate vendor-login action.
  - `subscription_model_request`: `{"runner": "claude-code"|"codex"|"cursor", "context": "offline_spike"|"offline_eval", "seat": {"account", "workspace", "plan"}, "run": {"data_classes": [..]}}`.
  - `read`: `{"run": {"ticket", "linked_docs": [..]}, "target": {"kind": "ticket"|"doc", "id"}}`.
  - `tracker_write`: `{"kind": "comment"|"transition"|"create_linked", "run": {"ticket", "linked_tickets": [..], "canceled": false}, "target": {"ticket"} or {"parent"}}`. `linked_tickets` holds only rework requests Kod itself created and linked to the run's ticket (the gateway records them); that is why a write may reach them while `read` stays on the ticket and its docs.
  - `draft_doc_write`: `{"run": {"id", "canceled": false}, "target": {"folder": "draft", "owner_run"}}`. The gateway sets `owner_run` to the run that created the draft.
  - `publish`: `{"run": {"id", "ticket", "repo", "canceled": false, "data_classes": [..]}, "target": {"repo", "branch", "draft": true}, "changed_paths": [..]}`. `changed_paths` are the repo-relative paths the diff touches; Task 4 refuses a publish whose diff touches a high-risk path.
  - Data: `data.config.routing.classes.<class>.endpoints`, `data.config.subscription.{contexts, classes, runners.<runner>.{plans, workspace}}`, `data.config.publish.cleared_classes`.

- [ ] **Step 1: Write the failing tests**

`crates/kod-policy/tests/agent_ops.rs`:

```rust
mod common;

use common::{allowed, allowed_by, decide, denied, denied_by, policy_with, set, unset};
use kod_policy::Policy;
use serde_json::{Value, json};

fn run() -> Value {
    json!({
        "id": "r1", "ticket": "KOD-1", "repo": "acme/app", "canceled": false,
        "data_classes": ["placeholder"], "auth_mode": "api_key",
        "linked_docs": ["doc-1"], "linked_tickets": ["KOD-2"]
    })
}

fn req(action: &str, extra: Value) -> Value {
    let mut v = json!({"action": action, "actor": {"kind": "agent"}, "run": run()});
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

// model_request

#[test]
fn model_request_to_an_allowed_endpoint_is_allowed() {
    allowed(req("model_request", json!({"endpoint": "placeholder"})));
}

#[test]
fn an_unknown_or_missing_data_class_is_refused() {
    let base = req("model_request", json!({"endpoint": "placeholder"}));
    denied(
        set(base.clone(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
    denied(
        set(
            base.clone(),
            "/run/data_classes",
            json!(["placeholder", "secret"]),
        ),
        "unknown data class: secret",
    );
    denied(
        set(base.clone(), "/run/data_classes", json!([])),
        "the run has no data class",
    );
    denied(
        set(base.clone(), "/run/data_classes", json!("placeholder")),
        "the run has no data class",
    );
    denied(
        unset(base, "/run/data_classes"),
        "the run has no data class",
    );
}

#[test]
fn a_disallowed_endpoint_is_refused() {
    denied(
        req("model_request", json!({"endpoint": "api.example.com"})),
        "endpoint api.example.com is not allowed for data class placeholder",
    );
    denied(req("model_request", json!({})), "does not meet the rule");
}

#[test]
fn model_requests_need_an_agent_and_api_key_or_cloud_auth() {
    let base = req("model_request", json!({"endpoint": "placeholder"}));
    denied(
        set(base.clone(), "/actor/kind", json!("human")),
        "only an agent can request this",
    );
    denied(
        unset(base.clone(), "/actor"),
        "only an agent can request this",
    );
    allowed(set(base.clone(), "/run/auth_mode", json!("cloud")));
    denied(
        set(base.clone(), "/run/auth_mode", json!("subscription")),
        "model_request needs auth_mode api_key or cloud",
    );
    denied(unset(base, "/run/auth_mode"), "does not meet the rule");
}

// subscription_model_request

fn sub() -> Value {
    req(
        "subscription_model_request",
        json!({
            "runner": "codex", "context": "offline_eval",
            "seat": {"account": "kod-bot", "workspace": "ws-1", "plan": "business"}
        }),
    )
}

fn sub_policy() -> Policy {
    policy_with(
        "subscription.json",
        json!({
            "contexts": ["offline_spike", "offline_eval"],
            "classes": ["placeholder"],
            "runners": {
                "claude-code": {"plans": ["team", "enterprise"]},
                "codex": {"plans": ["business", "enterprise"], "workspace": "ws-1"},
                "cursor": {"plans": ["business"]}
            }
        }),
    )
}

#[test]
fn subscription_is_refused_until_a_workspace_is_recorded() {
    denied(sub(), "no approved workspace is recorded for runner codex");
}

#[test]
fn subscription_with_the_recorded_workspace_is_allowed_offline_only() {
    let p = sub_policy();
    allowed_by(&p, sub());
    allowed_by(&p, set(sub(), "/context", json!("offline_spike")));
    denied_by(
        &p,
        set(sub(), "/context", json!("connected")),
        "subscription mode is allowed only in",
    );
    denied_by(
        &p,
        set(sub(), "/seat/plan", json!("plus")),
        "plan plus is not a business plan for runner codex",
    );
    denied_by(
        &p,
        set(sub(), "/runner", json!("kod-loop")),
        "runner kod-loop has no subscription seat",
    );
    denied_by(
        &p,
        set(sub(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
    denied_by(&p, unset(sub(), "/seat/account"), "does not meet the rule");
}

#[test]
fn a_null_or_wrong_workspace_never_matches() {
    let p = sub_policy();
    denied_by(
        &p,
        set(sub(), "/seat/workspace", json!("ws-2")),
        "the seat belongs to a different workspace",
    );
    denied_by(
        &p,
        set(sub(), "/seat/workspace", Value::Null),
        "the seat belongs to a different workspace",
    );
    // The repo policy records no workspace; a null from the caller must not match the gap.
    denied(
        set(sub(), "/seat/workspace", Value::Null),
        "no approved workspace is recorded",
    );
}

// read

#[test]
fn reads_are_limited_to_the_ticket_and_its_linked_docs() {
    allowed(req(
        "read",
        json!({"target": {"kind": "ticket", "id": "KOD-1"}}),
    ));
    allowed(req(
        "read",
        json!({"target": {"kind": "doc", "id": "doc-1"}}),
    ));
    denied(
        req("read", json!({"target": {"kind": "ticket", "id": "KOD-9"}})),
        "does not meet the rule",
    );
    denied(
        req("read", json!({"target": {"kind": "doc", "id": "doc-9"}})),
        "does not meet the rule",
    );
    denied(
        req("read", json!({"target": {"kind": "repo", "id": "KOD-1"}})),
        "does not meet the rule",
    );
}

#[test]
fn null_ids_never_match() {
    let read = set(
        req("read", json!({"target": {"kind": "ticket", "id": null}})),
        "/run/ticket",
        Value::Null,
    );
    denied(read, "does not meet the rule");

    // Everything else about this publish is valid: only the repo guard can refuse it.
    let publish = set(
        set(publish(), "/run/repo", Value::Null),
        "/target/repo",
        Value::Null,
    );
    let d = decide(publish);
    assert!(!d.allow, "{d:?}");
}

// tracker_write

#[test]
fn tracker_writes_are_limited_to_the_ticket_and_its_linked_requests() {
    allowed(req(
        "tracker_write",
        json!({"kind": "comment", "target": {"ticket": "KOD-1"}}),
    ));
    allowed(req(
        "tracker_write",
        json!({"kind": "transition", "target": {"ticket": "KOD-2"}}),
    ));
    allowed(req(
        "tracker_write",
        json!({"kind": "create_linked", "target": {"parent": "KOD-1"}}),
    ));
    denied(
        req(
            "tracker_write",
            json!({"kind": "comment", "target": {"ticket": "KOD-9"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "tracker_write",
            json!({"kind": "create_linked", "target": {"parent": "KOD-9"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "tracker_write",
            json!({"kind": "delete", "target": {"ticket": "KOD-1"}}),
        ),
        "unknown tracker write: delete",
    );
}

// draft_doc_write

#[test]
fn draft_doc_writes_are_limited_to_the_runs_own_draft() {
    allowed(req(
        "draft_doc_write",
        json!({"target": {"folder": "draft", "owner_run": "r1"}}),
    ));
    denied(
        req(
            "draft_doc_write",
            json!({"target": {"folder": "approved", "owner_run": "r1"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "draft_doc_write",
            json!({"target": {"folder": "draft", "owner_run": "r2"}}),
        ),
        "does not meet the rule",
    );
}

// publish

fn publish() -> Value {
    req(
        "publish",
        json!({
            "target": {"repo": "acme/app", "branch": "agent/KOD-1-r1", "draft": true},
            "changed_paths": ["src/lib.rs", "docs/readme.md"]
        }),
    )
}

#[test]
fn publish_to_the_runs_own_branch_as_a_draft_is_allowed() {
    allowed(publish());
}

#[test]
fn publish_anywhere_else_is_refused() {
    denied(
        set(publish(), "/target/repo", json!("acme/other")),
        "publish target must be the run's repo",
    );
    denied(
        set(publish(), "/target/branch", json!("main")),
        "publish branch must be agent/KOD-1-r1",
    );
    denied(
        set(publish(), "/target/branch", json!("agent/KOD-1-r2")),
        "publish branch must be agent/KOD-1-r1",
    );
    denied(
        set(publish(), "/target/draft", json!(false)),
        "publish must open a draft PR",
    );
    denied(unset(publish(), "/target/draft"), "does not meet the rule");
    denied(
        set(publish(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
}

#[test]
fn publish_needs_an_explicit_live_run() {
    denied(
        set(publish(), "/run/canceled", json!(true)),
        "the run is canceled",
    );
    denied(unset(publish(), "/run/canceled"), "does not meet the rule");
    denied(
        set(publish(), "/run/canceled", json!("false")),
        "does not meet the rule",
    );
}

#[test]
fn every_agent_write_is_refused_for_a_canceled_run() {
    for write in [
        publish(),
        req(
            "tracker_write",
            json!({"kind": "comment", "target": {"ticket": "KOD-1"}}),
        ),
        req(
            "draft_doc_write",
            json!({"target": {"folder": "draft", "owner_run": "r1"}}),
        ),
    ] {
        allowed(write.clone());
        denied(
            set(write, "/run/canceled", json!(true)),
            "the run is canceled",
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kod-policy --test agent_ops`
Expected: FAIL. The `allowed(...)` tests fail with "does not meet the rule" (no grant exists yet), and the tests that expect specific reasons fail because those reasons don't exist yet.

- [ ] **Step 3: Write the data files**

`policy/routing.json`:

```json
{
  "classes": {
    "placeholder": { "endpoints": ["placeholder"] }
  }
}
```

`policy/subscription.json` (no `workspace` recorded for any runner, so every request is refused until one is):

```json
{
  "contexts": ["offline_spike", "offline_eval"],
  "classes": ["placeholder"],
  "runners": {
    "claude-code": { "plans": ["team", "enterprise"] },
    "codex": { "plans": ["business", "enterprise"] },
    "cursor": { "plans": ["business"] }
  }
}
```

`policy/publish.json`:

```json
{
  "cleared_classes": ["placeholder"]
}
```

- [ ] **Step 4: Write the rules**

`policy/agent.rego`:

```rego
# Agent operations through the gateways: model requests (routing policy),
# vendor-login model requests, tracker and docs reads and writes, draft docs
# and publish. Callers: sub-plan 2 (LLM gateway) and sub-plan 5 (tool gateway).
package kod

agent_actions := {"model_request", "subscription_model_request", "read", "tracker_write", "draft_doc_write", "publish"}

write_actions := {"tracker_write", "draft_doc_write", "publish"}

class_actions := {"model_request", "subscription_model_request", "publish"}

is_agent if input.actor.kind == "agent"

# A run is live only when it says canceled: false.
live_run if input.run.canceled == false

has_classes if {
	is_array(input.run.data_classes)
	count(input.run.data_classes) > 0
}

# Every class the run read is a known class.
run_classes_known if {
	has_classes
	every c in input.run.data_classes {
		data.config.routing.classes[c]
	}
}

deny contains "only an agent can request this" if {
	input.action in agent_actions
	not is_agent
}

deny contains "the run is canceled" if {
	input.action in write_actions
	input.run.canceled == true
}

deny contains "the run has no data class" if {
	input.action in class_actions
	not has_classes
}

deny contains sprintf("unknown data class: %v", [c]) if {
	input.action in class_actions
	has_classes
	some c in input.run.data_classes
	not data.config.routing.classes[c]
}

# model_request: the endpoint must be allowed for every class the run read.

endpoint_allowed(c) if input.endpoint in data.config.routing.classes[c].endpoints

grant if {
	input.action == "model_request"
	is_agent
	input.run.auth_mode in {"api_key", "cloud"}
	run_classes_known
	every c in input.run.data_classes {
		endpoint_allowed(c)
	}
}

model_auth_ok if input.run.auth_mode in {"api_key", "cloud"}

deny contains "model_request needs auth_mode api_key or cloud" if {
	input.action == "model_request"
	input.run.auth_mode
	not model_auth_ok
}

deny contains sprintf("endpoint %v is not allowed for data class %v", [input.endpoint, c]) if {
	input.action == "model_request"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not endpoint_allowed(c)
}

# subscription_model_request: the credential exception. Offline only, a
# business plan, and the seat's workspace must be the one recorded.

sub_runner := data.config.subscription.runners[input.runner]

sub_context_ok if input.context in data.config.subscription.contexts

sub_plan_ok if input.seat.plan in sub_runner.plans

sub_class_ok(c) if c in data.config.subscription.classes

seat_ok if {
	sub_plan_ok
	is_string(sub_runner.workspace)
	input.seat.workspace == sub_runner.workspace
	is_string(input.seat.account)
}

grant if {
	input.action == "subscription_model_request"
	is_agent
	sub_context_ok
	seat_ok
	run_classes_known
	every c in input.run.data_classes {
		sub_class_ok(c)
	}
}

deny contains sprintf("subscription mode is allowed only in: %v", [concat(", ", data.config.subscription.contexts)]) if {
	input.action == "subscription_model_request"
	not sub_context_ok
}

deny contains sprintf("runner %v has no subscription seat", [input.runner]) if {
	input.action == "subscription_model_request"
	not sub_runner
}

deny contains sprintf("plan %v is not a business plan for runner %v", [input.seat.plan, input.runner]) if {
	input.action == "subscription_model_request"
	sub_runner
	not sub_plan_ok
}

deny contains sprintf("no approved workspace is recorded for runner %v", [input.runner]) if {
	input.action == "subscription_model_request"
	sub_runner
	not is_string(sub_runner.workspace)
}

deny contains "the seat belongs to a different workspace" if {
	input.action == "subscription_model_request"
	is_string(sub_runner.workspace)
	input.seat.workspace != sub_runner.workspace
}

deny contains sprintf("data class %v is not allowed in subscription mode", [c]) if {
	input.action == "subscription_model_request"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not sub_class_ok(c)
}

# read: the run's own ticket and the docs it links.

grant if {
	input.action == "read"
	is_agent
	input.target.kind == "ticket"
	is_string(input.run.ticket)
	input.target.id == input.run.ticket
}

grant if {
	input.action == "read"
	is_agent
	input.target.kind == "doc"
	is_string(input.target.id)
	input.target.id in input.run.linked_docs
}

# tracker_write: comment or transition on the run's ticket or a request
# linked to it; create a request linked to the run's ticket.

own_or_linked(t) if {
	is_string(t)
	t == input.run.ticket
}

own_or_linked(t) if {
	is_string(t)
	t in input.run.linked_tickets
}

grant if {
	input.action == "tracker_write"
	is_agent
	live_run
	input.kind in {"comment", "transition"}
	own_or_linked(input.target.ticket)
}

grant if {
	input.action == "tracker_write"
	is_agent
	live_run
	input.kind == "create_linked"
	is_string(input.run.ticket)
	input.target.parent == input.run.ticket
}

tracker_write_kind if input.kind in {"comment", "transition", "create_linked"}

deny contains sprintf("unknown tracker write: %v", [input.kind]) if {
	input.action == "tracker_write"
	not tracker_write_kind
}

# draft_doc_write: the run's own draft, in the Draft folder.

grant if {
	input.action == "draft_doc_write"
	is_agent
	live_run
	input.target.folder == "draft"
	is_string(input.run.id)
	input.target.owner_run == input.run.id
}

# publish: the run's repo, branch agent/<ticket>-<run>, a draft PR, and
# only data classes cleared for publish.

run_branch := sprintf("agent/%s-%s", [input.run.ticket, input.run.id]) if {
	is_string(input.run.ticket)
	is_string(input.run.id)
}

publish_class_ok(c) if c in data.config.publish.cleared_classes

grant if {
	input.action == "publish"
	is_agent
	live_run
	is_string(input.run.repo)
	input.target.repo == input.run.repo
	input.target.branch == run_branch
	input.target.draft == true
	run_classes_known
	every c in input.run.data_classes {
		publish_class_ok(c)
	}
}

deny contains "publish target must be the run's repo" if {
	input.action == "publish"
	input.target.repo != input.run.repo
}

deny contains sprintf("publish branch must be %v", [run_branch]) if {
	input.action == "publish"
	input.target.branch != run_branch
}

deny contains "publish must open a draft PR" if {
	input.action == "publish"
	input.target.draft != true
}

deny contains sprintf("data class %v is not cleared for publish", [c]) if {
	input.action == "publish"
	has_classes
	some c in input.run.data_classes
	data.config.routing.classes[c]
	not publish_class_ok(c)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p kod-policy`
Expected: PASS (`api` and `agent_ops`).

- [ ] **Step 6: Commit**

```bash
git add policy/agent.rego policy/routing.json policy/subscription.json policy/publish.json crates/kod-policy/tests/agent_ops.rs
git commit -m "kod-policy: agent operations, model routing and the subscription exception"
```

---

### Task 3: Human operations: merge, harness change, cancel

**Files:**
- Create: `policy/humans.rego`, `policy/people.json`, `policy/teams.json`
- Test: `crates/kod-policy/tests/human_ops.rs`

**Interfaces:**
- Consumes: Task 1's `Policy` and helpers; `grant` and `deny` from `kod.rego`.
- Produces, as input contracts for sub-plans 6 and 7:
  - `merge`: `{"team": "<team>", "requester": "<login>", "approvals": [{"login": "<login>", "agent": false}, ..]}`. The requester comes from the tracker (6a's adapter).
  - `harness_change`: `{"approvals": [{"login", "agent": false}, ..]}`.
  - `cancel`: `{"scope": "run"|"all", "actor": {"kind": "human", "login"} | {"kind": "label_removal"}, "run": {"id"}, "target": {"run"}}`. A label removal cancels only `target.run == run.id`, the run of the ticket that lost its label. Every login is a GitHub login (callers map tracker ids), compared lower-cased.
  - `resume`: `{"scope": "all", "actor": {"kind": "human", "login"}}`. Restart after the kill switch: owner or backup only.
  - Data: `data.config.people.{owner, backup, tech_leads}`, `data.config.teams.<team>.requester_approval`.
  - Rego rules later tasks may use: `owners` (set of lower-cased logins), `human_approvers` (set of lower-cased logins).

- [ ] **Step 1: Write the failing tests**

`crates/kod-policy/tests/human_ops.rs`:

```rust
mod common;

use common::{allowed, allowed_by, denied, denied_by, policy_with, set, unset};
use kod_policy::Policy;
use serde_json::{Value, json};

fn human(login: &str) -> Value {
    json!({"login": login, "agent": false})
}

fn merge(approvals: Vec<Value>) -> Value {
    json!({"action": "merge", "team": "core", "requester": "alice", "approvals": approvals})
}

fn team_setting(setting: &str) -> Policy {
    policy_with(
        "teams.json",
        json!({"core": {"requester_approval": setting}}),
    )
}

// merge

#[test]
fn not_sole_is_the_default_and_needs_another_human() {
    allowed(merge(vec![human("bob")]));
    allowed(merge(vec![human("alice"), human("bob")]));
    denied(
        merge(vec![human("alice")]),
        "the requester can't be the only approver",
    );
    denied(merge(vec![]), "no human approval");
    denied(
        json!({"action": "merge", "team": "core", "requester": "alice"}),
        "no human approval",
    );
}

#[test]
fn allowed_lets_the_requester_approve_alone() {
    let p = team_setting("allowed");
    allowed_by(&p, merge(vec![human("alice")]));
    denied_by(&p, merge(vec![]), "no human approval");
}

#[test]
fn forbidden_refuses_any_requester_approval() {
    let p = team_setting("forbidden");
    allowed_by(&p, merge(vec![human("bob")]));
    denied_by(
        &p,
        merge(vec![human("alice"), human("bob")]),
        "the requester approved",
    );
    denied_by(&p, merge(vec![human("alice")]), "the requester approved");
}

#[test]
fn an_unknown_requester_setting_is_refused() {
    let p = team_setting("allow");
    denied_by(
        &p,
        merge(vec![human("bob")]),
        "unknown requester_approval setting: allow",
    );
}

#[test]
fn agent_approvals_block_the_merge() {
    denied(
        merge(vec![
            human("bob"),
            json!({"login": "kod-agent", "agent": true}),
        ]),
        "agents never approve",
    );
    denied(
        merge(vec![human("bob"), json!({"login": "carol"})]),
        "agents never approve",
    );
    denied(
        merge(vec![json!({"login": "kod-agent", "agent": true})]),
        "no human approval",
    );
}

#[test]
fn a_merge_needs_a_named_requester() {
    let mut no_requester = merge(vec![human("bob")]);
    no_requester.as_object_mut().unwrap().remove("requester");
    denied(no_requester, "does not meet the rule");
    let null_requester =
        json!({"action": "merge", "team": "core", "requester": null, "approvals": [human("bob")]});
    denied(null_requester, "does not meet the rule");
}

// harness_change

fn harness(approvals: Vec<Value>) -> Value {
    json!({"action": "harness_change", "approvals": approvals})
}

#[test]
fn a_harness_change_needs_the_owner_and_the_backup_once_named() {
    allowed(harness(vec![human("kiennt23")]));
    denied(
        harness(vec![human("bob")]),
        "harness change needs approval from kiennt23",
    );
    denied(
        harness(vec![json!({"login": "kiennt23", "agent": true})]),
        "agents never approve",
    );

    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": []}),
    );
    denied_by(
        &p,
        harness(vec![human("kiennt23")]),
        "harness change needs approval from bk",
    );
    allowed_by(&p, harness(vec![human("kiennt23"), human("bk")]));
}

// cancel

fn cancel(scope: &str, actor: Value) -> Value {
    json!({"action": "cancel", "scope": scope, "actor": actor, "run": {"id": "r1"}, "target": {"run": "r1"}})
}

#[test]
fn who_may_cancel_a_run_or_pull_the_kill_switch() {
    let owner = json!({"kind": "human", "login": "kiennt23"});
    let tech_lead = json!({"kind": "human", "login": "tl"});
    let stranger = json!({"kind": "human", "login": "mallory"});
    let label = json!({"kind": "label_removal"});

    allowed(cancel("run", owner.clone()));
    allowed(cancel("all", owner));
    allowed(cancel("run", label.clone()));
    denied(cancel("all", label.clone()), "does not meet the rule");
    denied(
        set(cancel("run", label.clone()), "/target/run", json!("r2")),
        "does not meet the rule",
    );
    denied(
        unset(cancel("run", label.clone()), "/target"),
        "does not meet the rule",
    );
    denied(
        set(
            set(cancel("run", label), "/run/id", Value::Null),
            "/target/run",
            Value::Null,
        ),
        "does not meet the rule",
    );
    denied(cancel("run", stranger.clone()), "does not meet the rule");
    denied(cancel("all", stranger), "does not meet the rule");
    denied(
        cancel("run", json!({"kind": "agent"})),
        "does not meet the rule",
    );
    denied(
        cancel("everything", json!({"kind": "human", "login": "kiennt23"})),
        "does not meet the rule",
    );

    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": ["tl"]}),
    );
    allowed_by(&p, cancel("run", tech_lead.clone()));
    allowed_by(&p, cancel("all", tech_lead));
    allowed_by(&p, cancel("all", json!({"kind": "human", "login": "bk"})));
}

fn resume(actor: Value) -> Value {
    json!({"action": "resume", "scope": "all", "actor": actor})
}

#[test]
fn only_the_owner_or_backup_restarts_after_the_kill_switch() {
    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": ["tl"]}),
    );
    allowed_by(&p, resume(json!({"kind": "human", "login": "KienNT23"})));
    allowed_by(&p, resume(json!({"kind": "human", "login": "bk"})));
    denied_by(
        &p,
        resume(json!({"kind": "human", "login": "tl"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        resume(json!({"kind": "label_removal"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        resume(json!({"kind": "agent"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        set(
            resume(json!({"kind": "human", "login": "bk"})),
            "/scope",
            json!("run"),
        ),
        "does not meet the rule",
    );
}

#[test]
fn logins_compare_case_insensitively() {
    let alice_alone = json!({"action": "merge", "team": "core", "requester": "Alice", "approvals": [human("alice")]});
    denied(
        alice_alone.clone(),
        "the requester can't be the only approver",
    );
    let alice_and_bob = json!({"action": "merge", "team": "core", "requester": "ALICE", "approvals": [human("alice"), human("Bob")]});
    allowed(alice_and_bob.clone());
    denied_by(
        &team_setting("forbidden"),
        alice_and_bob,
        "the requester approved",
    );
    allowed_by(&team_setting("allowed"), alice_alone);
    allowed(harness(vec![human("KienNT23")]));
    allowed(cancel("all", json!({"kind": "human", "login": "KIENNT23"})));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kod-policy --test human_ops`
Expected: FAIL. No `merge`, `harness_change`, `cancel` or `resume` grant exists yet.

- [ ] **Step 3: Write the data files**

`policy/people.json` (`backup` stays `null` until the owner names one):

```json
{
  "owner": "kiennt23",
  "backup": null,
  "tech_leads": []
}
```

`policy/teams.json` (no team has a setting yet, so every team gets `not_sole`):

```json
{}
```

- [ ] **Step 4: Write the rules**

`policy/humans.rego`:

```rego
# Human and control operations: merge (requester_approval), harness changes,
# and cancel / kill switch. Callers: sub-plan 7 (requester-check and
# protected-paths) and sub-plan 6 (dispatcher).
package kod

# The spec's default for a team with no setting.
default_requester_approval := "not_sole"

requester_rule := object.get(object.get(data.config.teams, input.team, {}), "requester_approval", default_requester_approval)

known_requester_rule if requester_rule in {"allowed", "not_sole", "forbidden"}

human_approval(a) if {
	a.agent == false
	is_string(a.login)
}

# GitHub logins are case-insensitive, so every login is compared lower-cased.
human_approvers := {lower(a.login) | some a in input.approvals; human_approval(a)}

requester_login := lower(input.requester) if is_string(input.requester)

actor_login := lower(input.actor.login) if is_string(input.actor.login)

requester_approved if requester_login in human_approvers

# The owner and, once named, the backup.
owners contains login if {
	some key in ["owner", "backup"]
	p := data.config.people[key]
	is_string(p)
	login := lower(p)
}

deny contains "every approval must be from a named human; agents never approve" if {
	input.action in {"merge", "harness_change"}
	some a in input.approvals
	not human_approval(a)
}

# merge

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "allowed"
	count(human_approvers) > 0
}

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "not_sole"
	count(human_approvers - {requester_login}) > 0
}

grant if {
	input.action == "merge"
	requester_login
	requester_rule == "forbidden"
	count(human_approvers) > 0
	not requester_approved
}

deny contains "no human approval" if {
	input.action == "merge"
	count(human_approvers) == 0
}

deny contains sprintf("unknown requester_approval setting: %v", [requester_rule]) if {
	input.action == "merge"
	not known_requester_rule
}

deny contains "requester_approval is not_sole: the requester can't be the only approver" if {
	input.action == "merge"
	requester_rule == "not_sole"
	count(human_approvers) > 0
	count(human_approvers - {requester_login}) == 0
}

deny contains "requester_approval is forbidden: the requester approved" if {
	input.action == "merge"
	requester_rule == "forbidden"
	requester_approved
}

# harness_change: every named owner and backup approved.

approved_by(p) if p in human_approvers

grant if {
	input.action == "harness_change"
	count(owners) > 0
	every p in owners {
		approved_by(p)
	}
}

deny contains sprintf("harness change needs approval from %v", [p]) if {
	input.action == "harness_change"
	some p in owners
	not approved_by(p)
}

# cancel: one run by the owner, backup, a tech lead, or a label removal
# (only the run of the ticket that lost its label); every run (the kill
# switch) by the owner, backup or a tech lead. resume (restart after the
# kill switch) by the owner or backup only. The dispatcher must still honour the
# owner's kill switch when the policy fails to load or evaluate: a
# fail-closed policy must never block an emergency stop.

cancelers contains p if {
	some p in owners
}

cancelers contains login if {
	some p in data.config.people.tech_leads
	is_string(p)
	login := lower(p)
}

grant if {
	input.action == "cancel"
	input.scope == "run"
	input.actor.kind == "label_removal"
	is_string(input.run.id)
	input.target.run == input.run.id
}

grant if {
	input.action == "cancel"
	input.scope == "run"
	input.actor.kind == "human"
	actor_login in cancelers
}

grant if {
	input.action == "cancel"
	input.scope == "all"
	input.actor.kind == "human"
	actor_login in cancelers
}

grant if {
	input.action == "resume"
	input.scope == "all"
	input.actor.kind == "human"
	actor_login in owners
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p kod-policy`
Expected: PASS (`api`, `agent_ops`, `human_ops`).

- [ ] **Step 6: Commit**

```bash
git add policy/humans.rego policy/people.json policy/teams.json crates/kod-policy/tests/human_ops.rs
git commit -m "kod-policy: merge with requester_approval, harness changes and cancel"
```

---

### Task 4: Delegation, compliance profile and matrix coverage

**Files:**
- Create: `policy/delegation.rego`, `policy/compliance.rego`, `policy/delegation.json`, `policy/compliance.json`
- Modify: `docs/superpowers/plans/2026-10-14-phase1-overview.md` (row 3)
- Test: `crates/kod-policy/tests/delegation.rs`

**Interfaces:**
- Consumes: Task 1's `Policy` and helpers; `grant` and `deny` from `kod.rego`.
- Produces, as input contracts for sub-plans 6 and 4:
  - `delegate`: `{"ticket": {"repo": "<owner>/<name>", "risk_class": "low"|"medium"|"high", "paths": [..], "acceptance_criteria": true, "bounces": <number>}, "wip_in_progress": <number>}`. `paths` is optional (repo-relative paths the ticket names, an early filter only); `bounces` is the story's count of rework requests. Task 4 also adds the `publish` veto on `changed_paths` (Task 2's contract).
  - `retention_delete`: `{"record_type": "<type>", "age_days": <number>}`.
  - Data: `data.config.delegation.{risk_classes, allowed_classes, default_path_rules[{prefix, class, covers}], repo_path_rules.<repo>[{prefix, class}], wip_limit, bounce_limit}`, `data.config.compliance.profiles.<name>.retention_days.<record_type>`.

- [ ] **Step 1: Write the failing tests**

`crates/kod-policy/tests/delegation.rs`:

```rust
mod common;

use common::{allowed, allowed_by, decide, denied, denied_by, policy_with, set, unset};
use std::collections::BTreeSet;

use serde_json::{Value, json};

/// `known_actions` as policy/kod.rego defines it, read straight from the Rego.
fn known_actions() -> BTreeSet<String> {
    let mut engine = regorus::Engine::new();
    for entry in std::fs::read_dir(common::policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rego") {
            let text = std::fs::read_to_string(&path).unwrap();
            engine.add_policy(path.display().to_string(), text).unwrap();
        }
    }
    let value = engine
        .eval_rule("data.kod.known_actions".to_string())
        .unwrap();
    serde_json::from_str(&value.to_json_str().unwrap()).unwrap()
}

fn delegate() -> Value {
    json!({
        "action": "delegate",
        "ticket": {"repo": "acme/app", "risk_class": "low", "paths": ["docs/readme.md"], "acceptance_criteria": true, "bounces": 0},
        "wip_in_progress": 0
    })
}

// delegate

#[test]
fn low_and_medium_tickets_are_delegated_in_phase_1() {
    allowed(delegate());
    allowed(set(delegate(), "/ticket/risk_class", json!("medium")));
    allowed(unset(delegate(), "/ticket/paths"));
}

#[test]
fn a_high_unknown_or_missing_risk_class_is_refused() {
    denied(
        set(delegate(), "/ticket/risk_class", json!("high")),
        "risk class high is not delegated in this phase",
    );
    denied(
        set(delegate(), "/ticket/risk_class", json!("urgent")),
        "unknown risk class: urgent",
    );
    denied(
        set(delegate(), "/ticket/risk_class", json!("")),
        "unknown risk class",
    );
    denied(
        unset(delegate(), "/ticket/risk_class"),
        "ticket has no risk class",
    );
}

#[test]
fn a_path_rule_raises_the_class() {
    let auth = set(
        delegate(),
        "/ticket/paths",
        json!(["docs/readme.md", "auth/login.rs"]),
    );
    denied(
        auth.clone(),
        "risk class high is not delegated in this phase",
    );
    denied(auth, "path auth/login.rs raises the risk class to high");
    denied(
        set(
            delegate(),
            "/ticket/paths",
            json!([".github/workflows/ci.yml"]),
        ),
        "risk class high is not delegated in this phase",
    );
    denied(
        set(delegate(), "/ticket/paths", json!(["src/auth/login.rs"])),
        "risk class high is not delegated in this phase",
    );
    allowed(set(
        delegate(),
        "/ticket/paths",
        json!(["src/authors.rs", "docs/oauth.md"]),
    ));
}

#[test]
fn every_placeholder_path_rule_raises_to_high() {
    for path in [
        "auth/x",
        "payments/x",
        "crypto/x",
        "pii/x",
        "migrations/x",
        "infra/x",
        ".github/x",
        ".kod/x",
        "policy/x",
        "CODEOWNERS",
        "docs/CODEOWNERS",
    ] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "risk class high is not delegated in this phase",
        );
    }
}

/// policy/delegation.json with `repo_path_rules` replaced.
fn with_repo_rules(rules: Value) -> kod_policy::Policy {
    let text = std::fs::read_to_string(common::policy_dir().join("delegation.json")).unwrap();
    let mut config: Value = serde_json::from_str(&text).unwrap();
    config["repo_path_rules"] = rules;
    policy_with("delegation.json", config)
}

#[test]
fn a_repos_own_path_rules_add_to_the_defaults() {
    let p = with_repo_rules(json!({"acme/app": [{"prefix": "billing/", "class": "high"}]}));
    let billing = set(delegate(), "/ticket/paths", json!(["billing/invoice.rs"]));
    denied_by(
        &p,
        billing.clone(),
        "path billing/invoice.rs raises the risk class to high",
    );
    allowed_by(&p, set(billing, "/ticket/repo", json!("acme/other")));
    denied_by(
        &p,
        set(delegate(), "/ticket/paths", json!(["auth/x"])),
        "risk class high is not delegated in this phase",
    );
    denied_by(
        &p,
        publish(json!(["billing/invoice.rs"])),
        "publish: path billing/invoice.rs raises the risk class to high",
    );
}

#[test]
fn a_ticket_needs_a_repo() {
    denied(unset(delegate(), "/ticket/repo"), "ticket has no repo");
    denied(
        set(delegate(), "/ticket/repo", Value::Null),
        "ticket has no repo",
    );
}

#[test]
fn path_tricks_cannot_dodge_a_path_rule() {
    for path in [
        "./auth/login.rs",
        "Auth/login.rs",
        "AUTH/login.rs",
        "/auth/login.rs",
        "auth//login.rs",
        "./././auth/x",
        "auth\\login.rs",
        " auth/x",
        "src/./auth/x",
    ] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "risk class high is not delegated in this phase",
        );
    }
    for path in ["src/../auth/login.rs", "src/auth/..", "..", "", " ", "./"] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "is not a clean repo-relative path",
        );
    }
    denied(
        set(delegate(), "/ticket/paths", json!([5])),
        "is not a clean repo-relative path",
    );
    denied(
        set(delegate(), "/ticket/paths", json!("auth/login.rs")),
        "ticket.paths must be a list",
    );
}

#[test]
fn admission_needs_criteria_capacity_and_bounces_left() {
    denied(
        set(delegate(), "/ticket/acceptance_criteria", json!(false)),
        "ticket has no acceptance criteria",
    );
    denied(
        unset(delegate(), "/ticket/acceptance_criteria"),
        "ticket has no acceptance criteria",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!(1)),
        "WIP limit reached",
    );
    denied(
        unset(delegate(), "/wip_in_progress"),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!("0")),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/ticket/bounces", json!(2)),
        "bounce limit reached: assign the story to the tech lead",
    );
    denied(
        unset(delegate(), "/ticket/bounces"),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!(-1)),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/ticket/bounces", json!(-3)),
        "does not meet the rule",
    );
}

#[test]
fn a_malformed_ticket_is_a_policy_error_deny() {
    let d = decide(json!({"action": "delegate", "ticket": "KOD-1", "wip_in_progress": 0}));
    assert!(!d.allow, "{d:?}");
    assert!(
        d.reasons
            .iter()
            .any(|r| r.starts_with("policy error") || r.contains("ticket has no risk class")),
        "{d:?}"
    );
}

// retention_delete

#[test]
fn retention_delete_is_refused_while_the_profile_is_a_placeholder() {
    denied(
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 100000}),
        "compliance profile placeholder has no retention_days.run_audit",
    );
}

#[test]
fn the_longest_retention_across_active_profiles_wins() {
    let p = policy_with(
        "compliance.json",
        json!({"profiles": {
            "a": {"retention_days": {"run_audit": 365}},
            "b": {"retention_days": {"run_audit": 2555}}
        }}),
    );
    allowed_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 2555}),
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 400}),
        "does not meet the rule",
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": "9999"}),
        "does not meet the rule",
    );
}

#[test]
fn a_missing_value_in_any_active_profile_denies() {
    let p = policy_with(
        "compliance.json",
        json!({"profiles": {
            "a": {"retention_days": {"run_audit": 365, "prompts": 30}},
            "b": {"retention_days": {"run_audit": 2555}}
        }}),
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "prompts", "age_days": 100000}),
        "compliance profile b has no retention_days.prompts",
    );
    let none = policy_with("compliance.json", json!({"profiles": {}}));
    denied_by(
        &none,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 100000}),
        "no compliance profile is active",
    );
}

// coverage

/// Every row of the spec's action and permission matrix, and every other
/// policy action, with the tests that pin it. Adding an action to
/// `known_actions` in policy/kod.rego means adding it here, with its test.
const ACTIONS: &[(&str, &str)] = &[
    ("local_command", "api::external_rows_are_never_granted"),
    (
        "model_request",
        "agent_ops::model_request_*, a_disallowed_endpoint_is_refused",
    ),
    (
        "subscription_model_request",
        "agent_ops::subscription_*, a_null_or_wrong_workspace_never_matches",
    ),
    (
        "read",
        "agent_ops::reads_are_limited_to_the_ticket_and_its_linked_docs",
    ),
    (
        "tracker_write",
        "agent_ops::tracker_writes_are_limited_to_the_ticket_and_its_linked_requests",
    ),
    (
        "draft_doc_write",
        "agent_ops::draft_doc_writes_are_limited_to_the_runs_own_draft",
    ),
    ("publish", "agent_ops::publish_*"),
    (
        "merge",
        "human_ops::not_sole_*, allowed_*, forbidden_*, agent_approvals_block_the_merge",
    ),
    (
        "harness_change",
        "human_ops::a_harness_change_needs_the_owner_and_the_backup_once_named",
    ),
    (
        "cancel",
        "human_ops::who_may_cancel_a_run_or_pull_the_kill_switch",
    ),
    (
        "resume",
        "human_ops::only_the_owner_or_backup_restarts_after_the_kill_switch",
    ),
    ("sign_release", "api::external_rows_are_never_granted"),
    ("delegate", "delegation::*"),
    (
        "retention_delete",
        "delegation::retention_*, a_missing_value_in_any_active_profile_denies",
    ),
];

#[test]
fn every_action_is_known_and_denies_an_empty_request() {
    let listed: BTreeSet<String> = ACTIONS.iter().map(|(a, _)| a.to_string()).collect();
    assert_eq!(
        listed,
        known_actions(),
        "ACTIONS must match known_actions in policy/kod.rego"
    );
    for (action, _) in ACTIONS {
        let d = decide(json!({"action": action}));
        assert!(!d.allow, "{action}: {d:?}");
        assert!(
            !d.reasons.iter().any(|r| r == "unknown action"),
            "{action}: {d:?}"
        );
    }
}

// publish: the diff's own paths, whatever the ticket said

fn publish(changed: Value) -> Value {
    json!({
        "action": "publish", "actor": {"kind": "agent"},
        "run": {"id": "r1", "ticket": "KOD-1", "repo": "acme/app", "canceled": false, "data_classes": ["placeholder"]},
        "target": {"repo": "acme/app", "branch": "agent/KOD-1-r1", "draft": true},
        "changed_paths": changed
    })
}

#[test]
fn publish_refuses_a_diff_that_touches_a_high_risk_path() {
    allowed(publish(json!(["src/lib.rs"])));
    for path in [
        "auth/login.rs",
        "./AUTH/login.rs",
        "src/auth/login.rs",
        ".github/workflows/ci.yml",
    ] {
        denied(
            publish(json!(["src/lib.rs", path])),
            "raises the risk class to high",
        );
    }
    denied(
        publish(json!(["src/../auth/x.rs"])),
        "is not a clean repo-relative path",
    );
    denied(
        publish(json!("auth/x.rs")),
        "publish needs changed_paths as a list",
    );
    let mut missing = publish(json!([]));
    missing.as_object_mut().unwrap().remove("changed_paths");
    denied(missing, "publish needs changed_paths as a list");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kod-policy --test delegation`
Expected: FAIL. No `delegate` or `retention_delete` grant exists yet, and the specific reasons don't exist (`every_action_is_known_and_denies_an_empty_request` already passes, from Task 1's `known_actions`).

- [ ] **Step 3: Write the data files**

`policy/delegation.json`:

```json
{
  "risk_classes": {
    "low": 0,
    "medium": 1,
    "high": 2
  },
  "allowed_classes": [
    "low",
    "medium"
  ],
  "default_path_rules": [
    {
      "prefix": "auth/",
      "class": "high",
      "covers": "auth"
    },
    {
      "prefix": "payments/",
      "class": "high",
      "covers": "payments"
    },
    {
      "prefix": "crypto/",
      "class": "high",
      "covers": "crypto"
    },
    {
      "prefix": "pii/",
      "class": "high",
      "covers": "personal or regulated data"
    },
    {
      "prefix": "migrations/",
      "class": "high",
      "covers": "data migrations"
    },
    {
      "prefix": "infra/",
      "class": "high",
      "covers": "infrastructure"
    },
    {
      "prefix": ".github/",
      "class": "high",
      "covers": "CI"
    },
    {
      "prefix": ".kod/",
      "class": "high",
      "covers": "the harness itself (its config in a product repo)"
    },
    {
      "prefix": "policy/",
      "class": "high",
      "covers": "policy"
    },
    {
      "prefix": "codeowners",
      "class": "high",
      "covers": "CODEOWNERS"
    }
  ],
  "repo_path_rules": {},
  "wip_limit": 1,
  "bounce_limit": 2
}
```

`policy/compliance.json` (one placeholder profile with no values, so every rule that reads one denies):

```json
{
  "profiles": {
    "placeholder": {}
  }
}
```

- [ ] **Step 4: Write the delegation rules**

`policy/delegation.rego`:

```rego
# Delegation policy (spec, "Configurable policies"): which tickets the agent
# may take. Every ticket carries a risk class; path rules can only raise it.
# Caller: sub-plan 6 (dispatcher), which also makes admission atomic.
package kod

risk := data.config.delegation.risk_classes

ticket_paths := object.get(object.get(input, "ticket", {}), "paths", [])

# The defaults hold for every repo; a repo's own rules (set in config, per
# repo) add to them. Rules only raise the class, so adding is always safe.
rules_for(repo) := array.concat(
	data.config.delegation.default_path_rules,
	object.get(data.config.delegation.repo_path_rules, repo, []),
)

ticket_repo := object.get(object.get(input, "ticket", {}), "repo", null)

run_repo := object.get(object.get(input, "run", {}), "repo", null)

skip_segment(seg) if seg in {"", "."}

# Normalised for matching: trimmed, lower-cased, backslashes read as
# slashes, empty and "." segments dropped. Only for strings.
path_segments(p) := [seg |
	some seg in split(replace(lower(trim_space(p)), "\\", "/"), "/")
	not skip_segment(seg)
] if is_string(p)

clean_path(p) := concat("/", path_segments(p))

# Refused outright: not a string, empty, or with a ".." segment.
bad_path(p) if not is_string(p)

bad_path(p) if clean_path(p) == ""

bad_path(p) if ".." in path_segments(p)

# A rule matches at the top of the repo or below any directory, so
# src/auth/x is raised like auth/x.
path_matches(cp, prefix) if startswith(cp, lower(prefix))

path_matches(cp, prefix) if contains(cp, concat("", ["/", lower(prefix)]))

bad_paths_in(paths) := {p | some p in paths; bad_path(p)}

ranks_in(paths, rules) := {risk[r.class] |
	some p in paths
	some r in rules
	path_matches(clean_path(p), r.prefix)
}

class_allowed(c) if c in data.config.delegation.allowed_classes

bad_paths := bad_paths_in(ticket_paths) if is_array(ticket_paths)

raised_ranks := ranks_in(ticket_paths, rules_for(ticket_repo)) if is_array(ticket_paths)

effective_rank := max(raised_ranks | {risk[input.ticket.risk_class]})

effective_class := c if {
	some c, rank in risk
	rank == effective_rank
}

class_delegated if class_allowed(effective_class)

has_criteria if input.ticket.acceptance_criteria == true

grant if {
	input.action == "delegate"
	is_string(input.ticket.repo)
	is_array(ticket_paths)
	count(bad_paths) == 0
	class_delegated
	has_criteria
	is_number(input.wip_in_progress)
	input.wip_in_progress >= 0
	input.wip_in_progress < data.config.delegation.wip_limit
	is_number(input.ticket.bounces)
	input.ticket.bounces >= 0
	input.ticket.bounces < data.config.delegation.bounce_limit
}

deny contains "ticket has no repo" if {
	input.action == "delegate"
	not is_string(ticket_repo)
}

deny contains "ticket has no risk class" if {
	input.action == "delegate"
	not input.ticket.risk_class
}

deny contains sprintf("unknown risk class: %v", [input.ticket.risk_class]) if {
	input.action == "delegate"
	input.ticket.risk_class
	not risk[input.ticket.risk_class]
}

deny contains sprintf("risk class %v is not delegated in this phase", [effective_class]) if {
	input.action == "delegate"
	effective_class
	not class_delegated
}

deny contains sprintf("path %v raises the risk class to %v", [p, r.class]) if {
	input.action == "delegate"
	not class_delegated
	is_array(ticket_paths)
	some p in ticket_paths
	some r in rules_for(ticket_repo)
	path_matches(clean_path(p), r.prefix)
	risk[r.class] > risk[input.ticket.risk_class]
}

deny contains "ticket.paths must be a list" if {
	input.action == "delegate"
	not is_array(ticket_paths)
}

deny contains sprintf("path %v is not a clean repo-relative path", [p]) if {
	input.action == "delegate"
	some p in bad_paths
}

deny contains "ticket has no acceptance criteria" if {
	input.action == "delegate"
	not has_criteria
}

deny contains sprintf("WIP limit reached (%v)", [data.config.delegation.wip_limit]) if {
	input.action == "delegate"
	is_number(input.wip_in_progress)
	input.wip_in_progress >= data.config.delegation.wip_limit
}

deny contains "bounce limit reached: assign the story to the tech lead" if {
	input.action == "delegate"
	is_number(input.ticket.bounces)
	input.ticket.bounces >= data.config.delegation.bounce_limit
}

# publish: the diff's own paths decide, whatever the ticket said. A change
# that touches a path whose class isn't delegated is refused. Sub-plan 5
# passes the paths of the diff it is about to push.

deny contains "publish needs changed_paths as a list" if {
	input.action == "publish"
	not is_array(input.changed_paths)
}

deny contains sprintf("path %v is not a clean repo-relative path", [p]) if {
	input.action == "publish"
	is_array(input.changed_paths)
	some p in bad_paths_in(input.changed_paths)
}

deny contains sprintf("publish: path %v raises the risk class to %v", [p, r.class]) if {
	input.action == "publish"
	is_array(input.changed_paths)
	some p in input.changed_paths
	some r in rules_for(run_repo)
	path_matches(clean_path(p), r.prefix)
	not class_allowed(r.class)
}
```

Note on `"ticket": "KOD-1"` (`a_malformed_ticket_is_a_policy_error_deny`): `object.get("KOD-1", "paths", [])` is a builtin type error, which regorus reports as an evaluation error (strict builtin errors are on by default), so `decide` returns a `policy error` deny. The test also accepts a plain deny, in case a future regorus treats it as undefined instead; either way it must not allow.

- [ ] **Step 5: Write the compliance rules**

`policy/compliance.rego`:

```rego
# Compliance profile (spec, "Configurable policies"). Several profiles can be
# active and the strictest wins: for retention, the longest period. A rule
# that reads a missing value denies. Caller: sub-plan 4 (audit store).
package kod

profiles := data.config.compliance.profiles

retention_known if {
	count(profiles) > 0
	every _, p in profiles {
		is_number(p.retention_days[input.record_type])
	}
}

grant if {
	input.action == "retention_delete"
	retention_known
	is_number(input.age_days)
	input.age_days >= max({p.retention_days[input.record_type] | some p in profiles})
}

deny contains "no compliance profile is active" if {
	input.action == "retention_delete"
	count(profiles) == 0
}

deny contains sprintf("compliance profile %v has no retention_days.%v", [name, input.record_type]) if {
	input.action == "retention_delete"
	some name, p in profiles
	not is_number(p.retention_days[input.record_type])
}
```

- [ ] **Step 6: Link the plan from the overview**

In `docs/superpowers/plans/2026-10-14-phase1-overview.md`, change the start of row 3 from:

```markdown
| 3 | **Policy** (Rego with regorus, in-process) | None |
```

to:

```markdown
| 3 | **Policy** (Rego with regorus, in-process). Full plan: `2026-10-15-phase1-policy.md` | None |
```

(Skip this step if the plan PR already made the change.)

- [ ] **Step 7: Run the whole suite and the CI checks**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all pass. `kod-policy` runs `api` (10), `agent_ops` (15), `human_ops` (10) and `delegation` (14) tests.

- [ ] **Step 8: Commit**

```bash
git add policy/delegation.rego policy/compliance.rego policy/delegation.json policy/compliance.json crates/kod-policy/tests/delegation.rs docs/superpowers/plans/2026-10-14-phase1-overview.md
git commit -m "kod-policy: delegation policy, compliance profile placeholder and matrix coverage"
```

---

## Whole-branch review

After Task 4, one fresh reviewer reads the whole branch against the spec sections listed under **Spec** and against this plan's Decisions and Review Focus. The reviewer checks in particular:

- Every `grant` body states all its conditions positively, and every equality between two input values has an `is_string` (or `is_number`) guard on one side.
- No caller-facing path can produce `allow: true` from an error, a missing field or a placeholder value.
- `Cargo.lock` adds regorus `0.13.0` and only the crates listed in D2 and sha2's tree, and CI's cargo-deny step passes.

---

## Self-review

- **Spec coverage.**
  - Action and permission matrix: all 11 rows have an action and tests (Tasks 1 to 3), including the vendor-login row (Task 2). The two rows enforced elsewhere are always denied with the enforcer named (D6). The Task 4 coverage test compares its list with `data.kod.known_actions`.
  - "An operation not listed here is refused": `unknown action` and default deny (Task 1).
  - Model routing: unknown or missing class refused, one placeholder class and endpoint (Task 2, D8).
  - Delegation policy: risk classes, phase 1 eligibility, a ticket with no class refused, bounce limit, WIP placeholder (Task 4, D10). "A change that touches auth/ is high, whatever the ticket says" is enforced on the diff at publish (`changed_paths`); the ticket's own paths are only an early filter at delegate time.
  - Requester approval: `allowed`, `not_sole` (default), `forbidden`, and agents never approve (Task 3, D11).
  - Compliance profile: a placeholder, missing values deny, strictest wins (Task 4, D12).
  - Credential exception and eligibility check: offline contexts only, business plans, recorded workspace (Task 2, D13).
  - Ownership: owner and backup for harness changes and restart; owner, backup and tech leads for the kill switch (Task 3, D14).
  - Matrix rows for `delegate` and `retention_delete` (Task 4, D7), each with tests.
  - Policy version as a content hash for the audit log (Task 1, D4). regorus pinned and its license checked (Task 1, D2).
- **Placeholders.** The only placeholder values are the configuration ones the scope asks for (`placeholder` class, endpoint and profile, no recorded workspace, `backup: null`, `tech_leads: []`, `teams: {}`), and each is pinned by a test. No plan step is left open.
- **Type consistency.**
  - `Policy::load(&Path) -> anyhow::Result<Policy>`, `version(&self) -> &str` and `decide(&self, &Value) -> Decision` match between Task 1's code, its Interfaces block and `tests/common/mod.rs`.
  - The helpers `allowed`, `denied`, `allowed_by`, `denied_by`, `policy_with`, `set` and `unset` are defined in Task 1 and used with the same signatures in Tasks 2 to 4.
  - Data file stems (`routing`, `subscription`, `publish`, `people`, `teams`, `delegation`, `compliance`) match the `data.config.<stem>` paths in the Rego. Data can't shadow a rule: `data_files_cannot_override_rules` (Task 1).
  - Reason strings asserted in the tests appear verbatim in the Rego: for example "the run has no data class", "no approved workspace is recorded for runner", "requester_approval is not_sole: the requester can't be the only approver", "compliance profile %v has no retention_days.%v".
  - Rules defined more than once (`grant`, `own_or_linked`, `bad_path`, `owners`, `cancelers`) are boolean, function or partial-set rules. Single-value rules are defined once; `reasons` uses an `else` chain with bodies.
- **Review Focus.** Each of the seven lines names tests in its owning task. The action-coverage test (Task 4) evaluates `data.kod.known_actions` with regorus and compares it with its `ACTIONS` list, so an action added to the Rego without a test fails it.

## Open items

- **Compliance profile values**, **routing values** (data classes and endpoints), **team settings** for `requester_approval`, **tech leads**, the **backup owner** and the **real path rules** per product repo (`repo_path_rules`; the defaults are placeholders): each is a data-file change, reviewed like code. Until then the rules that read them deny or use the spec's defaults.
- **Harness changes before the backup is named** (D14): the rule needs the owner's approval, and GitHub doesn't let an author approve their own PR, so an owner-authored harness change can't pass `harness_change` until a second named approver exists.
- **Approved seat workspaces** for subscription mode (D13): record them once the spike's eligibility check is done.
