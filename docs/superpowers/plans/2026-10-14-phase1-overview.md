# Phase 1 (Team MVP) Overview

**Spec:** `docs/kod-harness-design.md`, "Roadmap → 1 · Team MVP". Phase 1 builds our Rust agent loop, the dispatcher, both gateways, policy, the audit log, the gate runner, tracing, and the workflow status comment and CLI. Our loop becomes the default runner once it matches the phase 0 baseline on the eval set; until then Claude Code keeps running. All 6 engineers delegate, with the WIP limit set to match review capacity.

**Phase 1 exit (from the spec):** our loop matches the baseline, with no escapes. In this plan that means:

- **Matches:** `kod-eval compare <claude-code held-out run> <kod-loop held-out run>` passes. It needs the same cases, every case verified in both runs, and no slice (split × language × risk) where kod-loop passes fewer cases than the baseline. Any tolerance is the owner's call (see Open questions).
- **No escapes:** zero `unsafe` and zero `not_verified` cases, in every eval run and in every conformance check, on every harness change. Any permission escape, unauthorized action or data leak blocks the change (spec, "Evals and metrics").

Each sub-plan is written in full only when the one before it is merged, so it can build on code that exists. Every sub-plan follows ADR 0001 (an execution mode per task) and ends with a whole-branch review. Every change goes through a PR. Harness changes need two approvals once the backup owner is named.

## Sub-plans

| # | Sub-plan | Depends on | Blocked by open decisions | Exit criteria |
|---|---|---|---|---|
| 1 | **Agent loop** (`kod-loop`): our Rust loop as a runner under `kod-eval`, plus `kod-eval compare`. Full plan: `2026-10-14-phase1-agent-loop.md` | Phase 0 code (merged). The comparison step needs the phase 0 baseline. | None to build it. The comparison waits on the **phase 0 baseline** (cases and runs not done yet). | Conformance checks that apply offline pass in the sandbox: AGENTS.md rule, egress blocked, protected path caught, model traffic only through the proxy, right end state for block and over-budget. The full eval set runs with zero unsafe and zero not verified. `compare` against the baseline is recorded. |
| 2 | **LLM gateway hardening** (`kod-llm-proxy` → LLM gateway): a long-running service in place of one proxy per case. Per-run budgets (tokens and cost, priced from a table), model routing policy per data class (an unknown class is refused), redacted prompt and output capture handed to the audit log, OpenAI-compatible route only if a runner needs it. | 1 (the loop is its first client). 4 for audit capture. | **Routing values** (data classes, endpoint list). Until then, one class and one endpoint, and anything else is refused. | Routing table tests: an unknown class is refused, a disallowed endpoint is refused. Budget tests. Spend can't exceed the budget whether the loop or a script calls it. Eval rerun shows no regression. |
| 3 | **Policy** (Rego with regorus, in-process) | None | **Compliance profile values** (only for the rules that read them) | Policy repo with tests for every row of the action and permission matrix. Anything not listed is denied. The same rules are used by 2, 5 and 6. |
| 4 | **Audit log**: append-only hash-chained table, external write-once checkpoints under a separate account, retention from the compliance profile, secret redaction before storage. | 3 (decisions are recorded with their policy version) | **Compliance profile values** (retention). Build with a placeholder profile that refuses to delete anything. **Backup owner** for checkpoint account access. | Tamper, truncation and store-outage tests pass (spec "Audit integrity"). When the audit write fails, the external write is refused. |
| 5 | **Tool gateway and publish**: an MCP endpoint (rmcp) with our tracker and docs tools, a token broker, and the publish operation (push `agent/<ticket>-<run>` plus a draft PR, idempotent, refused for a canceled run). Adds an MCP client to `kod-loop`. Runs in the same process as the LLM gateway at first. | 2, 3, 4, and 6's adapter interface | **First issue tracker** (only for the tracker tools; publish to GitHub doesn't need it) | Conformance: a tool call goes through the gateway MCP endpoint, a planted secret can't be read and the attempt is in the audit log, the PR carries the story ID and agent trailers. The publish matrix row has a test for each refusal. |
| 6 | **Dispatcher and tracker adapter**: a webhook service with a database table as its queue. Signature check, delivery dedup, atomic WIP admission, forward-only run states, cancel on label removal or ticket edit, kill switch. One adapter with a contract test against a recorded webhook. | 3, 4. 5 to start real runs. | **First issue tracker**. **Delegation policy** WIP value (from review capacity). | Contract test passes. Admission refusals are commented with the reason. A duplicate delivery never starts a second run. The kill switch drill on live runs is recorded. |
| 7 | **Gate runner and required checks**: our gate runner on trusted hosts (tests, SAST, dependency and secret scans), plus `inputs-current`, `requester-check` and `protected-paths` as GitHub status checks, and the GitHub rules (dismiss stale approvals, CODEOWNERS, no auto-merge, no bypass). | 4. 6's adapter for `requester-check`. | **First issue tracker** (requester lookup). **Compliance profile** (evidence export). | Each check fails on a seeded bad PR. An admin bypass shows up as an audit event. Gate runners share no host or cache with sandboxes. |
| 8 | **Workflow status**: one status comment per story plus `kod status <story>`, computed on read from existing records. | 4, 6, 7 | **First issue tracker** | The view matches the records for a seeded story with a stale PR and an open rework request. It has no store and no actions. |
| 9 | **Upstream doc drafting**: skills and templates for brief, design, threat model and test plan, Google Docs tools behind the gateway (Draft folder edit, Approved read), snapshot on approval, staleness marking. | 5, 6, 7 (`inputs-current`) | **Google Workspace checks** (Approvals API, data regions, audit log export). **First issue tracker** (sign-off fallback). | A draft round trip in a sandbox drive. The approval snapshot is stored with its revision ID. A new approved version marks linked PRs stale. Rejected drafts go into the eval set. |

**Order:** 1, then 3 and 4, then 2, then 6 and 5, then 7, then 8 and 9. Sub-plan 1 comes first because the roadmap gates the default runner on it, it needs nothing undecided, and every later piece reuses its runner contract. Tracing starts as structured logs in sub-plans 1 and 2 (spec: "structured logs first") and gets its own plan only when logs stop being enough.

**Cutover:** our loop becomes the default runner only when sub-plan 1's comparison passes on the held-out split, and only after 5 has added the MCP client, so the loop also passes the conformance checks that need the gateway. Claude Code stays available as a conforming runner for comparison and fallback.

## Open decisions that block phase 1

| Decision | Blocks |
|---|---|
| Phase 0 baseline (cases and both runs) | The comparison in 1 and the phase exit |
| First issue tracker | 5 (tracker tools), 6, 7 (`requester-check`), 8, 9 |
| Compliance profile values | 3 (rules that read them), 4 (retention), 7 (evidence export) |
| Model routing values | 2 (it refuses everything except one class and one endpoint until then) |
| Backup owner | Two-approval merges for harness changes, checkpoint account access in 4, kill-switch cover |
| Match tolerance for `compare` | Phase exit (the default is strict: no slice may be worse) |
