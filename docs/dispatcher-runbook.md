# Dispatcher runbook

`kod-dispatcher` takes tracker webhooks, queues them in SQLite, admits or refuses runs, and starts them through the tool gateway. **It is a library with an axum router; there is no binary and no CLI client yet.** Sub-plan 6b builds the process around it with the real tracker adapter and gateway. What follows is what the code accepts, so the wiring and your commands match it.

## Config

A JSON file with exactly three keys, read with `Config::load`:

    {"repo": "acme/payments", "owner": "<github login>", "backup": "<github login or null>"}

`repo` is lower-case `owner/name`. Logins are lower-cased. The backup must differ from the owner. Owner and backup live here, not only in the policy, so their kill switch works when the policy fails to load. The database path, the policy, the audit log, the tracker adapter and the gateway are passed to `Dispatcher::open`. If the policy failed to load, pass its error: everything that needs it is refused and the owner's or backup's kill switch still works.

## Webhook setup

The router serves `POST /webhook`. Point the tracker's webhook at it. The tracker adapter checks the signature, so give it the tracker's webhook secret (the adapter's own config, not the dispatcher's). The reply tells the tracker what to do:

| Status | Meaning |
| --- | --- |
| 202 | queued |
| 200 | duplicate (same signed body seen before) or an event Kod ignores |
| 401 | not genuine or malformed |
| 500 | the store failed; the tracker should retry |

Events acted on: label added (the delegation label), label removed, ticket edited, and a comment whose first line is `/kod kill`, `/kod cancel` or `/kod resume`. Other comments are ignored.

A failed delivery is retried with backoff (2 s doubling, five attempts). After that it is a dead letter and the ticket gets a comment saying so. Run `work_once` (or `work_at`) on a timer to process the queue.

## Kill and resume

`/kod kill` and `/kod cancel` can come from a signed tracker comment. `/kod resume` in a comment is refused. Resume only comes from the signed CLI request.

| Action | Who |
| --- | --- |
| Cancel one run | owner, backup, or anyone the policy's run-scope `cancel` rule allows (a tech lead) |
| Kill switch | owner or backup, always; anyone else through the policy's all-scope `cancel` rule |
| Resume | owner or backup only, through the policy's `resume` rule; fails while the policy is unavailable |

The kill switch cancels every live run, revokes credentials, stops all sandboxes and refuses new runs until resume. If the audit log is down, the owner's or backup's kill still works. The reply names every part that did not complete.

The signed request is `POST /command` with a JSON body and the header `x-kod-signature`:

    body='{"command":"kill","by":"<login>","at":'$(date +%s)'}'
    sig=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$KOD_CLI_KEY" -hex | awk '{print $NF}')
    curl -X POST "$DISPATCHER/command" -H "x-kod-signature: $sig" -d "$body"

`command` is `kill` or `resume`. The signature is the hex HMAC-SHA256 of the exact body bytes under that login's key (`with_cli_key`). The request must be within 300 s of the dispatcher's clock and can be used once. Answers: 200 `killed` or `resumed`, 403 with the reason. If a resume says a global stop is in flight, or a kill landed during it, retry with a fresh request.

## Recovery

- On open, a run left in `publishing` is settled: done if the gateway finds its draft PR, `needs_human` otherwise. No run is left live.
- Follow-ups (revoke, stop, ticket moves, comments) are stored with the state change and retried until they succeed. Stops go first.
- A failed global stop is retried. Check the ticket comments if a kill reported an error.
- If a cancel lands after publish already opened the PR, the ticket says so: close the PR, do not merge it.
- The database is one SQLite file with a single dispatcher process. Do not run two.
