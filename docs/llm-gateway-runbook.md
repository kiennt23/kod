# LLM gateway runbook

`kod-llm-proxy` is the LLM gateway: the only holder of the model key. It serves the Anthropic Messages API on `/v1/messages` and `/v1/messages/count_tokens`. Offline `kod-eval run` starts one gateway container per case using static `runs.json`; the standalone binary is built by `images/proxy/Dockerfile`. Connected [`kod-host`](kod-host-runbook.md) embeds the same router with dynamic registration, irreversible revocation and durable reservation/settlement callbacks to the dispatcher store. [Connected acceptance](connected-runbook.md) verifies the composed path.

## What you configure

Every file and variable is checked at start. A bad value stops the gateway; nothing has a hidden default except the limits marked below.

| Variable | Meaning |
| --- | --- |
| `KOD_API_KEY` | The provider key. Only in the gateway's environment, never in a file the sandbox can read. |
| `KOD_UPSTREAM` | The provider base URL. |
| `KOD_ENDPOINT` | The endpoint name the routing policy must allow. `kod-eval` uses `placeholder`. |
| `KOD_ALLOWED_MODELS` | Comma-separated model ids. Each needs a price. |
| `KOD_PRICES` | Path to `prices.json`. |
| `KOD_RUNS` | Path to `runs.json`. |
| `KOD_POLICY_DIR` | The policy directory (`policy/`). |
| `KOD_AUDIT_DB` | The audit database ([audit runbook](audit-runbook.md)). |
| `KOD_LISTEN` | Default `0.0.0.0:8080`. |
| `KOD_ALLOWED_PATHS` | Default both paths; a non-empty subset of them. |
| `KOD_MAX_TOKENS` | Largest `max_tokens` a request may ask for. Default 8192. |
| `KOD_MAX_BODY` | Request body bytes. Default 4 MiB. |
| `KOD_MAX_REQUESTS` | Requests per run, refused ones included. Default 200. |
| `KOD_MAX_CONCURRENT` | In-flight requests per run. Default 2. |

The limits must be whole numbers above zero.

`prices.json` maps each allowed model to micro-USD per million tokens:

    {"<model id>": {"input": 3000000, "output": 15000000, "cache_write": 3750000, "cache_read": 300000}}

Take the numbers from the provider's price page. A model without a price stops the start.

`runs.json` maps a run token to its run and budget. A token is at least 32 lower-case hex characters. Run ids are unique.

    {"<token>": {"id": "r1", "ticket": "PAY-123", "repo": "acme/payments",
                 "data_classes": ["placeholder"], "max_tokens": 2000000,
                 "max_cost_micro_usd": 5000000}}

Both budgets must be above zero. The standalone binary reads this file at start; `kod-eval` writes it from `--prices`, `--run-max-tokens` and `--run-max-cost-micro-usd`. Connected runs use the host-only `RunRegistry` instead: identical registrations preserve spend, changed or tombstoned IDs are refused, and no registration HTTP route is exposed. Local revocation cancels queued and active forwarding; it cannot promise that a remote provider immediately stops billing. Unknown dispatched usage retains the full reservation.

## How a run reaches it

The runner's base URL is `http://<gateway>/r/<token>`. The token is the run's identity and its only credential. An unknown token gets 401 and no log line. Keep `runs.json` out of any sandbox mount.

## Budgets

A request reserves its worst case (body bytes plus slack as input, `max_tokens` as output, at the dearest input rate) before it is forwarded. When the response ends it settles to the reported usage. Concurrent requests cannot together pass the budget. Tokens count input, output and both cache fields.

- Over the token or cost budget: the request is refused (403 `over_spend`).
- A response that cost more than its reservation is charged and halts the run: every later request gets 403 `halted`.
- Request count over `KOD_MAX_REQUESTS`: 429, then silence.
- Requests past `KOD_MAX_CONCURRENT` for a run wait for a free slot.

## Failure behaviour

The gateway refuses rather than forwards when it cannot decide or record.

| Case | Result |
| --- | --- |
| Routing policy denies the run's data class, endpoint or model | 403 `deny_route` or `deny_model` |
| Prompt capture cannot be written to the audit log | 503 `audit_error`, nothing forwarded |
| Output capture cannot be written | the run is halted |
| Upstream unreachable | 503 `upstream_error` |
| Body has a field, content block or beta header the gateway cannot price or see | 400 `deny_shape` |
| Policy or audit log fails to load | the gateway does not start |

Every refusal and every halt is a `run_audit` record. Log lines are JSON on stdout with the run id: one per request within the budget, and a `usage` line when a response ends. Prompts and outputs are redacted before they are stored and cut at 128 KiB (length and sha256 are kept).

## Operating it

- Rotate the key: restart the gateway with the new `KOD_API_KEY`.
- Stop a run: remove it from `runs.json` and restart, or use the kill switch (`kod-eval stop` for eval runs; the dispatcher's kill switch for connected runs, see the [dispatcher runbook](dispatcher-runbook.md)).
- Check spend: read the `usage` lines.
