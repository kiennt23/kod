mod common;

use common::{allowed, allowed_by, decide, denied, denied_by, policy_with, set, unset};
use kod_policy::Policy;
use serde_json::{Value, json};

fn run() -> Value {
    json!({
        "id": "r1", "ticket": "KOD-1", "repo": "acme/app", "canceled": false,
        "data_classes": ["placeholder"], "auth_mode": "api_key",
        "linked_docs": ["doc-1"], "linked_tickets": ["KOD-2"]
    })
}

fn req(action: &str, extra: Value) -> Value {
    let mut v = json!({"action": action, "actor": {"kind": "agent"}, "run": run()});
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

// model_request

#[test]
fn model_request_to_an_allowed_endpoint_is_allowed() {
    allowed(req("model_request", json!({"endpoint": "placeholder"})));
}

#[test]
fn an_unknown_or_missing_data_class_is_refused() {
    let base = req("model_request", json!({"endpoint": "placeholder"}));
    denied(
        set(base.clone(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
    denied(
        set(
            base.clone(),
            "/run/data_classes",
            json!(["placeholder", "secret"]),
        ),
        "unknown data class: secret",
    );
    denied(
        set(base.clone(), "/run/data_classes", json!([])),
        "the run has no data class",
    );
    denied(
        set(base.clone(), "/run/data_classes", json!("placeholder")),
        "the run has no data class",
    );
    denied(
        unset(base, "/run/data_classes"),
        "the run has no data class",
    );
}

#[test]
fn a_disallowed_endpoint_is_refused() {
    denied(
        req("model_request", json!({"endpoint": "api.example.com"})),
        "endpoint api.example.com is not allowed for data class placeholder",
    );
    denied(req("model_request", json!({})), "does not meet the rule");
}

#[test]
fn model_requests_need_an_agent_and_api_key_or_cloud_auth() {
    let base = req("model_request", json!({"endpoint": "placeholder"}));
    denied(
        set(base.clone(), "/actor/kind", json!("human")),
        "only an agent can request this",
    );
    denied(
        unset(base.clone(), "/actor"),
        "only an agent can request this",
    );
    allowed(set(base.clone(), "/run/auth_mode", json!("cloud")));
    denied(
        set(base.clone(), "/run/auth_mode", json!("subscription")),
        "model_request needs auth_mode api_key or cloud",
    );
    denied(unset(base, "/run/auth_mode"), "does not meet the rule");
}

// subscription_model_request

fn sub() -> Value {
    req(
        "subscription_model_request",
        json!({
            "runner": "codex", "context": "offline_eval",
            "seat": {"account": "kod-bot", "workspace": "ws-1", "plan": "business"}
        }),
    )
}

fn sub_policy() -> Policy {
    policy_with(
        "subscription.json",
        json!({
            "contexts": ["offline_spike", "offline_eval"],
            "classes": ["placeholder"],
            "runners": {
                "claude-code": {"plans": ["team", "enterprise"]},
                "codex": {"plans": ["business", "enterprise"], "workspace": "ws-1"},
                "cursor": {"plans": ["business"]}
            }
        }),
    )
}

#[test]
fn subscription_is_refused_until_a_workspace_is_recorded() {
    denied(sub(), "no approved workspace is recorded for runner codex");
}

#[test]
fn subscription_with_the_recorded_workspace_is_allowed_offline_only() {
    let p = sub_policy();
    allowed_by(&p, sub());
    allowed_by(&p, set(sub(), "/context", json!("offline_spike")));
    denied_by(
        &p,
        set(sub(), "/context", json!("connected")),
        "subscription mode is allowed only in",
    );
    denied_by(
        &p,
        set(sub(), "/seat/plan", json!("plus")),
        "plan plus is not a business plan for runner codex",
    );
    denied_by(
        &p,
        set(sub(), "/runner", json!("kod-loop")),
        "runner kod-loop has no subscription seat",
    );
    denied_by(
        &p,
        set(sub(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
    denied_by(&p, unset(sub(), "/seat/account"), "does not meet the rule");
}

#[test]
fn a_null_or_wrong_workspace_never_matches() {
    let p = sub_policy();
    denied_by(
        &p,
        set(sub(), "/seat/workspace", json!("ws-2")),
        "the seat belongs to a different workspace",
    );
    denied_by(
        &p,
        set(sub(), "/seat/workspace", Value::Null),
        "the seat belongs to a different workspace",
    );
    // The repo policy records no workspace; a null from the caller must not match the gap.
    denied(
        set(sub(), "/seat/workspace", Value::Null),
        "no approved workspace is recorded",
    );
}

// read

#[test]
fn reads_are_limited_to_the_ticket_and_its_linked_docs() {
    allowed(req(
        "read",
        json!({"target": {"kind": "ticket", "id": "KOD-1"}}),
    ));
    allowed(req(
        "read",
        json!({"target": {"kind": "doc", "id": "doc-1"}}),
    ));
    denied(
        req("read", json!({"target": {"kind": "ticket", "id": "KOD-9"}})),
        "does not meet the rule",
    );
    denied(
        req("read", json!({"target": {"kind": "doc", "id": "doc-9"}})),
        "does not meet the rule",
    );
    denied(
        req("read", json!({"target": {"kind": "repo", "id": "KOD-1"}})),
        "does not meet the rule",
    );
}

#[test]
fn null_ids_never_match() {
    let read = set(
        req("read", json!({"target": {"kind": "ticket", "id": null}})),
        "/run/ticket",
        Value::Null,
    );
    denied(read, "does not meet the rule");

    // Everything else about this publish is valid: only the repo guard can refuse it.
    let null_repo = set(
        set(publish(), "/run/repo", Value::Null),
        "/target/repo",
        Value::Null,
    );
    let d = decide(null_repo);
    assert!(!d.allow, "{d:?}");

    // A blank ticket or run id is no id, even when the input repeats it.
    let blank_read = set(
        req("read", json!({"target": {"kind": "ticket", "id": ""}})),
        "/run/ticket",
        json!(""),
    );
    denied(blank_read, "does not meet the rule");
    for (ticket, id) in [("", "r1"), ("KOD-1", ""), ("", "")] {
        let blank_ids = set(
            set(
                set(publish(), "/run/ticket", json!(ticket)),
                "/run/id",
                json!(id),
            ),
            "/target/branch",
            json!(format!("agent/{ticket}-{id}")),
        );
        let d = decide(blank_ids);
        assert!(!d.allow, "{d:?}");
    }

    // A blank repo is no repo, even when the target repeats it.
    for blank in ["", "  "] {
        let blank_repo = set(
            set(publish(), "/run/repo", json!(blank)),
            "/target/repo",
            json!(blank),
        );
        let d = decide(blank_repo);
        assert!(!d.allow, "{d:?}");
    }
}

// tracker_write

#[test]
fn tracker_writes_are_limited_to_the_ticket_and_its_linked_requests() {
    allowed(req(
        "tracker_write",
        json!({"kind": "comment", "target": {"ticket": "KOD-1"}}),
    ));
    allowed(req(
        "tracker_write",
        json!({"kind": "transition", "target": {"ticket": "KOD-2"}}),
    ));
    allowed(req(
        "tracker_write",
        json!({"kind": "create_linked", "target": {"parent": "KOD-1"}}),
    ));
    denied(
        req(
            "tracker_write",
            json!({"kind": "comment", "target": {"ticket": "KOD-9"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "tracker_write",
            json!({"kind": "create_linked", "target": {"parent": "KOD-9"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "tracker_write",
            json!({"kind": "delete", "target": {"ticket": "KOD-1"}}),
        ),
        "unknown tracker write: delete",
    );
}

// draft_doc_write

#[test]
fn draft_doc_writes_are_limited_to_the_runs_own_draft() {
    allowed(req(
        "draft_doc_write",
        json!({"target": {"folder": "draft", "owner_run": "r1"}}),
    ));
    denied(
        req(
            "draft_doc_write",
            json!({"target": {"folder": "approved", "owner_run": "r1"}}),
        ),
        "does not meet the rule",
    );
    denied(
        req(
            "draft_doc_write",
            json!({"target": {"folder": "draft", "owner_run": "r2"}}),
        ),
        "does not meet the rule",
    );
}

// publish

fn publish() -> Value {
    req(
        "publish",
        json!({
            "target": {"repo": "acme/app", "branch": "agent/KOD-1-r1", "draft": true},
            "changed_paths": ["src/lib.rs", "docs/readme.md"]
        }),
    )
}

#[test]
fn publish_to_the_runs_own_branch_as_a_draft_is_allowed() {
    allowed(publish());
}

#[test]
fn publish_anywhere_else_is_refused() {
    denied(
        set(publish(), "/target/repo", json!("acme/other")),
        "publish target must be the run's repo",
    );
    denied(
        set(publish(), "/target/branch", json!("main")),
        "publish branch must be agent/KOD-1-r1",
    );
    denied(
        set(publish(), "/target/branch", json!("agent/KOD-1-r2")),
        "publish branch must be agent/KOD-1-r1",
    );
    denied(
        set(publish(), "/target/draft", json!(false)),
        "publish must open a draft PR",
    );
    denied(unset(publish(), "/target/draft"), "does not meet the rule");
    denied(
        set(publish(), "/run/data_classes", json!(["secret"])),
        "unknown data class: secret",
    );
}

#[test]
fn publish_needs_an_explicit_live_run() {
    denied(
        set(publish(), "/run/canceled", json!(true)),
        "the run is canceled",
    );
    denied(unset(publish(), "/run/canceled"), "does not meet the rule");
    denied(
        set(publish(), "/run/canceled", json!("false")),
        "does not meet the rule",
    );
}

#[test]
fn every_agent_write_is_refused_for_a_canceled_run() {
    for write in [
        publish(),
        req(
            "tracker_write",
            json!({"kind": "comment", "target": {"ticket": "KOD-1"}}),
        ),
        req(
            "draft_doc_write",
            json!({"target": {"folder": "draft", "owner_run": "r1"}}),
        ),
    ] {
        allowed(write.clone());
        denied(
            set(write, "/run/canceled", json!(true)),
            "the run is canceled",
        );
    }
}

#[test]
fn linked_ids_must_be_lists() {
    let mut r = run();
    r["linked_docs"] = json!({"x": "doc-1"});
    r["linked_tickets"] = json!({"x": "KOD-2"});
    let mut read = req("read", json!({"target": {"kind": "doc", "id": "doc-1"}}));
    read["run"] = r.clone();
    denied(read, "does not meet the rule");
    let mut write = req(
        "tracker_write",
        json!({"kind": "comment", "target": {"ticket": "KOD-2"}}),
    );
    write["run"] = r;
    denied(write, "does not meet the rule");
}
