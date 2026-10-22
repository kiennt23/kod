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

/// The run token in the base URL is a credential: a failed request does not
/// put it in the outcome or the trace.
#[test]
fn a_failed_request_does_not_leak_the_run_token() {
    let ws = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:9/r/{TOKEN}");
    let (o, trace) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    let error = o.error.unwrap();
    assert!(error.contains("request failed"), "{error}");
    assert!(!error.contains(TOKEN), "{error}");
    for line in trace {
        assert!(!line.to_string().contains(TOKEN), "{line}");
    }
}
