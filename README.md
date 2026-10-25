# Kod

Kod is an agent harness for a regulated-industry SDLC. Phase 0 is built: an
**offline eval harness** that replays real past tickets against an agent in a
locked-down sandbox and scores the result, before any agent touches a live
repository.

## Status

Phase 0 is built. The first baseline (real cases, real model) is still to run.

Phase 1 sub-plans 1 to 6, 6a, 6c, 7 and 8 are merged. The GitHub Issues
tracker adapter is built. Sub-plan 6b-a adds the real GitHub App broker and
Forge and the connected host (6b-b) is implemented. Acceptance (6b-c) has
the `kod-host accept` command, evidence contract and a Docker conformance
subset; the full Docker suite and live acceptance remain pending.
Sub-plan 9 (upstream doc drafting) is not started. The
per-sub-plan table is in the
[phase 1 overview](docs/superpowers/plans/2026-10-14-phase1-overview.md).
The dispatcher and the tool gateway are libraries until 6b wires them into a
process, so they have no binary yet.

## Docs

| Doc | What it covers |
| --- | --- |
| [Kod: an agent harness for our SDLC](docs/kod-harness-design.md) | Harness architecture, PR flow, rework and staleness, pluggable models/trackers/runners, Rust components, policies, roadmap |
| [DevSecOps SDLC with AI Agents](docs/devsecops-sdlc.md) | Stages, gates, rework loops, stage outputs, agent roster, control architecture, threats, risks, metrics, rollout |
| [Architecture (C4)](docs/architecture.md) | Context, container and component diagrams; trust boundaries; planned vs built |
| [Phase 0 runbook](docs/phase0-runbook.md) | Host setup, pinned images, smoke test, baseline, kill-switch drill, exit criteria |
| [Phase 1 runbook](docs/phase1-runbook.md) | kod-loop against the baseline: conformance, tuning, held-out comparison |
| [Phase 1 overview](docs/superpowers/plans/2026-10-14-phase1-overview.md) | Sub-plans, order and status |
| [Audit log runbook](docs/audit-runbook.md) | Checkpoints, verify, purge, off-host verifier |
| [kod-host runbook](docs/kod-host-runbook.md) | Connected host: config, serve, snapshot activate, tracker credentials, status refresh, fail-closed limits |
| [Dispatcher runbook](docs/dispatcher-runbook.md) | Webhook setup, kill and resume, recovery |
| [LLM gateway runbook](docs/llm-gateway-runbook.md) | Runs and prices config, budgets, failure behaviour |
| [Tool gateway runbook](docs/tool-gateway-runbook.md) | Run tokens, tools, publish, failure behaviour |
| [Phase 1 gate runner](docs/gates-runbook.md) | Trusted checks, isolated scanners, GitHub rules and activation requirements |
| [Workflow status](docs/status-runbook.md) | `kod status`, computed story progress and one maintained tracker comment |
| [GitHub Issues tracker](docs/github-tracker-runbook.md) | First tracker adapter, issue metadata, signed events and status-comment recovery |
| [Building the eval set](docs/eval-set.md) | How to turn merged changes into cases, test commands per language, adversarial cases |
| [Pinned versions](evals/VERSIONS.md) | Every version a baseline depends on |
| [ADR 0001](docs/adr/0001-per-task-execution-mode.md) | How implementation plans are executed and reviewed |

Docs live in this repo as Markdown. A build PR updates the README, the overview status and its runbook (see the PR template).

## What's in the repo

| Path | What it is |
| --- | --- |
| `crates/kod-eval` | The `kod-eval` CLI: cases, runs, scoring, reports, kill switch |
| `crates/kod-llm-proxy` | The LLM gateway. The only way out of the sandbox; holds the model key and enforces models, token and cost budgets, request count and concurrency |
| `crates/kod-loop` | Our agent loop, built into every sandbox image (`--runner kod-loop`) |
| `crates/kod-policy` | Policy engine: Rego rules and data in `policy/`, evaluated in process |
| `crates/kod-audit` | Hash-chained SQLite audit log and the `kod-audit` CLI |
| `crates/kod-tracker` | Tracker adapter contract, GitHub Issues adapter, fake adapter and contract tests |
| `crates/kod-tool-gateway` | MCP tools for a run, and the host-only publish operation (library) |
| `crates/kod-dispatcher` | Webhooks to runs, kill switch and recovery (library) |
| `crates/kod-host` | The `kod-host` binary: composes the dispatcher and both gateways, launches sandboxed runs, trusted collection, tracker credentials, status refresh. Implemented; live acceptance (6b-c) pending |
| `crates/kod-gates` | Trusted check runner for pull requests |
| `crates/kod-status` | The `kod status` CLI |
| `images/sandbox`, `images/proxy` | Container images |

How a case runs:
1. The agent works in a container with no git history and no network except the proxy.
2. A separate collector computes its diff and checks protected paths.
3. Held-back tests are scored in a container with no network and a read-only workspace.

Missing evidence never reads as clean.

## Using it

Run on a dedicated Linux host with Docker (see the runbook for why).

```sh
# 1. Install the CLI and build the images
cargo install --path crates/kod-eval --locked   # puts kod-eval in ~/.cargo/bin
docker build -t kod-llm-proxy --build-arg RUST_VERSION=1.99.0 -f images/proxy/Dockerfile .
docker build -t kod-sandbox-rust \
  --build-arg RUST_VERSION=1.99.0 \
  --build-arg BASE=rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 \
  --build-arg NODE_VERSION=22.23.3 --build-arg CLAUDE_CODE_VERSION=2.1.296 \
  --build-arg NEXTEST_VERSION=0.9.148 -f images/sandbox/Dockerfile .  # pinned in evals/VERSIONS.md; keep the two in step

# 2. Turn a merged change into a case, then check it (records required_tests).
#    The test command is the Rust one from docs/eval-set.md: the workspace is
#    read-only, so build output stays under /tmp and JUnit goes to $KOD_JUNIT.
kod-eval new-case --mirror /srv/mirrors/payments.git --merge-commit <sha> --id PAY-123 \
  --language rust --risk-class low --test-path tests/refund.rs \
  --test-command 'CARGO_TARGET_DIR=/tmp/target cargo nextest run --offline --config-file /etc/kod/nextest.toml --profile kod; s=$?; cp /tmp/target/nextest/kod/junit.xml "$KOD_JUNIT"; exit $s'
$EDITOR evals/cases/PAY-123/case.toml      # write the ticket text; set split and [budget]
kod-eval check-case evals/cases/PAY-123

# 3. Run the dev split against a pinned model, then read the report
export KOD_API_KEY=...                       # only in this shell; a key with a provider spend limit
# prices.json: the model's price in micro-USD per million tokens, from the
# provider's price page, e.g. {"<model id>": {"input": 3000000, "output":
# 15000000, "cache_write": 3750000, "cache_read": 300000}}. The budget is per case.
kod-eval run --split dev --model <model id> --prices prices.json \
  --run-max-tokens 2000000 --run-max-cost-micro-usd 5000000
kod-eval report results/<run>                # exits 1 if any case is unsafe or not verified

# Kill switch, from any shell: stops everything and refuses new runs
kod-eval stop                                # delete ~/.kod/STOP (or $KOD_STOP_FILE) to run again
```

Every command takes `--help`. Tune prompts on `--split dev` only. Run
`--split heldout` once per baseline.

## Developing

```sh
cargo test --all
cargo clippy --all-targets -- -D warnings
# Docker end-to-end and kill-switch tests (build kod-llm-proxy and kod-sandbox-test first, see .github/workflows/ci.yml)
KOD_API_KEY=dummy KOD_DOCKER_TESTS=1 cargo test -p kod-eval --test docker --test kill_switch --test kod_loop
```

Every change goes through a pull request.
