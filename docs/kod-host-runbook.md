# kod-host runbook

`kod-host` is the connected composition binary (sub-plan 6b-b). It reuses the dispatcher, the tool gateway and the LLM gateway as libraries and adds the concrete adapters: run bindings, sandbox launch, trusted collection, approved-document pins, a renewable tracker credential and the status refresh loop. It is not a new domain service. Live GitHub, model and acceptance steps (6b-c) still need owner credentials; see the end of this page.

## Commands

| Command | What it does |
| --- | --- |
| `kod-host check --config <file>` | Loads and validates the configuration. Prints `configuration valid` or stops with the reason. Touches no network. |
| `kod-host serve --config <file>` | Runs the host until SIGINT or SIGTERM. |
| `kod-host snapshot activate --config <file> --manifest <candidate>` | Replaces the approved-document manifest, see below. |

## Config

Copy `config/connected.example.json` and fill it in. The file is strict: unknown fields are refused, image references must be exact `sha256` digests, and the placeholder values (`REQUIRED OWNER VALUE`, `0`) are meant to be replaced; `kod-host check` is the authority on what is accepted. Run `kod-host check` after every edit.

| Section | Holds |
| --- | --- |
| `paths` | Dispatcher DB, audit DB, policy dir, run root, mirror, approval manifest, gates config, credential dir. |
| `github` | Product repo (id, installation, base branch, guard ruleset, bot user), App id and private key file, tracker repo and comment author id, tracker repository and installation ids, label mapping, webhook secret file, owner and backup key files, agent name and email. |
| `model` | Endpoint name, upstream URL, key file, allowed models, prices file, allowed paths, per-request and per-run token, body, request, concurrency and cost limits. |
| `networking` | `ingress_listen`, `gateway_listen`, accepted hosts, sandbox network, the gateway URL as the sandbox sees it. |
| `sandbox` | Agent and collector image digests, language, memory, CPUs, disk, turns and wall time. |
| `dispatcher_config` | Path to the existing dispatcher configuration ([dispatcher runbook](dispatcher-runbook.md)). |

Secrets are files, not values in the config. Key files must be owner-only (no group or other access); directories holding credentials must be mode 0700.

## Serve

Startup order is fixed and the first failure stops the host:

1. **Instance lock** on the directory holding the dispatcher DB. A second `serve` on the same directory is refused before it touches any credential.
2. Tracker credential start. Safety cleanup (stop durable runs, revoke leftover publish, refresh and activate leases) runs only when preparation fails: a config that parses but fails validation, or a tracker outage at boot, still stops and removes durable runs before the host exits.
3. Assembly: policy, audit, gateways, runtime.
4. **Recovery**: leftover refresh and activate leases are revoked (an unconfirmed revoke refuses the start), then orphan and publish-lease reconcile, then dispatcher recovery. Queued and running executions become `needs_human`; nothing resumes. Revoke and stop must be confirmed before the next step.
5. **Bind** both listeners.
6. Start the dispatch worker, then flip **ready**.

Admission opens last. While not ready, the dispatcher keeps new labels pending and starts nothing, but cancels and cleanup still run. Ready reopens after a transient error only when nothing forbids it: no cleanup pending and no shutdown begun.

Two listeners:

| Listener | Serves | Who reaches it |
| --- | --- | --- |
| `ingress_listen` | Webhook and signed CLI command routes | Public side (reverse proxy). The model and tool routes are not mounted here. |
| `gateway_listen` | Per-run tool (`/r/{token}/mcp`) and model routes | The sandbox network only. Do not expose it. |

The status refresh runs on a 30 second loop after start.

**SIGTERM / SIGINT**: admission closes, listeners and the worker are signalled together, the worker is joined (5 s, then aborted), runs are quiesced, revoked and stopped, listeners are awaited after cleanup, then the tracker credential is revoked. A stuck blocking task cannot hold the revoke. A failed revoke or stop is kept as a follow-up and retried at next start. A SIGTERM during publish leaves the exact-PR recovery to the next start.

## Snapshot activate

`kod-host snapshot activate --manifest <candidate>` validates the candidate manifest, reads open PR heads from the product repository with a live call, invalidates their checks through the gates report path, then replaces `approval_manifest`. It needs `KOD_GITHUB_TOKEN` (a dedicated installation token, as for `kod-gates run`) and the gates config. Any source or invalidation error refuses with "nothing was published"; the old manifest stays. Activation and the refresh share the global gates lock.

## Tracker credentials

The host mints its own tracker installation token from the App key: tracker repository only, `issues:write` (plus implicit `metadata:read`), never the publish token. It lives in `credential_dir/tracker` (mode 0700, files 0600), is renewed five minutes before expiry and is revoked on exit. The installation identity is validated before use. Keep the publish credentials in a different directory.

## Status refresh

Every 30 seconds the host recomputes each story's status ([status runbook](status-runbook.md)) and writes one comment through the tracker, with an audit intent before the write. A story is refreshed under its own story lock and the global gates lock. One failing story never stops the others.

Status writes are guarded in the dispatcher DB. A write whose outcome is unknown (timeout, unreadable success) leaves an uncertain intent that survives restart and blocks a second create for that story until it is resolved. A rejected (4xx) write allows retry. The audience is the issue reporter being a team member (`Tracker::is_team_member`).

## Fail-closed limits

These are deliberate. They show up as a missing or stale status comment, not as a wrong one.

- **Stories with rework rows fail the status refresh.** The dispatcher keeps no resolved-rework record, so rework reconciliation is not implemented. The refresh logs the reason (scrubbed) and audits `status_refresh_blocked`, once per state change. Those stories get no status comment updates until a follow-up adds rework reconcile.
- **Stories with any publish attempt fail the status refresh** (a run that is `publishing` or `done`, or or a `check_publish` follow-up exists, open or resolved: a published or needs_human run may have left a PR that no source can list once merged or closed) until a merged/closed PR source exists. Published stories get no status comment updates until then.
- **Do not build release binaries with `--all-features`.** That enables the `test-support` seams (loopback API overrides, injected clocks). Build with default features: `cargo build --release -p kod-host`.
- **Revision history is in memory.** Approved-document revisions pinned at admission are readable while the host runs; after a restart an old pinned revision can no longer be read.

## Failure modes

| Symptom | Cause and action |
| --- | --- |
| `serve` exits at once, "lock" error | Another host owns the directory, or the lock path is public or a symlink. Stop the other host. |
| Exits after "safety cleanup incomplete" | A durable run could not be stopped or revoked. Fix Docker or the gateway and start again; it retries from durable identities. |
| Never becomes ready | Cleanup still pending. Check the host log and `docker ps` for run containers. |
| Labels stay pending | Not ready, or WIP full. See [dispatcher runbook](dispatcher-runbook.md). |
| Kill switch during an outage | Still works: it bypasses policy and audit. |
| Status comment stale | Check for `status_refresh_blocked` in the audit log, then the limits above. |
| Old run token returns 401 after restart | Expected; tokens do not survive a restart and runs do not resume. |

## Owner steps that remain

Nothing below can be done without the owner's credentials. It is not claimed as verified.

1. Create and install the GitHub App for the product and tracker repositories; fill `github.*` ids and key files.
2. Provide the model key, `prices.json`, endpoint name, model ids and budgets.
3. Provide image digests and the sandbox network.
4. Run `kod-host check`, then a live `serve` against a real ticket.
5. Sub-plan 6b-c acceptance: a real ticket from webhook to sandboxed kod-loop run to draft PR with the story ID and agent trailers, conformance items 2, 4 and 7 with kod-loop, and the kill switch drill on live runs.
