# Kod architecture (C4)

C4 views of Kod, as Mermaid. Levels 1 to 3 are here; Level 4 (code) is the Rust docs (`cargo doc --workspace --no-deps`). Elements marked **planned** are not built yet: `kod-host` is specified by [sub-plan 6b-b](superpowers/plans/2026-10-23-phase1-connected-host.md) and accepted by [6b-c](superpowers/plans/2026-10-23-phase1-connected-acceptance.md). Status of every sub-plan is in the [phase 1 overview](superpowers/plans/2026-10-14-phase1-overview.md). Rationale is in the [design doc](kod-harness-design.md).

## Level 1: System context

```mermaid
C4Context
  title Kod: system context
  Person(eng, "Engineer / requester", "Delegates tickets, reviews draft PRs")
  Person(po, "Product owner / approver", "Approves upstream docs")
  Person(kill, "Owner, backup, tech lead", "Kill switch; owner and backup also resume")
  Person(aud, "Auditor", "Verifies audit log and checkpoints")
  System(kod, "Kod harness", "Runs agents on tickets, publishes draft PRs")
  System_Ext(gh, "GitHub", "Repos, Issues, Actions, PR rules")
  System_Ext(llm, "Model providers", "Reached only via the LLM gateway")
  System_Ext(gdocs, "Google Docs (planned)", "Upstream drafts, sub-plan 9")
  System_Ext(cp, "Off-host checkpoint store", "Write-once audit checkpoints")
  Rel(eng, gh, "Labels tickets, reviews PRs")
  Rel(po, gdocs, "Approves docs")
  Rel(kill, kod, "Stop and resume, signed CLI")
  Rel(aud, cp, "Reads checkpoints")
  Rel(gh, kod, "Signed webhooks")
  Rel(kod, gh, "Comments, draft PR, checks")
  Rel(kod, llm, "Model calls")
  Rel(kod, gdocs, "Read approved docs (planned)")
  Rel(kod, cp, "Writes checkpoints")
```

Kod receives signed GitHub webhooks, runs an agent in a sandbox and publishes only a draft PR to its own `agent/<ticket>` branch. Humans own every merge; nothing auto-merges.
Every external operation (GitHub, model, docs) goes through a gateway that checks policy, injects a short-lived credential and records the decision. The sandbox holds no long-lived credentials.
Audit checkpoints go to a store the harness host cannot alter, so an auditor can detect tampering or truncation.
Google Docs is planned (sub-plan 9, not started). Until then approved documents are local trusted exports.

## Level 2: Containers

```mermaid
C4Container
  title Kod: containers and trust boundaries
  Person(eng, "Engineer", "Requester")
  Person(kill, "Owner / backup / tech lead", "Kill switch")
  Person(aud, "Auditor", "Verifies audit")
  System_Ext(gh, "GitHub", "Issues, repos, Actions")
  System_Ext(llm, "Model providers", "")
  System_Ext(cp, "Checkpoint store", "Off-host, write-once")

  Boundary(untrusted, "Untrusted local execution") {
    Container(sandbox, "Run sandbox", "Docker, gVisor planned", "Agent, shell, builds, tests; no secrets")
    Container(runner, "kod-loop / runner", "Rust, pluggable", "Agent loop; Claude Code also conforms")
    Container(collector, "Collector", "Docker, no network", "Clean-base patch export after run")
  }

  Boundary(gw, "Gateway-mediated external operations") {
    Container(host, "kod-host (planned)", "Rust binary", "Composes dispatcher and both gateways; see Level 3")
    Container(toolgw, "Tool gateway", "kod-tool-gateway, MCP", "Tracker, docs, publish; run tokens")
    Container(llmgw, "LLM gateway", "kod-llm-proxy", "Budgets, routing, redacted capture")
    Container(disp, "Dispatcher", "kod-dispatcher", "Webhook, WIP, run states, kill")
    ContainerDb(dispdb, "Dispatcher DB", "SQLite", "Queue, runs, follow-ups")
  }

  Boundary(trusted, "Trusted zone, no agent code") {
    Container(policy, "kod-policy", "In-process lib, Rego", "Allow or deny, default deny")
    ContainerDb(auditdb, "Audit log", "SQLite, hash chain", "Append-only evidence")
    Container(verifier, "Audit verifier", "kod-audit, off-host", "verify, checkpoint capture")
    Container(gates, "Gate runner", "kod-gates, separate host", "Tests, scans, required checks")
    Container(status, "kod-status", "CLI", "Computed story status")
    Container(evalh, "kod-eval", "Offline harness", "Cases, scoring, compare")
  }

  Rel(gh, disp, "Signed webhooks", "HTTPS")
  Rel(eng, gh, "Labels, reviews")
  Rel(disp, dispdb, "Reads, writes")
  Rel(host, disp, "Hosts, implements Gateway")
  Rel(host, toolgw, "Mounts")
  Rel(host, llmgw, "Mounts")
  Rel(host, sandbox, "Starts, stops, opaque tokens")
  Rel(sandbox, runner, "Runs")
  Rel(runner, toolgw, "MCP tool calls")
  Rel(runner, llmgw, "Model calls")
  Rel(collector, host, "Patch output")
  Rel(toolgw, gh, "Publish, comments")
  Rel(llmgw, llm, "Upstream calls")
  Rel(disp, policy, "Checks")
  Rel(toolgw, policy, "Checks")
  Rel(llmgw, policy, "Checks")
  Rel(disp, auditdb, "Appends")
  Rel(toolgw, auditdb, "Appends")
  Rel(llmgw, auditdb, "Appends")
  Rel(auditdb, cp, "Checkpoints")
  Rel(verifier, cp, "Reads")
  Rel(verifier, auditdb, "Verifies")
  Rel(aud, verifier, "Runs")
  Rel(gates, gh, "Required checks")
  Rel(gates, auditdb, "Appends")
  Rel(status, dispdb, "Reads")
  Rel(status, auditdb, "Reads")
  Rel(kill, disp, "Signed CLI")
  Rel(evalh, runner, "Runs offline cases")
```

Untrusted zone: only the sandbox, runner and collector run agent or repo code. They get opaque gateway tokens, no GitHub or model keys, no host filesystem or Docker socket, and an egress allowlist. Today the offline harness (`kod-eval`) starts them; `kod-host` taking over that role is planned.
Gateway zone: every external write passes policy, then an audit intent, then the call. Policy is a library inside each gateway and the dispatcher, not a service. The tool and LLM gateways exist today and run in one process; `kod-host` (planned, 6b-b) is the composition binary, and 6b-a adds the GitHub broker and forge to the tool gateway.
Trusted zone: gates run on a separate host and never share a sandbox or cache. The audit chain is checked off-host against write-once checkpoints. `kod-status` reads records and has no store of its own; `kod-eval` is offline and not part of the runtime path.
Not drawn: the owner's and backup's kill switch keeps working when policy or audit is down; there is no third database. Gate scanners run in their own images (`images/gates`).

## Level 3: Components of kod-host (planned)

```mermaid
C4Component
  title kod-host components (planned, sub-plan 6b-b)
  Container(disp, "Dispatcher", "kod-dispatcher", "Queue and run states")
  Container(toolgw, "Tool gateway", "kod-tool-gateway", "MCP, broker, publish")
  Container(llmgw, "LLM gateway", "kod-llm-proxy", "Budgets, routing")
  ContainerDb(dispdb, "Dispatcher DB", "SQLite", "Existing store")
  Container(sandbox, "Run sandbox", "Docker", "Agent execution")
  Container_Ext(gh, "GitHub", "API")

  Container_Boundary(host, "kod-host (planned)") {
    Component(cfg, "config, deadlines", "Rust", "Strict config; fixed defaults")
    Component(runtime, "runtime, binding", "Rust", "Implements Gateway; run bindings")
    Component(runner, "runner, workspace", "Rust", "Start sandbox; trusted collection")
    Component(art, "artifacts, records", "Rust", "Approved doc pins; trusted records")
    Component(svc, "service, refresh", "Rust", "Startup, shutdown, status refresh")
    Component(cred, "tracker_credentials", "Rust", "Renewable GitHub App token")
  }

  Rel(cfg, svc, "Configures")
  Rel(svc, disp, "Serves, retires runs on restart")
  Rel(svc, toolgw, "Mounts router")
  Rel(svc, llmgw, "Mounts router")
  Rel(runtime, disp, "Gateway impl")
  Rel(runtime, dispdb, "Bindings, spend, intents")
  Rel(runtime, toolgw, "Registers run tokens")
  Rel(runtime, llmgw, "Registers, revokes budgets")
  Rel(runtime, runner, "Launch, stop")
  Rel(runner, sandbox, "Docker, collector")
  Rel(art, toolgw, "Admitted doc pins")
  Rel(svc, art, "Resolves approvals")
  Rel(svc, cred, "Token renewal")
  Rel(cred, gh, "App JWT, installation token")
```

`kod-host` is an assembly binary, not a new domain service: it reuses the existing routers and libraries and adds concrete adapters. Component names are the planned source files in the 6b-b plan; the plan, not this diagram, is authoritative if they diverge.
Enforced here: the dispatcher row is the sole durable stop flag; revocation starts before waiting for stop; no execution resumes after restart (active runs become NeedsHuman, `publishing` resolves only if the exact draft PR exists).
Also enforced: collection runs only after the process is quiescent, and refuses binary files, symlinks, renames and mode changes before any forge write. Admitted document revisions are pinned and a live issue edit cannot widen them.
The additive tables (`run_bindings`, `model_requests`, `tracker_write_intents`) live in the existing dispatcher database.
