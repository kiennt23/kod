mod common;

use common::{allowed, allowed_by, decide, denied, denied_by, policy_with, set, unset};
use std::collections::BTreeSet;

use serde_json::{Value, json};

/// `known_actions` as policy/kod.rego defines it, read straight from the Rego.
fn known_actions() -> BTreeSet<String> {
    let mut engine = regorus::Engine::new();
    for entry in std::fs::read_dir(common::policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rego") {
            let text = std::fs::read_to_string(&path).unwrap();
            engine.add_policy(path.display().to_string(), text).unwrap();
        }
    }
    let value = engine
        .eval_rule("data.kod.known_actions".to_string())
        .unwrap();
    serde_json::from_str(&value.to_json_str().unwrap()).unwrap()
}

fn delegate() -> Value {
    json!({
        "action": "delegate",
        "ticket": {"repo": "acme/app", "risk_class": "low", "paths": ["docs/readme.md"], "acceptance_criteria": true, "bounces": 0},
        "wip_in_progress": 0
    })
}

// delegate

#[test]
fn low_and_medium_tickets_are_delegated_in_phase_1() {
    allowed(delegate());
    allowed(set(delegate(), "/ticket/risk_class", json!("medium")));
    allowed(unset(delegate(), "/ticket/paths"));
}

#[test]
fn a_high_unknown_or_missing_risk_class_is_refused() {
    denied(
        set(delegate(), "/ticket/risk_class", json!("high")),
        "risk class high is not delegated in this phase",
    );
    denied(
        set(delegate(), "/ticket/risk_class", json!("urgent")),
        "unknown risk class: urgent",
    );
    denied(
        set(delegate(), "/ticket/risk_class", json!("")),
        "unknown risk class",
    );
    denied(
        unset(delegate(), "/ticket/risk_class"),
        "ticket has no risk class",
    );
}

#[test]
fn a_path_rule_raises_the_class() {
    let auth = set(
        delegate(),
        "/ticket/paths",
        json!(["docs/readme.md", "auth/login.rs"]),
    );
    denied(
        auth.clone(),
        "risk class high is not delegated in this phase",
    );
    denied(auth, "path auth/login.rs raises the risk class to high");
    denied(
        set(
            delegate(),
            "/ticket/paths",
            json!([".github/workflows/ci.yml"]),
        ),
        "risk class high is not delegated in this phase",
    );
    denied(
        set(delegate(), "/ticket/paths", json!(["src/auth/login.rs"])),
        "risk class high is not delegated in this phase",
    );
    allowed(set(
        delegate(),
        "/ticket/paths",
        json!(["src/authors.rs", "docs/oauth.md"]),
    ));
}

#[test]
fn every_placeholder_path_rule_raises_to_high() {
    for path in [
        "auth/x",
        "payments/x",
        "crypto/x",
        "pii/x",
        "migrations/x",
        "infra/x",
        ".github/x",
        ".kod/x",
        "policy/x",
        "CODEOWNERS",
        "docs/CODEOWNERS",
    ] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "risk class high is not delegated in this phase",
        );
    }
}

/// policy/delegation.json with `repo_path_rules` replaced.
fn with_repo_rules(rules: Value) -> kod_policy::Policy {
    let text = std::fs::read_to_string(common::policy_dir().join("delegation.json")).unwrap();
    let mut config: Value = serde_json::from_str(&text).unwrap();
    config["repo_path_rules"] = rules;
    policy_with("delegation.json", config)
}

#[test]
fn a_repos_own_path_rules_add_to_the_defaults() {
    let p = with_repo_rules(json!({"acme/app": [
        {"prefix": "billing/", "class": "high"},
        {"prefix": "auth/", "class": "low"}
    ]}));
    let billing = set(delegate(), "/ticket/paths", json!(["billing/invoice.rs"]));
    denied_by(
        &p,
        billing.clone(),
        "path billing/invoice.rs raises the risk class to high",
    );
    denied_by(
        &p,
        set(billing.clone(), "/ticket/repo", json!("Acme/App")),
        "path billing/invoice.rs raises the risk class to high",
    );
    denied_by(
        &p,
        set(billing.clone(), "/ticket/repo", json!(" acme/app ")),
        "path billing/invoice.rs raises the risk class to high",
    );
    allowed_by(&p, set(billing, "/ticket/repo", json!("acme/other")));
    denied_by(
        &p,
        set(delegate(), "/ticket/paths", json!(["auth/x"])),
        "risk class high is not delegated in this phase",
    );
    denied_by(
        &p,
        publish(json!(["billing/invoice.rs"])),
        "publish: path billing/invoice.rs raises the risk class to high",
    );
    denied_by(
        &p,
        set(
            set(
                publish(json!(["billing/invoice.rs"])),
                "/run/repo",
                json!("Acme/App"),
            ),
            "/target/repo",
            json!("Acme/App"),
        ),
        "publish: path billing/invoice.rs raises the risk class to high",
    );
}

/// The error `Policy::load` gives for policy/delegation.json with `key`
/// replaced.
#[test]
fn path_tricks_cannot_dodge_a_path_rule() {
    for path in [
        "./auth/login.rs",
        "Auth/login.rs",
        "AUTH/login.rs",
        "/auth/login.rs",
        "auth//login.rs",
        "./././auth/x",
        "auth\\login.rs",
        " auth/x",
        "src/./auth/x",
        "auth",
        "auth/",
        "src/auth",
    ] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "risk class high is not delegated in this phase",
        );
    }
    for path in ["src/../auth/login.rs", "src/auth/..", "..", "", " ", "./"] {
        denied(
            set(delegate(), "/ticket/paths", json!([path])),
            "is not a clean repo-relative path",
        );
    }
    // a longer name that merely starts with a rule's text is not that directory
    allowed(set(
        delegate(),
        "/ticket/paths",
        json!(["authx/y", "src/authx"]),
    ));
    denied(
        set(delegate(), "/ticket/paths", json!([5])),
        "is not a clean repo-relative path",
    );
    denied(
        set(delegate(), "/ticket/paths", json!("auth/login.rs")),
        "ticket.paths must be a list",
    );
}

#[test]
fn admission_needs_criteria_capacity_and_bounces_left() {
    denied(
        set(delegate(), "/ticket/acceptance_criteria", json!(false)),
        "ticket has no acceptance criteria",
    );
    denied(
        unset(delegate(), "/ticket/acceptance_criteria"),
        "ticket has no acceptance criteria",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!(1)),
        "WIP limit reached",
    );
    denied(
        unset(delegate(), "/wip_in_progress"),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!("0")),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/ticket/bounces", json!(2)),
        "bounce limit reached: assign the story to the tech lead",
    );
    denied(
        unset(delegate(), "/ticket/bounces"),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/wip_in_progress", json!(-1)),
        "does not meet the rule",
    );
    denied(
        set(delegate(), "/ticket/bounces", json!(-3)),
        "does not meet the rule",
    );
}

#[test]
fn a_ticket_needs_a_repo() {
    denied(unset(delegate(), "/ticket/repo"), "ticket has no repo");
    for repo in [Value::Null, json!(""), json!("  ")] {
        denied(set(delegate(), "/ticket/repo", repo), "ticket has no repo");
    }
}

#[test]
fn a_malformed_ticket_is_a_policy_error_deny() {
    let d = decide(json!({"action": "delegate", "ticket": "KOD-1", "wip_in_progress": 0}));
    assert!(!d.allow, "{d:?}");
    assert!(
        d.reasons
            .iter()
            .any(|r| r.starts_with("policy error") || r.contains("ticket has no risk class")),
        "{d:?}"
    );
}

// retention_delete

#[test]
fn retention_delete_is_refused_while_the_profile_is_a_placeholder() {
    denied(
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 100000}),
        "compliance profile placeholder has no retention_days.run_audit",
    );
}

#[test]
fn the_longest_retention_across_active_profiles_wins() {
    let p = policy_with(
        "compliance.json",
        json!({"profiles": {
            "a": {"retention_days": {"run_audit": 365}},
            "b": {"retention_days": {"run_audit": 2555}}
        }}),
    );
    allowed_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 2555}),
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 400}),
        "does not meet the rule",
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": "9999"}),
        "does not meet the rule",
    );
}

#[test]
fn a_missing_value_in_any_active_profile_denies() {
    let p = policy_with(
        "compliance.json",
        json!({"profiles": {
            "a": {"retention_days": {"run_audit": 365, "prompts": 30}},
            "b": {"retention_days": {"run_audit": 2555}}
        }}),
    );
    denied_by(
        &p,
        json!({"action": "retention_delete", "record_type": "prompts", "age_days": 100000}),
        "compliance profile b has no retention_days.prompts",
    );
    let none = policy_with("compliance.json", json!({"profiles": {}}));
    denied_by(
        &none,
        json!({"action": "retention_delete", "record_type": "run_audit", "age_days": 100000}),
        "no compliance profile is active",
    );
}

// coverage

/// Every row of the spec's action and permission matrix, and every other
/// policy action, with the tests that pin it. Adding an action to
/// `known_actions` in policy/kod.rego means adding it here, with its test.
const ACTIONS: &[(&str, &str)] = &[
    ("local_command", "api::external_rows_are_never_granted"),
    (
        "model_request",
        "agent_ops::model_request_*, a_disallowed_endpoint_is_refused",
    ),
    (
        "subscription_model_request",
        "agent_ops::subscription_*, a_null_or_wrong_workspace_never_matches",
    ),
    (
        "read",
        "agent_ops::reads_are_limited_to_the_ticket_and_its_linked_docs",
    ),
    (
        "tracker_write",
        "agent_ops::tracker_writes_are_limited_to_the_ticket_and_its_linked_requests",
    ),
    (
        "draft_doc_write",
        "agent_ops::draft_doc_writes_are_limited_to_the_runs_own_draft",
    ),
    ("publish", "agent_ops::publish_*"),
    (
        "merge",
        "human_ops::not_sole_*, allowed_*, forbidden_*, agent_approvals_block_the_merge",
    ),
    (
        "harness_change",
        "human_ops::a_harness_change_needs_the_owner_and_the_backup_once_named",
    ),
    (
        "cancel",
        "human_ops::who_may_cancel_a_run_or_pull_the_kill_switch",
    ),
    (
        "resume",
        "human_ops::only_the_owner_or_backup_restarts_after_the_kill_switch",
    ),
    ("sign_release", "api::external_rows_are_never_granted"),
    ("delegate", "delegation::*"),
    (
        "retention_delete",
        "delegation::retention_*, a_missing_value_in_any_active_profile_denies",
    ),
];

#[test]
fn every_action_is_known_and_denies_an_empty_request() {
    let listed: BTreeSet<String> = ACTIONS.iter().map(|(a, _)| a.to_string()).collect();
    assert_eq!(
        listed,
        known_actions(),
        "ACTIONS must match known_actions in policy/kod.rego"
    );
    for (action, _) in ACTIONS {
        let d = decide(json!({"action": action}));
        assert!(!d.allow, "{action}: {d:?}");
        assert!(
            !d.reasons.iter().any(|r| r == "unknown action"),
            "{action}: {d:?}"
        );
    }
}

// publish: the diff's own paths, whatever the ticket said

fn publish(changed: Value) -> Value {
    json!({
        "action": "publish", "actor": {"kind": "agent"},
        "run": {"id": "r1", "ticket": "KOD-1", "repo": "acme/app", "canceled": false, "data_classes": ["placeholder"]},
        "target": {"repo": "acme/app", "branch": "agent/KOD-1-r1", "draft": true},
        "changed_paths": changed
    })
}

#[test]
fn publish_refuses_a_diff_that_touches_a_high_risk_path() {
    allowed(publish(json!(["src/lib.rs"])));
    for path in [
        "auth/login.rs",
        "./AUTH/login.rs",
        "src/auth/login.rs",
        ".github/workflows/ci.yml",
        "auth",
    ] {
        denied(
            publish(json!(["src/lib.rs", path])),
            "raises the risk class to high",
        );
    }
    denied(
        publish(json!(["src/../auth/x.rs"])),
        "is not a clean repo-relative path",
    );
    denied(
        publish(json!("auth/x.rs")),
        "publish needs changed_paths as a list",
    );
    let mut missing = publish(json!([]));
    missing.as_object_mut().unwrap().remove("changed_paths");
    denied(missing, "publish needs changed_paths as a list");
}
