# Subscription Auth Spike

**Spec:** `docs/kod-harness-design.md`, "Subscription auth for vendor runners".

**Goal:** find out, before building anything, whether Claude Code, Codex CLI and Cursor CLI can run headless in the Kod sandbox on a subscription login, and what Kod keeps and loses in each case.

**Needs the owner's seats.** The spike uses a dedicated business seat per vendor (Claude Team or Enterprise, ChatGPT Business or Enterprise, Cursor Business). Nothing starts until the owner provides them. Personal seats are not used.

## Checks

Run each check for each tool, inside the Kod sandbox, and record the result.

| Check | Claude Code | Codex CLI | Cursor CLI |
|---|---|---|---|
| Headless login works with no browser | `claude setup-token`, then a run with the token | `codex login` on the host, then a run with a read-only copy of `~/.codex/auth.json` | `CURSOR_API_KEY` from the dashboard, passed by name |
| Token lifetime and refresh behaviour | Lifetime of the setup-token; what happens at expiry | Does `auth.json` refresh itself; what a read-only copy does when a refresh is due | Key lifetime; what happens on revoke |
| Traffic through kod-llm-proxy | The proxy injects the subscription token as the request credential | Whether subscription traffic can be routed through the proxy | Expected no (Cursor runs models on its own servers); confirm, and list the domains egress must allow |
| Usage and cost the tool reports | Record what the run output shows | Record what the run output shows | Record what the run output shows |
| Expiry is detected | A pre-run check in `kod-eval` fails with "log in again" on an expired or revoked token | Same | Same |

## Exit criteria

- Every cell above has a recorded result, with the exact command and version used.
- For each tool, a decision: `subscription` mode is supported (with the setup to use), or not supported (with the reason).
- The proxy-in-path answer is known for each tool, and the list of controls lost without it is confirmed against the spec.
- The pre-run credential check detects expiry for every supported tool.
- No credential appears on a command line, in results or in logs during the spike.
- The vendor-terms open item is answered for each vendor, or still listed as open.
