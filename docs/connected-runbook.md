# Connected host acceptance and operations

The GitHub broker/Forge (6b-a, #33), dispatcher recovery storage (#36), and
`kod-host` composition (6b-b, #40) are merged. The 6b-c acceptance work adds
an explicit command and real-container scenarios. Live acceptance, the
connected pilot and the independent offline baseline remain pending until
their actual results exist. The [roadmap](superpowers/plans/2026-10-14-phase1-overview.md)
records these separate boundaries; [issue #39](https://github.com/kiennt23/kod/issues/39)
tracks the host's remaining limitations.

## Prepare the host

Use a dedicated sandbox host with Docker, a separate gate host/daemon and
separate caches. Follow the [host configuration](kod-host-runbook.md),
[GitHub App setup](tool-gateway-runbook.md), [tracker mapping](github-tracker-runbook.md)
and [gate activation](gates-runbook.md). No acceptance command provisions
an App, changes protection rules, merges a PR or enables auto-merge.

Build normal deployment binaries without test features:

```sh
cargo build --release -p kod-host --locked
```

The owner supplies absolute, symlink-free paths, owner-only secrets and
approved snapshots; run/credential directories are mode 0700 and files
mode 0600. Keep databases and credentials outside agent-visible mounts.
Image references are immutable digests. Confirm sandbox network egress
restrictions and gate-host isolation from actual host inventory.

Set these variables from operator-provided paths before the commands below:

| Variable | Source and meaning |
| --- | --- |
| `CONNECTED_CONFIG` | Reviewed active host configuration with owner-supplied repository/App/model IDs, secret references and positive limits |
| `CANDIDATE_CONFIG` | A copy of that configuration pointing only `paths.approval_manifest` at the staged manifest |
| `SNAPSHOT_STAGE` | A new private staging directory outside every sandbox and PR workspace |
| `APPROVAL_EXPORT` | Owner-provided export of the exact revision already signed off by the artifact's named approver |
| `EVIDENCE_DIR` | A new private directory under ignored `.omo/evidence/phase1-connected/` for this acceptance invocation; the command creates it exclusively |

`kod-host check` validates local files without network calls or database
creation. A successful check does not prove remote permissions, egress,
gate isolation, live conformance or cleanup.

## Stage and activate approved documents

Obtain recorded sign-off from the artifact's named approver, then export
that exact revision. The harness owner operates the export; a backup can
operate it but cannot supply a missing approval merely by being backup.
Retain the original sign-off reference, timestamp, revision and approved
classification. A file being present is not evidence of approval.

```sh
umask 077
install -d -m 700 "$SNAPSHOT_STAGE"
install -m 600 "$APPROVAL_EXPORT/approved.txt" "$SNAPSHOT_STAGE/approved.txt"
sha256sum "$SNAPSHOT_STAGE/approved.txt"
```

The owner writes the staged `manifest.json` from those facts, including
every required requirements/design/threat-model/API input. This concrete
test example describes bytes `approved design` followed by one newline;
its identifiers and sign-off are fixtures, not deployment clearance.

<!-- tested: approval-manifest -->
```json
{
  "version": 1,
  "artifacts": [{
    "link": "design",
    "revision": "design-v3",
    "sha256": "3ac7337a4f321d6ad1074a18f9b1dd7b18f6be0b26b95b2d2b76b9ae3735c750",
    "class": "placeholder",
    "path": "approved.txt",
    "approved_by": "kiennt23",
    "approved_at": "2026-10-25T00:00:00Z",
    "approval_ref": "fixture-signoff-design-v3"
  }]
}
```

Keep exported paths immutable and relative to the manifest directory.
Reusing a revision for different bytes or classification is refused.
Canonical workflow links such as `design` describe stages; `class` is
the separate data classification enforced for model/document access.

```sh
kod-host check --config "$CANDIDATE_CONFIG"
kod-host snapshot activate --config "$CONNECTED_CONFIG" --manifest "$SNAPSHOT_STAGE/manifest.json"
kod-host check --config "$CONNECTED_CONFIG"
kod-host serve --config "$CONNECTED_CONFIG"
```

Activation requires the dedicated gate App token described in the
[gate runbook](gates-runbook.md), not a sandbox token or PAT fallback.
It validates the staged snapshot, takes the shared gates and story-refresh
locks, reads open PR heads, invalidates their checks through the existing
gate reporting path, and only then publishes the manifest. Source or
invalidation failure leaves the previous manifest current. Never copy a
file directly over the active manifest. The status worker reconciles
known stories periodically; its [fail-closed limits](kod-host-runbook.md#fail-closed-limits)
still apply to published stories and rework.

## Run acceptance

Always select a mode explicitly:

```sh
kod-host accept --config "$CONNECTED_CONFIG" --evidence "$EVIDENCE_DIR" --mode docker
kod-host accept --config "$CONNECTED_CONFIG" --evidence "$EVIDENCE_DIR" --mode live
```

Each invocation needs a distinct evidence directory. Docker mode is a
test build using `connected-test-support`, local generated credentials
and literal loopback provider fixtures. It runs the real internal
components, agent and collector. The ordinary deployment build refuses
Docker mode. Fixture results never count as live acceptance.

Live mode requires the owner-provided acceptance section and its private
authorization record before external writes or paid calls. The record
must name the configured owner/backup, exact disposable repository,
endpoint/model, expiry, request/token/cost caps and allowed actions
`issue`, `draft_pr`, `checks`, `comment`, `cancel`, `restart`, `kill`,
`cleanup`. Do not invent or approve this record as an execution step.
The suite also needs approved inputs, image digests, signed reachable
webhook ingress, operator keys, enforced App-bound gates, reviewer/queue
prerequisites and isolated hosts. Missing prerequisites remain blocked.

The suite-wide reservation ledger supplements per-run budgets; neither a
retry nor a crash refunds unknown dispatched usage. Execution stops before
another worst-case reservation would exceed an authorized cap. A run's
request output cap is distinct from its total token budget.

| Exit | Meaning |
| --- | --- |
| 0 | Every required scenario ran and passed with complete observations |
| 1 | A required scenario or evidence write failed |
| 2 | Missing authorization/infrastructure or an incomplete required scenario set |

The report identifies mode, source commit, images, App/repository,
authorized limits, actual counters/spend and named outcomes. Observations
contain allowlisted codes/counts/public references, not source text,
provider secrets or arbitrary raw error messages. Keep raw evidence
private and ignored. Evidence is a report, never a third operational DB.

Inspect `report.json` and cleanup outcomes after every invocation. A
timeout, skipped row, empty run or missing observation is not a pass.
Docker tests without explicit opt-in print a skip and produce no passing
acceptance report. Use the exact Docker image builds and test invocations
in [CI](../.github/workflows/ci.yml); production gate-host separation is
not established by a developer's shared Docker daemon.

## Cancel, kill, restart and rollback

Use the existing [signed command](dispatcher-runbook.md#kill-and-resume)
for owner/backup kill and resume. Label removal and signed cancel stop
the affected run. Runtime resume does not revive old execution or reset
spend. After SIGTERM or a crash, retained dispatcher/audit/lease records
remain the authority; the next start reconciles and confirms cleanup
before opening admission.

Expected measured bounds are model cancellation within two seconds,
TERM grace five seconds, then KILL and confirmed owned-container absence
within ten seconds. An unconfirmed stop or credential revoke stays pending
and visible. Shutdown starts independent stop while remote revocation is
pending. Do not delete lease files or operational rows to bypass recovery.

To roll back an approved snapshot:

1. Issue a fresh signed kill and confirm containers and old tokens are ended.
2. Retain databases, audit records, immutable snapshots and acceptance evidence.
3. Select a retained snapshot only if its approval is still valid and a new
   recorded rollback approval authorizes making it current. Otherwise stay stopped.
4. Point `CANDIDATE_CONFIG` at that retained snapshot, validate it and activate
   it through the same command and invalidation path above.
5. Restore a reviewed binary/config without downgrading the database schema;
   run `check`, start `serve`, confirm recovery, then send a fresh signed resume.

A backup may operate this sequence but cannot grant the artifact's approval.
An unapproved edit is not an approval refresh or rollback candidate.

## Record actual live results

Live acceptance must observe actual MCP use, a denied/audited nonsecret
sentinel, a real collected draft PR with matching head/tree/trailers,
head-bound gates and status, approval invalidation, cancellation, crash
recovery and kill/resume drills. The host must observe signed deliveries;
direct admission calls cannot substitute for webhooks. No automatic merge
is part of acceptance.

Only a complete passing live report may be curated and copied to
`docs/evidence/<actual-UTC-date>-6b-acceptance.json`. The report's export
guard refuses Docker and partial results. Inspect the allowlisted output
for private values before checking it in. Until actual live results and
review exist, keep 6b and pilot cutover pending. The offline baseline
comparison in the [phase 1 runbook](phase1-runbook.md) is independent.
