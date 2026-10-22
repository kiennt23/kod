# GitHub Issues tracker

`kod_tracker::github::GitHub` is the first real `Tracker` adapter. Its
mapping is `registry/trackers/github.json`; construction takes a trusted
`Config { repo, comment_author_id }`, the parsed mapping, an installation
token and a webhook secret. The service wires this instance into the
dispatcher and tool gateway in sub-plan 6b. This change installs no service.

## GitHub setup

Use a dedicated GitHub App installed only on the tracker repository:

- Repository Issues: read/write (issues, comments, labels, sub-issues).
- Repository Metadata: read (effective collaborator permissions).
- Subscribe to `issues` and `issue_comment` webhooks, JSON content type,
  with a strong secret. The trusted host routes verified repository payloads
  to the matching adapter and the dispatcher owns durable delivery dedup.
- Resolve and pin the App bot's numeric user ID as `comment_author_id`.
  Production status writes require that bot identity; an OAuth/PAT user's
  comments cannot substitute for it.

The registry's `repository-writers` team group explicitly grants instruction
membership to human write/admin collaborators, including outside collaborators
with those permissions. It excludes read/triage-only users and bots. Review
that group choice before connecting the pilot; an organization-team mapping
is a separate adapter extension, not an implicit fallback.

Provision the trigger label `kod` and workflow labels `kod:in-progress`,
`kod:in-review`, and `kod:blocked`. Native open with no workflow label is
Ready; native closed is Done. Multiple mapped labels are an error. Workflow
labels use the reserved `kod:` namespace; an unconfigured label in that
namespace is an error. The trigger is outside that namespace. A transition
preserves unrelated labels, makes separate label/state writes,
then verifies the resulting state. Serialize trusted writes; concurrent
human changes can cause a refusal requiring reconciliation.
Plain labels named `open` or `closed` are unrelated to workflow state and
are preserved. Human additions/removals in the `kod:` namespace emit an edit
event so the dispatcher can invalidate a running task.

API calls use GitHub.com HTTPS with a pinned API version, sensitive auth
headers, disabled redirects, connect/request timeouts and bounded responses.
Lists page through at most 100 pages; reaching the cap is an error rather
than silently returning incomplete records. The adapter does not refresh
tokens or automatically retry writes/rate limits. The host owns token renewal,
backoff and durable retries, preserving audit-before-action ordering.

## Issue fields

Keys are canonical `owner/repo#number`. The adapter refuses another repo's
key, leading-zero/nonpositive numbers, and pull requests returned by the
Issues API. To supply harness fields, put one block in the raw issue body:

```text
<!-- kod:issue
{
  "requirement_id": "REQ-7",
  "risk_class": "low",
  "acceptance_criteria": ["Downloads a CSV with the approved columns"],
  "approved_versions": ["https://docs.example/design?rev=3"],
  "links": ["acme/app#2"]
}
-->
```

Repository identity comparisons ignore ASCII case; returned keys use the
configured spelling. Metadata accepts LF and CRLF line endings. Native
sub-issues in other repositories are omitted from the adapter's local links;
explicit metadata links remain restricted to the configured repository.

The requirement/risk property names come from `mapping.fields`. The other
keys are fixed. Absence of the block yields absent optional fields and empty
lists; it grants no approval. The dispatcher still refuses missing required
inputs. Malformed/duplicate blocks, duplicate JSON keys, unexpected fields,
wrong field types and foreign issue links fail closed. Approved links are
references; approval authenticity/currentness still comes from the registry
and gate controls.

`create_linked` stores the original stage, artifact, version, text, evidence
and kind under `rework`, plus the story back-link. GitHub's native sub-issue
API supplies the story's forward link, avoiding a parent-body rewrite. If
creation succeeds but linking fails, the error includes the new issue key.
Reconcile that issue instead of creating another. An unknown create response
also needs reconciliation before retry; the adapter does not promise an
idempotency guarantee GitHub's create endpoint does not provide.

## Webhook trust and recovery

Verify exactly one `X-Hub-Signature-256` against the raw bytes in constant
time before decoding. Malformed issue-shaped events and wrong repositories
are errors; pings/non-issue events, PRs, bot instructions and unrelated labels
are ignored after authentication. Event and delivery headers are not trusted
for classification or deduplication. The resulting `Delivery.id` is the
body's SHA-256; replaying it gives the same ID and spends nothing in the
adapter. The existing dispatcher commits/deduplicates deliveries durably.

GitHub does not authenticate a delivery timestamp in those headers. Keep the
dispatcher database and its dedup history across restarts; do not replace it
with an empty store while old deliveries remain replayable. The adapter
re-reads current issue state for operations; it does not treat historical
webhook issue contents as current policy evidence.

## One owned status comment

The updater selects only comments with both the configured bot ID and the
`kod:status` marker. Human comments with the same marker are preserved, and
ordinary adapter comments cannot forge the reserved namespace. The renderer
uses a dynamically sized Markdown code fence so content stays literal.

Serialize refreshes from record read through final write, including across
processes, as required by the status runbook. A create with an uncertain
response marks that issue uncertain in the adapter instance. A later call
may update a discovered owned comment, but cannot blindly create a second
one while uncertainty remains. Duplicate owned comments are an error.
Definite 4xx rejections clear the in-process create guard and allow a later
retry. Transport failures, 5xx write responses, and malformed successful
responses remain uncertain; a server error can follow a committed write.
Read failures do not claim an uncertain write. Transport causes are retained
with request URLs removed.

Before restarting/replacing an adapter after an uncertain write, use the
status intent/outcome audit records and remote comment list to reconcile.
That recovery belongs to the durable 6b integration. Do not reconstruct a
fresh writer and retry an unconfirmed create without that check.

## Validation

```sh
cargo test -p kod-tracker
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

The read-only example needs `KOD_TRACKER_TOKEN`,
`KOD_TRACKER_WEBHOOK_SECRET` and `KOD_TRACKER_COMMENT_AUTHOR_ID` in the host
environment, then:

```sh
cargo run -p kod-tracker --example read_github_issue -- OWNER/REPO NUMBER
```

Only GET requests execute in that example. An OAuth credential can validate
reads locally, but does not establish production App identity/write acceptance.
Fixture provenance is in `crates/kod-tracker/tests/fixtures/github/README.md`.
The full shared contract uses a local HTTP sandbox, including write paths;
it does not modify a live GitHub repository.

## API references

- [Webhook authentication](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries)
- [Webhook payloads and headers](https://docs.github.com/en/webhooks/webhook-events-and-payloads)
- [Issues](https://docs.github.com/en/rest/issues/issues)
- [Labels](https://docs.github.com/en/rest/issues/labels)
- [Sub-issues](https://docs.github.com/en/rest/issues/sub-issues)
- [Issue comments](https://docs.github.com/en/rest/issues/comments)
- [Collaborator permissions](https://docs.github.com/en/rest/collaborators/collaborators#get-repository-permissions-for-a-user)
