# ADR 0001: Choose the execution mode per task

- Status: Proposed
- Date: 2026-10-13
- Deciders: Kien Nguyen (harness owner)
- Applies to: implementation plans under `docs/superpowers/plans/`, first used in the [phase 0 plan](../superpowers/plans/2026-10-12-phase0-offline-evals.md)

## Context

An agent can implement a plan in two ways:

- **Subagent + review:** a fresh subagent implements one task, then a fresh reviewer checks it before the next task starts.
- **Native:** the orchestrating session implements the tasks itself.

Both end with one review of the whole branch.

Subagent + review catches mistakes inside the task that made them, before later tasks build on them, and each task starts with a clean context. It costs more and is slower, because every task and every review starts cold.

Native is cheaper and faster. Mistakes surface only in the final review, and the context grows over the whole run.

One mode for the whole plan is wrong either way. The phase 0 plan mixes scaffolding, where a mistake fails the next task's tests, with safety controls, where a silent mistake makes the baseline look clean when it isn't. The kill switch, read-only scoring, trusted diff and proxy limits are examples of the second kind.

## Decision

Each plan sets an execution mode for every task in an **Execution** table in its header.

- **Rule:** a task gets Subagent + review if it owns a safety guarantee or a Review Focus test. Every other task runs Native.
- Every plan ends with one whole-branch review, whatever modes its tasks use.
- A task can move to the stricter mode at any time. It moves to the lighter mode only with the owner's agreement.
- Changes go through a branch and PR in either mode. Nothing is pushed to `main`.

For the phase 0 plan, Tasks 2 to 7 use Subagent + review: proxy, sandbox, workspaces, run loop, report and check-case. Tasks 1 and 8 run Native: scaffolding, plus the runbook with its human-run baseline.

## Consequences

- The tasks where a silent mistake costs the most get reviewed while the mistake is still one task's problem.
- The savings depend on the plan. In phase 0, 6 of 8 tasks own a safety test, so the cost is close to running every task as Subagent + review. Plans made mostly of scaffolding will save more.
- Plan authors have to name each task's mode and the reason for it. A missing mode is a plan defect.
- The rule depends on the Review Focus list being right. A safety test missing from that list also takes away its task's review.

## Alternatives considered

- **Subagent + review for every task:** the safest option, but it spends review rounds on scaffolding and docs, where the next task's tests already catch mistakes.
- **Native for every task:** cheapest, but problems in safety controls surface only at the final review, after later tasks have been built on top of them.
- **Native, with reviews at milestones:** fewer review rounds. A milestone still bundles several safety tasks into one review, so it loses the "one task's problem" property.
