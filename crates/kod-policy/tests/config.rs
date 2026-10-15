//! `Policy::load` is the single gate for the data files: a file the rules
//! read that is missing, or a value whose wrong shape would let a request
//! through or skip a rule, refuses the whole policy.

mod common;

use common::load_error;
use serde_json::{Value, json};

/// `file` from `policy/` with `key` set to `value`, as a load error.
fn error_with(file: &str, key: &str, value: Value) -> String {
    let text = std::fs::read_to_string(common::policy_dir().join(file)).unwrap();
    let mut config: Value = serde_json::from_str(&text).unwrap();
    config[key] = value;
    load_error(file, Some(config))
}

fn assert_refused(error: String, file: &str, problem: &str) {
    assert!(
        error.contains(file) && error.contains(problem),
        "{problem}: {error}"
    );
}

#[test]
fn every_data_file_the_rules_read_is_required() {
    for stem in [
        "compliance",
        "delegation",
        "people",
        "publish",
        "routing",
        "subscription",
        "teams",
    ] {
        let file = format!("{stem}.json");
        assert_refused(load_error(&file, None), &file, "missing");
    }
}

#[test]
fn malformed_path_rule_config_fails_to_load() {
    let repo = |rule: Value| json!({"acme/app": [rule]});
    let rule = |prefix: &str| json!({"prefix": prefix, "class": "high"});
    let mut cases = vec![
        (
            "repo_path_rules",
            repo(json!({"prefix": "billing/"})),
            "repo_path_rules.acme/app[0]: class must be one of risk_classes",
        ),
        (
            "repo_path_rules",
            repo(json!({"prefix": "billing/", "class": "hgh"})),
            "repo_path_rules.acme/app[0]: class must be one of risk_classes",
        ),
        (
            "repo_path_rules",
            repo(json!({"class": "high"})),
            "repo_path_rules.acme/app[0]: prefix must be a non-empty string",
        ),
        (
            "repo_path_rules",
            repo(rule("")),
            "repo_path_rules.acme/app[0]: prefix must be a non-empty string",
        ),
        (
            "repo_path_rules",
            repo(json!({"prefix": "billing/", "class": "high", "clas": "high"})),
            "repo_path_rules.acme/app[0]: unexpected key clas",
        ),
        (
            "repo_path_rules",
            json!({"Acme/App": [rule("billing/")]}),
            "repo_path_rules key \"Acme/App\" must be lower-case owner/name",
        ),
        (
            "repo_path_rules",
            json!({" acme/app": [rule("billing/")]}),
            "repo_path_rules key \" acme/app\" must be lower-case owner/name",
        ),
        (
            "repo_path_rules",
            json!({"acme/app": rule("billing/")}),
            "repo_path_rules.acme/app must be a list of rules",
        ),
        (
            "default_path_rules",
            json!([{"prefix": "auth/"}]),
            "default_path_rules[0]: class must be one of risk_classes",
        ),
        (
            "default_path_rules",
            json!([]),
            "default_path_rules must not be empty",
        ),
    ];
    // Rego normalises every path before matching, so a prefix that isn't
    // already normalised would never match: refused, not skipped.
    for prefix in [
        " auth/",
        "/auth/",
        "./auth/",
        "src\\auth/",
        "src//auth/",
        "src/./auth/",
        "src/../auth/",
        "Auth/",
    ] {
        cases.push((
            "repo_path_rules",
            repo(rule(prefix)),
            "is not a normalised path",
        ));
    }
    for (key, value, problem) in cases {
        assert_refused(
            error_with("delegation.json", key, value),
            "delegation.json",
            problem,
        );
    }
}

#[test]
fn delegation_classes_and_limits_must_be_well_formed() {
    for (key, value, problem) in [
        (
            "risk_classes",
            json!({}),
            "risk_classes must be a non-empty object",
        ),
        (
            "risk_classes",
            json!({"low": 0, "medium": 1, "high": 1}),
            "risk_classes.medium must be a rank no other class has",
        ),
        (
            "risk_classes",
            json!({"low": 0, "medium": "1", "high": 2}),
            "risk_classes.medium must be a rank no other class has",
        ),
        (
            "allowed_classes",
            json!(["low", "hgh"]),
            "allowed_classes entry \"hgh\" is not in risk_classes",
        ),
        (
            "allowed_classes",
            json!("low"),
            "allowed_classes must be a list",
        ),
        (
            "wip_limit",
            json!("1"),
            "wip_limit must be a non-negative integer",
        ),
        (
            "wip_limit",
            json!(1.5),
            "wip_limit must be a non-negative integer",
        ),
        (
            "bounce_limit",
            json!(-1),
            "bounce_limit must be a non-negative integer",
        ),
        (
            "bounce_limit",
            Value::Null,
            "bounce_limit must be a non-negative integer",
        ),
    ] {
        assert_refused(
            error_with("delegation.json", key, value),
            "delegation.json",
            problem,
        );
    }
}

#[test]
fn people_must_name_the_owner_and_only_real_logins() {
    for (key, value, problem) in [
        ("owner", Value::Null, "owner must be a login"),
        ("owner", json!(""), "owner must be a login"),
        ("backup", json!(""), "backup must be null or a login"),
        ("backup", json!(" bk"), "backup must be null or a login"),
        (
            "tech_leads",
            json!([""]),
            "tech_leads must be a list of logins",
        ),
        (
            "tech_leads",
            json!("tl"),
            "tech_leads must be a list of logins",
        ),
    ] {
        assert_refused(
            error_with("people.json", key, value),
            "people.json",
            problem,
        );
    }
}

#[test]
fn team_settings_must_be_known_and_canonical() {
    let problem = "must be {\"requester_approval\": \"allowed\" | \"not_sole\" | \"forbidden\"}";
    for teams in [
        json!({"core": {"requester_approval": "allow"}}),
        json!({"core": {"requestor_approval": "forbidden"}}),
        json!({"core": {"requester_approval": "forbidden", "x": 1}}),
        json!({"core": "forbidden"}),
    ] {
        assert_refused(load_error("teams.json", Some(teams)), "teams.json", problem);
    }
    assert_refused(
        load_error(
            "teams.json",
            Some(json!({"Core": {"requester_approval": "forbidden"}})),
        ),
        "teams.json",
        "team key \"Core\" must be lower-case with no spaces",
    );
    assert_refused(
        load_error("teams.json", Some(json!([]))),
        "teams.json",
        "must be an object of teams",
    );
}

#[test]
fn retention_periods_must_be_non_negative_numbers() {
    for (profiles, problem) in [
        (json!([]), "profiles must be an object"),
        (json!({"a": "x"}), "profile a must be an object"),
        (
            json!({"a": {"retention_days": 30}}),
            "a.retention_days must be an object",
        ),
        (
            json!({"a": {"retention_days": {"run_audit": -1}}}),
            "a.retention_days.run_audit must be a non-negative number",
        ),
        (
            json!({"a": {"retention_days": {"run_audit": "30"}}}),
            "a.retention_days.run_audit must be a non-negative number",
        ),
    ] {
        assert_refused(
            error_with("compliance.json", "profiles", profiles),
            "compliance.json",
            problem,
        );
    }
}

#[test]
fn routing_subscription_and_publish_hold_only_names() {
    // A name is a non-empty string with no spaces at the ends. An object
    // where a list is expected is refused: `in` would match its values.
    let names = "must be a list of non-empty names with no spaces at the ends";
    let runner = |r: Value| json!({"codex": r});
    let cases = [
        (
            "routing.json",
            "classes",
            json!({}),
            "classes must be a non-empty object".to_string(),
        ),
        (
            "routing.json",
            "classes",
            json!({"placeholder": {"endpoints": {"a": "placeholder"}}}),
            format!("classes.placeholder.endpoints {names}"),
        ),
        (
            "routing.json",
            "classes",
            json!({"placeholder": {"endpoints": [""]}}),
            format!("classes.placeholder.endpoints {names}"),
        ),
        (
            "routing.json",
            "classes",
            json!({"placeholder": {"endpoints": ["x"], "x": 1}}),
            "classes.placeholder: unexpected key x".to_string(),
        ),
        (
            "routing.json",
            "classes",
            json!({" placeholder": {"endpoints": ["x"]}}),
            "class key \" placeholder\" must be lower-case with no spaces".to_string(),
        ),
        (
            "subscription.json",
            "contexts",
            json!(["offline_eval", " "]),
            format!("contexts {names}"),
        ),
        (
            "subscription.json",
            "classes",
            json!("placeholder"),
            format!("classes {names}"),
        ),
        (
            "subscription.json",
            "runners",
            json!([]),
            "runners must be an object".to_string(),
        ),
        (
            "subscription.json",
            "runners",
            runner(json!({"plans": {"x": "business"}})),
            format!("runners.codex.plans {names}"),
        ),
        (
            "subscription.json",
            "runners",
            runner(json!({"plans": ["business", ""]})),
            format!("runners.codex.plans {names}"),
        ),
        (
            "subscription.json",
            "runners",
            runner(json!({"plans": ["business"], "workspace": ""})),
            "runners.codex.workspace must be null or a name".to_string(),
        ),
        (
            "subscription.json",
            "runners",
            runner(json!({"plans": ["business"], "workspace": " "})),
            "runners.codex.workspace must be null or a name".to_string(),
        ),
        (
            "subscription.json",
            "runners",
            runner(json!({"plans": ["business"], "worksapce": "ws-1"})),
            "runners.codex: unexpected key worksapce".to_string(),
        ),
        (
            "publish.json",
            "cleared_classes",
            json!({"a": "placeholder"}),
            format!("cleared_classes {names}"),
        ),
        (
            "publish.json",
            "cleared_classes",
            json!([""]),
            format!("cleared_classes {names}"),
        ),
    ];
    for (file, key, value, problem) in cases {
        assert_refused(error_with(file, key, value), file, &problem);
    }
}
