mod common;

use common::{allowed, allowed_by, denied, denied_by, policy_with, set, unset};
use kod_policy::Policy;
use serde_json::{Value, json};

fn human(login: &str) -> Value {
    json!({"login": login, "agent": false})
}

fn merge(approvals: Vec<Value>) -> Value {
    json!({"action": "merge", "team": "core", "requester": "alice", "approvals": approvals})
}

fn team_setting(setting: &str) -> Policy {
    policy_with(
        "teams.json",
        json!({"core": {"requester_approval": setting}}),
    )
}

// merge

#[test]
fn not_sole_is_the_default_and_needs_another_human() {
    allowed(merge(vec![human("bob")]));
    allowed(merge(vec![human("alice"), human("bob")]));
    denied(
        merge(vec![human("alice")]),
        "the requester can't be the only approver",
    );
    denied(merge(vec![]), "no human approval");
    denied(
        json!({"action": "merge", "team": "core", "requester": "alice"}),
        "no human approval",
    );
}

#[test]
fn the_team_lookup_ignores_case_and_spaces() {
    let p = team_setting("forbidden");
    let alice_approved = merge(vec![human("alice"), human("bob")]);
    for team in ["core", "Core", " CORE "] {
        denied_by(
            &p,
            set(alice_approved.clone(), "/team", json!(team)),
            "requester_approval is forbidden: the requester approved",
        );
    }
}

#[test]
fn allowed_lets_the_requester_approve_alone() {
    let p = team_setting("allowed");
    allowed_by(&p, merge(vec![human("alice")]));
    denied_by(&p, merge(vec![]), "no human approval");
}

#[test]
fn forbidden_refuses_any_requester_approval() {
    let p = team_setting("forbidden");
    allowed_by(&p, merge(vec![human("bob")]));
    denied_by(
        &p,
        merge(vec![human("alice"), human("bob")]),
        "the requester approved",
    );
    denied_by(&p, merge(vec![human("alice")]), "the requester approved");
}

#[test]
fn agent_approvals_block_the_merge() {
    denied(
        merge(vec![
            human("bob"),
            json!({"login": "kod-agent", "agent": true}),
        ]),
        "agents never approve",
    );
    denied(
        merge(vec![human("bob"), json!({"login": "carol"})]),
        "agents never approve",
    );
    denied(
        merge(vec![json!({"login": "kod-agent", "agent": true})]),
        "no human approval",
    );
}

#[test]
fn a_merge_needs_a_named_requester() {
    let mut no_requester = merge(vec![human("bob")]);
    no_requester.as_object_mut().unwrap().remove("requester");
    denied(no_requester, "does not meet the rule");
    let null_requester =
        json!({"action": "merge", "team": "core", "requester": null, "approvals": [human("bob")]});
    denied(null_requester, "does not meet the rule");
    for blank in ["", "  "] {
        let p = team_setting("forbidden");
        denied_by(
            &p,
            set(merge(vec![human("bob")]), "/requester", json!(blank)),
            "does not meet the rule",
        );
    }
}

// harness_change

fn harness(approvals: Vec<Value>) -> Value {
    json!({"action": "harness_change", "approvals": approvals})
}

#[test]
fn a_harness_change_needs_the_owner_and_the_backup_once_named() {
    allowed(harness(vec![human("kiennt23")]));
    denied(
        harness(vec![human("bob")]),
        "harness change needs approval from kiennt23",
    );
    denied(
        harness(vec![json!({"login": "kiennt23", "agent": true})]),
        "agents never approve",
    );

    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": []}),
    );
    denied_by(
        &p,
        harness(vec![human("kiennt23")]),
        "harness change needs approval from bk",
    );
    allowed_by(&p, harness(vec![human("kiennt23"), human("bk")]));
}

// cancel

fn cancel(scope: &str, actor: Value) -> Value {
    json!({"action": "cancel", "scope": scope, "actor": actor, "run": {"id": "r1"}, "target": {"run": "r1"}})
}

#[test]
fn who_may_cancel_a_run_or_pull_the_kill_switch() {
    let owner = json!({"kind": "human", "login": "kiennt23"});
    let tech_lead = json!({"kind": "human", "login": "tl"});
    let stranger = json!({"kind": "human", "login": "mallory"});
    let label = json!({"kind": "label_removal"});

    allowed(cancel("run", owner.clone()));
    allowed(cancel("all", owner));
    allowed(cancel("run", label.clone()));
    denied(cancel("all", label.clone()), "does not meet the rule");
    denied(
        set(cancel("run", label.clone()), "/target/run", json!("r2")),
        "does not meet the rule",
    );
    denied(
        unset(cancel("run", label.clone()), "/target"),
        "does not meet the rule",
    );
    denied(
        set(
            set(cancel("run", label.clone()), "/run/id", Value::Null),
            "/target/run",
            Value::Null,
        ),
        "does not meet the rule",
    );
    for blank in ["", "  "] {
        denied(
            set(
                set(cancel("run", label.clone()), "/run/id", json!(blank)),
                "/target/run",
                json!(blank),
            ),
            "does not meet the rule",
        );
    }
    denied(cancel("run", stranger.clone()), "does not meet the rule");
    denied(cancel("all", stranger), "does not meet the rule");
    denied(
        cancel("run", json!({"kind": "agent"})),
        "does not meet the rule",
    );
    denied(
        cancel("everything", json!({"kind": "human", "login": "kiennt23"})),
        "does not meet the rule",
    );

    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": ["tl"]}),
    );
    allowed_by(&p, cancel("run", tech_lead.clone()));
    allowed_by(&p, cancel("all", tech_lead));
    allowed_by(&p, cancel("all", json!({"kind": "human", "login": "bk"})));
}

fn resume(actor: Value) -> Value {
    json!({"action": "resume", "scope": "all", "actor": actor})
}

#[test]
fn only_the_owner_or_backup_restarts_after_the_kill_switch() {
    let p = policy_with(
        "people.json",
        json!({"owner": "kiennt23", "backup": "bk", "tech_leads": ["tl"]}),
    );
    allowed_by(&p, resume(json!({"kind": "human", "login": "KienNT23"})));
    allowed_by(&p, resume(json!({"kind": "human", "login": "bk"})));
    denied_by(
        &p,
        resume(json!({"kind": "human", "login": "tl"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        resume(json!({"kind": "label_removal"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        resume(json!({"kind": "agent"})),
        "does not meet the rule",
    );
    denied_by(
        &p,
        set(
            resume(json!({"kind": "human", "login": "bk"})),
            "/scope",
            json!("run"),
        ),
        "does not meet the rule",
    );
}

#[test]
fn logins_compare_case_insensitively() {
    let alice_alone = json!({"action": "merge", "team": "core", "requester": "Alice", "approvals": [human("alice")]});
    denied(
        alice_alone.clone(),
        "the requester can't be the only approver",
    );
    let alice_and_bob = json!({"action": "merge", "team": "core", "requester": "ALICE", "approvals": [human("alice"), human("Bob")]});
    allowed(alice_and_bob.clone());
    denied_by(
        &team_setting("forbidden"),
        alice_and_bob,
        "the requester approved",
    );
    allowed_by(&team_setting("allowed"), alice_alone);
    allowed(harness(vec![human("KienNT23")]));
    allowed(cancel("all", json!({"kind": "human", "login": "KIENNT23"})));
}

#[test]
fn whitespace_cannot_turn_self_approval_into_another_person() {
    for rule in ["not_sole", "forbidden"] {
        let p = team_setting(rule);
        for (requester, approver) in [("alice", "alice "), (" alice", "alice")] {
            let input = json!({"action": "merge", "team": "core", "requester": requester,
                               "approvals": [human(approver)]});
            assert!(!p.decide(&input).allow, "{rule}: {input}");
        }
    }
}

#[test]
fn malformed_requester_logins_are_refused_for_every_team_setting() {
    for rule in ["allowed", "not_sole", "forbidden"] {
        let p = team_setting(rule);
        for login in [
            "alice ",
            " alice",
            "ali ce",
            "alice\t",
            "alice\n",
            "alice\u{a0}",
        ] {
            let input = set(merge(vec![human("bob")]), "/requester", json!(login));
            assert!(!p.decide(&input).allow, "{rule}: {input}");
        }
    }
}

#[test]
fn malformed_approvals_are_refused_even_with_another_valid_human() {
    for login in [
        "alice ",
        " alice",
        "ali ce",
        "alice\t",
        "alice\n",
        "alice\u{a0}",
    ] {
        denied(
            merge(vec![human("bob"), human(login)]),
            "agents never approve",
        );
        denied(
            harness(vec![human("kiennt23"), human(login)]),
            "agents never approve",
        );
    }
}

#[test]
fn malformed_actor_logins_cannot_cancel_or_resume() {
    for login in ["kiennt23 ", " kiennt23", "kiennt23\t", "kiennt23\u{a0}"] {
        let actor = json!({"kind": "human", "login": login});
        denied(cancel("all", actor.clone()), "does not meet the rule");
        denied(resume(actor), "does not meet the rule");
    }
}

#[test]
fn approvals_must_be_a_list() {
    let obj = json!({"x": {"login": "bob", "agent": false}});
    denied(
        set(merge(vec![]), "/approvals", obj.clone()),
        "approvals must be a list",
    );
    denied(
        json!({"action": "harness_change", "approvals": {"x": {"login": "kiennt23", "agent": false}}}),
        "approvals must be a list",
    );
}

#[test]
fn a_merge_needs_a_named_team() {
    let p = team_setting("forbidden");
    for team in [json!(""), json!("  "), Value::Null, json!(7)] {
        denied_by(
            &p,
            set(merge(vec![human("alice"), human("bob")]), "/team", team),
            "merge needs a named team",
        );
    }
}
