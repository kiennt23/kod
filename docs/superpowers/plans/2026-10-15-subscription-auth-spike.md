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
| Identity: account and workspace match the approved business seat | Record non-secret account and org id; a negative test with valid credentials for the wrong workspace is refused | Same, and set `forced_login_method` and `forced_chatgpt_workspace_id` ([Codex auth](https://developers.openai.com/codex/auth/)) | Same | 
| Adversarial: untrusted code tries to read or exfiltrate the credential into workspace output or the diff | Applies to the `CLAUDE_CODE_OAUTH_TOKEN` fallback only | Applies | Applies |
| Egress to non-vendor hosts is blocked | Applies to the fallback | Applies | Applies |
| Cancel a run: local use stops | Applies to the fallback | Applies | Applies |
| Expiry is detected | A pre-run check in `kod-eval` fails with "log in again" on an expired or revoked token | Same | Same |

## Exit criteria

- Every cell above has a recorded result, with the exact command and version used.
- For each tool, a decision: `subscription` mode is supported (with the setup to use), or not supported (with the reason).
- The proxy-in-path answer is known for each tool, and the list of controls lost without it is confirmed against the spec.
- The pre-run credential check detects expiry for every supported tool.
- No credential appears on a command line, in results or in logs during the spike.
- Where eligibility can't be checked programmatically, an admin attestation is on file for the seat.
- The credential-in-sandbox runs stay in the sanitized offline spike. The result states that cancel stops local use only, and that revoking a compromised token at the vendor is a separate manual step.
- Feasibility is not readiness. Enabling subscription mode also needs the design's manifest requirements: the effective auth mode is persisted, `kod-eval compare` rejects runs with different modes, and a legacy manifest with no recorded mode is refused (read as `api_key` only when recorded). Each gets an acceptance test in the implementation plan.
- The vendor-terms open item is answered for each vendor, or still listed as open.
