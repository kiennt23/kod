# Kod

Kod is an agent harness for a regulated-industry SDLC. Phase 0 is built: an
**offline eval harness** that replays real past tickets against an agent in a
locked-down sandbox and scores the result, before any agent touches a live
repository.

## Docs

| Doc | What it covers |
| --- | --- |
| [Kod: an agent harness for our SDLC](docs/kod-harness-design.md) | Harness architecture, PR flow, rework and staleness, pluggable models/trackers/runners, Rust components, policies, roadmap |
| [DevSecOps SDLC with AI Agents](docs/devsecops-sdlc.md) | Stages, gates, rework loops, stage outputs, agent roster, control architecture, threats, risks, metrics, rollout |
| [Phase 0 runbook](docs/phase0-runbook.md) | Host setup, pinned images, smoke test, baseline, kill-switch drill, exit criteria |
| [Phase 1 gate runner](docs/gates-runbook.md) | Trusted checks, isolated scanners, GitHub rules and activation requirements |
| [Building the eval set](docs/eval-set.md) | How to turn merged changes into cases, test commands per language, adversarial cases |
| [Pinned versions](evals/VERSIONS.md) | Every version a baseline depends on |
| [ADR 0001](docs/adr/0001-per-task-execution-mode.md) | How implementation plans are executed and reviewed |

## What's in the repo

- `crates/kod-eval`: the `kod-eval` CLI (cases, runs, scoring, reports, kill switch).
- `crates/kod-llm-proxy`: the only way out of the sandbox. It holds the model key and enforces the allowed models, token cap, request budget and concurrency.
- `images/sandbox`, `images/proxy`: container images.
- `crates/kod-loop`: our agent loop, a runner built into every sandbox image (`--runner kod-loop`).

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

Status: phase 0 code complete; the first baseline (real cases, real model) is still to run.
