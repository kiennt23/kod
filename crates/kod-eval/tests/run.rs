use kod_eval::{
    case::EndState,
    run::{decide, proxy_args},
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

/// Every proxy setting is passed by name, so the key never appears on a
/// command line and unset ones fall back to the proxy's defaults.
#[test]
fn proxy_gets_its_settings_by_name() {
    let a = proxy_args("p", "run-1", "m1,m2", "kod-llm-proxy").join(" ");
    for k in [
        "KOD_API_KEY",
        "KOD_ALLOWED_PATHS",
        "KOD_MAX_TOKENS",
        "KOD_MAX_BODY",
        "KOD_MAX_REQUESTS",
        "KOD_MAX_CONCURRENT",
    ] {
        assert!(a.contains(&format!("-e {k} ")), "missing {k} in {a}");
    }
    assert!(!a.contains("KOD_API_KEY="), "{a}");
    assert!(a.contains("--label kod.run=run-1"), "{a}");
    assert!(
        a.ends_with("-e KOD_ALLOWED_MODELS=m1,m2 kod-llm-proxy"),
        "{a}"
    );
}
