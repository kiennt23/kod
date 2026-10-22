# Workflow status

Build and install the operator CLI:

```sh
cargo install --path crates/kod-status --locked
kod status STORY-142 --records /var/lib/kod/stories/STORY-142.json \
  --dispatcher /var/lib/kod/dispatcher.db
kod status STORY-142 --records /var/lib/kod/stories/STORY-142.json \
  --dispatcher /var/lib/kod/dispatcher.db --json
```

Both formats compute status on every read. The text is also the exact body
used by the status-comment updater. The CLI has no write commands and opens
the dispatcher database read-only. Missing files, corrupt records, another
story's snapshot and invalid associations are errors, not an empty success.

## Source records

The trusted host owns the snapshot outside every sandbox/PR workspace.
The source schema is `kod_status::records::Snapshot`:

| Field | Source |
|---|---|
| `story` | Standard tracker `Issue` (key, title, requester, workflow state, required approved links, linked issue keys) |
| `tickets` | Standard tracker issues linked to the story |
| `artifacts` | Approval registry records: link, upstream stage (`requirements`, `plan`, `design`), approved revision, optional draft revision, whether the draft is in review |
| `pulls` | Ticket key, immutable `kod_gates::Evidence`, current head, state (`open`, `closed`, `merged`), draft flag, and effective checks tagged with their evaluated SHA |
| `rework` | Linked issue key, kind (`question`, `amendment`, `replan`), target stage and standard workflow state |

The immutable revisions in `Evidence.pins` must use the same representation
as `artifacts[].approved`, matching the existing gates contract. The source
supplies each effective check once per head/name; duplicate generations are
rejected rather than choosing an arbitrary success. PR checks against older
heads remain historical and cannot turn the current head green.

The reader obtains runs from the existing dispatcher DB, scoped to the
story and its listed tickets. It does not need a running dispatcher or
its policy/credentials, and never creates an absent database. The latest
run is chosen by numeric sequence; run history remains visible. A completed
agent run is displayed as `done`, while its ticket still awaits PR review.

An absent stage is `unknown`; this means no authoritative activity was
provided. In particular release, deploy and operate stay unknown until
their source records are integrated. Approval and check statuses are
informational and do not authorize a transition or merge.

## Updating the tracker comment

The trusted integration invokes
`kod_status::refresh(source, tracker, audit, story)` after relevant records
commit. It uses the same loader and renderer as the CLI. Audit failure
prevents a comment write. A tracker failure is returned so the caller can
retry; this module has no queue or status database.

Serialize refreshes and snapshot replacement per story from the initial
read through the final tracker write. Do not let concurrent workers publish
an older view after a newer one. Atomically replace source snapshots and
retry after failed source reads; never replace a good comment with an error
or partial view.

The tracker adapter implements `status_comment` with a stable Kod-owned
comment identity. It must preserve human comments, reconcile uncertain
creates before retrying, and serialize creation/update per story. Never
discover ownership from a text marker alone. The fake demonstrates the
contract; older adapters return an explicit unsupported error.

The operator and comment readers may only receive records they can already
see in both the tracker and GitHub. Keep snapshot/DB access restricted to
the trusted host operators. Before posting a shared story comment, the real
integration must ensure that every included record is visible to that
comment's audience. Do not copy private data from a broader App identity
into a less restricted issue. Secrets and terminal/Markdown controls in
free-text fields are removed from rendered text; source access controls
remain necessary.

## Acceptance

```sh
cargo test -p kod-status -p kod-tracker
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Connected acceptance belongs to 6c/6b: real source reads, permission scoping,
effective-check selection, event-triggered refresh and recovery of uncertain
comment creates. No live tracker comments are posted by the offline tests.
The implementation's recorded checks and CLI QA are in
`docs/superpowers/plans/2026-10-22-phase1-status.md`.
