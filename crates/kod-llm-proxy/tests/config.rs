use kod_llm_proxy::config::{Budget, Price, parse_limit, parse_paths, parse_prices, parse_runs};

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

#[test]
fn allowed_paths_are_a_subset_of_the_priced_ones() {
    assert_eq!(
        parse_paths(" /v1/messages, /v1/messages/count_tokens ").unwrap(),
        ["/v1/messages", "/v1/messages/count_tokens"]
    );
    for (text, want) in [
        ("", "KOD_ALLOWED_PATHS is empty"),
        (" , ", "KOD_ALLOWED_PATHS is empty"),
        (
            "/v1/messages,/v1/messages/batches",
            "/v1/messages/batches is not one of",
        ),
        ("/v1/files", "/v1/files is not one of"),
        ("/v1/messages/", "/v1/messages/ is not one of"),
    ] {
        let e = err(parse_paths(text));
        assert!(e.contains(want), "{text}: {e}");
    }
}

#[test]
fn limits_are_whole_numbers_above_zero() {
    assert_eq!(parse_limit("KOD_MAX_CONCURRENT", "2").unwrap(), 2);
    for text in ["0", "-1", "1.5", "", "two"] {
        let e = err(parse_limit("KOD_MAX_CONCURRENT", text));
        assert_eq!(e, "KOD_MAX_CONCURRENT must be a whole number above zero");
    }
}

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
    let start = |runs: &str, skip: &str, set: (&str, &str)| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_kod-llm-proxy"));
        c.env_clear()
            .env("KOD_API_KEY", "k")
            .env("KOD_ENDPOINT", "placeholder")
            .env("KOD_UPSTREAM", "http://127.0.0.1:9")
            .env("KOD_ALLOWED_MODELS", "claude-test-1")
            .env("KOD_PRICES", path("prices.json"))
            .env("KOD_RUNS", path(runs))
            .env("KOD_POLICY_DIR", &policy)
            .env("KOD_AUDIT_DB", path("audit.db"))
            .env("KOD_LISTEN", "127.0.0.1:0")
            .env_remove(skip);
        if !set.0.is_empty() {
            c.env(set.0, set.1);
        }
        c.output().unwrap()
    };
    let none = ("", "");
    for (runs, skip, set, want) in [
        ("bad-runs.json", "", none, "at least 32 lower-case hex"),
        ("runs.json", "KOD_PRICES", none, "KOD_PRICES is required"),
        ("runs.json", "KOD_RUNS", none, "KOD_RUNS is required"),
        (
            "runs.json",
            "KOD_POLICY_DIR",
            none,
            "KOD_POLICY_DIR is required",
        ),
        (
            "runs.json",
            "KOD_AUDIT_DB",
            none,
            "KOD_AUDIT_DB is required",
        ),
        (
            "runs.json",
            "KOD_ENDPOINT",
            none,
            "KOD_ENDPOINT is required",
        ),
        (
            "runs.json",
            "KOD_UPSTREAM",
            none,
            "KOD_UPSTREAM is required",
        ),
        ("missing.json", "", none, "KOD_RUNS: reading"),
        (
            "runs.json",
            "",
            ("KOD_MAX_CONCURRENT", "0"),
            "KOD_MAX_CONCURRENT must be a whole number above zero",
        ),
        (
            "runs.json",
            "",
            ("KOD_ALLOWED_PATHS", "/v1/messages/batches"),
            "/v1/messages/batches is not one of",
        ),
        (
            "runs.json",
            "",
            ("KOD_ENDPOINT", " "),
            "KOD_ENDPOINT must be",
        ),
    ] {
        let out = start(runs, skip, set);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{runs} {skip} {set:?}");
        assert!(err.contains(want), "{runs} {skip} {set:?}: {err}");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("ready"));
    }
}
