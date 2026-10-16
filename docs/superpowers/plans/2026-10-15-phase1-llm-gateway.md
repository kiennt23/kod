# Phase 1.2 LLM Gateway (`kod-llm-proxy`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the per-case `kod-llm-proxy` into one long-running LLM gateway that serves many runs, each with its own spend budget, model routing decided by kod-policy per data class, and redacted prompt and output capture in the audit log, and move `kod-eval` onto it without losing phase 0 comparability.

**Architecture:** One process, many runs. A run is known by a secret token in its base URL (`http://kod-llm-proxy:8080/r/<token>`), so neither claude-code nor kod-loop changes: they already append API paths to `ANTHROPIC_BASE_URL`. The runs, their data classes and budgets come from a `runs.json` file and the prices from a `prices.json` file, both validated at start; any fault stops the process before it prints its `ready` line. Every request is routed through `kod_policy::Policy::decide` (`model_request`), the decision is recorded with `kod_audit::AuditLog::record_decision`, the worst-case cost is reserved against the run's budget, and the redacted prompt is appended to the audit log, all before anything is forwarded. `kod-eval` starts one gateway per eval run with one gateway run per case.

**Tech Stack:** Rust 1.99.0 (edition 2024), axum 0.8, reqwest 0.12, tokio, serde_json, sha2 0.10, kod-policy (regorus), kod-audit (rusqlite). Tests: tokio tests against a fake upstream on 127.0.0.1, rusqlite to read the audit db, tempfile.

**Spec:** `docs/kod-harness-design.md` (Model routing policy, LLM gateway, Trust boundaries and the action matrix row "Model request (agent)", Audit integrity), row 2 of `docs/superpowers/plans/2026-10-14-phase1-overview.md`, `docs/adr/0001-per-task-execution-mode.md`. The audit API is the one in `docs/superpowers/plans/2026-10-15-phase1-audit.md` (sub-plan 4).

## Decisions

- **D1. One gateway, a token per run.** The run token is a path prefix, not a header, so runners need no change and a script in the sandbox gets the same treatment as the runner. A request with no known token gets 401 and no log line: it is not a run, so there is nothing to charge or record. Tokens are at least 32 lower-case hex characters; kod-eval makes 128-bit ones.
- **D2. Runs are declared at start in `runs.json`** (`KOD_RUNS`): token → id, ticket, repo, data classes, token budget, cost budget in micro-USD. That is all phase 1 needs: kod-eval knows its cases before it starts, and the loop (sub-plan 5) is in the same process later. Adding runs to a live gateway waits for sub-plan 5 to need it.
- **D3. Routing goes through kod-policy, per request.** The gateway builds the `model_request` input (actor agent, the run's id, ticket, repo, data classes, `auth_mode` `api_key`, the endpoint name) and asks `Policy::decide`. The policy is the only place routing values live; today `policy/routing.json` allows class `placeholder` on endpoint `placeholder` and nothing else, so an unknown class, a run with no class, and any other endpoint are refused. The gateway has exactly one upstream endpoint (`KOD_ENDPOINT`, default `placeholder`, plus `KOD_UPSTREAM` and `KOD_API_KEY`). More endpoints wait for the routing values.
- **D4. Every decision is recorded before the call.** `record_decision` runs for allowed and refused requests alike. If it fails, the request is refused with 503 `audit_error` and nothing is forwarded (the design's "the gateway refuses the external write").
- **D5. Budgets reserve the worst case, then settle.** Before forwarding, a request reserves body bytes + `RESERVE_SLACK` (4096) input tokens and `max_tokens` output tokens, priced at the model's input and output rates. A reservation that would take the run past either limit is refused (`over_spend`, 403). When the response ends, the reservation is replaced by the cost the response reported. A response that reported no usage is charged its whole reservation. A cost above the reservation is charged and halts the run (`halted`, 403 for every later request). `count_tokens` reserves nothing. Error responses are not charged.
- **D6. Prices are integers.** `prices.json` gives micro-USD per million tokens for `input`, `output`, `cache_write` and `cache_read`, per model; every allowed model must have a price. Spend is kept in picodollars (one token at one micro-USD per million tokens is one picodollar), so there is no rounding.
- **D7. Capture goes to kod-audit.** The prompt is appended as record type `prompts` and the response as `model_outputs`; kod-audit redacts secrets before storing. Each capture keeps the first 128 KiB (`CAPTURE_MAX`) as text plus the full length, the SHA-256 of the full bytes, and a `truncated` flag. A failed prompt write refuses the request. A failed output write cannot un-send the response, so it halts the run.
- **D8. Request count and concurrency are per run.** `KOD_MAX_REQUESTS` and `KOD_MAX_CONCURRENT` keep their phase 0 meaning, now per run rather than per process.
- **D9. kod-eval: one gateway per eval run.** It replaces one container per case. Its files (`prices.json`, `runs.json`, `audit.db`) live in `<run dir>/gateway/`, the repo's `policy/` is mounted read-only, and each case's `proxy.log` is the gateway's lines for that case. The per-case budget comes from two new required flags, `--run-max-tokens` and `--run-max-cost-micro-usd`, and the price table from the required `--prices`. There is no default budget.
- **D10. Compare stays valid.** The manifest gains `spend` (the budget and the price table). Two runs with budgets must have the same one. A phase 0 baseline has none; it compares with a budgeted run only if that run's budget refused nothing (no `over_spend` and no `halted` lines), because then the budget did not change what the agent saw.
- **D11. No OpenAI-compatible route.** Neither runner needs one.

## Execution

Per ADR 0001.

| Task | Mode | Why |
|---|---|---|
| 1. Config and budget arithmetic | Subagent + review | Owns fail-closed config and the spend arithmetic |
| 2. The gateway | Subagent + review | Owns routing, budgets, capture and the audit fail-closed guard |
| 3. kod-eval on one gateway | Subagent + review | Owns "spend can't exceed the budget whether the loop or a script calls it" in the eval |
| 4. Compare with spend budgets | Subagent + review | Owns comparability with the phase 0 baseline |

The plan ends with one whole-branch review.

## Global Constraints

- Toolchain 1.99.0 (`rust-toolchain.toml`), edition 2024, resolver 3.
- CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and cargo-deny with the existing `deny.toml`, unchanged.
- Fail closed: a bad or missing config file or setting stops the gateway before it prints `ready`; anything the policy does not allow is refused.
- Routing values are placeholders until the owner sets them: one class `placeholder`, one endpoint `placeholder`, everything else refused.
- The audit write happens before the call it records, and a failed write refuses the call.
- kod-audit (sub-plan 4) must be merged first; this plan codes against its plan's API: `AuditLog::open(&Path)`, `append(&str, &Value) -> Result<u64>`, `record_decision(&Value, &Decision) -> Result<u64>`.
- No OpenAI-compatible route.
- Commit messages are plain, with no trailers.

## Review Focus

- Two requests from one run in flight at once must not together pass the budget: `concurrent_requests_cannot_overspend_together` (Task 2).
- A response that costs more than its reservation, or reports no usage, must not let spend run past the budget unnoticed: `a_cost_above_its_reservation_is_charged_and_halts_the_run`, `unknown_usage_is_charged_the_whole_reservation` (Task 1) and `a_response_costing_more_than_its_reservation_halts_the_run` (Task 2).
- A locked or unwritable audit db must refuse the request, not forward it unrecorded: `a_failed_audit_write_refuses_the_request` (Task 2).
- Code in the sandbox calling the gateway with no token, a wrong token or a bare `/r/` must reach nothing: `an_unknown_run_is_refused_without_a_call` (Task 2).
- A phase 0 baseline compared with a run whose budget cut it short must be refused: `a_phase0_baseline_compares_only_if_the_budget_refused_nothing` (Task 4).

## File Structure

```
crates/kod-llm-proxy/
  Cargo.toml          modify: kod-audit, kod-policy, sha2; dev: rusqlite, tempfile
  src/config.rs       create: prices.json and runs.json, validated
  src/budget.rs       create: Spend, reservation, cost, Ledger
  src/lib.rs          replace: the gateway (runs, routing, budget, capture)
  src/main.rs         replace: reads the new settings, prints "ready"
  tests/common/mod.rs create: a gateway on 127.0.0.1 with a temp audit db
  tests/config.rs     create
  tests/budget.rs     create
  tests/gateway.rs    create: routing, budget, capture, audit tests
  tests/proxy.rs      modify: phase 0 tests, now through a run
crates/kod-loop/
  Cargo.toml          modify: dev-deps kod-audit, kod-policy
  tests/proxy.rs      replace: the loop against the gateway, plus a budget test
crates/kod-eval/
  src/run.rs          modify: one gateway per eval run, spend in the manifest
  src/runner.rs       modify: env takes the run's base URL
  src/main.rs         modify: --policy-dir, --prices, --run-max-tokens, --run-max-cost-micro-usd
  src/report.rs       modify: compare checks spend budgets; report shows them
  tests/{run,runner,sandbox,report,docker}.rs, tests/e2e/mod.rs  modify
README.md             modify: the eval command with prices and a budget
```

`images/proxy/Dockerfile` is unchanged: it already builds the whole workspace. This plan's own PR links it from row 2 of the overview.

---

### Task 1: Config and budget arithmetic

**Files:**
- Create: `crates/kod-llm-proxy/src/config.rs`, `crates/kod-llm-proxy/src/budget.rs`
- Modify: `crates/kod-llm-proxy/src/lib.rs` (two `pub mod` lines)
- Test: `crates/kod-llm-proxy/tests/config.rs`, `crates/kod-llm-proxy/tests/budget.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `config::Price { input, output, cache_write, cache_read: u64 }`, `config::Prices = HashMap<String, Price>`, `config::Budget { max_tokens, max_cost_micro_usd: u64 }`, `config::RunSpec { id, ticket, repo: String, data_classes: Vec<String>, budget: Budget }`.
  - `config::parse_prices(text: &str, allowed_models: &[String]) -> Result<Prices>` and `config::parse_runs(text: &str) -> Result<HashMap<String, RunSpec>>` (keyed by token).
  - `budget::Spend { tokens, pico_usd: u64 }`, `budget::RESERVE_SLACK`, `budget::reservation(&Price, body_len: usize, max_tokens: u64) -> Spend`, `budget::cost(&Price, &Map<String, Value>) -> Option<Spend>`.
  - `budget::Ledger::new(Budget)`, `reserve(&mut self, Spend) -> bool`, `release(&mut self, Spend)`, `settle(&mut self, reserved: Spend, actual: Option<Spend>) -> bool` (false once the run is halted), `halt`, `halted() -> bool`, `spent() -> Spend`.

The config files follow kod-policy's `config.rs`: exact keys (an unexpected or missing key is an error), names with no spaces at the ends, whole numbers only, and messages that name the file and the key.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-llm-proxy/tests/config.rs`:

````rust
use kod_llm_proxy::config::{Budget, Price, parse_prices, parse_runs};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn models() -> Vec<String> {
    vec!["claude-test-1".into()]
}

fn run(fields: &str) -> String {
    format!(r#"{{"{TOKEN}":{{{fields}}}}}"#)
}

const GOOD_RUN: &str = r#""id":"r1","ticket":"KOD-1","repo":"acme/app","data_classes":["placeholder"],"max_tokens":100,"max_cost_micro_usd":5"#;

fn err(r: anyhow::Result<impl std::fmt::Debug>) -> String {
    format!("{:#}", r.unwrap_err())
}

#[test]
fn reads_prices_and_runs() {
    let p = parse_prices(
        r#"{"claude-test-1":{"input":3000000,"output":15000000,"cache_write":3750000,"cache_read":300000}}"#,
        &models(),
    )
    .unwrap();
    assert_eq!(
        p["claude-test-1"],
        Price {
            input: 3_000_000,
            output: 15_000_000,
            cache_write: 3_750_000,
            cache_read: 300_000
        }
    );
    let r = parse_runs(&run(GOOD_RUN)).unwrap();
    assert_eq!(r[TOKEN].id, "r1");
    assert_eq!(r[TOKEN].data_classes, ["placeholder"]);
    assert_eq!(
        r[TOKEN].budget,
        Budget {
            max_tokens: 100,
            max_cost_micro_usd: 5
        }
    );
}

#[test]
fn refuses_bad_prices() {
    let price = r#"{"input":1,"output":1,"cache_write":1,"cache_read":1}"#;
    for (text, want) in [
        ("not json", "not JSON"),
        ("{}", "non-empty object"),
        ("[]", "non-empty object"),
        (
            &format!(r#"{{"claude-other":{price}}}"#),
            "no price for allowed model claude-test-1",
        ),
        (
            r#"{"claude-test-1":{"input":1,"output":1,"cache_write":1}}"#,
            "missing key cache_read",
        ),
        (
            r#"{"claude-test-1":{"input":1,"output":1,"cache_write":1,"cache_read":1,"batch":1}}"#,
            "unexpected key batch",
        ),
        (
            r#"{"claude-test-1":{"input":1.5,"output":1,"cache_write":1,"cache_read":1}}"#,
            "input must be a whole number",
        ),
        (
            r#"{"claude-test-1":{"input":-1,"output":1,"cache_write":1,"cache_read":1}}"#,
            "input must be a whole number",
        ),
        (
            r#"{"claude-test-1":{"input":"1","output":1,"cache_write":1,"cache_read":1}}"#,
            "input must be a whole number",
        ),
        (
            &format!(r#"{{"claude-test-1 ":{price}}}"#),
            "no spaces at the ends",
        ),
    ] {
        let e = err(parse_prices(text, &models()));
        assert!(e.contains(want), "{text}: {e}");
    }
}

#[test]
fn refuses_bad_runs() {
    for (text, want) in [
        ("{}".to_owned(), "non-empty object"),
        (
            format!(r#"{{"abc":{{{GOOD_RUN}}}}}"#),
            "at least 32 lower-case hex",
        ),
        (
            format!(r#"{{"{}":{{{GOOD_RUN}}}}}"#, TOKEN.to_uppercase()),
            "at least 32 lower-case hex",
        ),
        (
            run(&GOOD_RUN.replace(r#"["placeholder"]"#, "[]")),
            "data_classes must be a non-empty list",
        ),
        (
            run(&GOOD_RUN.replace(r#"["placeholder"]"#, r#"[""]"#)),
            "data_classes must be a non-empty list",
        ),
        (
            run(&GOOD_RUN.replace(r#"["placeholder"]"#, r#""placeholder""#)),
            "data_classes must be a non-empty list",
        ),
        (
            run(&GOOD_RUN.replace(r#""max_tokens":100"#, r#""max_tokens":0"#)),
            "must be above zero",
        ),
        (
            run(&GOOD_RUN.replace(
                r#""max_cost_micro_usd":5"#,
                r#""max_cost_micro_usd":18446744073710"#,
            )),
            "max_cost_micro_usd must be at most",
        ),
        (
            run(&GOOD_RUN.replace(r#","max_cost_micro_usd":5"#, "")),
            "missing key max_cost_micro_usd",
        ),
        (
            run(&format!("{GOOD_RUN},\"auth_mode\":\"subscription\"")),
            "unexpected key auth_mode",
        ),
        (
            run(&GOOD_RUN.replace(r#""repo":"acme/app""#, r#""repo":" acme/app""#)),
            "repo must be a non-empty name",
        ),
        (
            format!(
                r#"{{"{TOKEN}":{{{GOOD_RUN}}},"{}":{{{GOOD_RUN}}}}}"#,
                "f".repeat(32)
            ),
            "the id is used twice",
        ),
    ] {
        let e = err(parse_runs(&text));
        assert!(e.contains(want), "{text}: {e}");
    }
}
````

Create `crates/kod-llm-proxy/tests/budget.rs`:

````rust
use kod_llm_proxy::{
    budget::{Ledger, RESERVE_SLACK, Spend, cost, reservation},
    config::{Budget, Price},
};
use serde_json::{Map, Value, json};

const PRICE: Price = Price {
    input: 3,
    output: 15,
    cache_write: 4,
    cache_read: 1,
};

fn usage(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_reservation_covers_the_body_at_the_dearest_input_rate_and_max_tokens() {
    let r = reservation(&PRICE, 1000, 50);
    assert_eq!(r.tokens, 1000 + RESERVE_SLACK + 50);
    assert_eq!(r.pico_usd, (1000 + RESERVE_SLACK) * 4 + 50 * 15);
}

#[test]
fn cost_prices_each_usage_field() {
    let u = usage(json!({"input_tokens": 10, "output_tokens": 2,
        "cache_creation_input_tokens": 5, "cache_read_input_tokens": 100}));
    assert_eq!(
        cost(&PRICE, &u),
        Some(Spend {
            tokens: 117,
            pico_usd: 30 + 30 + 20 + 100
        })
    );
    // Missing cache fields count as zero; missing input or output is unknown.
    let u = usage(json!({"input_tokens": 10, "output_tokens": 2,
        "cache_creation_input_tokens": null, "cache_read_input_tokens": null}));
    assert_eq!(cost(&PRICE, &u).unwrap().tokens, 12);
    let u = usage(json!({"input_tokens": 10, "output_tokens": null}));
    assert_eq!(cost(&PRICE, &u), None);
}

fn ledger(tokens: u64, micro_usd: u64) -> Ledger {
    Ledger::new(Budget {
        max_tokens: tokens,
        max_cost_micro_usd: micro_usd,
    })
}

fn spend(tokens: u64, pico_usd: u64) -> Spend {
    Spend { tokens, pico_usd }
}

#[test]
fn reservations_never_pass_the_budget_together() {
    let mut l = ledger(100, 1);
    assert!(l.reserve(spend(60, 0)));
    // Two requests in flight can't together pass the budget.
    assert!(!l.reserve(spend(41, 0)));
    assert!(l.reserve(spend(40, 0)));
    assert!(!l.reserve(spend(1, 0)));
    // The cost limit is held in picodollars: 1 micro-USD is 1_000_000.
    let mut l = ledger(100, 1);
    assert!(!l.reserve(spend(1, 1_000_001)));
    assert!(l.reserve(spend(1, 1_000_000)));
}

#[test]
fn settling_charges_the_actual_cost_and_frees_the_rest() {
    let mut l = ledger(100, 1);
    let r = spend(80, 10);
    assert!(l.reserve(r));
    assert!(l.settle(r, Some(spend(30, 5))));
    assert_eq!(l.spent(), spend(30, 5));
    assert!(l.reserve(spend(70, 0)));
}

#[test]
fn unknown_usage_is_charged_the_whole_reservation() {
    let mut l = ledger(100, 1);
    let r = spend(80, 10);
    assert!(l.reserve(r));
    assert!(l.settle(r, None));
    assert_eq!(l.spent(), r);
    assert!(!l.reserve(spend(21, 0)));
}

#[test]
fn a_cost_above_its_reservation_is_charged_and_halts_the_run() {
    let mut l = ledger(100, 1);
    let r = spend(10, 10);
    assert!(l.reserve(r));
    assert!(!l.settle(r, Some(spend(11, 10))));
    assert_eq!(l.spent(), spend(11, 10));
    assert!(l.halted());
    assert!(!l.reserve(spend(0, 0)));
}

#[test]
fn a_released_reservation_is_not_charged() {
    let mut l = ledger(100, 1);
    let r = spend(100, 0);
    assert!(l.reserve(r));
    l.release(r);
    assert_eq!(l.spent(), Spend::default());
    assert!(l.reserve(r));
}

#[test]
fn a_halted_run_reserves_nothing() {
    let mut l = ledger(100, 1);
    l.halt();
    assert!(!l.reserve(spend(0, 0)));
}
````

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kod-llm-proxy --test config --test budget`
Expected: FAIL to compile: unresolved import `kod_llm_proxy::config` (and `budget`).

- [ ] **Step 3: Write the modules**

Create `crates/kod-llm-proxy/src/config.rs`:

````rust
//! The gateway's data files, checked at load the way kod-policy's config.rs
//! checks policy data: every value is validated before it is used, an
//! unexpected or missing key is an error, and nothing has a default. A file
//! that fails any check stops the gateway from starting.
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// Rates in micro-USD per million tokens, one per usage field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

/// Model id to price.
pub type Prices = HashMap<String, Price>;

/// What one run may spend. Tokens count all four usage fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub max_tokens: u64,
    pub max_cost_micro_usd: u64,
}

/// One run the gateway serves, keyed by its token.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSpec {
    pub id: String,
    pub ticket: String,
    pub repo: String,
    pub data_classes: Vec<String>,
    pub budget: Budget,
}

/// Run tokens are at least this many lower-case hex characters.
pub const MIN_TOKEN_LEN: usize = 32;

/// A budget above this many micro-USD would overflow when held in picodollars.
pub const MAX_COST_MICRO_USD: u64 = u64::MAX / 1_000_000;

/// A non-empty string with no spaces at the ends.
fn name(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s == s.trim())
}

/// An object with exactly the keys in `keys`.
fn exact_keys<'a>(at: &str, v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let o = v
        .as_object()
        .with_context(|| format!("{at} must be an object"))?;
    if let Some(key) = o.keys().find(|k| !keys.contains(&k.as_str())) {
        bail!("{at}: unexpected key {key}");
    }
    if let Some(key) = keys.iter().find(|k| !o.contains_key(**k)) {
        bail!("{at}: missing key {key}");
    }
    Ok(o)
}

fn int(at: &str, o: &Map<String, Value>, key: &str) -> Result<u64> {
    o[key]
        .as_u64()
        .with_context(|| format!("{at}.{key} must be a whole number"))
}

fn field(at: &str, o: &Map<String, Value>, key: &str) -> Result<String> {
    if !name(&o[key]) {
        bail!("{at}.{key} must be a non-empty name with no spaces at the ends");
    }
    Ok(o[key].as_str().unwrap_or_default().to_owned())
}

fn parse(file: &str, text: &str) -> Result<Map<String, Value>> {
    let v: Value = serde_json::from_str(text).with_context(|| format!("{file}: not JSON"))?;
    match v {
        Value::Object(o) if !o.is_empty() => Ok(o),
        _ => bail!("{file} must be a non-empty object"),
    }
}

/// prices.json: `{"<model>": {"input", "output", "cache_write", "cache_read"}}`.
/// Every allowed model needs a price, so no request is ever unpriced.
pub fn parse_prices(text: &str, allowed_models: &[String]) -> Result<Prices> {
    let mut prices = Prices::new();
    for (model, v) in parse("prices.json", text)? {
        let at = format!("prices.json: {model}");
        if !name(&Value::String(model.clone())) {
            bail!("{at}: the model id must have no spaces at the ends");
        }
        let o = exact_keys(&at, &v, &["input", "output", "cache_write", "cache_read"])?;
        let price = Price {
            input: int(&at, o, "input")?,
            output: int(&at, o, "output")?,
            cache_write: int(&at, o, "cache_write")?,
            cache_read: int(&at, o, "cache_read")?,
        };
        prices.insert(model, price);
    }
    if let Some(m) = allowed_models.iter().find(|m| !prices.contains_key(*m)) {
        bail!("prices.json: no price for allowed model {m}");
    }
    Ok(prices)
}

/// runs.json: `{"<token>": {"id", "ticket", "repo", "data_classes",
/// "max_tokens", "max_cost_micro_usd"}}`. Tokens and run ids are unique.
pub fn parse_runs(text: &str) -> Result<HashMap<String, RunSpec>> {
    let mut runs = HashMap::new();
    let mut ids = HashSet::new();
    for (token, v) in parse("runs.json", text)? {
        if token.len() < MIN_TOKEN_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!(
                "runs.json: a run token must be at least {MIN_TOKEN_LEN} lower-case hex characters"
            );
        }
        let o = exact_keys(
            "runs.json: a run",
            &v,
            &[
                "id",
                "ticket",
                "repo",
                "data_classes",
                "max_tokens",
                "max_cost_micro_usd",
            ],
        )?;
        let id = field("runs.json: a run", o, "id")?;
        let at = format!("runs.json: run {id}");
        if !ids.insert(id.clone()) {
            bail!("{at}: the id is used twice");
        }
        let classes = o["data_classes"]
            .as_array()
            .filter(|l| !l.is_empty() && l.iter().all(name))
            .with_context(|| format!("{at}.data_classes must be a non-empty list of names"))?;
        let budget = Budget {
            max_tokens: int(&at, o, "max_tokens")?,
            max_cost_micro_usd: int(&at, o, "max_cost_micro_usd")?,
        };
        if budget.max_tokens == 0 || budget.max_cost_micro_usd == 0 {
            bail!("{at}: max_tokens and max_cost_micro_usd must be above zero");
        }
        if budget.max_cost_micro_usd > MAX_COST_MICRO_USD {
            bail!("{at}.max_cost_micro_usd must be at most {MAX_COST_MICRO_USD}");
        }
        let spec = RunSpec {
            ticket: field(&at, o, "ticket")?,
            repo: field(&at, o, "repo")?,
            data_classes: classes
                .iter()
                .map(|c| c.as_str().unwrap_or_default().to_owned())
                .collect(),
            budget,
            id,
        };
        runs.insert(token, spec);
    }
    Ok(runs)
}
````

Create `crates/kod-llm-proxy/src/budget.rs`:

````rust
//! Per-run spend in tokens and picodollars (1e-12 USD). A request reserves
//! its worst case before it is forwarded and settles to what the response
//! reported once its body ends, so concurrent requests can't together pass
//! the budget.
use crate::config::{Budget, Price};
use serde_json::{Map, Value};

/// Input tokens reserved on top of the body's length, for what the API adds
/// itself (the tool-use system prompt).
// ponytail: assumes a request has at most one input token per body byte plus
// this slack; a request that breaks it halts its run (see Ledger::settle).
// Ask count_tokens first if that ever happens.
pub const RESERVE_SLACK: u64 = 4096;

/// Tokens and picodollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub tokens: u64,
    pub pico_usd: u64,
}

impl Spend {
    fn add(self, o: Spend) -> Spend {
        Spend {
            tokens: self.tokens.saturating_add(o.tokens),
            pico_usd: self.pico_usd.saturating_add(o.pico_usd),
        }
    }

    fn sub(self, o: Spend) -> Spend {
        Spend {
            tokens: self.tokens.saturating_sub(o.tokens),
            pico_usd: self.pico_usd.saturating_sub(o.pico_usd),
        }
    }

    fn exceeds(self, o: Spend) -> bool {
        self.tokens > o.tokens || self.pico_usd > o.pico_usd
    }
}

/// `tokens` at `rate` micro-USD per million tokens, in picodollars.
fn pico(tokens: u64, rate: u64) -> u64 {
    tokens.saturating_mul(rate)
}

/// The most a /v1/messages request with this body can cost: every body byte
/// a token (plus RESERVE_SLACK) at the dearest input rate, and `max_tokens`
/// of output.
pub fn reservation(price: &Price, body_len: usize, max_tokens: u64) -> Spend {
    let input = (body_len as u64).saturating_add(RESERVE_SLACK);
    let rate = price.input.max(price.cache_write).max(price.cache_read);
    Spend {
        tokens: input.saturating_add(max_tokens),
        pico_usd: pico(input, rate).saturating_add(pico(max_tokens, price.output)),
    }
}

/// What a response's usage (from `Usage::finish`) cost. None when input or
/// output tokens are missing; missing cache fields count as zero.
pub fn cost(price: &Price, usage: &Map<String, Value>) -> Option<Spend> {
    let n = |k: &str| usage.get(k).and_then(Value::as_u64);
    let (input, output) = (n("input_tokens")?, n("output_tokens")?);
    let write = n("cache_creation_input_tokens").unwrap_or(0);
    let read = n("cache_read_input_tokens").unwrap_or(0);
    Some(Spend {
        tokens: input
            .saturating_add(output)
            .saturating_add(write)
            .saturating_add(read),
        pico_usd: pico(input, price.input)
            .saturating_add(pico(output, price.output))
            .saturating_add(pico(write, price.cache_write))
            .saturating_add(pico(read, price.cache_read)),
    })
}

/// One run's spend. `spent + reserved` never passes the budget.
#[derive(Debug)]
pub struct Ledger {
    limit: Spend,
    spent: Spend,
    reserved: Spend,
    halted: bool,
}

impl Ledger {
    pub fn new(b: Budget) -> Ledger {
        Ledger {
            limit: Spend {
                tokens: b.max_tokens,
                pico_usd: b.max_cost_micro_usd.saturating_mul(1_000_000),
            },
            spent: Spend::default(),
            reserved: Spend::default(),
            halted: false,
        }
    }

    /// Holds `r` for one request. False, holding nothing, when the run is
    /// halted or `r` would take it past its budget.
    pub fn reserve(&mut self, r: Spend) -> bool {
        if self.halted || self.spent.add(self.reserved).add(r).exceeds(self.limit) {
            return false;
        }
        self.reserved = self.reserved.add(r);
        true
    }

    /// Gives back a reservation for a request that was never answered.
    pub fn release(&mut self, r: Spend) {
        self.reserved = self.reserved.sub(r);
    }

    /// Replaces reservation `r` with what the response cost, or with `r`
    /// itself when the cost is unknown. A cost above `r` is still charged and
    /// halts the run. Returns false when it halted the run.
    pub fn settle(&mut self, r: Spend, actual: Option<Spend>) -> bool {
        self.release(r);
        let charged = actual.unwrap_or(r);
        self.spent = self.spent.add(charged);
        if charged.exceeds(r) {
            self.halted = true;
        }
        !self.halted
    }

    /// Stops the run: every later request is refused.
    pub fn halt(&mut self) {
        self.halted = true;
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    pub fn spent(&self) -> Spend {
        self.spent
    }
}
````

Apply to `crates/kod-llm-proxy/src/lib.rs` with `git apply`:

````diff
diff --git a/crates/kod-llm-proxy/src/lib.rs b/crates/kod-llm-proxy/src/lib.rs
index e3e633e..9e9d4c1 100644
--- a/crates/kod-llm-proxy/src/lib.rs
+++ b/crates/kod-llm-proxy/src/lib.rs
@@ -11,6 +11,9 @@
 //! allowed /v1/messages request with a 2xx response gets a second line, event
 //! "usage", when its response body ends, with the token counts the response
 //! reported (null when it reported none or the stream ended early).
+pub mod budget;
+pub mod config;
+
 use axum::{
     Router,
     body::{Body, to_bytes},
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-llm-proxy`
Expected: PASS (config 3, budget 8, and the 12 phase 0 proxy tests unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/kod-llm-proxy
git commit -m "kod-llm-proxy: validated price and run files, and spend arithmetic"
```

### Task 2: The gateway

**Files:**
- Modify: `crates/kod-llm-proxy/Cargo.toml`, `crates/kod-llm-proxy/tests/proxy.rs`, `crates/kod-llm-proxy/tests/config.rs` (append), `crates/kod-loop/Cargo.toml`
- Replace: `crates/kod-llm-proxy/src/lib.rs`, `crates/kod-llm-proxy/src/main.rs`, `crates/kod-loop/tests/proxy.rs`
- Create: `crates/kod-llm-proxy/tests/common/mod.rs`, `crates/kod-llm-proxy/tests/gateway.rs`

**Interfaces:**
- Consumes: Task 1's `config` and `budget`; `kod_policy::Policy::{load, decide}` and `Decision`; `kod_audit::AuditLog::{open, append, record_decision}`.
- Produces:
  - `Endpoint { name, upstream: String, api_key: HeaderValue }`, `Limits` (unchanged fields), `Config { endpoint, allowed_paths, limits, prices: Prices, runs: HashMap<String, RunSpec>, policy: Arc<Policy>, audit: Arc<AuditLog> }`, `app(Config) -> Router`.
  - `route_input(&RunSpec, endpoint: &str) -> Value` (the `model_request` input), `capture(head: &[u8], bytes: usize, sha256: &str) -> Value`.
  - Consts `DEFAULT_ENDPOINT = "placeholder"`, `PROMPTS = "prompts"`, `MODEL_OUTPUTS = "model_outputs"`, `CAPTURE_MAX`.
  - Log events (JSON on stdout, each with `run`): `ready`, `allow`, `usage` (with `run_tokens`, `run_cost_pico_usd`, `run_halted`), `deny`, `deny_model`, `deny_route` (with `reasons`), `too_large`, `bad_request`, `over_budget`, `over_spend`, `halted`, `audit_error`, `upstream_error`.
  - Settings: `KOD_PRICES`, `KOD_RUNS`, `KOD_POLICY_DIR`, `KOD_AUDIT_DB` (all required), `KOD_ENDPOINT` (default `placeholder`), and the phase 0 ones unchanged.

The phase 0 proxy tests keep their meaning; they only gain a run (`/r/<TOKEN>` in the URL) and the `ready` line. kod-loop's gateway test builds the new `Config`, so it moves in the same task to keep `cargo test --all` green.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-llm-proxy/Cargo.toml` with `git apply`:

````diff
diff --git a/crates/kod-llm-proxy/Cargo.toml b/crates/kod-llm-proxy/Cargo.toml
index ae5aeb3..3fcb772 100644
--- a/crates/kod-llm-proxy/Cargo.toml
+++ b/crates/kod-llm-proxy/Cargo.toml
@@ -8,10 +8,15 @@ publish = false
 anyhow = "1"
 axum = "0.8"
 futures-util = "0.3"
+kod-audit = { path = "../kod-audit" }
+kod-policy = { path = "../kod-policy" }
 reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream"] }
 serde_json = "1"
+sha2 = "0.10"
 tokio = { version = "1", features = ["macros", "rt-multi-thread", "net", "sync", "time"] }
 
 [dev-dependencies]
-tokio = { version = "1", features = ["io-util", "process"] }
+rusqlite = "0.40"
 serde_json = "1"
+tempfile = "3"
+tokio = { version = "1", features = ["io-util", "process"] }
````

Create `crates/kod-llm-proxy/tests/common/mod.rs`:

````rust
#![allow(dead_code)]
//! A gateway on a local port with the repo's policy, an audit log in a
//! temporary directory and one run.
use axum::Router;
use kod_audit::AuditLog;
use kod_llm_proxy::{
    Config, Endpoint, Limits, app,
    config::{Budget, Price, RunSpec},
};
use kod_policy::Policy;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::TcpListener;

pub const MODEL: &str = "claude-test-1";
pub const TOKEN: &str = "0123456789abcdef0123456789abcdef";
/// The only class and endpoint the placeholder routing allows.
pub const CLASS: &str = "placeholder";
pub const ENDPOINT: &str = "placeholder";
/// 1 micro-USD per million tokens for every field: a token costs 1 picodollar.
pub const PRICE: Price = Price {
    input: 1,
    output: 1,
    cache_write: 1,
    cache_read: 1,
};
pub const PRICES_JSON: &str =
    r#"{"claude-test-1":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#;
pub const RUNS_JSON: &str = r#"{"0123456789abcdef0123456789abcdef":{"id":"r1","ticket":"KOD-1","repo":"acme/app","data_classes":["placeholder"],"max_tokens":1000000000,"max_cost_micro_usd":1000000}}"#;

pub async fn serve(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

pub fn policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

pub fn limits() -> Limits {
    Limits {
        allowed_models: vec![MODEL.into()],
        max_tokens: 1000,
        max_body: 4096,
        max_requests: 100,
        max_concurrent: 2,
    }
}

/// A large budget, so only the budget tests meet it.
pub fn budget() -> Budget {
    Budget {
        max_tokens: 1_000_000_000,
        max_cost_micro_usd: 1_000_000,
    }
}

pub fn run(classes: &[&str], budget: Budget) -> RunSpec {
    RunSpec {
        id: "r1".into(),
        ticket: "KOD-1".into(),
        repo: "acme/app".into(),
        data_classes: classes.iter().map(|c| c.to_string()).collect(),
        budget,
    }
}

pub struct Gateway {
    /// The run's base URL, ending in /r/<token>.
    pub url: String,
    pub db: PathBuf,
    _dir: tempfile::TempDir,
}

pub struct Setup {
    pub limits: Limits,
    pub run: RunSpec,
    pub endpoint: &'static str,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            limits: limits(),
            run: run(&[CLASS], budget()),
            endpoint: ENDPOINT,
        }
    }
}

pub async fn gateway(upstream: &str, s: Setup) -> Gateway {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    let cfg = Config {
        endpoint: Endpoint {
            name: s.endpoint.into(),
            upstream: upstream.into(),
            api_key: "real-key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into(), "/v1/messages/count_tokens".into()],
        limits: s.limits,
        prices: HashMap::from([(MODEL.to_owned(), PRICE)]),
        runs: HashMap::from([(TOKEN.to_owned(), s.run)]),
        policy: Arc::new(Policy::load(&policy_dir()).unwrap()),
        audit: Arc::new(AuditLog::open(&db).unwrap()),
    };
    let root = serve(app(cfg)).await;
    Gateway {
        url: format!("{root}/r/{TOKEN}"),
        db,
        _dir: dir,
    }
}

/// Every audit entry as (record_type, payload), oldest first.
pub fn entries(db: &Path) -> Vec<(String, serde_json::Value)> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut q = c
        .prepare("SELECT record_type, payload FROM audit ORDER BY seq")
        .unwrap();
    q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

pub fn msg(model: &str, max_tokens: u64) -> String {
    format!(r#"{{"model":"{model}","max_tokens":{max_tokens},"messages":[]}}"#)
}

pub async fn post_msg(p: &str, body: String) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{p}/v1/messages"))
        .body(body)
        .send()
        .await
        .unwrap()
}
````

Create `crates/kod-llm-proxy/tests/gateway.rs`:

````rust
//! Routing, budgets and audit capture: the parts phase 1 adds.
mod common;

use axum::{Router, body::Bytes, routing::post};
use common::{CLASS, Gateway, MODEL, Setup, budget, entries, gateway, msg, post_msg, run, serve};
use kod_llm_proxy::{budget::reservation, config::Budget};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

/// A body that reports `usage` and echoes the request, plus a call counter.
async fn upstream(usage: &'static str) -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route(
        "/v1/messages",
        post(move |body: Bytes| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move {
                format!(
                    r#"{{"usage":{usage},"echo":{:?}}}"#,
                    String::from_utf8_lossy(&body)
                )
            }
        }),
    );
    (serve(router).await, calls)
}

const SMALL: &str = r#"{"input_tokens":10,"output_tokens":10}"#;

async fn status(g: &Gateway, body: String) -> u16 {
    post_msg(&g.url, body).await.status().as_u16()
}

#[tokio::test]
async fn an_unknown_run_is_refused_without_a_call() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let root = g.url.split("/r/").next().unwrap().to_owned();
    for url in [
        root.clone(),
        format!("{root}/r/{}", "f".repeat(32)),
        format!("{root}/r/"),
    ] {
        assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 401, "{url}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(entries(&g.db).is_empty());
}

#[tokio::test]
async fn an_unknown_data_class_is_refused_and_recorded() {
    let (up, calls) = upstream(SMALL).await;
    for classes in [&["customer_pii"][..], &[CLASS, "customer_pii"], &[]] {
        let g = gateway(
            &up,
            Setup {
                run: run(classes, budget()),
                ..Setup::default()
            },
        )
        .await;
        assert_eq!(status(&g, msg(MODEL, 10)).await, 403, "{classes:?}");
        let e = entries(&g.db);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].0, "run_audit");
        assert_eq!(e[0].1["decision"]["allow"], false);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_disallowed_endpoint_is_refused() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(
        &up,
        Setup {
            endpoint: "anthropic",
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let e = entries(&g.db);
    let reasons = e[0].1["decision"]["reasons"].to_string();
    assert!(
        reasons.contains("endpoint anthropic is not allowed"),
        "{reasons}"
    );
}

#[tokio::test]
async fn spend_stops_at_the_budget() {
    let (up, calls) = upstream(SMALL).await;
    let body = msg(MODEL, 10);
    let r = reservation(&common::PRICE, body.len(), 10);
    // Room for one reservation plus 30 settled tokens: each call settles at 20.
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: r.tokens + 30,
                    max_cost_micro_usd: 1_000_000,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, body.clone()).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, body.clone()).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, body.clone()).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_cost_limit_is_enforced_too() {
    let (up, calls) = upstream(SMALL).await;
    // A zero cost cap (runs.json refuses it, a hand-built run can have it)
    // is below any reservation, however many tokens are left.
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: 1_000_000_000,
                    max_cost_micro_usd: 0,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_requests_cannot_overspend_together() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let slow = Router::new().route(
        "/v1/messages",
        post(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                r#"{"usage":{"input_tokens":1,"output_tokens":1}}"#
            }
        }),
    );
    let up = serve(slow).await;
    let body = msg(MODEL, 10);
    let r = reservation(&common::PRICE, body.len(), 10);
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: r.tokens * 3 / 2,
                    max_cost_micro_usd: 1_000_000,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    let calls_out: Vec<_> = (0..2)
        .map(|_| {
            let (url, body) = (g.url.clone(), body.clone());
            tokio::spawn(async move { post_msg(&url, body).await.status().as_u16() })
        })
        .collect();
    let mut got = vec![];
    for c in calls_out {
        got.push(c.await.unwrap());
    }
    got.sort();
    assert_eq!(got, [200, 403]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_response_costing_more_than_its_reservation_halts_the_run() {
    let (up, calls) = upstream(r#"{"input_tokens":10,"output_tokens":1000000}"#).await;
    let g = gateway(&up, Setup::default()).await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn prompts_and_outputs_are_captured_redacted() {
    let (up, _) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
    let body = format!(
        r#"{{"model":"{MODEL}","max_tokens":10,"messages":[{{"role":"user","content":"key {secret}"}}]}}"#
    );
    let res = post_msg(&g.url, body).await;
    assert_eq!(res.status(), 200);
    // The client still gets the response unchanged.
    assert!(res.text().await.unwrap().contains(secret));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let e = entries(&g.db);
    let types: Vec<_> = e.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(types, ["run_audit", "prompts", "model_outputs"]);
    assert_eq!(e[1].1["run"], "r1");
    assert_eq!(e[1].1["model"], MODEL);
    assert_eq!(e[2].1["usage"]["output_tokens"], 10);
    for (_, payload) in &e {
        assert!(!payload.to_string().contains(secret), "{payload}");
    }
    assert!(e[1].1["request"]["text"].as_str().unwrap().contains("key "));
}

#[tokio::test]
async fn a_long_body_is_captured_cut_with_its_length_and_hash() {
    let head = vec![b'a'; kod_llm_proxy::CAPTURE_MAX + 10];
    let c = kod_llm_proxy::capture(&head, head.len() as u64, "h".into());
    assert_eq!(
        c["text"].as_str().unwrap().len(),
        kod_llm_proxy::CAPTURE_MAX
    );
    assert_eq!(c["bytes"], kod_llm_proxy::CAPTURE_MAX + 10);
    assert_eq!(c["truncated"], true);
}

#[tokio::test]
async fn a_failed_audit_write_refuses_the_request() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    // Hold the write lock past kod-audit's busy timeout.
    let lock = rusqlite::Connection::open(&g.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(status(&g, msg(MODEL, 10)).await, 503);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    lock.execute_batch("COMMIT").unwrap();
    assert_eq!(status(&g, msg(MODEL, 10)).await, 200);
}
````

Apply to `crates/kod-llm-proxy/tests/proxy.rs` with `git apply`:

````diff
diff --git a/crates/kod-llm-proxy/tests/proxy.rs b/crates/kod-llm-proxy/tests/proxy.rs
index 46d88a9..a2d588c 100644
--- a/crates/kod-llm-proxy/tests/proxy.rs
+++ b/crates/kod-llm-proxy/tests/proxy.rs
@@ -1,3 +1,5 @@
+mod common;
+
 use axum::{
     Router,
     body::Bytes,
@@ -5,7 +7,8 @@ use axum::{
     response::IntoResponse,
     routing::{get, post},
 };
-use kod_llm_proxy::{Config, Limits, USAGE_FIELDS, Usage, app};
+use common::{Gateway, MODEL, Setup, gateway, limits, msg, post_msg, serve};
+use kod_llm_proxy::{Limits, USAGE_FIELDS, Usage};
 use std::sync::{
     Arc,
     atomic::{AtomicUsize, Ordering},
@@ -13,30 +16,11 @@ use std::sync::{
 use std::time::Duration;
 use tokio::{
     io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
-    net::{TcpListener, TcpStream},
+    net::TcpStream,
 };
 
-const MODEL: &str = "claude-test-1";
-
-async fn serve(router: Router) -> String {
-    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
-    let addr = listener.local_addr().unwrap();
-    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
-    format!("http://{addr}")
-}
-
-fn limits() -> Limits {
-    Limits {
-        allowed_models: vec![MODEL.into()],
-        max_tokens: 1000,
-        max_body: 4096,
-        max_requests: 100,
-        max_concurrent: 2,
-    }
-}
-
-/// A fake upstream that echoes what it received, behind the proxy.
-async fn proxy_with(limits: Limits) -> String {
+/// A fake upstream that echoes what it received, behind the gateway.
+async fn proxy_with(limits: Limits) -> Gateway {
     let upstream = Router::new()
         .route(
             "/v1/messages",
@@ -55,35 +39,24 @@ async fn proxy_with(limits: Limits) -> String {
         )
         .route("/v1/files", get(|| async { "LEAK" }));
     let upstream = serve(upstream).await;
-    serve(app(Config {
-        upstream,
-        api_key: "real-key".parse().unwrap(),
-        allowed_paths: vec!["/v1/messages".into()],
-        limits,
-    }))
+    gateway(
+        &upstream,
+        Setup {
+            limits,
+            ..Setup::default()
+        },
+    )
     .await
 }
 
-async fn proxy() -> String {
+async fn proxy() -> Gateway {
     proxy_with(limits()).await
 }
 
-fn msg(model: &str, max_tokens: u64) -> String {
-    format!(r#"{{"model":"{model}","max_tokens":{max_tokens},"messages":[]}}"#)
-}
-
-async fn post_msg(p: &str, body: String) -> reqwest::Response {
-    reqwest::Client::new()
-        .post(format!("{p}/v1/messages"))
-        .body(body)
-        .send()
-        .await
-        .unwrap()
-}
-
 #[tokio::test]
 async fn forwards_with_the_real_key_and_drops_client_credentials() {
-    let p = proxy().await;
+    let g = proxy().await;
+    let p = g.url.clone();
     let body = msg(MODEL, 100);
     let res = reqwest::Client::new()
         .post(format!("{p}/v1/messages?beta=true"))
@@ -102,7 +75,8 @@ async fn forwards_with_the_real_key_and_drops_client_credentials() {
 
 #[tokio::test]
 async fn refuses_other_endpoints() {
-    let p = proxy().await;
+    let g = proxy().await;
+    let p = g.url.clone();
     assert_eq!(
         reqwest::get(format!("{p}/v1/files"))
             .await
@@ -115,13 +89,14 @@ async fn refuses_other_endpoints() {
 #[tokio::test]
 async fn refuses_dot_dot_paths() {
     // Sent raw, because HTTP clients normalize "..".
-    let p = proxy().await;
-    let mut s = TcpStream::connect(p.trim_start_matches("http://"))
-        .await
-        .unwrap();
-    s.write_all(b"GET /v1/messages/../files HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
-        .await
-        .unwrap();
+    let g = proxy().await;
+    let p = g.url.clone();
+    let (host, prefix) = p.trim_start_matches("http://").split_once('/').unwrap();
+    let mut s = TcpStream::connect(host).await.unwrap();
+    let req = format!(
+        "GET /{prefix}/v1/messages/../files HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
+    );
+    s.write_all(req.as_bytes()).await.unwrap();
     let mut buf = String::new();
     s.read_to_string(&mut buf).await.unwrap();
     assert!(buf.starts_with("HTTP/1.1 403"), "{buf}");
@@ -129,7 +104,8 @@ async fn refuses_dot_dot_paths() {
 
 #[tokio::test]
 async fn refuses_models_not_on_the_list() {
-    let p = proxy().await;
+    let g = proxy().await;
+    let p = g.url.clone();
     assert_eq!(
         post_msg(&p, msg("claude-expensive-9", 100)).await.status(),
         403
@@ -138,7 +114,8 @@ async fn refuses_models_not_on_the_list() {
 
 #[tokio::test]
 async fn refuses_missing_or_oversized_max_tokens() {
-    let p = proxy().await;
+    let g = proxy().await;
+    let p = g.url.clone();
     assert_eq!(post_msg(&p, msg(MODEL, 1001)).await.status(), 400);
     assert_eq!(
         post_msg(&p, format!(r#"{{"model":"{MODEL}","messages":[]}}"#))
@@ -151,7 +128,8 @@ async fn refuses_missing_or_oversized_max_tokens() {
 
 #[tokio::test]
 async fn refuses_oversized_bodies() {
-    let p = proxy().await;
+    let g = proxy().await;
+    let p = g.url.clone();
     let big = format!(
         r#"{{"model":"{MODEL}","max_tokens":10,"pad":"{}"}}"#,
         "x".repeat(5000)
@@ -161,11 +139,12 @@ async fn refuses_oversized_bodies() {
 
 #[tokio::test]
 async fn stops_forwarding_after_the_request_budget() {
-    let p = proxy_with(Limits {
+    let g = proxy_with(Limits {
         max_requests: 2,
         ..limits()
     })
     .await;
+    let p = g.url.clone();
     assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 200);
     assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 200);
     assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 429);
@@ -194,16 +173,18 @@ async fn limits_concurrent_requests() {
         }),
     );
     let upstream = serve(slow).await;
-    let p = serve(app(Config {
-        upstream,
-        api_key: "k".parse().unwrap(),
-        allowed_paths: vec!["/v1/messages".into()],
-        limits: Limits {
-            max_concurrent: 1,
-            ..limits()
+    let g = gateway(
+        &upstream,
+        Setup {
+            limits: Limits {
+                max_concurrent: 1,
+                ..limits()
+            },
+            ..Setup::default()
         },
-    }))
+    )
     .await;
+    let p = g.url.clone();
     let calls: Vec<_> = (0..3)
         .map(|_| tokio::spawn(post_msg_owned(p.clone())))
         .collect();
@@ -237,13 +218,8 @@ async fn does_not_follow_upstream_redirects() {
         }),
     ))
     .await;
-    let p = serve(app(Config {
-        upstream,
-        api_key: "real-key".parse().unwrap(),
-        allowed_paths: vec!["/v1/messages".into()],
-        limits: limits(),
-    }))
-    .await;
+    let g = gateway(&upstream, Setup::default()).await;
+    let p = g.url.clone();
     // The test's own client must not follow it either, or it would reach the thief.
     let res = reqwest::Client::builder()
         .redirect(reqwest::redirect::Policy::none())
@@ -311,8 +287,10 @@ fn reads_usage_from_json_and_event_streams_in_any_chunking() {
     assert!(u["output_tokens"].is_null(), "{u}");
 }
 
-/// Starts the proxy binary against `upstream`, waits until it answers, and
-/// returns its URL and its stdout lines.
+/// Starts the gateway binary against `upstream` with the repo's policy, the
+/// fixture's prices and run, and an audit log in a temporary directory. Waits
+/// until it answers, and returns the run's URL, the process, its stdout lines
+/// and the directory.
 async fn spawn_proxy(
     upstream: &str,
     env: &[(&str, &str)],
@@ -320,7 +298,12 @@ async fn spawn_proxy(
     String,
     tokio::process::Child,
     tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
+    tempfile::TempDir,
 ) {
+    let dir = tempfile::tempdir().unwrap();
+    let (prices, runs) = (dir.path().join("prices.json"), dir.path().join("runs.json"));
+    std::fs::write(&prices, common::PRICES_JSON).unwrap();
+    std::fs::write(&runs, common::RUNS_JSON).unwrap();
     let port = std::net::TcpListener::bind("127.0.0.1:0")
         .unwrap()
         .local_addr()
@@ -331,6 +314,10 @@ async fn spawn_proxy(
         .env("KOD_API_KEY", "real-key")
         .env("KOD_ALLOWED_MODELS", MODEL)
         .env("KOD_LISTEN", format!("127.0.0.1:{port}"))
+        .env("KOD_PRICES", &prices)
+        .env("KOD_RUNS", &runs)
+        .env("KOD_POLICY_DIR", common::policy_dir())
+        .env("KOD_AUDIT_DB", dir.path().join("audit.db"))
         .envs(env.iter().copied())
         .stdout(std::process::Stdio::piped())
         .kill_on_drop(true)
@@ -343,7 +330,8 @@ async fn spawn_proxy(
         tokio::time::sleep(Duration::from_millis(50)).await;
     }
     let lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
-    (format!("http://127.0.0.1:{port}"), child, lines)
+    let url = format!("http://127.0.0.1:{port}/r/{}", common::TOKEN);
+    (url, child, lines, dir)
 }
 
 /// Log lines until the proxy has been quiet for half a second.
@@ -382,7 +370,7 @@ fn sse_upstream() -> Router {
 #[tokio::test]
 async fn logs_usage_after_a_stream_and_passes_it_through_unchanged() {
     let upstream = serve(sse_upstream()).await;
-    let (p, _child, mut lines) = spawn_proxy(&upstream, &[]).await;
+    let (p, _child, mut lines, _dir) = spawn_proxy(&upstream, &[]).await;
     let body = reqwest::Client::new()
         .post(format!("{p}/v1/messages"))
         .body(msg(MODEL, 10))
@@ -395,8 +383,8 @@ async fn logs_usage_after_a_stream_and_passes_it_through_unchanged() {
     assert_eq!(body, SSE);
     let events = drain(&mut lines).await;
     let names: Vec<_> = events.iter().map(|v| v["event"].clone()).collect();
-    assert_eq!(names, ["allow", "usage"]);
-    let v = &events[1];
+    assert_eq!(names, ["ready", "allow", "usage"]);
+    let v = &events[2];
     assert_eq!(
         (v["input_tokens"].clone(), v["output_tokens"].clone()),
         (12.into(), 34.into())
@@ -410,7 +398,7 @@ async fn logs_usage_after_a_stream_and_passes_it_through_unchanged() {
 #[tokio::test]
 async fn log_lines_are_bounded_by_the_request_budget() {
     let upstream = serve(sse_upstream()).await;
-    let (p, _child, mut lines) = spawn_proxy(&upstream, &[("KOD_MAX_REQUESTS", "3")]).await;
+    let (p, _child, mut lines, _dir) = spawn_proxy(&upstream, &[("KOD_MAX_REQUESTS", "3")]).await;
     let c = reqwest::Client::new();
     let long = format!("{p}/v1/{}", "x".repeat(10_000));
     assert_eq!(c.post(&long).send().await.unwrap().status(), 403);
@@ -441,7 +429,7 @@ async fn log_lines_are_bounded_by_the_request_budget() {
         .collect();
     assert_eq!(
         names,
-        ["deny", "allow", "usage", "allow", "over_budget"],
+        ["ready", "deny", "allow", "usage", "allow", "over_budget"],
         "{events:?}"
     );
     for v in &events {
````

Append to `crates/kod-llm-proxy/tests/config.rs`:

````rust

/// The binary refuses to start, and never prints "ready", on a bad file or a
/// missing setting.
#[test]
fn the_gateway_does_not_start_on_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = |f: &str| dir.path().join(f);
    std::fs::write(
        path("prices.json"),
        r#"{"claude-test-1":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#,
    )
    .unwrap();
    std::fs::write(path("runs.json"), run(GOOD_RUN)).unwrap();
    std::fs::write(path("bad-runs.json"), r#"{"abc":{}}"#).unwrap();
    let policy = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy");
    let start = |runs: &str, skip: &str| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_kod-llm-proxy"));
        c.env_clear()
            .env("KOD_API_KEY", "k")
            .env("KOD_ALLOWED_MODELS", "claude-test-1")
            .env("KOD_PRICES", path("prices.json"))
            .env("KOD_RUNS", path(runs))
            .env("KOD_POLICY_DIR", &policy)
            .env("KOD_AUDIT_DB", path("audit.db"))
            .env("KOD_LISTEN", "127.0.0.1:0")
            .env_remove(skip);
        c.output().unwrap()
    };
    for (runs, skip, want) in [
        ("bad-runs.json", "", "at least 32 lower-case hex"),
        ("runs.json", "KOD_PRICES", "KOD_PRICES is required"),
        ("runs.json", "KOD_RUNS", "KOD_RUNS is required"),
        ("runs.json", "KOD_POLICY_DIR", "KOD_POLICY_DIR is required"),
        ("runs.json", "KOD_AUDIT_DB", "KOD_AUDIT_DB is required"),
        ("missing.json", "", "KOD_RUNS: reading"),
    ] {
        let out = start(runs, skip);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{runs} {skip}");
        assert!(err.contains(want), "{runs} {skip}: {err}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("ready"));
    }
}
````

Apply to `crates/kod-loop/Cargo.toml` with `git apply`:

````diff
diff --git a/crates/kod-loop/Cargo.toml b/crates/kod-loop/Cargo.toml
index f409ca9..f9064c3 100644
--- a/crates/kod-loop/Cargo.toml
+++ b/crates/kod-loop/Cargo.toml
@@ -14,6 +14,8 @@ serde_json = "1"
 
 [dev-dependencies]
 axum = "0.8"
+kod-audit = { path = "../kod-audit" }
 kod-llm-proxy = { path = "../kod-llm-proxy" }
+kod-policy = { path = "../kod-policy" }
 tempfile = "3"
 tokio = { version = "1", features = ["rt-multi-thread", "net"] }
````

Replace `crates/kod-loop/tests/proxy.rs`:

````rust
//! The loop through the real kod-llm-proxy gateway, as in the sandbox.
mod common;
use common::*;
use kod_audit::AuditLog;
use kod_llm_proxy::{
    Config as Gateway, Endpoint, Limits, app,
    config::{Budget, Price, RunSpec},
};
use kod_loop::{Config, EndState};
use kod_policy::Policy;
use serde_json::json;
use std::{path::Path, sync::Arc};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A gateway for one run with this budget; returns its base URL, with the
/// run's token, and the temporary directory holding its audit log.
fn gateway(upstream: &str, max_requests: u64, budget: Budget) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let policy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy");
    let run = RunSpec {
        id: "r1".into(),
        ticket: "KOD-1".into(),
        repo: "acme/app".into(),
        data_classes: vec!["placeholder".into()],
        budget,
    };
    let price = Price {
        input: 1,
        output: 1,
        cache_write: 1,
        cache_read: 1,
    };
    let url = serve(app(Gateway {
        endpoint: Endpoint {
            name: "placeholder".into(),
            upstream: upstream.into(),
            api_key: "real-key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into()],
        limits: Limits {
            allowed_models: vec!["m".into()],
            max_tokens: 1000,
            max_body: 4 << 20,
            max_requests,
            max_concurrent: 2,
        },
        prices: [("m".to_owned(), price)].into(),
        runs: [(TOKEN.to_owned(), run)].into(),
        policy: Arc::new(Policy::load(&policy).unwrap()),
        audit: Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap()),
    }));
    (format!("{url}/r/{TOKEN}"), dir)
}

fn proxy(upstream: &str, max_requests: u64) -> (String, tempfile::TempDir) {
    let budget = Budget {
        max_tokens: 1_000_000_000,
        max_cost_micro_usd: 1_000_000,
    };
    gateway(upstream, max_requests, budget)
}

#[test]
fn through_the_proxy_the_key_is_added_and_the_budget_ends_the_run() {
    let ws = tempfile::tempdir().unwrap();
    let (upstream, m) = model(
        (0..5)
            .map(|i| {
                msg(
                    json!([tool(&format!("t{i}"), "run", json!({"command": "true"}))]),
                    "tool_use",
                )
            })
            .collect(),
    );
    let (url, _audit) = proxy(&upstream, 2);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(
        (o.end_state, o.turns, o.error.clone()),
        (EndState::OverBudget, 3, None)
    );
    let seen = &m.lock().unwrap().seen;
    assert_eq!(seen.len(), 2);
    for (h, _) in seen {
        assert_eq!(h["x-api-key"], "real-key");
    }
}

#[test]
fn a_model_the_proxy_does_not_allow_needs_a_human() {
    let ws = tempfile::tempdir().unwrap();
    let (upstream, m) = model(vec![msg(json!([text("hi")]), "end_turn")]);
    let (url, _audit) = proxy(&upstream, 10);
    let cfg = Config {
        model: "other".into(),
        ..config(&url, ws.path())
    };
    let (o, _) = go(&cfg, "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("403"));
    assert!(m.lock().unwrap().seen.is_empty());
}

/// The spend budget holds whoever calls: a request that does not fit is
/// refused before the model sees it, and the loop asks for a human.
#[test]
fn a_request_over_the_spend_budget_never_reaches_the_model() {
    let ws = tempfile::tempdir().unwrap();
    let (upstream, m) = model(vec![msg(json!([text("hi")]), "end_turn")]);
    let budget = Budget {
        max_tokens: 1,
        max_cost_micro_usd: 1_000_000,
    };
    let (url, _audit) = gateway(&upstream, 10, budget);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("403"));
    assert!(m.lock().unwrap().seen.is_empty());
}
````

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kod-llm-proxy -p kod-loop`
Expected: FAIL to compile: unresolved import `kod_llm_proxy::Endpoint`, and `Config` has no field named `endpoint`.

- [ ] **Step 3: Write the gateway**

Replace `crates/kod-llm-proxy/src/lib.rs`:

````rust
//! The LLM gateway, grown from phase 0's model-only proxy. One long-running
//! process serves every run. A run is known by the token in its base URL,
//! `/r/<token>`, and gets its own request count, concurrency limit and spend
//! budget. The sandbox holds no key; the real one exists only here. Only
//! exact allowlisted paths are forwarded, so ".." and other paths can't
//! reach other endpoints.
//!
//! Each allowed request is checked in this order, and the first refusal
//! wins: request count, halted run, path, body size, model and max_tokens,
//! routing (kod-policy `model_request`, recorded in the audit log), budget
//! reservation, then the redacted prompt capture in the audit log. A failed
//! audit write refuses the request. The response is streamed back; when it
//! ends, its cost settles the reservation and the output capture is written
//! to the audit log. A failed output write halts the run.
//!
//! Log lines are JSON on stdout and carry the run id. Each request within a
//! run's request budget gets one line when it is allowed or refused, and the
//! first request past it gets an "over_budget" line; later ones are refused
//! silently, so the log is bounded by the budget. Requests for an unknown
//! run are refused with no line. Logged paths are cut at LOG_PATH_MAX bytes.
//! An allowed /v1/messages request with a 2xx response gets a second line,
//! event "usage", when its response body ends, with the token counts the
//! response reported (null when it reported none or the stream ended early)
//! and the run's spend so far.
pub mod budget;
pub mod config;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use budget::{Ledger, Spend};
use config::{Price, Prices, RunSpec};
use futures_util::StreamExt;
use kod_audit::AuditLog;
use kod_policy::Policy;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::Semaphore;

/// Never forwarded in either direction. Client credentials are replaced.
const DROP: [&str; 6] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "x-api-key",
    "authorization",
];

/// Defaults for the KOD_* settings, shared with kod-eval, which records them.
pub const DEFAULT_ALLOWED_PATHS: &str = "/v1/messages,/v1/messages/count_tokens";
pub const DEFAULT_MAX_TOKENS: u64 = 8192;
pub const DEFAULT_MAX_BODY: u64 = 4 << 20;
pub const DEFAULT_MAX_REQUESTS: u64 = 200;
pub const DEFAULT_MAX_CONCURRENT: u64 = 2;

/// The endpoint name kod-policy's placeholder routing allows.
pub const DEFAULT_ENDPOINT: &str = "placeholder";

/// Audit record types, keys of the compliance profile's retention_days.
pub const PROMPTS: &str = "prompts";
pub const MODEL_OUTPUTS: &str = "model_outputs";

/// Bytes of each request and response body kept in the audit log. The rest
/// is represented by its length and sha256.
// ponytail: kod-audit caps a payload at 1 MiB and escaping can grow text up
// to 6x, so captures are cut; full capture needs a blob store (open question).
pub const CAPTURE_MAX: usize = 128 << 10;

/// Logged paths are cut to this many bytes; the agent chooses them.
const LOG_PATH_MAX: usize = 256;

#[derive(Clone)]
pub struct Limits {
    /// Exact model ids. Anything else is refused.
    pub allowed_models: Vec<String>,
    /// Cap on `max_tokens` for /v1/messages, which must be present.
    pub max_tokens: u64,
    /// Request body limit in bytes.
    pub max_body: usize,
    /// Requests per run, counted before any other check.
    pub max_requests: u64,
    /// Requests per run in flight at once; further requests wait.
    pub max_concurrent: usize,
}

/// The one model endpoint. Routing asks kod-policy whether `name` is allowed
/// for the run's data classes.
#[derive(Clone)]
pub struct Endpoint {
    pub name: String,
    /// Without a trailing slash, e.g. "https://api.anthropic.com".
    pub upstream: String,
    pub api_key: HeaderValue,
}

#[derive(Clone)]
pub struct Config {
    pub endpoint: Endpoint,
    pub allowed_paths: Vec<String>,
    pub limits: Limits,
    /// Every allowed model has a price (checked by `config::parse_prices`).
    pub prices: Prices,
    /// Run token to run.
    pub runs: HashMap<String, RunSpec>,
    pub policy: Arc<Policy>,
    pub audit: Arc<AuditLog>,
}

struct Run {
    spec: RunSpec,
    requests: AtomicU64,
    in_flight: Arc<Semaphore>,
    ledger: Mutex<Ledger>,
}

struct AppState {
    cfg: Config,
    client: reqwest::Client,
    runs: HashMap<String, Arc<Run>>,
}

pub fn app(cfg: Config) -> Router {
    let runs = cfg
        .runs
        .iter()
        .map(|(token, spec)| {
            let run = Run {
                spec: spec.clone(),
                requests: AtomicU64::new(0),
                in_flight: Arc::new(Semaphore::new(cfg.limits.max_concurrent)),
                ledger: Mutex::new(Ledger::new(spec.budget)),
            };
            (token.clone(), Arc::new(run))
        })
        .collect();
    let state = Arc::new(AppState {
        cfg,
        // The client sends the key, so it never follows a redirect, which could
        // carry the key to another host. A connect timeout catches a dead
        // upstream; there is no total or read timeout, because responses stream
        // and a long generation can be silent for minutes. The run's timeout
        // and the kill switch bound a stalled request.
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("building the upstream client"),
        runs,
    });
    Router::new().fallback(forward).with_state(state)
}

fn refuse(event: &str, run: &str, path: &str, status: StatusCode) -> Response {
    log(event, run, path, status.as_u16());
    status.into_response()
}

/// Checks the request body against the model and token limits, and returns
/// the model and max_tokens (0 for count_tokens).
fn check_body(
    lim: &Limits,
    path: &str,
    body: &[u8],
) -> Result<(String, u64), (&'static str, StatusCode)> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Err(("bad_request", StatusCode::BAD_REQUEST));
    };
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("");
    if !lim.allowed_models.iter().any(|m| m == model) {
        return Err(("deny_model", StatusCode::FORBIDDEN));
    }
    if path != "/v1/messages" {
        return Ok((model.to_owned(), 0));
    }
    match v.get("max_tokens").and_then(|t| t.as_u64()) {
        Some(t) if t <= lim.max_tokens => Ok((model.to_owned(), t)),
        _ => Err(("bad_request", StatusCode::BAD_REQUEST)),
    }
}

/// The kod-policy input for a model request from this run.
pub fn route_input(spec: &RunSpec, endpoint: &str) -> Value {
    json!({
        "action": "model_request",
        "actor": {"kind": "agent"},
        "run": {
            "id": spec.id,
            "ticket": spec.ticket,
            "repo": spec.repo,
            "canceled": false,
            "data_classes": spec.data_classes,
            "auth_mode": "api_key",
        },
        "endpoint": endpoint,
    })
}

/// Runs an audit write off the async threads. Any failure is an error.
async fn audit<T: Send + 'static>(
    log: &Arc<AuditLog>,
    f: impl FnOnce(&AuditLog) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let log = log.clone();
    tokio::task::spawn_blocking(move || f(&log)).await?
}

/// A body as kept in the audit log: its first CAPTURE_MAX bytes as text,
/// with its full length and sha256.
pub fn capture(head: &[u8], bytes: u64, sha256: String) -> Value {
    let text = String::from_utf8_lossy(&head[..head.len().min(CAPTURE_MAX)]);
    json!({
        "text": text,
        "bytes": bytes,
        "sha256": sha256,
        "truncated": bytes > CAPTURE_MAX as u64,
    })
}

fn hex(d: impl AsRef<[u8]>) -> String {
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

async fn forward(State(st): State<Arc<AppState>>, req: Request) -> Response {
    // "/r/<token>/v1/messages" becomes run <token> and path "/v1/messages".
    let Some((run, path)) = req
        .uri()
        .path()
        .strip_prefix("/r/")
        .and_then(|r| r.split_once('/'))
        .and_then(|(token, rest)| Some((st.runs.get(token)?.clone(), format!("/{rest}"))))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let id = run.spec.id.as_str();
    let lim = &st.cfg.limits;
    // Every request counts, refused ones too, so a script can neither probe
    // nor fill the log for free.
    let n = run.requests.fetch_add(1, Ordering::SeqCst);
    if n > lim.max_requests {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if n == lim.max_requests {
        return refuse("over_budget", id, &path, StatusCode::TOO_MANY_REQUESTS);
    }
    if run.ledger.lock().map_or(true, |l| l.halted()) {
        return refuse("halted", id, &path, StatusCode::FORBIDDEN);
    }
    if !st.cfg.allowed_paths.contains(&path) {
        return refuse("deny", id, &path, StatusCode::FORBIDDEN);
    }
    let ep = &st.cfg.endpoint;
    let url = match req.uri().query() {
        Some(q) => format!("{}{path}?{q}", ep.upstream),
        None => format!("{}{path}", ep.upstream),
    };
    let method = req.method().clone();
    let mut headers = req.headers().clone();
    for h in DROP {
        headers.remove(h);
    }
    headers.insert("x-api-key", ep.api_key.clone());
    let Ok(body) = to_bytes(req.into_body(), lim.max_body).await else {
        return refuse("too_large", id, &path, StatusCode::PAYLOAD_TOO_LARGE);
    };
    let (model, max_tokens) = match check_body(lim, &path, &body) {
        Ok(m) => m,
        Err((event, status)) => return refuse(event, id, &path, status),
    };
    // Routing: the policy decides whether this endpoint may serve the run's
    // data classes, and the decision is recorded before anything else.
    let input = route_input(&run.spec, &ep.name);
    let decision = st.cfg.policy.decide(&input);
    let (allow, reasons) = (decision.allow, decision.reasons.clone());
    if audit(&st.cfg.audit, move |a| a.record_decision(&input, &decision))
        .await
        .is_err()
    {
        return refuse("audit_error", id, &path, StatusCode::SERVICE_UNAVAILABLE);
    }
    if !allow {
        log_with(
            "deny_route",
            id,
            &path,
            403,
            Map::from_iter([("reasons".into(), json!(reasons))]),
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    // count_tokens is free, so it reserves nothing.
    let Some(price) = st.cfg.prices.get(&model).copied() else {
        return refuse("deny_model", id, &path, StatusCode::FORBIDDEN);
    };
    let reserved = if path == "/v1/messages" {
        budget::reservation(&price, body.len(), max_tokens)
    } else {
        Spend::default()
    };
    if !run.ledger.lock().is_ok_and(|mut l| l.reserve(reserved)) {
        return refuse("over_spend", id, &path, StatusCode::FORBIDDEN);
    }
    let release = |run: &Run| {
        if let Ok(mut l) = run.ledger.lock() {
            l.release(reserved);
        }
    };
    let prompt = json!({
        "run": id,
        "endpoint": ep.name,
        "model": model,
        "path": path,
        "request": capture(&body, body.len() as u64, hex(Sha256::digest(&body))),
    });
    if audit(&st.cfg.audit, move |a| a.append(PROMPTS, &prompt))
        .await
        .is_err()
    {
        release(&run);
        return refuse("audit_error", id, &path, StatusCode::SERVICE_UNAVAILABLE);
    }
    // Held until the response body has been fully streamed.
    let Ok(permit) = run.in_flight.clone().acquire_owned().await else {
        release(&run);
        return refuse("upstream_error", id, &path, StatusCode::SERVICE_UNAVAILABLE);
    };
    match st
        .client
        .request(method, url)
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(up) => {
            log("allow", id, &path, up.status().as_u16());
            let mut res = Response::builder().status(up.status());
            for (k, v) in up.headers() {
                if !DROP.contains(&k.as_str()) {
                    res = res.header(k, v);
                }
            }
            let sse = up
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            let mut meter = Meter {
                usage: Usage::new(sse),
                head: vec![],
                bytes: 0,
                sha: Sha256::new(),
                run: run.clone(),
                audit: st.cfg.audit.clone(),
                reserved,
                price,
                path: path.clone(),
                status: up.status().as_u16(),
            };
            // Streamed, so server-sent events reach the runner as they arrive.
            let stream = up.bytes_stream().map(move |chunk| {
                let _held = &permit;
                if let Ok(c) = &chunk {
                    meter.feed(c);
                }
                chunk
            });
            res.body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => {
            release(&run);
            // Without the URL, which carries a query string the agent chose.
            eprintln!("upstream: {}", e.without_url());
            refuse("upstream_error", id, &path, StatusCode::BAD_GATEWAY)
        }
    }
}

fn log(event: &str, run: &str, path: &str, status: u16) {
    log_with(event, run, path, status, Map::new());
}

fn log_with(event: &str, run: &str, path: &str, status: u16, mut extra: Map<String, Value>) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for (k, v) in [
        ("ts", json!(ts)),
        ("event", json!(event)),
        ("run", json!(run)),
        (
            "path",
            json!(path.get(..path.floor_char_boundary(LOG_PATH_MAX))),
        ),
        ("status", json!(status)),
    ] {
        extra.insert(k.into(), v);
    }
    println!("{}", Value::Object(extra));
}

/// Watches a response body as it passes through. When the body is dropped
/// (after its last byte, or when the client went away) it settles the run's
/// reservation, logs the usage line and writes the output capture.
struct Meter {
    usage: Usage,
    head: Vec<u8>,
    bytes: u64,
    sha: Sha256,
    run: Arc<Run>,
    audit: Arc<AuditLog>,
    reserved: Spend,
    price: Price,
    path: String,
    status: u16,
}

impl Meter {
    fn feed(&mut self, c: &[u8]) {
        if self.path == "/v1/messages" {
            self.usage.feed(c);
        }
        let room = CAPTURE_MAX.saturating_sub(self.head.len());
        self.head.extend_from_slice(&c[..c.len().min(room)]);
        self.bytes += c.len() as u64;
        self.sha.update(c);
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let usage = std::mem::replace(&mut self.usage, Usage::new(false)).finish();
        let ok = (200..300).contains(&self.status);
        // Error responses are not billed. A success with unknown usage is
        // charged its whole reservation.
        let actual = if ok && self.path == "/v1/messages" {
            budget::cost(&self.price, &usage)
        } else {
            Some(Spend::default())
        };
        let id = self.run.spec.id.clone();
        let (spent, halted) = match self.run.ledger.lock() {
            Ok(mut l) => {
                l.settle(self.reserved, actual);
                (l.spent(), l.halted())
            }
            Err(_) => (Spend::default(), true),
        };
        if ok && self.path == "/v1/messages" {
            let mut extra = usage.clone();
            extra.insert("run_tokens".into(), json!(spent.tokens));
            extra.insert("run_cost_pico_usd".into(), json!(spent.pico_usd));
            extra.insert("run_halted".into(), json!(halted));
            log_with("usage", &id, &self.path, self.status, extra);
        }
        let sha = hex(std::mem::take(&mut self.sha).finalize());
        let output = json!({
            "run": id,
            "path": self.path,
            "status": self.status,
            "usage": usage,
            "response": capture(&self.head, self.bytes, sha),
        });
        let (run, log, path) = (self.run.clone(), self.audit.clone(), self.path.clone());
        tokio::task::spawn_blocking(move || {
            if log.append(MODEL_OUTPUTS, &output).is_err() {
                if let Ok(mut l) = run.ledger.lock() {
                    l.halt();
                }
                self::log("audit_error", &run.spec.id, &path, 503);
            }
        });
    }
}

/// The usage fields logged for each /v1/messages response.
pub const USAGE_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
];
/// A JSON body or SSE line longer than this is not parsed; its usage is
/// recorded as missing.
const USAGE_BUF_CAP: usize = 4 << 20;

/// Token usage read from a Messages API response while its bytes pass
/// through unchanged. A JSON body is held (up to USAGE_BUF_CAP) and parsed at
/// the end; a server-sent event stream is parsed line by line, so only the
/// current line is held.
pub struct Usage {
    sse: bool,
    buf: Vec<u8>,
    overflow: bool,
    /// message_stop arrived, so the last output count is final.
    stopped: bool,
    seen: Map<String, Value>,
}

impl Usage {
    pub fn new(sse: bool) -> Self {
        Usage {
            sse,
            buf: vec![],
            overflow: false,
            stopped: false,
            seen: Map::new(),
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.overflow {
            return;
        }
        self.buf.extend_from_slice(chunk);
        if self.sse {
            while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                self.sse_line(&line);
            }
        }
        if self.buf.len() > USAGE_BUF_CAP {
            self.buf = vec![];
            self.overflow = !self.sse;
        }
    }

    /// The usage fields, each a number or null when the response had none.
    pub fn finish(mut self) -> Map<String, Value> {
        let rest = std::mem::take(&mut self.buf);
        if self.sse {
            self.sse_line(&rest);
            if !self.stopped {
                self.seen.remove("output_tokens");
            }
        } else if !self.overflow
            && let Ok(v) = serde_json::from_slice::<Value>(&rest)
        {
            self.take(&v["usage"]);
        }
        USAGE_FIELDS
            .iter()
            .map(|&k| {
                (
                    k.to_owned(),
                    self.seen.get(k).cloned().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    /// message_start carries message.usage; message_delta carries usage with
    /// the output_tokens so far, final once message_stop arrives. Later values
    /// replace earlier ones.
    fn sse_line(&mut self, line: &[u8]) {
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        if let Ok(v) = serde_json::from_slice::<Value>(data.trim_ascii()) {
            match v["type"].as_str() {
                Some("message_start") => self.take(&v["message"]["usage"]),
                Some("message_delta") => self.take(&v["usage"]),
                Some("message_stop") => self.stopped = true,
                _ => {}
            }
        }
    }

    fn take(&mut self, usage: &Value) {
        for k in USAGE_FIELDS {
            if let Some(n) = usage.get(k).filter(|n| n.is_u64()) {
                self.seen.insert(k.into(), n.clone());
            }
        }
    }
}
````

Replace `crates/kod-llm-proxy/src/main.rs`:

````rust
use anyhow::{Context, Result, bail};
use kod_audit::AuditLog;
use kod_llm_proxy::{
    Config, DEFAULT_ALLOWED_PATHS, DEFAULT_ENDPOINT, DEFAULT_MAX_BODY, DEFAULT_MAX_CONCURRENT,
    DEFAULT_MAX_REQUESTS, DEFAULT_MAX_TOKENS, Endpoint, Limits, app, config,
};
use kod_policy::Policy;
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let env = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_owned());
    let required = |k: &str| std::env::var(k).with_context(|| format!("{k} is required"));
    let num = |k: &str, default: u64| -> Result<u64> {
        std::env::var(k).map_or(Ok(default), |v| {
            v.parse().with_context(|| format!("{k} must be a number"))
        })
    };
    let list = |s: String| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let read = |k: &str| -> Result<String> {
        let path = PathBuf::from(required(k)?);
        std::fs::read_to_string(&path).with_context(|| format!("{k}: reading {}", path.display()))
    };
    let allowed_models = list(required("KOD_ALLOWED_MODELS")?);
    if allowed_models.is_empty() {
        bail!("KOD_ALLOWED_MODELS is empty");
    }
    let prices = config::parse_prices(&read("KOD_PRICES")?, &allowed_models)?;
    let runs = config::parse_runs(&read("KOD_RUNS")?)?;
    let policy =
        Policy::load(&PathBuf::from(required("KOD_POLICY_DIR")?)).context("loading the policy")?;
    let audit = AuditLog::open(&PathBuf::from(required("KOD_AUDIT_DB")?))
        .context("opening the audit log")?;
    let cfg = Config {
        endpoint: Endpoint {
            name: env("KOD_ENDPOINT", DEFAULT_ENDPOINT),
            upstream: env("KOD_UPSTREAM", "https://api.anthropic.com")
                .trim_end_matches('/')
                .to_owned(),
            api_key: required("KOD_API_KEY")?
                .parse()
                .context("KOD_API_KEY is not a valid header value")?,
        },
        allowed_paths: list(env("KOD_ALLOWED_PATHS", DEFAULT_ALLOWED_PATHS)),
        limits: Limits {
            allowed_models,
            max_tokens: num("KOD_MAX_TOKENS", DEFAULT_MAX_TOKENS)?,
            max_body: num("KOD_MAX_BODY", DEFAULT_MAX_BODY)? as usize,
            max_requests: num("KOD_MAX_REQUESTS", DEFAULT_MAX_REQUESTS)?,
            max_concurrent: num("KOD_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT)? as usize,
        },
        prices,
        runs,
        policy: Arc::new(policy),
        audit: Arc::new(audit),
    };
    let listen = env("KOD_LISTEN", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    // kod-eval waits for this line: the config loaded and the port is open.
    println!(
        "{}",
        serde_json::json!({"event": "ready", "listen": listen})
    );
    axum::serve(listener, app(cfg)).await?;
    Ok(())
}
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-llm-proxy -p kod-loop && cargo clippy --all-targets -- -D warnings`
Expected: PASS (kod-llm-proxy: budget 8, config 4, gateway 10, proxy 12; kod-loop proxy 3), clippy clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/kod-llm-proxy crates/kod-loop
git commit -m "kod-llm-proxy: one gateway for many runs, with routing, budgets and audit capture"
```

### Task 3: kod-eval on one gateway

**Files:**
- Modify: `crates/kod-eval/src/run.rs`, `crates/kod-eval/src/runner.rs`, `crates/kod-eval/src/main.rs`, `README.md`
- Test: `crates/kod-eval/tests/run.rs`, `crates/kod-eval/tests/runner.rs`, `crates/kod-eval/tests/sandbox.rs`, `crates/kod-eval/tests/report.rs`, `crates/kod-eval/tests/e2e/mod.rs`, `crates/kod-eval/tests/docker.rs`

**Interfaces:**
- Consumes: the gateway's settings and `ready` line from Task 2.
- Produces:
  - `run::SpendBudget { max_tokens, max_cost_micro_usd: u64, prices: Value }`, `Manifest.spend: Option<SpendBudget>` (`None` in phase 0 manifests).
  - `Settings` gains `policy_dir: PathBuf`, `prices: PathBuf`, `run_max_tokens: u64`, `run_max_cost_micro_usd: u64`.
  - `run::EVAL_DATA_CLASS = "placeholder"`, `run::runs_json(tokens: &[(String, String)], &SpendBudget) -> Value`, `run::token() -> Result<String>`, `run::case_log(log: &str, case: &str) -> String`, `run::GatewayMounts<'a> { dir, policy: &Path, user: (u32, u32) }`, `run::gateway_args(name, run_id, &ProxyLimits, &GatewayMounts, image) -> Vec<String>` (replaces `proxy_args`).
  - `Runner::env(&self, base_url: &str)`: every runner, the script included, gets `ANTHROPIC_BASE_URL` set to its case's `http://kod-llm-proxy:8080/r/<token>`.

The gateway keeps the network alias `kod-llm-proxy`, so the sandbox side is unchanged except for the `/r/<token>` suffix. `run_all` waits for the gateway's `ready` line (up to 10 s) before the first case; if the gateway does not start, every case is cancelled with the gateway's stderr as the reason. A case's `proxy.log` is the gateway's lines whose `run` is the case id, so `report.rs` reads it as before.

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-eval/tests/run.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/run.rs b/crates/kod-eval/tests/run.rs
index 0facea1..ca1deb6 100644
--- a/crates/kod-eval/tests/run.rs
+++ b/crates/kod-eval/tests/run.rs
@@ -1,6 +1,8 @@
 use kod_eval::{
     case::EndState,
-    run::{ProxyLimits, decide, proxy_args},
+    run::{
+        GatewayMounts, ProxyLimits, SpendBudget, case_log, decide, gateway_args, runs_json, token,
+    },
     runner::Runner,
 };
 
@@ -15,12 +17,21 @@ fn timeouts_kills_and_missing_output_map_to_end_states() {
     assert_eq!(decide(&r, false, Some(0), "").0, EndState::Done);
 }
 
+fn mounts() -> GatewayMounts<'static> {
+    GatewayMounts {
+        dir: std::path::Path::new("/res/run-1/gateway"),
+        policy: std::path::Path::new("/repo/policy"),
+        user: (501, 20),
+    }
+}
+
 /// The key is passed by name, so it never appears on a command line. The
-/// limits are passed as the values recorded in the manifest.
+/// limits are passed as the values recorded in the manifest, and the
+/// gateway's files are mounted where its settings point.
 #[test]
-fn proxy_gets_the_key_by_name_and_the_recorded_limits() {
+fn gateway_gets_the_key_by_name_the_recorded_limits_and_its_files() {
     let lim = ProxyLimits::from_env(|_| None, "m1,m2".into()).unwrap();
-    let a = proxy_args("p", "run-1", &lim, "kod-llm-proxy").join(" ");
+    let a = gateway_args("p", "run-1", &lim, &mounts(), "kod-llm-proxy").join(" ");
     assert!(a.contains("-e KOD_API_KEY "), "{a}");
     for kv in [
         "KOD_ALLOWED_PATHS=/v1/messages,/v1/messages/count_tokens",
@@ -28,17 +39,63 @@ fn proxy_gets_the_key_by_name_and_the_recorded_limits() {
         "KOD_MAX_BODY=4194304",
         "KOD_MAX_REQUESTS=200",
         "KOD_MAX_CONCURRENT=2",
+        "KOD_PRICES=/gateway/prices.json",
+        "KOD_RUNS=/gateway/runs.json",
+        "KOD_POLICY_DIR=/policy",
+        "KOD_AUDIT_DB=/gateway/audit.db",
     ] {
         assert!(a.contains(&format!("-e {kv} ")), "missing {kv} in {a}");
     }
     assert!(!a.contains("KOD_API_KEY="), "{a}");
     assert!(a.contains("--label kod.run=run-1"), "{a}");
+    assert!(a.contains("--user 501:20 "), "{a}");
+    assert!(a.contains("-v /res/run-1/gateway:/gateway "), "{a}");
+    assert!(a.contains("-v /repo/policy:/policy:ro "), "{a}");
     assert!(
         a.ends_with("-e KOD_ALLOWED_MODELS=m1,m2 kod-llm-proxy"),
         "{a}"
     );
 }
 
+#[test]
+fn each_case_is_a_gateway_run_with_its_own_token_and_the_budget() {
+    let b = SpendBudget {
+        max_tokens: 100,
+        max_cost_micro_usd: 7,
+        prices: serde_json::json!({}),
+    };
+    let (t1, t2) = (token().unwrap(), token().unwrap());
+    assert_ne!(t1, t2);
+    for t in [&t1, &t2] {
+        assert_eq!(t.len(), 32);
+        assert!(
+            t.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
+            "{t}"
+        );
+    }
+    let runs = runs_json(&[(t1.clone(), "a".into()), (t2.clone(), "b".into())], &b);
+    assert_eq!(
+        runs[&t1],
+        serde_json::json!({"id": "a", "ticket": "a", "repo": "kod-eval",
+            "data_classes": ["placeholder"], "max_tokens": 100, "max_cost_micro_usd": 7})
+    );
+    assert_eq!(runs[&t2]["id"], "b");
+}
+
+#[test]
+fn a_case_log_keeps_only_that_cases_lines() {
+    let log = "{\"event\":\"ready\"}\n\
+{\"event\":\"allow\",\"run\":\"a\"}\n\
+not json\n\
+{\"event\":\"allow\",\"run\":\"b\"}\n\
+{\"event\":\"usage\",\"run\":\"a\"}\n";
+    assert_eq!(
+        case_log(log, "a"),
+        "{\"event\":\"allow\",\"run\":\"a\"}\n{\"event\":\"usage\",\"run\":\"a\"}\n"
+    );
+    assert_eq!(case_log(log, "c"), "");
+}
+
 #[test]
 fn proxy_limits_come_from_the_environment() {
     let env = |k: &str| match k {
@@ -103,7 +160,7 @@ fn proxy_upstream_defaults_to_anthropic_and_is_passed_and_recorded() {
         "m".into(),
     )
     .unwrap();
-    let a = proxy_args("p", "r", &lim, "img").join(" ");
+    let a = gateway_args("p", "r", &lim, &mounts(), "img").join(" ");
     assert!(a.contains("-e KOD_UPSTREAM=http://10.0.0.1:9 "), "{a}");
     // Manifests from before this field still load.
     let old: ProxyLimits = serde_json::from_str(
````

Apply to `crates/kod-eval/tests/runner.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/runner.rs b/crates/kod-eval/tests/runner.rs
index 6b0f0a4..ba160ef 100644
--- a/crates/kod-eval/tests/runner.rs
+++ b/crates/kod-eval/tests/runner.rs
@@ -58,10 +58,11 @@ fn claude_command_pins_model_and_turns() {
 }
 
 #[test]
-fn claude_env_points_at_the_proxy_with_a_dummy_key() {
-    let env = claude().env();
+fn claude_env_points_at_the_gateway_with_a_dummy_key() {
+    let base = format!("{PROXY_URL}/r/t1");
+    let env = claude().env(&base);
     let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
-    assert_eq!(get("ANTHROPIC_BASE_URL"), Some(PROXY_URL));
+    assert_eq!(get("ANTHROPIC_BASE_URL"), Some(base.as_str()));
     assert_eq!(get("ANTHROPIC_API_KEY"), Some("sk-kod-dummy"));
 }
 
@@ -88,11 +89,18 @@ fn kod_loop_command_and_env() {
             "4096"
         ]
     );
-    // The base URL only: no key, not even a dummy one.
-    assert_eq!(
-        r.env(),
-        vec![("ANTHROPIC_BASE_URL".to_owned(), PROXY_URL.to_owned())]
-    );
+    // The base URL only: no key, not even a dummy one. A script gets the
+    // same, so its model calls meet the same gateway budget.
+    let base = format!("{PROXY_URL}/r/t1");
+    let script = Runner::Script {
+        command: "true".into(),
+    };
+    for r in [r, script] {
+        assert_eq!(
+            r.env(&base),
+            vec![("ANTHROPIC_BASE_URL".to_owned(), base.clone())]
+        );
+    }
 }
 
 #[test]
````

Apply to `crates/kod-eval/tests/sandbox.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/sandbox.rs b/crates/kod-eval/tests/sandbox.rs
index 10c23ca..138e59d 100644
--- a/crates/kod-eval/tests/sandbox.rs
+++ b/crates/kod-eval/tests/sandbox.rs
@@ -130,7 +130,7 @@ fn no_sandbox_container_gets_the_model_key() {
             "net",
             &m(&agent_ro, &["/work", "/tmp"]),
             (1, 1),
-            &runner.env(),
+            &runner.env("http://kod-llm-proxy:8080/r/t"),
         ),
         runner.command(&c),
     ]
@@ -160,7 +160,7 @@ fn no_sandbox_container_gets_the_model_key() {
     }
     assert!(
         runner
-            .env()
+            .env("http://kod-llm-proxy:8080/r/t")
             .contains(&("ANTHROPIC_API_KEY".into(), "sk-kod-dummy".into())),
         "the agent gets a dummy key"
     );
````

Apply to `crates/kod-eval/tests/report.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/report.rs b/crates/kod-eval/tests/report.rs
index 7f49fca..584d372 100644
--- a/crates/kod-eval/tests/report.rs
+++ b/crates/kod-eval/tests/report.rs
@@ -2,7 +2,7 @@ use kod_eval::{
     case::Split,
     report::{compare, load, median, proxy_denials, proxy_tokens, render, write},
     result::{CaseResult, Safety},
-    run::{Manifest, ProxyLimits},
+    run::{Manifest, ProxyLimits, SpendBudget},
 };
 use std::fs;
 
@@ -262,6 +262,11 @@ fn heads_the_report_with_the_manifest_versions_and_limits() {
         kod_eval_git_sha: "abc123".into(),
         proxy: ProxyLimits::from_env(|_| None, "claude-x".into()).unwrap(),
         proxy_image_id: "sha256:p".into(),
+        spend: Some(SpendBudget {
+            max_tokens: 5000,
+            max_cost_micro_usd: 70,
+            prices: serde_json::json!({}),
+        }),
         sandbox_image_ids: [("rust".to_owned(), "sha256:r".to_owned())].into(),
         base_commits: [("a".to_owned(), "f".repeat(40))].into(),
     };
````

Apply to `crates/kod-eval/tests/e2e/mod.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/e2e/mod.rs b/crates/kod-eval/tests/e2e/mod.rs
index e965548..f4d9c78 100644
--- a/crates/kod-eval/tests/e2e/mod.rs
+++ b/crates/kod-eval/tests/e2e/mod.rs
@@ -28,7 +28,7 @@ case "$t" in
 *BLOCK*) echo 'KOD_END_STATE: blocked' ;;
 *EGRESS*)
   curl -s --max-time 5 https://1.1.1.1 > /dev/null && echo leaked > leaked.txt
-  curl -s -o /dev/null -w '%{http_code}' http://kod-llm-proxy:8080/v1/files > proxy_status.txt
+  curl -s -o /dev/null -w '%{http_code}' "$ANTHROPIC_BASE_URL/v1/files" > proxy_status.txt
   echo 42 > answer.txt ;;
 *TAMPER*)
   # Wrong answer, plus a build step that tries to rewrite the held-back test
@@ -188,9 +188,26 @@ pub fn settings(root: &Path) -> Settings {
         extra_models: vec![],
         // Per test, so the tests never touch the host's real stop file.
         stop_file: root.join("STOP"),
+        policy_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"),
+        prices: prices(root),
+        run_max_tokens: 1_000_000,
+        run_max_cost_micro_usd: 1_000_000,
     }
 }
 
+/// A price table for the test runners' models: the script's "none" and
+/// kod-loop's scripted "fake-model".
+fn prices(root: &Path) -> std::path::PathBuf {
+    let price = r#"{"input": 3, "output": 15, "cache_write": 4, "cache_read": 1}"#;
+    let path = root.join("prices.json");
+    fs::write(
+        &path,
+        format!(r#"{{"none": {price}, "fake-model": {price}}}"#),
+    )
+    .unwrap();
+    path
+}
+
 pub fn result(dir: &Path, id: &str) -> CaseResult {
     serde_json::from_str(&fs::read_to_string(dir.join(id).join("result.json")).unwrap()).unwrap()
 }
````

Apply to `crates/kod-eval/tests/docker.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/docker.rs b/crates/kod-eval/tests/docker.rs
index dabc254..651f2d0 100644
--- a/crates/kod-eval/tests/docker.rs
+++ b/crates/kod-eval/tests/docker.rs
@@ -60,6 +60,9 @@ fn end_to_end() {
     assert!(m.proxy_image_id.starts_with("sha256:"), "{text}");
     assert!(m.sandbox_image_ids["test"].starts_with("sha256:"), "{text}");
     assert_eq!(m.proxy.max_requests, 200, "{text}");
+    assert_eq!(m.spend.as_ref().unwrap().max_tokens, 1_000_000, "{text}");
+    // One gateway for the run, with its own audit log.
+    assert!(dir.join("gateway/audit.db").is_file());
     assert_eq!(m.kod_eval_version, env!("CARGO_PKG_VERSION"));
     // The commit this test binary was built from, so "unknown" fails.
     let head = std::process::Command::new("git")
````

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kod-eval`
Expected: FAIL to compile: `Settings` has no field named `policy_dir`, no field `spend` on `Manifest`, and `env` takes 0 arguments.

- [ ] **Step 3: Move kod-eval to the gateway**

Apply to `crates/kod-eval/src/run.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/src/run.rs b/crates/kod-eval/src/run.rs
index 853c222..22a81a5 100644
--- a/crates/kod-eval/src/run.rs
+++ b/crates/kod-eval/src/run.rs
@@ -3,7 +3,7 @@ use crate::{
     case::{EndState, EvalCase},
     prompt, report,
     result::{CaseResult, Safety, parse_junit},
-    runner::Runner,
+    runner::{PROXY_URL, Runner},
     sandbox::{self, Mounts},
     workspace,
 };
@@ -28,6 +28,13 @@ pub struct Settings {
     /// The kill switch. `stop` kills every Kod container on the host, so the
     /// file is host-wide too: see `default_stop_file`.
     pub stop_file: PathBuf,
+    /// The repo's policy directory, mounted read-only into the gateway.
+    pub policy_dir: PathBuf,
+    /// The gateway's price table, copied into the run and its manifest.
+    pub prices: PathBuf,
+    /// Each case's spend budget at the gateway. No default: the caller sets it.
+    pub run_max_tokens: u64,
+    pub run_max_cost_micro_usd: u64,
 }
 
 /// Written before any case starts. Never holds KOD_API_KEY or another secret.
@@ -43,6 +50,8 @@ pub struct Manifest {
     pub kod_eval_git_sha: String,
     pub proxy: ProxyLimits,
     pub proxy_image_id: String,
+    /// Each case's spend budget. None in phase 0 manifests, which had none.
+    pub spend: Option<SpendBudget>,
     /// Image ID per language.
     pub sandbox_image_ids: BTreeMap<String, String>,
     /// Each case's base commit as resolved in its mirror.
@@ -92,6 +101,50 @@ impl ProxyLimits {
     }
 }
 
+/// Each case's spend budget at the gateway, recorded in the manifest.
+#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
+pub struct SpendBudget {
+    pub max_tokens: u64,
+    pub max_cost_micro_usd: u64,
+    /// The price table as loaded, so two runs' costs mean the same thing.
+    pub prices: serde_json::Value,
+}
+
+/// The one data class eval runs carry: the placeholder routing's only class.
+pub const EVAL_DATA_CLASS: &str = "placeholder";
+
+/// The gateway's runs file: one run per case, known by its token. The run id
+/// is the case id, so the gateway's log lines carry it.
+pub fn runs_json(tokens: &[(String, String)], b: &SpendBudget) -> serde_json::Value {
+    let runs = tokens.iter().map(|(token, case)| {
+        let run = serde_json::json!({
+            "id": case,
+            "ticket": case,
+            "repo": "kod-eval",
+            "data_classes": [EVAL_DATA_CLASS],
+            "max_tokens": b.max_tokens,
+            "max_cost_micro_usd": b.max_cost_micro_usd,
+        });
+        (token.clone(), run)
+    });
+    serde_json::Value::Object(runs.collect())
+}
+
+/// A fresh run token: 16 random bytes as 32 lower-case hex characters.
+pub fn token() -> Result<String> {
+    let mut b = [0u8; 16];
+    std::io::Read::read_exact(&mut fs::File::open("/dev/urandom")?, &mut b)?;
+    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
+}
+
+/// The gateway's log lines for one case: those whose run is the case id.
+pub fn case_log(log: &str, case: &str) -> String {
+    log.lines()
+        .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| v["run"] == case))
+        .map(|l| format!("{l}\n"))
+        .collect()
+}
+
 fn image_id(image: &str) -> Result<String> {
     Ok(
         docker(&sv(&["image", "inspect", "--format", "{{.Id}}", image]))?
@@ -231,6 +284,17 @@ pub fn run_all(cases: &[EvalCase], runner: &Runner, s: &Settings) -> Result<Path
             limits.upstream
         );
     }
+    let prices = fs::read_to_string(&s.prices)
+        .with_context(|| format!("reading the price table {}", s.prices.display()))?;
+    let spend = SpendBudget {
+        max_tokens: s.run_max_tokens,
+        max_cost_micro_usd: s.run_max_cost_micro_usd,
+        prices: serde_json::from_str(&prices).context("the price table is not JSON")?,
+    };
+    let policy_dir = s
+        .policy_dir
+        .canonicalize()
+        .with_context(|| format!("the policy directory {}", s.policy_dir.display()))?;
     let mut sandbox_image_ids = BTreeMap::new();
     let mut base_commits = BTreeMap::new();
     for c in cases {
@@ -253,6 +317,7 @@ pub fn run_all(cases: &[EvalCase], runner: &Runner, s: &Settings) -> Result<Path
         kod_eval_git_sha: GIT_SHA.into(),
         proxy: limits.clone(),
         proxy_image_id: image_id(&s.proxy_image)?,
+        spend: Some(spend.clone()),
         sandbox_image_ids,
         base_commits,
     };
@@ -271,13 +336,42 @@ pub fn run_all(cases: &[EvalCase], runner: &Runner, s: &Settings) -> Result<Path
     let net = format!("kod-{id}");
     docker(&sv(&["network", "create", "--internal", &net]))?;
     let _env = RunEnv { id: id.clone() };
-    for case in cases {
+    // One gateway serves every case; each case has its own token, budget,
+    // request count and log lines.
+    let gw_dir = dir.join("gateway");
+    fs::create_dir(&gw_dir)?;
+    let tokens = cases
+        .iter()
+        .map(|c| Ok((token()?, c.id.clone())))
+        .collect::<Result<Vec<_>>>()?;
+    fs::write(gw_dir.join("prices.json"), &prices)?;
+    fs::write(
+        gw_dir.join("runs.json"),
+        runs_json(&tokens, &spend).to_string(),
+    )?;
+    let gw = format!("kod-llm-proxy-{id}");
+    let mounts = GatewayMounts {
+        dir: &gw_dir,
+        policy: &policy_dir,
+        user: host_ids(&gw_dir)?,
+    };
+    let up = start_gateway(
+        &gw,
+        &net,
+        &gateway_args(&gw, &id, &limits, &mounts, &s.proxy_image),
+        s,
+    )?;
+    for (case, (token, _)) in cases.iter().zip(&tokens) {
         let case_dir = dir.join(&case.id);
         fs::create_dir_all(&case_dir)?;
-        let r = if stopped(s) {
+        let r = if !up || stopped(s) {
             cancelled(case, "the kill switch was on before it started")
         } else {
-            run_case(case, runner, s, &limits, &id, &net, &case_dir)?
+            let gateway = CaseGateway {
+                name: &gw,
+                base_url: format!("{PROXY_URL}/r/{token}"),
+            };
+            run_case(case, runner, s, &gateway, &id, &net, &case_dir)?
         };
         fs::write(
             case_dir.join("result.json"),
@@ -308,10 +402,26 @@ fn cancelled(case: &EvalCase, why: &str) -> CaseResult {
     }
 }
 
-/// `docker run` arguments for a case's proxy. The key is passed by name, so
-/// it never appears on a command line. Unset settings fall back to the
-/// proxy's defaults.
-pub fn proxy_args(name: &str, run_id: &str, lim: &ProxyLimits, image: &str) -> Vec<String> {
+/// Where the gateway's files live on the host.
+pub struct GatewayMounts<'a> {
+    /// Holds prices.json and runs.json, and gets audit.db. Mounted at /gateway.
+    pub dir: &'a Path,
+    /// The policy directory, mounted read-only at /policy.
+    pub policy: &'a Path,
+    /// The host uid and gid, so the gateway can write audit.db.
+    pub user: (u32, u32),
+}
+
+/// `docker run` arguments for the run's gateway. The key is passed by name,
+/// so it never appears on a command line. Unset settings fall back to the
+/// gateway's defaults.
+pub fn gateway_args(
+    name: &str,
+    run_id: &str,
+    lim: &ProxyLimits,
+    m: &GatewayMounts,
+    image: &str,
+) -> Vec<String> {
     let mut args = sv(&[
         "run",
         "-d",
@@ -319,9 +429,19 @@ pub fn proxy_args(name: &str, run_id: &str, lim: &ProxyLimits, image: &str) -> V
         name,
         "--label",
         &format!("kod.run={run_id}"),
+        "--user",
+        &format!("{}:{}", m.user.0, m.user.1),
+        "-v",
+        &format!("{}:/gateway", m.dir.display()),
+        "-v",
+        &format!("{}:/policy:ro", m.policy.display()),
     ]);
     args.extend(sv(&["-e", "KOD_API_KEY"]));
     for kv in [
+        "KOD_PRICES=/gateway/prices.json".to_owned(),
+        "KOD_RUNS=/gateway/runs.json".to_owned(),
+        "KOD_POLICY_DIR=/policy".to_owned(),
+        "KOD_AUDIT_DB=/gateway/audit.db".to_owned(),
         format!("KOD_UPSTREAM={}", lim.upstream),
         format!("KOD_ALLOWED_PATHS={}", lim.allowed_paths),
         format!("KOD_MAX_TOKENS={}", lim.max_tokens),
@@ -336,23 +456,16 @@ pub fn proxy_args(name: &str, run_id: &str, lim: &ProxyLimits, image: &str) -> V
     args
 }
 
-/// One proxy per case, so the request budget and the log are per case.
-/// Returns false if the kill switch is on.
-fn start_proxy(
-    name: &str,
-    run_id: &str,
-    net: &str,
-    limits: &ProxyLimits,
-    s: &Settings,
-) -> Result<bool> {
-    let args = proxy_args(name, run_id, limits, &s.proxy_image);
-    // The proxy runs detached, so `launch` can't watch it. Check on both
+/// Starts the run's gateway and waits until it is ready. Returns false if
+/// the kill switch is on.
+fn start_gateway(name: &str, net: &str, args: &[String], s: &Settings) -> Result<bool> {
+    // The gateway runs detached, so `launch` can't watch it. Check on both
     // sides; if stop finished in between, the next launch sees the file and
-    // the run's cleanup removes the proxy.
+    // the run's cleanup removes the gateway.
     if stopped(s) {
         return Ok(false);
     }
-    docker(&args)?;
+    docker(args)?;
     docker(&sv(&[
         "network",
         "connect",
@@ -361,14 +474,47 @@ fn start_proxy(
         net,
         name,
     ]))?;
+    wait_ready(name)?;
     Ok(!stopped(s))
 }
 
+/// Waits up to 10 seconds for the gateway's "ready" line. It checks its
+/// config before it prints it, so a bad price table, runs file or policy
+/// stops the run here, with the gateway's error.
+fn wait_ready(name: &str) -> Result<()> {
+    for _ in 0..100 {
+        let logs = docker(&sv(&["logs", name]))?;
+        let ready = logs.lines().any(|l| {
+            serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| v["event"] == "ready")
+        });
+        if ready {
+            return Ok(());
+        }
+        if docker(&sv(&["inspect", "-f", "{{.State.Running}}", name]))?.trim() != "true" {
+            break;
+        }
+        std::thread::sleep(Duration::from_millis(100));
+    }
+    let err = Command::new("docker")
+        .args(["logs", name])
+        .output()
+        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
+        .unwrap_or_default();
+    bail!("the gateway did not start: {}", err.trim())
+}
+
+/// The run's gateway as one case sees it.
+struct CaseGateway<'a> {
+    name: &'a str,
+    /// The gateway URL with the case's token, `.../r/<token>`.
+    base_url: String,
+}
+
 fn run_case(
     case: &EvalCase,
     runner: &Runner,
     s: &Settings,
-    limits: &ProxyLimits,
+    gateway: &CaseGateway,
     run_id: &str,
     net: &str,
     dir: &Path,
@@ -380,10 +526,6 @@ fn run_case(
     fs::write(input.join("prompt.txt"), prompt::render(case))?;
 
     let image = sandbox::image(case, &s.image_prefix);
-    let proxy = format!("kod-llm-proxy-{run_id}-{}", case.id);
-    if !start_proxy(&proxy, run_id, net, limits, s)? {
-        return Ok(cancelled(case, "the kill switch was turned on"));
-    }
     let name = format!("kod-{run_id}-{}", case.id);
     let mounts = Mounts {
         read_only: &[(&work, "/seed"), (&input, "/in")],
@@ -397,7 +539,7 @@ fn run_case(
         net,
         &mounts,
         host_ids(dir)?,
-        &runner.env(),
+        &runner.env(&gateway.base_url),
     );
     args.push(image.clone());
     args.push("agent".into());
@@ -412,8 +554,9 @@ fn run_case(
     let agent_wall_ms = started.elapsed().as_millis() as u64;
     // The agent only ever had a copy; collect reads out/work.tar.
     let _ = fs::remove_dir_all(&work);
-    let proxy_log = docker(&sv(&["logs", &proxy]));
-    let _ = docker(&sv(&["rm", "-f", &proxy]));
+    // The case's lines from the shared gateway. A response's usage line is
+    // written when its body ends, before the agent sees the end.
+    let proxy_log = docker(&sv(&["logs", gateway.name])).map(|l| case_log(&l, &case.id));
     if launched == Launch::Stopped {
         return Ok(cancelled(case, "the kill switch was turned on"));
     }
````

Apply to `crates/kod-eval/src/runner.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/src/runner.rs b/crates/kod-eval/src/runner.rs
index 0d26a67..18cdce5 100644
--- a/crates/kod-eval/src/runner.rs
+++ b/crates/kod-eval/src/runner.rs
@@ -2,7 +2,8 @@
 //! runner's output to an end state (the runner adapter contract in the spec).
 use crate::case::{EndState, EvalCase};
 
-/// The proxy's alias on each run's internal network.
+/// The gateway's alias on each run's internal network. Each case's base URL
+/// adds `/r/<token>`, which tells the gateway which case is calling.
 pub const PROXY_URL: &str = "http://kod-llm-proxy:8080";
 
 pub enum Runner {
@@ -64,19 +65,22 @@ impl Runner {
         }
     }
 
-    pub fn env(&self) -> Vec<(String, String)> {
+    /// `base_url` is the case's gateway URL, `PROXY_URL/r/<token>`. A script
+    /// gets it too, so a script's model calls meet the same budget.
+    pub fn env(&self, base_url: &str) -> Vec<(String, String)> {
         match self {
             Runner::ClaudeCode { .. } => [
-                ("ANTHROPIC_BASE_URL", PROXY_URL),
+                ("ANTHROPIC_BASE_URL", base_url),
                 // The real key lives only in kod-llm-proxy.
                 ("ANTHROPIC_API_KEY", "sk-kod-dummy"),
                 ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
             ]
             .map(|(k, v)| (k.to_owned(), v.to_owned()))
             .to_vec(),
-            // No key at all: the proxy adds the real one.
-            Runner::KodLoop { .. } => vec![("ANTHROPIC_BASE_URL".into(), PROXY_URL.into())],
-            Runner::Script { .. } => vec![],
+            // No key at all: the gateway adds the real one.
+            Runner::KodLoop { .. } | Runner::Script { .. } => {
+                vec![("ANTHROPIC_BASE_URL".into(), base_url.into())]
+            }
         }
     }
 
````

Apply to `crates/kod-eval/src/main.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/src/main.rs b/crates/kod-eval/src/main.rs
index 100509f..b39fe05 100644
--- a/crates/kod-eval/src/main.rs
+++ b/crates/kod-eval/src/main.rs
@@ -67,6 +67,18 @@ enum Cli {
         /// background model. Repeatable.
         #[arg(long)]
         extra_model: Vec<String>,
+        #[arg(long, default_value = "policy")]
+        policy_dir: PathBuf,
+        /// The gateway's price table: model id to micro-USD per million
+        /// tokens for input, output, cache_write and cache_read
+        #[arg(long)]
+        prices: PathBuf,
+        /// Each case's token budget at the gateway
+        #[arg(long)]
+        run_max_tokens: u64,
+        /// Each case's cost budget at the gateway, in micro-USD
+        #[arg(long)]
+        run_max_cost_micro_usd: u64,
     },
     /// Kill switch: no new containers start, and every Kod container on this
     /// host is killed. Delete the stop file ($KOD_STOP_FILE, or ~/.kod/STOP)
@@ -96,6 +108,10 @@ fn main() -> Result<()> {
             proxy_image,
             out,
             extra_model,
+            policy_dir,
+            prices,
+            run_max_tokens,
+            run_max_cost_micro_usd,
         } => {
             let runner = match (runner.as_str(), model, script) {
                 ("claude-code", Some(model), _) => Runner::ClaudeCode { model },
@@ -132,6 +148,10 @@ fn main() -> Result<()> {
                 out_root: out,
                 extra_models: extra_model,
                 stop_file: run::default_stop_file(),
+                policy_dir,
+                prices,
+                run_max_tokens,
+                run_max_cost_micro_usd,
             };
             let dir = run::run_all(&cases, &runner, &settings)?;
             println!("results: {}", dir.display());
````

Apply to `README.md` with `git apply`:

````diff
diff --git a/README.md b/README.md
index d57c7cc..740dc37 100644
--- a/README.md
+++ b/README.md
@@ -55,7 +55,11 @@ kod-eval check-case evals/cases/PAY-123
 
 # 3. Run the dev split against a pinned model, then read the report
 export KOD_API_KEY=...                       # only in this shell; a key with a provider spend limit
-kod-eval run --split dev --model <model id>
+# prices.json: the model's price in micro-USD per million tokens, from the
+# provider's price page, e.g. {"<model id>": {"input": 3000000, "output":
+# 15000000, "cache_write": 3750000, "cache_read": 300000}}. The budget is per case.
+kod-eval run --split dev --model <model id> --prices prices.json \
+  --run-max-tokens 2000000 --run-max-cost-micro-usd 5000000
 kod-eval report results/<run>                # exits 1 if any case is unsafe or not verified
 
 # Kill switch, from any shell: stops everything and refuses new runs
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p kod-eval && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clippy clean.

Then the docker tests, which build the real images and run every runner through one gateway:

Run: `docker build -t kod-llm-proxy --build-arg RUST_VERSION=1.99.0 -f images/proxy/Dockerfile . && KOD_DOCKER_TESTS=1 KOD_API_KEY=dummy cargo test -p kod-eval --test docker --test kill_switch --test kod_loop`
Expected: PASS (docker 2, kill_switch 1, kod_loop 2). Each run dir has `gateway/audit.db`, and the script case's call to `$ANTHROPIC_BASE_URL/v1/files` gets 403.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-eval README.md
git commit -m "kod-eval: run every case through one gateway with a per-case budget"
```

### Task 4: Compare with spend budgets

**Files:**
- Modify: `crates/kod-eval/src/report.rs`
- Test: `crates/kod-eval/tests/report.rs`

**Interfaces:**
- Consumes: `Manifest.spend` and `SpendBudget` from Task 3; the `over_spend` and `halted` events from Task 2.
- Produces: `report::spend_refusals(run_dir: &Path, m: &Manifest) -> usize`; `compare` refuses different budgets, and a budgeted run that refused requests against a run with none; the report header gains "Spend budget per case".

- [ ] **Step 1: Write the failing tests**

Apply to `crates/kod-eval/tests/report.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/tests/report.rs b/crates/kod-eval/tests/report.rs
index 584d372..f0ede8a 100644
--- a/crates/kod-eval/tests/report.rs
+++ b/crates/kod-eval/tests/report.rs
@@ -280,6 +280,7 @@ fn heads_the_report_with_the_manifest_versions_and_limits() {
         "rust: sha256:r",
         "max_tokens 8192",
         "max_requests 200",
+        "Spend budget per case: max_tokens 5000; max_cost_micro_usd 70",
     ] {
         assert!(text.contains(want), "missing {want}: {text}");
     }
@@ -412,6 +413,61 @@ fn compare_refuses_runs_with_a_different_model_or_gateway() {
     assert!(ok);
 }
 
+fn budget(max_tokens: u64) -> Option<SpendBudget> {
+    Some(SpendBudget {
+        max_tokens,
+        max_cost_micro_usd: 1_000_000,
+        prices: serde_json::json!({"m": {"input": 3, "output": 15}}),
+    })
+}
+
+#[test]
+fn compare_refuses_runs_with_different_budgets_or_prices() {
+    let changes: [fn(&mut Manifest); 2] = [
+        |m| m.spend = budget(1),
+        |m| m.spend.as_mut().unwrap().prices = serde_json::json!({}),
+    ];
+    for change in changes {
+        let base = run_with(&[passing("a")]);
+        let cand = run_with(&[passing("a")]);
+        edit_manifest(base.path(), |m| m.spend = budget(100));
+        edit_manifest(cand.path(), |m| m.spend = budget(100));
+        edit_manifest(cand.path(), change);
+        let err = compare(base.path(), cand.path()).unwrap_err().to_string();
+        assert!(err.contains("different spend budgets"), "{err}");
+    }
+}
+
+/// A phase 0 baseline had no budget. It still compares with a budgeted run,
+/// unless the budget refused a request and so may have changed the result.
+#[test]
+fn a_phase0_baseline_compares_only_if_the_budget_refused_nothing() {
+    let base = run_with(&[passing("a")]);
+    let cand = run_with(&[passing("a")]);
+    edit_manifest(cand.path(), |m| m.spend = budget(100));
+    fs::write(
+        cand.path().join("a/proxy.log"),
+        "{\"event\":\"allow\",\"run\":\"a\"}\n",
+    )
+    .unwrap();
+    let (_, ok) = compare(base.path(), cand.path()).unwrap();
+    assert!(ok);
+    for event in ["over_spend", "halted"] {
+        fs::write(
+            cand.path().join("a/proxy.log"),
+            format!("{{\"event\":\"{event}\",\"run\":\"a\"}}\n"),
+        )
+        .unwrap();
+        for (b, c) in [(&base, &cand), (&cand, &base)] {
+            let err = compare(b.path(), c.path()).unwrap_err().to_string();
+            assert!(
+                err.contains("refused 1 requests on its spend budget"),
+                "{err}"
+            );
+        }
+    }
+}
+
 fn put_input(d: &std::path::Path, id: &str, file: &str, text: &str) {
     let dir = d.join(id).join("inputs");
     fs::create_dir_all(&dir).unwrap();
````

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kod-eval --test report`
Expected: FAIL: `compare_refuses_runs_with_different_budgets_or_prices`, `a_phase0_baseline_compares_only_if_the_budget_refused_nothing` and `heads_the_report_with_the_manifest_versions_and_limits` (missing "Spend budget per case").

- [ ] **Step 3: Check budgets in compare**

Apply to `crates/kod-eval/src/report.rs` with `git apply`:

````diff
diff --git a/crates/kod-eval/src/report.rs b/crates/kod-eval/src/report.rs
index 4149a1e..622ba0d 100644
--- a/crates/kod-eval/src/report.rs
+++ b/crates/kod-eval/src/report.rs
@@ -60,6 +60,20 @@ pub fn proxy_denials(log: &str) -> usize {
         .count()
 }
 
+/// Requests the gateway refused on a case's spend budget, over every case's
+/// proxy log. A missing log counts nothing; that case is not verified anyway.
+pub fn spend_refusals(run_dir: &Path, m: &Manifest) -> usize {
+    m.cases
+        .iter()
+        .map(|id| {
+            let log = fs::read_to_string(run_dir.join(id).join("proxy.log")).unwrap_or_default();
+            proxy_events(&log)
+                .filter(|v| v["event"] == "over_spend" || v["event"] == "halted")
+                .count()
+        })
+        .sum()
+}
+
 /// Input and output tokens summed over the proxy's usage lines (None when no
 /// line had a value), and how many model responses lack either count: usage
 /// lines with a null, plus successful /v1/messages responses that never got a
@@ -132,6 +146,7 @@ pub fn render(m: &Manifest, results: &[CaseResult], denials: usize) -> String {
          - Proxy image: {}\n\
          - Sandbox images: {}\n\
          - Proxy limits: models {}; paths {}; max_tokens {}; max_body {}; max_requests {}; max_concurrent {}\n\
+         - Spend budget per case: {}\n\
          - Inputs and base commits: manifest.json and each case's inputs/\n\
          \n## Safety failures\n\n",
         m.run_id,
@@ -146,7 +161,11 @@ pub fn render(m: &Manifest, results: &[CaseResult], denials: usize) -> String {
         p.max_tokens,
         p.max_body,
         p.max_requests,
-        p.max_concurrent
+        p.max_concurrent,
+        m.spend.as_ref().map_or("none".to_owned(), |b| format!(
+            "max_tokens {}; max_cost_micro_usd {}",
+            b.max_tokens, b.max_cost_micro_usd
+        ))
     );
     let safety: Vec<String> = results
         .iter()
@@ -313,6 +332,26 @@ pub fn compare(baseline: &Path, candidate: &Path) -> Result<(String, bool)> {
             setup(&cm)
         );
     }
+    // Budgets and prices must match. A phase 0 run had no spend budget; it
+    // compares with a budgeted run only if that budget refused nothing, so
+    // the budget did not change the result.
+    match (&bm.spend, &cm.spend) {
+        (Some(a), Some(b)) if a != b => {
+            bail!("the runs had different spend budgets or price tables; rerun one of them")
+        }
+        (Some(_), None) | (None, Some(_)) => {
+            for (d, m) in [(baseline, &bm), (candidate, &cm)] {
+                let n = spend_refusals(d, m);
+                if m.spend.is_some() && n > 0 {
+                    bail!(
+                        "run {} refused {n} requests on its spend budget and the other run had no budget; rerun both with one",
+                        m.run_id
+                    );
+                }
+            }
+        }
+        _ => {}
+    }
     // The same ids must also mean the same experiment: the same resolved base
     // commits and the same saved inputs (ticket, tests, budget, held-back
     // patch). No runner-specific difference is allowed in these, and missing
````

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS, clean.

Then the phase exit check for this sub-plan: rerun the eval cases used for the phase 0 baseline with the gateway (`kod-eval run` with `--prices`, `--run-max-tokens` and `--run-max-cost-micro-usd` set high enough that nothing is refused), and `kod-eval compare <baseline> <rerun>`.
Expected: compare accepts the pair and shows no regression in passes, zero unsafe and zero not_verified.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-eval
git commit -m "kod-eval: compare checks spend budgets"
```

## Whole-branch review

One fresh reviewer on the most capable model reads the whole branch against the design doc and this plan, with these questions:
- Is anything forwarded before its routing decision and prompt are in the audit log?
- Can any path through `forward` reach the upstream without a reservation, or leave a reservation unsettled?
- Does every config fault stop the gateway before `ready`?
- Do phase 0 runs still compare, and only when the comparison is fair?

## Self-review

- **Spec coverage.** Long-running service in place of one per case: Tasks 2 and 3. Per-run budgets in tokens and cost from a price table: Tasks 1 and 2. Routing per data class, unknown class refused, disallowed endpoint refused: Task 2 (`an_unknown_data_class_is_refused_and_recorded`, `a_disallowed_endpoint_is_refused`). Redacted capture to the audit log, failing closed: Task 2. Spend can't exceed the budget whether the loop or a script calls it: Task 2 (kod-loop's `a_request_over_the_spend_budget_never_reaches_the_model`, and the gateway tests use plain HTTP clients, as a script would) and Task 3 (the script runner gets the same base URL). Eval rerun with no regression: Task 4, Step 4. OpenAI route: not built (D11). Structured-log tracing: every line is JSON with the run id.
- **Placeholders.** None. Routing values are the policy repo's placeholders, by design.
- **Type consistency.** `Budget`, `RunSpec`, `Price`, `Spend`, `Ledger`, `Config`, `Endpoint`, `SpendBudget`, `GatewayMounts` are spelled the same in every task; all code was compiled and tested in this order.
- **Review Focus.** Each of the five lines names its test and task above.

## Open items

- **Routing values** (owner): the real data classes and their endpoints. Until then only `placeholder` → `placeholder` is allowed.
- **Prices** (owner): the real per-model price table for evals; `--prices` has no default on purpose.
- **Budget size** (owner): the per-case token and cost budget for the phase 1 eval rerun; the flags are required, so each run states it.
- **Halting on a failed output write**: the run stops rather than continue with a missing record. Say if a warning is preferred.
- **Live run registration**: runs are fixed at start (D2). Sub-plan 5 adds them when the loop needs it.
