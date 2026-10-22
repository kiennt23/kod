# Phase 1.7: gate runner and required checks

Goal: trusted, head-bound checks for tests, SAST, dependencies, secrets,
current inputs, requester approval and protected paths. Depends on the
merged audit log and tracker contract; acceptance uses the fake tracker.

## Boundaries and decisions

- Reuse `Policy` for `merge` and `harness_change`. Read the requester from
  `Tracker`, never PR prose. Only the latest effective human review for the
  current head counts; a comment does not revoke approval, dismissal does.
- The host supplies the run's immutable evidence and the approved artifact
  registry. These are adapter contracts, not files the PR can author. Missing
  required pins, unknown revisions, changed policy or a mismatched head fail.
- Fetch PR metadata, reviews and changed paths through GitHub, including
  rename sources and every page. Report GitHub check runs from a dedicated
  GitHub App. Rules pin that App's ID, with no bypass actors.
- A trusted host downloads the exact head's source archive. Run each scan
  in a fresh, unprivileged, networkless container with a read-only root and
  archive, no credentials, host sockets, host workspace or shared writable cache.
  The gate daemon must carry the exclusive `kod.role=gate` Docker label.
  Source extraction and all repository execution happen inside the container.
- Scanner image digests, commands, policy, owners and evidence are trusted
  deployment inputs. Per-candidate preparation refreshes locked dependencies
  and advisories through a package-only network, without running source code;
  scans consume the prepared cache read-only and stay offline. Rust SAST is clippy.
- Re-evaluate on PR/review updates and artifact approval, and on merge-group
  candidates. An adapter must serialize evaluation/invalidation per PR.
  Merge candidates use the PR head for approvals and the candidate SHA for
  scans and check results, with trusted constituent PR bindings.
- Audit intent before every external check update and record results. Admin
  bypass observations are recorded using their delivery ID before advancing
  an external ingestion cursor. Compliance export remains owner-configured;
  no invented retention or export destination.

## Execution

| Task | Mode | Verification |
| --- | --- | --- |
| Core checks and audit | Native | Seeded PRs against tracker fake, stale/dismissed/bot reviews, missing pins, audit outage |
| GitHub adapter and rules | Native | HTTP contract tests, pagination/truncation, head changes, App binding |
| Isolated scan runner | Native | Real Docker happy/failure/timeout, no credentials or writable host mount |
| CLI and deployment runbook | Native | Actual CLI invocations, workspace tests, clippy, fmt, deny; whole-branch self-review |

ADR 0001 proposes subagent review for safety work. This implementation uses
the session's native execution and self-review workflow; deployment acceptance
still requires a whole-branch review and the host/rules activation checks.

## Acceptance and rollout

Each named check must reject its seeded bad PR. Errors never become a pass.
An audit failure prevents GitHub updates. A changed head invalidates results.
No scan host or writable cache is shared with agent sandboxes. Rules require
CODEOWNERS, fresh approvals and merge queue rechecks, with auto-merge off.
The branch supplies deployment configuration and commands; changing live
GitHub rules, installing hosts/App credentials and choosing compliance export
are explicit deployment steps, not side effects of building this crate.
The shipped profile is an inactive example and generated rules are disabled.
The owner-authored approval conflict must be decided before activation.
