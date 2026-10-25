# Kod architecture (C4)

C4 views of Kod, as Mermaid flowcharts drawn in C4 style. Levels 1 to 3 are here; Level 4 (code) is the Rust docs (`cargo doc --workspace --no-deps`). Elements marked **planned** are not built yet. `kod-host` ([sub-plan 6b-b](superpowers/plans/2026-10-23-phase1-connected-host.md), [runbook](kod-host-runbook.md)) is built; its live acceptance is [6b-c](superpowers/plans/2026-10-23-phase1-connected-acceptance.md). Status of every sub-plan is in the [phase 1 overview](superpowers/plans/2026-10-14-phase1-overview.md). Rationale is in the [design doc](kod-harness-design.md).

## Level 1: System context

```mermaid
---
title: "Kod: system context"
---
flowchart TB
  eng["<b>Engineer / requester</b><br/>Person<br/>Delegates tickets, reviews draft PRs"]
  po["<b>Product owner / approver</b><br/>Person<br/>Approves upstream docs"]
  kill["<b>Owner, backup, tech lead</b><br/>Person<br/>Kill switch, owner and backup also resume"]
  aud["<b>Auditor</b><br/>Person<br/>Verifies audit log and checkpoints"]
  kod["<b>Kod harness</b><br/>Software system<br/>Runs agents on tickets, publishes draft PRs"]
  gh["<b>GitHub</b><br/>External system<br/>Repos, Issues, Actions, PR rules"]
  llm["<b>Model providers</b><br/>External system<br/>Reached only via the LLM gateway"]
  gdocs["<b>Google Docs (planned)</b><br/>External system<br/>Upstream drafts, sub-plan 9"]
  cp["<b>Off-host checkpoint store</b><br/>External system<br/>Write-once audit checkpoints"]
  eng -->|"Labels tickets, reviews PRs"| gh
  po -.->|"Approves docs (planned)"| gdocs
  kill -->|"Stop and resume, signed CLI"| kod
  aud -->|"Reads checkpoints"| cp
  gh -->|"Signed webhooks"| kod
  kod -->|"Comments, draft PR, checks"| gh
  kod -->|"Model calls"| llm
  kod -.->|"Read approved docs (planned)"| gdocs
  kod -->|"Writes checkpoints"| cp
  classDef person fill:#08427b,stroke:#052e56,color:#fff
  classDef internal fill:#1168bd,stroke:#0b4884,color:#fff
  classDef ext fill:#6b6b6b,stroke:#444,color:#fff
  classDef planned stroke-dasharray:5 5,stroke-width:2px
  class eng,po,kill,aud person
  class kod internal
  class gh,llm,gdocs,cp ext
  class gdocs planned
```

Kod receives signed GitHub webhooks, runs an agent in a sandbox and publishes only a draft PR to its own `agent/<ticket>` branch. Humans own every merge; nothing auto-merges.
Every external operation (GitHub, model, docs) goes through a gateway that checks policy, injects a short-lived credential and records the decision. The sandbox holds no long-lived credentials.
Audit checkpoints go to a store the harness host cannot alter, so an auditor can detect tampering or truncation.
Google Docs is planned (sub-plan 9, not started). Until then approved documents are local trusted exports.

## Level 2: Containers

Split in two views so each stays readable: 2a is the runtime path, 2b is control and evidence.

### Level 2a: Runtime path

```mermaid
---
title: "Kod: containers, runtime path"
---
flowchart TB
  eng["<b>Engineer</b><br/>Person<br/>Requester"]
  kill["<b>Owner / backup / tech lead</b><br/>Person<br/>Kill switch"]
  gh["<b>GitHub</b><br/>External system<br/>Issues, repos, Actions"]
  llm["<b>Model providers</b><br/>External system"]

  subgraph untrusted["Untrusted local execution"]
    sandbox["<b>Run sandbox</b><br/>[Docker, gVisor planned]<br/>Agent, shell, builds, tests, no secrets"]
    runner["<b>kod-loop / runner</b><br/>[Rust, pluggable]<br/>Agent loop, Claude Code also conforms"]
    collector["<b>Collector</b><br/>[Docker, no network]<br/>Clean-base patch export after run"]
  end

  subgraph gw["Gateway-mediated external operations"]
    host["<b>kod-host</b><br/>[Rust binary]<br/>Composes dispatcher and both gateways, see Level 3"]
    toolgw["<b>Tool gateway</b><br/>[kod-tool-gateway, MCP]<br/>Tracker, docs, publish, run tokens"]
    llmgw["<b>LLM gateway</b><br/>[kod-llm-proxy]<br/>Budgets, routing, redacted capture"]
    disp["<b>Dispatcher</b><br/>[kod-dispatcher]<br/>Webhook, WIP, run states, kill"]
    dispdb[("<b>Dispatcher DB</b><br/>[SQLite]<br/>Queue, runs, follow-ups")]
  end

  gh -->|"Signed webhooks, HTTPS"| disp
  eng -->|"Labels, reviews"| gh
  kill -->|"Signed CLI"| disp
  disp -->|"Reads, writes"| dispdb
  host -.->|"Hosts, implements Gateway"| disp
  host -.->|"Mounts"| toolgw
  host -.->|"Mounts"| llmgw
  host -.->|"Starts, stops, opaque tokens"| sandbox
  sandbox -->|"Runs"| runner
  runner -->|"MCP tool calls"| toolgw
  runner -->|"Model calls"| llmgw
  collector -.->|"Patch output"| host
  toolgw -->|"Publish, comments"| gh
  llmgw -->|"Upstream calls"| llm
  classDef person fill:#08427b,stroke:#052e56,color:#fff
  classDef internal fill:#1168bd,stroke:#0b4884,color:#fff
  classDef db fill:#1168bd,stroke:#0b4884,color:#fff
  classDef ext fill:#6b6b6b,stroke:#444,color:#fff
  classDef planned stroke-dasharray:5 5,stroke-width:2px
  class eng,kill person
  class sandbox,runner,collector,host,toolgw,llmgw,disp internal
  class dispdb db
  class gh,llm ext
```

Untrusted zone: only the sandbox, runner and collector run agent or repo code. They get opaque gateway tokens, no GitHub or model keys, no host filesystem or Docker socket, and an egress allowlist. The offline harness (`kod-eval`) starts them for evals; `kod-host` starts them for connected runs.
Gateway zone: every external write passes policy, then an audit intent, then the call. Policy is a library inside each gateway and the dispatcher, not a service. The tool and LLM gateways exist today and run in one process; `kod-host` (6b-b) is the composition binary, and 6b-a adds the GitHub broker and forge to the tool gateway.
Policy (`kod-policy`) and the audit log are not drawn here; policy and audit wiring: see 2b.

### Level 2b: Control and evidence

```mermaid
---
title: "Kod: containers, control and evidence"
---
flowchart TB
  aud["<b>Auditor</b><br/>Person<br/>Verifies audit"]
  gh["<b>GitHub</b><br/>External system<br/>Issues, repos, Actions"]
  cp["<b>Checkpoint store</b><br/>External system<br/>Off-host, write-once"]

  subgraph untrusted["Untrusted local execution"]
    runner["<b>kod-loop / runner</b><br/>[Rust, pluggable]<br/>Agent loop, Claude Code also conforms"]
  end

  subgraph gw["Gateway-mediated external operations"]
    disp["<b>Dispatcher</b><br/>[kod-dispatcher]<br/>Webhook, WIP, run states, kill"]
    toolgw["<b>Tool gateway</b><br/>[kod-tool-gateway, MCP]<br/>Tracker, docs, publish, run tokens"]
    llmgw["<b>LLM gateway</b><br/>[kod-llm-proxy]<br/>Budgets, routing, redacted capture"]
    dispdb[("<b>Dispatcher DB</b><br/>[SQLite]<br/>Queue, runs, follow-ups")]
  end

  subgraph trusted["Trusted zone, no agent code"]
    policy["<b>kod-policy</b><br/>[In-process lib, Rego]<br/>Allow or deny, default deny"]
    auditdb[("<b>Audit log</b><br/>[SQLite, hash chain]<br/>Append-only evidence")]
    verifier["<b>Audit verifier</b><br/>[kod-audit, off-host]<br/>verify, checkpoint capture"]
    gates["<b>Gate runner</b><br/>[kod-gates, separate host]<br/>Tests, scans, required checks"]
    status["<b>kod-status</b><br/>[CLI]<br/>Computed story status"]
    evalh["<b>kod-eval</b><br/>[Offline harness]<br/>Cases, scoring, compare"]
  end

  disp & toolgw & llmgw -->|"Checks"| policy
  disp & toolgw & llmgw -->|"Appends"| auditdb
  auditdb -->|"Checkpoints"| cp
  verifier -->|"Reads"| cp
  verifier -->|"Verifies"| auditdb
  aud -->|"Runs"| verifier
  gates -->|"Required checks"| gh
  gates -->|"Appends"| auditdb
  status -->|"Reads"| dispdb
  status -->|"Reads"| auditdb
  evalh -->|"Runs offline cases"| runner
  classDef person fill:#08427b,stroke:#052e56,color:#fff
  classDef internal fill:#1168bd,stroke:#0b4884,color:#fff
  classDef db fill:#1168bd,stroke:#0b4884,color:#fff
  classDef ext fill:#6b6b6b,stroke:#444,color:#fff
  class aud person
  class runner,disp,toolgw,llmgw,policy,verifier,gates,status,evalh internal
  class dispdb,auditdb db
  class gh,cp ext
```

Trusted zone: gates run on a separate host and never share a sandbox or cache. The audit chain is checked off-host against write-once checkpoints. `kod-status` reads records and has no store of its own; `kod-eval` is offline and not part of the runtime path.
Not drawn: the owner's and backup's kill switch path bypasses policy and keeps working when policy or audit is down; there is no third database. Gate scanners run in their own images (`images/gates`).

## Level 3: Components of kod-host

```mermaid
---
title: "kod-host components (sub-plan 6b-b)"
---
flowchart TB
  disp["<b>Dispatcher</b><br/>[kod-dispatcher]<br/>Queue and run states"]
  toolgw["<b>Tool gateway</b><br/>[kod-tool-gateway]<br/>MCP, broker, publish"]
  llmgw["<b>LLM gateway</b><br/>[kod-llm-proxy]<br/>Budgets, routing"]
  dispdb[("<b>Dispatcher DB</b><br/>[SQLite]<br/>Existing store")]
  sandbox["<b>Run sandbox</b><br/>[Docker]<br/>Agent execution"]
  gh["<b>GitHub</b><br/>External system<br/>API"]

  subgraph khost["kod-host"]
    cfg["<b>config, deadlines</b><br/>[Rust]<br/>Strict config, fixed defaults"]
    runtime["<b>runtime, binding</b><br/>[Rust]<br/>Implements Gateway, run bindings"]
    hostrunner["<b>runner, workspace</b><br/>[Rust]<br/>Start sandbox, trusted collection"]
    art["<b>artifacts, records</b><br/>[Rust]<br/>Approved doc pins, trusted records"]
    svc["<b>service, refresh</b><br/>[Rust]<br/>Startup, shutdown, status refresh"]
    cred["<b>tracker_credentials</b><br/>[Rust]<br/>Renewable GitHub App token"]
  end

  cfg -.->|"Configures"| svc
  svc -.->|"Serves, retires runs on restart"| disp
  svc -.->|"Mounts router"| toolgw
  svc -.->|"Mounts router"| llmgw
  runtime -.->|"Gateway impl"| disp
  runtime -.->|"Bindings, spend, intents"| dispdb
  runtime -.->|"Registers run tokens"| toolgw
  runtime -.->|"Registers, revokes budgets"| llmgw
  runtime -.->|"Launch, stop"| hostrunner
  hostrunner -.->|"Docker, collector"| sandbox
  art -.->|"Admitted doc pins"| toolgw
  svc -.->|"Resolves approvals"| art
  svc -.->|"Token renewal"| cred
  cred -.->|"App JWT, installation token"| gh
  classDef internal fill:#1168bd,stroke:#0b4884,color:#fff
  classDef component fill:#85bbf0,stroke:#5d82a8,color:#000
  classDef db fill:#1168bd,stroke:#0b4884,color:#fff
  classDef ext fill:#6b6b6b,stroke:#444,color:#fff
  classDef planned stroke-dasharray:5 5,stroke-width:2px
  class disp,toolgw,llmgw,sandbox internal
  class dispdb db
  class gh ext
  class cfg,runtime,hostrunner,art,svc,cred component
```

`kod-host` is an assembly binary, not a new domain service: it reuses the existing routers and libraries and adds concrete adapters. Component names are the source files in `crates/kod-host/src`.
Enforced here: the dispatcher row is the sole durable stop flag; revocation starts before waiting for stop; no execution resumes after restart (active runs become NeedsHuman, `publishing` resolves only if the exact draft PR exists).
Also enforced: collection runs only after the process is quiescent, and refuses binary files, symlinks, renames and mode changes before any forge write. Admitted document revisions are pinned and a live issue edit cannot widen them.
The additive tables (`run_bindings`, `model_requests`, `tracker_write_intents`) live in the existing dispatcher database.
