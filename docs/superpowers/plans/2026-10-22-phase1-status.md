# Phase 1.8: workflow status

Depends on merged sub-plans 4, 6 and 7. Implements the harness design's
workflow-status section: one computed story view, printed by `kod status`
and maintained as one tracker comment. Acceptance uses the 6a fake; live
tracker and event integration remain 6c/6b work.

## Execution

| Task | Mode | Reason | Acceptance |
|---|---|---|---|
| Source records and computed view | Native | Information projection, no policy or workflow decisions | Approved revisions, current-head gates, ticket/run states and rework combine correctly; unavailable stages remain unknown |
| Tracker status comment | Native | Reuses audited comment writes; adds no sandbox capability | One Kod-owned comment survives repeated and concurrent updates; human comments survive; audit outage prevents the write |
| CLI and host reader | Native | Read-only adapter over existing dispatcher records | CLI matches the comment renderer, rereads source records, rejects missing/corrupt sources and never creates a dispatcher DB |
| Whole-branch review and QA | Native | Self-review with executable evidence | Targeted and workspace tests, clippy, formatting, CLI execution and diff review |

## Interfaces

- `kod-status` exports `compute`, `load` and `refresh`, plus typed source
  records. Its binary is `kod`, with `status <story> --records <file>
  --dispatcher <db>` and optional `--json`.
- `Source::load` supplies the story, linked tickets, approval/draft records,
  immutable publish evidence, PR head/state/check records and linked rework.
  Runs use the existing dispatcher's `RunRow` and `RunState`.
- `HostRecords` rereads the host's atomically replaced source-record snapshot
  and opens the existing dispatcher DB read-only. It creates no schema,
  materialized view or status store.
- `Tracker::status_comment` updates a single adapter-owned comment. Its
  default is an explicit unsupported error, so older/read-only adapters
  cannot silently append duplicates. The fake implements the operation.
- `refresh` loads and renders the view, verifies the target story exists,
  records an intent with the exact text before the write, then records the
  posted/failed outcome linked to that intent. The trusted host serializes the entire operation
  with other refreshes and record replacement for that story.

## Computation rules

Only an approved revision supersedes a PR pin. A newer draft puts the doc
stage in progress/review, without invalidating the prior approval. Missing
required approvals, changed pins or a changed PR head mark an open PR stale.
Only checks for the current head count; missing checks remain unknown.
The adapter supplies the effective check generation, not all check history.
The required set uses only the story's approved links and is checked with
the same helper as `inputs-current`.

Ticket state and the numerically latest run determine activity; an ended
agent run does not prove that its PR merged. All recorded runs and PRs are
listed. Merged PRs complete their ticket and supply historical Build/Test
results, while newer runs and Ready transitions preserve reopened work.
Ready tickets are not overridden by historical run outcomes. Open amendments
and replans use their own workflow state without hiding staleness; replans
must target Plan. Questions are shown without rewinding a stage;
closed rework does not rewind anything. Unknown stages are not reported as
approved. The view grants no permission and cannot substitute for gates.

## Boundary and verification

This step builds the projection, CLI and comment integration hook. It does
not install an event ingester, poll external services or implement a real
tracker adapter. 6b must wire refresh after relevant committed events and
reconcile uncertain comment creates using the adapter's authenticated author
and stable comment identity. It must enforce visibility before assembling
source records or publishing a comment; the CLI is for trusted host operators.
Document tools in sub-plan 9 will supply document lifecycle records.

Tests cover the seeded stale-PR/open-rework story, same text on both surfaces,
fresh reads, current-head checks, approval/draft transitions, questions and
replans, numeric run ordering, malformed/cross-story records, redaction,
audit failure, read-only DB behavior and concurrent comment upserts.
See `docs/status-runbook.md` for operation and the final validation evidence.

## Implementation verification

- `cargo test --all`: 510 passed, zero failures; existing Docker tests kept
  their normal opt-in behavior (`KOD_DOCKER_TESTS` unset).
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all --check` and
  `git diff --check`: passed.
- Executed the `kod status` binary in text and JSON modes against a seeded
  story and SQLite run records. It showed PR #91 pinned to design-v3 as stale
  against approved design-v4, plus amendment REWORK-320 at its own workflow
  state. Missing stage/check records showed unknown.
- CLI tests compare the actual process output with the shared comment
  renderer and verify fresh reads and unchanged dispatcher DB contents.
  Comment tests verify repeated and concurrent upserts preserve human text,
  while audit failure prevents a write.
- Whole-branch self-review covered the source/permission boundary, effective
  check generations, numeric run ordering, PR history, aggregation and
  redaction. No blocking finding remains within this offline acceptance scope.
- No live tracker/App integration, Docker opt-in suite, deployment or held-out
  model eval was run. This projection changes no runner, model or gate policy;
  connected acceptance remains 6b/6c.

## Review corrections

The status view and gate now share pin validation. Merged PRs contribute
Build/Test results and completion without masking a newer run or Ready
transition. Rework uses its recorded state, preserves staleness, and rejects
replans outside Plan. Free-text punctuation is neutralized to prevent URL
autolinks and tracker markup. Comment audit records distinguish intent,
posted and failed outcomes, including redacted errors and explicit outcome
recording failures. SQL filters runs before decoding them, using a shared
story/ticket validator.

Twelve regression cases failed before the fixes. The final workspace run
passes all 510 tests, including audit outcome failures and an unrelated
malformed run row that must never be read for this story. Whole-branch
self-review covered the shared gate helper, parameter binding, outcome
ordering, conservative status aggregation and retained integration boundary.
