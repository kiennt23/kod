mod common;

use common::*;
use kod_tool_gateway::Run;
use serde_json::json;

#[test]
fn reads_the_runs_ticket_its_rework_requests_and_linked_docs_only() {
    let h = harness();
    let own =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-1"}))
            .unwrap();
    assert_eq!(own["requirement_id"], "REQ-7");
    let rework =
        h.gw.call(TOKEN, "get_issue", &json!({"key": REWORK}))
            .unwrap();
    assert_eq!(rework["body"], "a question Kod asked");
    let doc =
        h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}))
            .unwrap();
    assert_eq!(doc["text"], "the approved brief");
    // A human's link is on the live ticket but not in the run's snapshot.
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": HUMAN_LINK}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
}

#[test]
fn a_planted_secret_cannot_be_read_and_the_attempt_is_audited() {
    let h = harness();
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-9"}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    let e =
        h.gw.call(TOKEN, "read_doc", &json!({"id": SECRET_DOC}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    assert!(!e.contains(PLANTED));
    let refused: Vec<_> = h.decisions().into_iter().filter(|(a, _)| !a).collect();
    assert_eq!(refused.len(), 2);
    assert_eq!(
        refused[0].1["target"],
        json!({"kind": "ticket", "id": "FAKE-9"})
    );
    assert_eq!(
        refused[1].1["target"],
        json!({"kind": "doc", "id": SECRET_DOC})
    );
    assert!(!h.raw_log().contains(PLANTED));
}

#[test]
fn writes_reach_only_the_runs_ticket_and_its_rework_requests() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    assert_eq!(h.tracker.comments("FAKE-1"), ["on it"]);
    h.gw.call(
        TOKEN,
        "comment",
        &json!({"key": REWORK, "text": "still open"}),
    )
    .unwrap();
    assert_eq!(h.tracker.comments(REWORK), ["still open"]);
    for key in [HUMAN_LINK, "FAKE-9"] {
        let e =
            h.gw.call(TOKEN, "comment", &json!({"key": key, "text": "hi"}))
                .unwrap_err();
        assert!(e.starts_with("refused by policy"), "{e}");
        assert!(h.tracker.comments(key).is_empty());
    }
}

#[test]
fn the_agent_moves_only_its_own_ticket_between_in_progress_and_blocked() {
    let h = harness();
    let to = |key: &str, state: &str| {
        h.gw.call(TOKEN, "transition", &json!({"key": key, "state": state}))
    };
    to("FAKE-1", "blocked").unwrap();
    assert_eq!(h.tracker.issue("FAKE-1").unwrap().workflow_state, "Waiting");
    for state in ["done", "in_review", "ready"] {
        let e = to("FAKE-1", state).unwrap_err();
        assert_eq!(
            e,
            format!("the agent may not move its ticket from blocked to {state}")
        );
    }
    to("FAKE-1", "in_progress").unwrap();
    assert_eq!(h.tracker.issue("FAKE-1").unwrap().workflow_state, "Doing");
    let e = to(REWORK, "blocked").unwrap_err();
    assert_eq!(e, "the agent may move only its own ticket");
    assert_eq!(
        to("FAKE-1", "shipped").unwrap_err(),
        "unknown state \"shipped\""
    );
    assert_eq!(h.tracker.issue(REWORK).unwrap().workflow_state, "Backlog");
}

#[test]
fn every_call_is_audited_with_its_target_before_it_happens() {
    let h = harness();
    h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "on it"}))
        .unwrap();
    h.gw.call(TOKEN, "get_issue", &json!({"key": "FAKE-1"}))
        .unwrap();
    let kinds: Vec<_> = h.records().iter().map(|r| r["kind"].clone()).collect();
    let call = ["policy_decision", "tool_call", "tool_result"];
    assert_eq!(kinds, [call, call].concat());
    assert_eq!(h.records()[1]["target"], json!({"ticket": "FAKE-1"}));
    assert_eq!(
        h.records()[4]["target"],
        json!({"kind": "ticket", "id": "FAKE-1"})
    );
}

#[test]
fn create_linked_opens_a_request_the_run_may_then_read_and_comment_on() {
    let h = harness();
    let args = json!({"kind": "question", "stage": "build", "artifact": "brief",
        "version": "3", "text": "Which approver?", "evidence": ["src/refund.rs:12"]});
    let r = h.gw.call(TOKEN, "create_linked", &args).unwrap();
    let key = r["key"].as_str().unwrap();
    let read = h.gw.call(TOKEN, "get_issue", &json!({"key": key})).unwrap();
    assert!(read["body"].as_str().unwrap().contains("Which approver?"));
    h.gw.call(TOKEN, "comment", &json!({"key": key, "text": "more"}))
        .unwrap();
    assert_eq!(h.tracker.comments(key), ["more"]);
    let bad = json!({"kind": "rumour", "stage": "", "artifact": "", "version": "",
        "text": "", "evidence": []});
    let e = h.gw.call(TOKEN, "create_linked", &bad).unwrap_err();
    assert_eq!(e, "unknown kind \"rumour\"");
}

#[test]
fn a_run_canceled_in_the_dispatchers_database_gets_nothing() {
    let h = harness();
    h.stop("run-1");
    for (tool, args) in [
        ("get_issue", json!({"key": "FAKE-1"})),
        ("comment", json!({"key": "FAKE-1", "text": "x"})),
    ] {
        let e = h.gw.call(TOKEN, tool, &args).unwrap_err();
        assert_eq!(e, "refused: the run is canceled");
    }
    assert!(h.tracker.comments("FAKE-1").is_empty());
    assert!(h.records().is_empty());
}

#[test]
fn a_run_whose_state_cannot_be_read_gets_nothing() {
    let h = harness();
    h.runs
        .broken
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let e =
        h.gw.call(TOKEN, "comment", &json!({"key": "FAKE-1", "text": "x"}))
            .unwrap_err();
    assert!(
        e.starts_with("refused: the run's state could not be read"),
        "{e}"
    );
    assert!(h.tracker.comments("FAKE-1").is_empty());
}

#[test]
fn an_ended_runs_token_stops_working() {
    let h = harness();
    let other = "2".repeat(64);
    h.gw.start_run(
        &other,
        Run {
            id: "run-2".into(),
            ..run()
        },
    )
    .unwrap();
    h.gw.end_run("run-1").unwrap();
    let args = json!({"key": "FAKE-1"});
    assert_eq!(
        h.gw.call(TOKEN, "get_issue", &args).unwrap_err(),
        "unknown run"
    );
    h.gw.call(&other, "get_issue", &args).unwrap();
    h.gw.end_all().unwrap();
    assert_eq!(
        h.gw.call(&other, "get_issue", &args).unwrap_err(),
        "unknown run"
    );
}

#[test]
fn unknown_tokens_tools_and_bad_arguments_are_refused() {
    let h = harness();
    let other = "f".repeat(64);
    let e =
        h.gw.call(&other, "get_issue", &json!({"key": "FAKE-1"}))
            .unwrap_err();
    assert_eq!(e, "unknown run");
    let e = h.gw.call(TOKEN, "run", &json!({})).unwrap_err();
    assert_eq!(e, "unknown tool \"run\"");
    let e =
        h.gw.call(TOKEN, "get_issue", &json!({"key": 7}))
            .unwrap_err();
    assert_eq!(e, "key must be a string");
    assert!(h.records().is_empty());
}

#[test]
fn errors_are_scrubbed_of_run_tokens_and_credentials() {
    let leak = format!(
        "error sending request for url (http://gw/r/{TOKEN}/mcp): token ghs_0123456789abcdef0123 refused"
    );
    let clean = kod_tool_gateway::scrub(&leak);
    assert!(!clean.contains(TOKEN), "{clean}");
    assert!(!clean.contains("ghs_0123456789abcdef0123"), "{clean}");
    assert!(clean.contains("http://gw/r/[REDACTED]/mcp"), "{clean}");
    assert_eq!(kod_tool_gateway::scrub("/r/abc/mcp"), "/r/abc/mcp");
}

#[test]
fn start_run_validates_the_run() {
    let h = harness();
    let mut r = run();
    r.id = "run-2".into();
    let bad_token = h.gw.start_run("ABC", r.clone()).unwrap_err();
    assert!(bad_token.to_string().contains("lower-case hex"));
    let mut short = r.clone();
    short.base = "abc".into();
    assert!(h.gw.start_run(&"1".repeat(64), short).is_err());
    let mut classless = r.clone();
    classless.data_classes = vec![];
    assert!(h.gw.start_run(&"1".repeat(64), classless).is_err());
    let mut links = r.clone();
    links.linked_tickets = vec!["".into()];
    assert!(h.gw.start_run(&"1".repeat(64), links).is_err());
    let again = h.gw.start_run(&"1".repeat(64), run()).unwrap_err();
    assert_eq!(again.to_string(), "run run-1 is already started");
    h.gw.start_run(&"1".repeat(64), r).unwrap();
}

#[test]
fn a_doc_outside_the_runs_admitted_classes_is_not_returned() {
    let h = harness();
    h.docs
        .add_classed(LINKED_DOC, "restricted contents", "customer_pii");
    let result = h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}));
    assert!(result.is_err(), "{result:?}");
}

#[test]
fn a_run_pinned_to_v3_cannot_read_an_approved_but_unpinned_revision() {
    let h = harness();
    // v4 is a real, approved doc and the issue lists it, but this run was admitted at v3.
    let v4 = "https://docs.example/brief?rev=4";
    h.docs.add(v4, "the newer brief");
    let mut issue = h.tracker.issue("FAKE-1").unwrap();
    issue.approved_versions.push(v4.into());
    h.tracker.add_issue("FAKE-1", issue);
    let e =
        h.gw.call(TOKEN, "read_doc", &json!({"id": v4}))
            .unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    assert!(!e.contains("newer brief"));
}
