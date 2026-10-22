# Phase 1.6c: GitHub Issues adapter

The owner selected GitHub Issues as the first tracker. This step implements
the 6a `Tracker` interface against GitHub.com and supplies its registry mapping.
The dispatcher/gateway connection and live delegation remain sub-plan 6b.

## Execution and acceptance

Implementation runs Native with a whole-branch self-review and recorded
protocol/HTTP tests. No independent review or live write acceptance is claimed.

| Task | Scope | Evidence |
|---|---|---|
| Mapping and issue fields | Native open/closed states, three workflow labels, issue-body metadata, duplicate-key rejection | Mapping tests and HTTP issue round trips |
| Writes and identity | Repository-bound issue operations, native sub-issue links, writer membership, owned status comments | Shared contract against the HTTP sandbox; missing-issue and partial-write tests |
| Webhook trust boundary | HMAC-SHA256 before parsing, payload-derived delivery identity and classification, scoped repository, no parser-side dedup | Pinned upstream webhook fixtures and adversarial cases |
| Documentation and review | Registry file, operator runbook, roadmap choice, local and live read-only QA | Workspace tests, clippy, formatting, diff review and the read-only example |

## Decisions

- Add `kod_tracker::github::GitHub`, implementing every `Tracker` operation
  including the status-comment extension. Keep the core tracker-neutral.
- `Config` binds one repository and the stable numeric ID of the dedicated
  App's bot. Token and webhook secret are passed separately and never stored
  in registry files or rendered errors. API origin is fixed in production,
  redirects are disabled, and HTTP failures never dump response bodies.
- Use canonical issue keys `owner/repo#number`; reject foreign repositories,
  noncanonical numbers and PRs masquerading as issues. PR references may
  point to another product repository, but are canonical GitHub HTTPS URLs.
  The gateway retains authorization of the product repo.
- `registry/trackers/github.json` maps Ready to `open`, Done to `closed`, and
  the middle states to configured workflow labels. Transitions add/remove
  only mapped workflow labels, preserve unrelated labels, and verify the
  resulting state. Ambiguous workflow labels fail rather than guessing.
- `team_group=repository-writers` means current human collaborators with
  effective write/admin permission. Read/triage-only accounts and bots do not
  count. Other group modes are rejected, never widened silently.
- Use one documented `kod:issue` JSON metadata block for requirement ID,
  risk class, acceptance criteria, approved links and explicit issue links.
  Rework stores all original fields and kind in the same block, links back
  to the story, and is added as a native sub-issue. Partial failures identify
  the created issue when known; callers reconcile before retrying creates.
- The shared contract now checks every rework field and the back-link, with
  negative cases for dropped data and every missing-issue operation. Mapping
  and metadata parsing reject duplicate JSON keys at any depth. This covers
  the relevant contract gaps from issue #19; it does not claim that every
  historical backlog item is closed.
- GitHub authenticates the raw body, not the event/delivery headers. Derive
  event classification and a SHA-256 delivery ID from authenticated payload
  content. Replays parse identically; the dispatcher owns durable dedup.
  There is no authenticated delivery timestamp to enforce here. Payloads
  with no issue (including pings) and PR/bot events are not instructions.
- Status updates match both the configured bot ID and the reserved marker.
  Ordinary comments cannot use that namespace. The text is fenced literally
  for GitHub Markdown. Serialize writes and discover existing comments across
  all pages. An uncertain create blocks another create in that instance
  until an owned comment can be reconciled. After process restart, 6b must
  consult the durable intent/outcome audit records before allowing a create.

## Fixture provenance and boundary

Four GitHub.com webhook examples are projected from the Octokit repository
at commit `aa80c24a3f55354465b8b1438d33dd5602956d0d`, with source links and
the upstream MIT license beside the fixtures. Tests replay their original
identities and adapt them to the HTTP sandbox for the full 6a contract.
Signatures use a local test key. These are provider fixtures, not deliveries
captured from a newly configured project webhook.

The read-only example exercises the production client against GitHub without
posting comments, changing labels, creating issues or configuring webhooks.
Live write round trips, durable recovery, App provisioning and event routing
remain connected acceptance in 6b. See `docs/github-tracker-runbook.md`.

## Verification

- `cargo test --all`: 520 passed, zero failures, with the existing Docker
  opt-in left unset. Includes the HTTP adapter contract and signed fixture replay.
- Workspace Clippy with warnings denied, formatting, and `git diff --check`
  passed.
- The production read-only example loaded `kiennt23/kod#19` as Ready with
  title `kod-tracker 6a: deferred minor review findings`.
- Native whole-branch self-review completed; no independent review claimed.
  No live GitHub writes, App provisioning, deployment, or Docker opt-in run
  was performed. Connected write acceptance remains in 6b as described above.
