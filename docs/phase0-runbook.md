# Phase 0 offline runbook

## Host

- A dedicated Linux host. Membership in the docker group is root-equivalent,
  so nothing else runs there.
- Check cgroup v2 with `stat -fc %T /sys/fs/cgroup`, which must print
  `cgroup2fs`, and a kernel of 5.19 or newer for `memory.peak`.
- Docker and git installed.
- The model key lives in the host's secret store and is exported as
  `KOD_API_KEY` only in the shell that runs `kod-eval`.
- Use a key for a provider workspace with a monthly spend limit set at the
  provider. The proxy's per-case limits (`KOD_MAX_REQUESTS`, `KOD_MAX_TOKENS`,
  `KOD_MAX_CONCURRENT`, `KOD_MAX_BODY`) bound one case; the provider limit
  bounds everything, including a bug in the proxy.

## Build pinned images

Record every version in `evals/VERSIONS.md`.

    RUST=$(sed -n 's/^channel = "\([^"]*\)".*/\1/p' rust-toolchain.toml)
    docker build -t kod-llm-proxy --build-arg RUST_VERSION=$RUST -f images/proxy/Dockerfile .
    # The sandbox image: use the exact command and versions in evals/VERSIONS.md.

Build one sandbox image per product language and pin its base image by digest.
Claude Code runs on the Node.js the Dockerfile installs from nodejs.org
(`NODE_VERSION`, checked against its SHASUMS256.txt), because distribution
Node.js is too old. Check it with
`docker run --rm --entrypoint claude kod-sandbox-rust --version` and
`docker run --rm --entrypoint cargo kod-sandbox-rust nextest --version`.

## Memory

The case template's 6g memory limit (8g for the collect container, which mounts
one more tmpfs) can exceed the VM of a small Docker Desktop or OrbStack install.
If the VM has less available memory than the container limit, the VM may run
out of memory before the container reaches its configured limit, so the run is
bounded by VM capacity rather than the case budget, and neighboring containers
can be affected. Allocate enough VM memory for the collector, concurrent
services and OS overhead, or lower `memory` and `disk` in the cases. Run real
evals on the dedicated Linux host.

## Smoke test

1. Create one trivial case, then run `kod-eval check-case evals/cases/<id>`
   until it reports `ok`; it records `required_tests`, and `run` refuses a
   functional case without them. Then run it alone:
   `kod-eval run --only <id> --model <pinned model id>`.
2. In `results/<run>/<id>/out/agent.json`, check that `subtype`, `is_error`,
   `result` and `total_cost_usd` exist. If the field names differ in the pinned
   version, update `Runner::end_state` and its tests first.
3. In `results/<run>/<id>/proxy.log`, check that `/v1/messages` requests are
   `allow`. If a denied path is one Claude Code needs, decide explicitly whether
   to add it to `KOD_ALLOWED_PATHS`, and record the decision. If requests are
   `deny_model`, Claude Code is calling a second model (such as a small one for
   background work): either pass it with `--extra-model` or configure Claude
   Code not to, and record the decision. If requests are `bad_request`, the
   usual cause is Claude Code asking for a `max_tokens` above the proxy's cap
   (`KOD_MAX_TOKENS`, default 8192). Either raise `KOD_MAX_TOKENS` in the
   environment `kod-eval run` starts from, or lower Claude Code's request with
   `CLAUDE_CODE_MAX_OUTPUT_TOKENS` in `Runner::env`, and record the decision.
   Each allowed `/v1/messages` request with a 2xx response also gets a
   `usage` line when its response ends, with `input_tokens`, `output_tokens`,
   `cache_creation_input_tokens` and `cache_read_input_tokens` (null when the
   response reported none; `output_tokens` is also null for a stream that
   ended before `message_stop`). Check that they are numbers. Refused requests
   count against `KOD_MAX_REQUESTS`; only the first request past it is logged,
   as `over_budget`.
4. Check that the result says `safety: verified`. Anything else means evidence
   is missing; fix that before going on.

## Baseline

1. Build the eval set following `docs/eval-set.md`, and run `check-case` on
   every case. `run` refuses a functional case that has no `required_tests`.
2. Tune prompts, AGENTS.md and skills against the dev split only:
   `kod-eval run --split dev --model <id>`, then `kod-eval report results/<run>`.
3. Run the held-out split once: `kod-eval run --split heldout --model <id>`.
4. Copy both reports to `evals/baselines/<date>-claude-code.md`, with the
   versions used.
5. Put the numbers in the Kod doc's Evals section, and replace the resource
   hypotheses in Language choice with the measured figures.

## Kill-switch drill

1. Start a run on the dev split with at least three cases.
2. During the first case, run `kod-eval stop` from another shell, and time it.
3. Check that `docker ps --filter label=kod.run` stays empty for a minute, that
   the run exited, and that every remaining case's result says `cancelled`.
4. Check that a new `kod-eval run` is refused, then delete the stop file
   (`~/.kod/STOP`, or `$KOD_STOP_FILE`).
5. Record the time and the result in the baseline file.

## Phase 0 offline exit

- At least 30 cases pass the `check-case` for their kind: a third held out,
  plus every adversarial kind in `docs/eval-set.md`.
- Baseline reports exist for both splits, with zero `unsafe` and zero
  `not_verified` cases. A report with either can't be used for the exit.
- The kill-switch drill is recorded.
- Compliance has seen the control mapping and a sample report.
