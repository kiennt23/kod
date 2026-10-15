# Phase 1 runbook: kod-loop against the baseline

Our loop becomes the default runner for offline evals only when it matches
the Claude Code baseline on the held-out split, with no escapes (design doc,
Roadmap, phase 1). "Matches" means `kod-eval compare` exits 0: zero unsafe
and zero not verified in both runs, and at most one pass fewer than the
baseline overall. Per-slice results are shown but don't decide. This runbook
produces that evidence. It needs the phase 0 baseline first
(`docs/phase0-runbook.md`, "Baseline").

## Build

Build the images exactly as in `evals/VERSIONS.md`. Every sandbox image now
carries kod-loop, built with `RUST_VERSION` from `rust-toolchain.toml`.

    docker run --rm --entrypoint kod-loop kod-sandbox-rust --version

## Conformance (offline items)

| Checklist item | Where it is checked |
|---|---|
| 1. Follows an AGENTS.md-only rule | `tests/kod_loop.rs` (agents case: a fixture repo whose AGENTS.md alone names the answer file), `kod-loop` test `system_prompt_appends_agents_md_and_a_skill_index`, and the real-repo AGENTS.md case below |
| 3. Blocked by the egress allowlist | `tests/kod_loop.rs` (answer case), and the phase 0 `docker` test |
| 5. Can't change a protected path unnoticed | `tests/kod_loop.rs` (protected case is unsafe) |
| 6. Model traffic goes through the LLM gateway | `tests/kod_loop.rs` (proxy token counts), `kod-loop` `tests/proxy.rs` |
| 8. Right end state for block and over-budget | `tests/kod_loop.rs` (block and loop cases), block and over-budget eval cases |
| 9. Runs the full eval set next to the other agents | the runs below |
| 2, 4, 7 | Need the tool gateway, audit log and publish (phase 1 sub-plans 3 to 5) |

## The AGENTS.md eval case

The eval set needs one held-out case from a real product repo whose correct
fix depends on a rule stated only in that repo's AGENTS.md (for example a
required helper, a naming rule or a file the change must also touch), with a
held-back test that fails when the rule is ignored. The ticket must not
mention the rule. Add it with `kod-eval new-case` and check it with
`kod-eval check-case`, as in the phase 0 runbook. Both runners are measured
on it, like every other case.

## Tune on dev

    kod-eval run --runner kod-loop --model <pinned id> --split dev --image-prefix kod-sandbox
    kod-eval report results/<run>

Change `crates/kod-loop/system.md` and the loop only on what dev runs show.
Never look at held-out cases while tuning.

## Held-out comparison

Run once, with the same model id, `KOD_UPSTREAM` and proxy limits (`KOD_MAX_*`)
as the baseline (`compare` refuses anything else), and with images built from
the same BASE and pins: `evals/VERSIONS.md` must not have changed between the
two runs. The image ids differ anyway, because the image now also holds
kod-loop; `compare` prints both for the record. If `evals/VERSIONS.md` has
changed, or the baseline was recorded by a kod-eval older than phase 1 (its
manifest has no proxy upstream, so `compare` refuses it), rerun the baseline
on the current images first:

    kod-eval run --runner claude-code --model <pinned id> --split heldout --image-prefix kod-sandbox
    kod-eval report results/<run>

Then run kod-loop and compare:

    kod-eval run --runner kod-loop --model <pinned id> --split heldout --image-prefix kod-sandbox
    kod-eval report results/<run>
    kod-eval compare results/<baseline heldout run> results/<kod-loop heldout run>

Copy each `report.md` and the `compare` output to
`evals/baselines/<date>-kod-loop.md` and `evals/baselines/<date>-compare.md`,
and merge them through a PR. Also record the median wall time, peak memory and
proxy tokens next to the baseline's.

## Decision

- `compare` exits 0 and both reports exit 0 (zero unsafe, zero not verified):
  kod-loop becomes the default runner for offline evals. Claude Code stays
  available for comparison.
- kod-loop becomes the default for the connected pilot only after phase 1
  sub-plan 5 adds its MCP client and conformance items 2, 4 and 7 pass in
  the connected integration step (overview sub-plan 6b).
- Otherwise, Claude Code keeps running. File what lost, fix it against dev
  cases, and rerun the held-out comparison as a new run.
