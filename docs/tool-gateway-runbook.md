# Tool gateway runbook

`kod-tool-gateway` is the MCP endpoint a run's sandbox uses for tracker and docs tools, and the host-side publish operation. **It is a library with a router; there is no binary yet.** The connected integration step (sub-plan 6b) wires a process around it with the real tracker, docs, forge and token-broker adapters. Until then it runs only in tests, against fakes. This runbook describes what the code does so that whoever wires it knows what to configure.

## What the process must supply

| Piece | Where it comes from |
| --- | --- |
| `Policy` | the `policy/` directory, loaded with `Policy::load` |
| `AuditLog` | the audit database ([audit runbook](audit-runbook.md)) |
| Tracker, docs, forge, workspace | adapters passed in `Adapters` |
| Broker | a `Broker` that mints a credential able to push `agent/*` branches and open draft PRs in one repo, and nothing else |
| `stopped` | `Dispatcher::stopped`: true unless the run is live in the dispatcher's database |
| Agent identity | the committer name and email, each on one line |
| `router(gateway, hosts)` | `hosts` are the `Host` header values the endpoint accepts |

## Runs and tokens

The dispatcher calls `start_run(token, run)` when it starts a run. The token is at least 32 lower-case hex characters; the gateway keeps only its SHA-256. The sandbox reaches `/r/{token}/mcp`; an unknown token gets 404. `run` carries the id, ticket, repo, the 40-character base commit, the data classes, the rework tickets linked before the run, and the start time. A duplicate run id or token is refused. `end_run(id)` stops serving a run and revokes its credentials; `end_all()` is the kill switch's counterpart.

## Tools

`get_issue`, `read_doc`, `comment`, `transition`, `create_linked`. The agent can move only its own ticket, and only between in progress and blocked. `read_doc` refuses a document outside the run's data classes. Publish is not a tool.

## Failure behaviour

| Case | Result |
| --- | --- |
| Run canceled or ended | every call refused |
| `stopped` cannot be read | treated as stopped |
| Policy says no, or cannot decide | refused with the policy's reasons |
| The intent cannot be written to the audit log | the call does not happen |
| Error text | redacted, and run tokens in `/r/...` paths removed, before it leaves the gateway |

## Publish

The host calls `publish(run_id)` after the run ends. The change comes from the host's collector output, never from the sandbox. At most 1000 files and 8 MiB, no path twice. It pushes `agent/<ticket>-<run>` through the forge's API and opens a draft PR whose commit ends with `Kod-Ticket`, `Kod-Run` and `Co-Authored-By` trailers. The commit date is the run's start time, so a retry makes the same commit and finds the same PR. A run canceled before or during publish is refused. The run's credential is revoked when publish returns. After a restart, `published()` tells the dispatcher whether a draft PR already exists.
