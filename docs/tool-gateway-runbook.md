# Tool gateway runbook

`kod-tool-gateway` supplies the MCP endpoint and host-side publish operation mounted by [`kod-host`](kod-host-runbook.md). The GitHub App broker/Forge (#33) and host composition (#40) are merged. [6b-c acceptance](connected-runbook.md) supplies real-container and live connected proofs; the adapter's loopback HTTP tests and real Git objects alone do not establish deployment readiness.

## GitHub App activation and recovery

The owner supplies the App ID, installation ID, numeric repository ID, `owner/repo`, default branch, bot user ID, existing guard ruleset ID, and an RSA private-key path. The supported installation grants only contents/write and pull_requests/write (metadata/read is implicit). There is no PAT fallback, production API-origin override or automatic repository provisioning.

Keep the key in an owner-only regular file (0600) under a private directory. Create a separate lease directory with mode 0700, on a local filesystem supporting exclusive file locks and durable rename/fsync. Use absolute paths without symlink components; on macOS use the canonical `/private/...` path for temporary directories. The adapter holds `owner.lock` for its lifetime. A second adapter cannot share the directory. Never mount or copy keys, lease files, or returned broker tokens into a sandbox, and never print their contents.

Before admitting any run:

1. Construct `GitHub::new(config, app, lease_dir)`. This validates local inputs, takes the directory lock and loads records; it makes no HTTP requests. `Config::validate()` is the pure configuration check and takes no lock.
2. Call `recover()`. It revokes every saved token and retains all unconfirmed cleanup. It does not resume execution or expose saved tokens.
3. Call `verify_repository()`. This **mints and revokes a probe token**, so it requires owner authorization and is not a read-only config check. It checks the repository/default branch and configured guard ruleset.
4. Register each admitted `RunPermit` with its immutable run, native ticket key, repository and pinned base SHA. Share one `Arc<GitHub>` as both `Broker` and `Forge`. Identical registration may repeat; changed bindings are refused.

These are blocking APIs. Construct, invoke and drop the adapter outside async workers, or inside `spawn_blocking`; never block the model/MCP event loop with provider requests.

The guard must already target branches, be active, include exactly `~ALL`, exclude exactly `refs/heads/agent/*` and `refs/heads/agent/**/*`, and contain creation, update and deletion restrictions. Only explicitly listed Team actors with `always` bypass are supported; App, repository-role and unrecognized bypass actors are refused. The owner controls team membership. The adapter rechecks protection before returning every minted token. It never edits rulesets. GitHub tokens cannot encode the `agent/*` restriction; host mediation plus this repository protection enforce it.

Lease records use hashed run filenames and mode 0600. They retain immutable permits and nonsecret publication intent after credential revocation. A lost issuance response blocks new issuance for that run only, until recovery confirms cleanup or the maximum one-hour token lifetime plus 60 seconds has elapsed. Token expiry is checked with 60 seconds of clock-skew tolerance. Keep the host clock synchronized: after a backwards clock step, ordinary operations fail until the clock catches up, but revoke, recovery and the kill switch still run and revoke tokens explicitly instead of assuming expiry. Do not delete a pending record to bypass this wait. A leftover `*.json.pending` file from a crash during a save is discarded at startup. At startup, records of settled runs (no token, no issuance in doubt, no PR awaiting review) that are older than seven days are deleted. A received 4xx on PR creation is a definite refusal: the intent is cleared and the cause reported. A 5xx or transport failure stays uncertain until reconciled. If GitHub creates a PR that fails identity validation (for example not a draft, or a foreign URL), the gateway closes it. Until GitHub confirms the close, the run reports that it needs human review: close the PR on GitHub or retry. App JWTs are signed in-process with ring. A 403, server error or transport failure during revoke stays pending; a successful DELETE, authenticated 401, or validated expiry confirms unusability. Run recovery before admission after every restart.

Publication supports ordinary UTF-8 additions, modifications and deletions; existing regular executable modes are preserved. The collector must reject raw rename, binary, symlink, gitlink and mode-change intent before constructing `Change`. The Forge separately rejects unsafe paths, unsupported touched remote entries, binary blobs and truncated trees before any Git write. It refuses changed commit inputs for an existing run and never force-pushes a divergent branch.

An uncertain PR POST is recorded before transmission. Retries search all same-origin pages for the exact bot-owned draft PR, repository, branch, base and recorded SHA. Empty or hidden results do not authorize another POST. Keep the record and stop admission for inspection if reconciliation remains uncertain. The adapter never closes, edits or merges a PR as compensation. GitHub ref deletion has no compare-and-swap: a trusted owner can still move the ref between the verified GET and DELETE. Coordinate manual ref changes with a quiesced host; the adapter does not claim atomic protection against that race.

Offline verification:

```sh
cargo test -p kod-tool-gateway --all-features
```

For a separately owner-authorized disposable installation, prepare a private JSON file with fields `config` (the six Config fields), `app` (`app_id`, `private_key`), `lease_dir`, and `base` (full pinned commit SHA). Run only after the owner supplies and authorizes those resources:

```sh
KOD_LIVE_BROKER=1 KOD_GITHUB_BROKER_CONFIG="$CONFIG_FILE" \
  cargo test -p kod-tool-gateway --lib github::tests::live::live_broker_contract -- --ignored --exact
```

The ignored test fails when authorization/config is missing. It mints, reads the repository, revokes, then verifies HTTP 401; it makes no Git/PR changes. No live execution is implied by the offline test result. Connected ticket-to-PR acceptance remains 6b-c.

## Host composition inputs

| Piece | Where it comes from |
| --- | --- |
| `Policy` | the `policy/` directory, loaded with `Policy::load` |
| `AuditLog` | the audit database ([audit runbook](audit-runbook.md)) |
| Tracker, docs, forge, workspace | adapters passed in `Adapters` |
| Broker | a repository-scoped `Broker`; the paired Forge enforces the exact run branch on the host |
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
