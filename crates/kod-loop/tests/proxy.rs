//! The loop through the real kod-llm-proxy, as in the sandbox.
mod common;
use common::*;
use kod_llm_proxy::{Config as Proxy, Limits, app};
use kod_loop::{Config, EndState};
use serde_json::json;

fn proxy(upstream: &str, max_requests: u64) -> String {
    serve(app(Proxy {
        upstream: upstream.into(),
        api_key: "real-key".parse().unwrap(),
        allowed_paths: vec!["/v1/messages".into()],
        limits: Limits {
            allowed_models: vec!["m".into()],
            max_tokens: 1000,
            max_body: 4 << 20,
            max_requests,
            max_concurrent: 2,
        },
    }))
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
    let (o, _) = go(&config(&proxy(&upstream, 2), ws.path()), "t");
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
    let cfg = Config {
        model: "other".into(),
        ..config(&proxy(&upstream, 10), ws.path())
    };
    let (o, _) = go(&cfg, "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("403"));
    assert!(m.lock().unwrap().seen.is_empty());
}
