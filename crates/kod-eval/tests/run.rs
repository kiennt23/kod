use kod_eval::{
    case::EndState,
    run::{
        GatewayMounts, ProxyLimits, SpendBudget, case_log, decide, gateway_args, runs_json, token,
    },
    runner::Runner,
};

#[test]
fn timeouts_kills_and_missing_output_map_to_end_states() {
    let r = Runner::Script {
        command: "true".into(),
    };
    assert_eq!(decide(&r, true, Some(0), "").0, EndState::OverBudget);
    assert_eq!(decide(&r, false, Some(137), "").0, EndState::OverBudget); // OOM kill
    assert_eq!(decide(&r, false, None, "").0, EndState::NeedsHuman);
    assert_eq!(decide(&r, false, Some(0), "").0, EndState::Done);
}

fn mounts() -> GatewayMounts<'static> {
    GatewayMounts {
        dir: std::path::Path::new("/res/run-1/gateway"),
        policy: std::path::Path::new("/repo/policy"),
        user: (501, 20),
    }
}

/// The key is passed by name, so it never appears on a command line. The
/// limits are passed as the values recorded in the manifest, and the
/// gateway's files are mounted where its settings point.
#[test]
fn gateway_gets_the_key_by_name_the_recorded_limits_and_its_files() {
    let lim = ProxyLimits::from_env(|_| None, "m1,m2".into()).unwrap();
    let a = gateway_args("p", "run-1", &lim, &mounts(), "kod-llm-proxy").join(" ");
    assert!(a.contains("-e KOD_API_KEY "), "{a}");
    for kv in [
        "KOD_ALLOWED_PATHS=/v1/messages,/v1/messages/count_tokens",
        "KOD_MAX_TOKENS=8192",
        "KOD_MAX_BODY=4194304",
        "KOD_MAX_REQUESTS=200",
        "KOD_MAX_CONCURRENT=2",
        "KOD_PRICES=/gateway/prices.json",
        "KOD_RUNS=/gateway/runs.json",
        "KOD_POLICY_DIR=/policy",
        "KOD_AUDIT_DB=/gateway/audit.db",
        "KOD_ENDPOINT=placeholder",
    ] {
        assert!(a.contains(&format!("-e {kv} ")), "missing {kv} in {a}");
    }
    assert!(!a.contains("KOD_API_KEY="), "{a}");
    assert!(a.contains("--label kod.run=run-1"), "{a}");
    assert!(a.contains("--user 501:20 "), "{a}");
    assert!(a.contains("-v /res/run-1/gateway:/gateway "), "{a}");
    assert!(a.contains("-v /repo/policy:/policy:ro "), "{a}");
    assert!(
        a.ends_with("-e KOD_ALLOWED_MODELS=m1,m2 kod-llm-proxy"),
        "{a}"
    );
}

#[test]
fn each_case_is_a_gateway_run_with_its_own_token_and_the_budget() {
    let b = SpendBudget {
        max_tokens: 100,
        max_cost_micro_usd: 7,
        prices: serde_json::json!({}),
    };
    let (t1, t2) = (token().unwrap(), token().unwrap());
    assert_ne!(t1, t2);
    for t in [&t1, &t2] {
        assert_eq!(t.len(), 32);
        assert!(
            t.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
            "{t}"
        );
    }
    let runs = runs_json(&[(t1.clone(), "a".into()), (t2.clone(), "b".into())], &b);
    assert_eq!(
        runs[&t1],
        serde_json::json!({"id": "a", "ticket": "a", "repo": "kod-eval",
            "data_classes": ["placeholder"], "max_tokens": 100, "max_cost_micro_usd": 7})
    );
    assert_eq!(runs[&t2]["id"], "b");
}

#[test]
fn a_case_log_keeps_only_that_cases_lines() {
    let log = "{\"event\":\"ready\"}\n\
{\"event\":\"allow\",\"run\":\"a\"}\n\
not json\n\
{\"event\":\"allow\",\"run\":\"b\"}\n\
{\"event\":\"usage\",\"run\":\"a\"}\n";
    assert_eq!(
        case_log(log, "a"),
        "{\"event\":\"allow\",\"run\":\"a\"}\n{\"event\":\"usage\",\"run\":\"a\"}\n"
    );
    assert_eq!(case_log(log, "c"), "");
}

#[test]
fn proxy_limits_come_from_the_environment() {
    let env = |k: &str| match k {
        "KOD_MAX_TOKENS" => Some("100".to_owned()),
        "KOD_ALLOWED_PATHS" => Some("/v1/messages".to_owned()),
        _ => Some("sekret-key-value".to_owned()),
    };
    assert!(
        ProxyLimits::from_env(env, "m".into()).is_err(),
        "non-numbers are refused"
    );
    let env = |k: &str| match k {
        "KOD_API_KEY" => Some("sekret-key-value".to_owned()),
        "KOD_MAX_TOKENS" => Some("100".to_owned()),
        _ => None,
    };
    let lim = ProxyLimits::from_env(env, "m".into()).unwrap();
    assert_eq!((lim.max_tokens, lim.max_requests), (100, 200));
}

/// Unset settings are recorded as the values kod-llm-proxy actually uses.
#[test]
fn recorded_proxy_defaults_are_the_proxys_own() {
    use kod_llm_proxy as p;
    let lim = ProxyLimits::from_env(|_| None, "m".into()).unwrap();
    assert_eq!(lim.allowed_paths, p::DEFAULT_ALLOWED_PATHS);
    assert_eq!(
        (
            lim.max_tokens,
            lim.max_body,
            lim.max_requests,
            lim.max_concurrent
        ),
        (
            p::DEFAULT_MAX_TOKENS,
            p::DEFAULT_MAX_BODY,
            p::DEFAULT_MAX_REQUESTS,
            p::DEFAULT_MAX_CONCURRENT
        )
    );
}

#[test]
fn only_a_recorded_zero_passes_the_process_gate() {
    use kod_eval::run::process_gate;
    assert!(process_gate(Some("0\n")).is_ok());
    assert!(
        process_gate(Some("3"))
            .unwrap_err()
            .contains("alive at archive: 3")
    );
    assert!(process_gate(Some("")).is_err());
    assert!(process_gate(None).unwrap_err().contains("no process check"));
}

#[test]
fn proxy_upstream_defaults_to_anthropic_and_is_passed_and_recorded() {
    let lim = ProxyLimits::from_env(|_| None, "m".into()).unwrap();
    assert_eq!(lim.upstream, "https://api.anthropic.com");
    let lim = ProxyLimits::from_env(
        |k| (k == "KOD_UPSTREAM").then(|| "http://10.0.0.1:9".to_owned()),
        "m".into(),
    )
    .unwrap();
    let a = gateway_args("p", "r", &lim, &mounts(), "img").join(" ");
    assert!(a.contains("-e KOD_UPSTREAM=http://10.0.0.1:9 "), "{a}");
    // Manifests from before this field still load.
    let old: ProxyLimits = serde_json::from_str(
        r#"{"allowed_models":"m","allowed_paths":"/v1/messages","max_tokens":1,"max_body":1,"max_requests":1,"max_concurrent":1}"#,
    )
    .unwrap();
    assert_eq!(old.upstream, "");
}
