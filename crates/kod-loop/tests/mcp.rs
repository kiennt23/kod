mod common;

use common::*;
use kod_audit::AuditLog;
use kod_loop::{EndState, mcp::Mcp};
use kod_policy::Policy;
use kod_tool_gateway::{
    Adapters, Gateway, Run,
    docs::FakeDocs,
    forge::{FakeGitHub, FakeWorkspace, Identity},
};
use kod_tracker::fake::{self, FakeIssue};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PLANTED: &str = "PLANTED-SECRET-7f3a";

/// A tool gateway over the fakes serving run-1 for FAKE-1; FAKE-9 holds a
/// planted secret. Returns the MCP URL and the audit log's path.
fn gateway(dir: &std::path::Path) -> (String, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(Policy::load(&root.join("policy")).unwrap());
    let audit = dir.join("audit.db");
    let log = Arc::new(AuditLog::open(&audit).unwrap());
    let (tracker, _) = fake::sandbox();
    let planted = FakeIssue {
        body: PLANTED.into(),
        workflow_state: "Backlog".into(),
        ..FakeIssue::default()
    };
    tracker.add_issue("FAKE-9", planted);
    let github = Arc::new(FakeGitHub::default());
    let adapters = Adapters {
        tracker: Arc::new(tracker),
        docs: Arc::new(FakeDocs::default()),
        stopped: Arc::new(|_: &str| Ok(false)),
        broker: github.clone(),
        forge: github,
        workspace: Arc::new(FakeWorkspace::default()),
    };
    let agent = Identity {
        name: "kod-agent".into(),
        email: "kod-agent@users.noreply.example".into(),
    };
    let gw = Arc::new(Gateway::new(policy, log, adapters, agent).unwrap());
    let run = Run {
        id: "run-1".into(),
        ticket: "FAKE-1".into(),
        repo: "acme/app".into(),
        base: "b".repeat(40),
        data_classes: vec!["placeholder".into()],
        linked_tickets: vec![],
        started_at: 1_700_000_000,
    };
    gw.start_run(TOKEN, run).unwrap();
    let base = serve(kod_tool_gateway::router(gw, vec!["127.0.0.1".into()]));
    (format!("{base}/r/{TOKEN}/mcp"), audit)
}

#[test]
fn the_agents_tracker_calls_go_through_the_gateway() {
    let ws = tempfile::tempdir().unwrap();
    let (mcp_url, audit) = gateway(ws.path());
    let mcp = Mcp::connect(&mcp_url).unwrap();
    let (url, m) = model(vec![
        msg(
            json!([tool("t1", "get_issue", json!({"key": "FAKE-1"}))]),
            "tool_use",
        ),
        msg(
            json!([tool("t2", "get_issue", json!({"key": "FAKE-9"}))]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t3",
                "finish",
                json!({"state": "done", "summary": "read it"})
            )]),
            "tool_use",
        ),
    ]);
    let mut trace = vec![];
    let o = kod_loop::run_with(
        &config(&url, ws.path()),
        Some(&mcp),
        "the ticket",
        &mut trace,
    );
    assert_eq!((o.end_state, o.error), (EndState::Done, None));
    let sent = bodies(&m);
    let offered: Vec<&str> = sent[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(offered.contains(&"get_issue") && offered.contains(&"run"));
    assert!(!offered.contains(&"publish"), "publish is never offered");
    let result =
        |i: usize| sent[i]["messages"].as_array().unwrap().last().unwrap()["content"][0].clone();
    assert_eq!(result(1)["is_error"], false);
    assert!(result(1)["content"].as_str().unwrap().contains("REQ-7"));
    assert_eq!(result(2)["is_error"], true);
    assert!(!result(2).to_string().contains(PLANTED));
    let db = rusqlite::Connection::open(&audit).unwrap();
    let refused: String = db
        .query_row(
            "SELECT payload FROM audit WHERE payload LIKE '%\"allow\":false%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(
        refused["input"]["target"],
        json!({"kind": "ticket", "id": "FAKE-9"})
    );
}

#[test]
fn connecting_to_an_unknown_run_fails() {
    let ws = tempfile::tempdir().unwrap();
    let (mcp_url, _) = gateway(ws.path());
    let wrong = mcp_url.replace(TOKEN, &"e".repeat(64));
    let e = Mcp::connect(&wrong).err().unwrap();
    assert!(format!("{e:#}").contains("404"), "{e:#}");
}

#[test]
fn an_unreachable_gateway_does_not_leak_the_run_token() {
    // Nothing listens on port 9.
    let e = Mcp::connect(&format!("http://127.0.0.1:9/r/{TOKEN}/mcp"))
        .err()
        .unwrap();
    let e = format!("{e:#}");
    assert!(e.contains("reaching the tool gateway"), "{e}");
    assert!(!e.contains(TOKEN), "{e}");
}

#[test]
fn an_error_page_that_echoes_the_path_does_not_leak_the_run_token() {
    let app = axum::Router::new().fallback(|uri: axum::http::Uri| async move {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("no handler for {uri}"),
        )
    });
    let base = serve(app);
    let e = Mcp::connect(&format!("{base}/r/{TOKEN}/mcp"))
        .err()
        .unwrap();
    let e = format!("{e:#}");
    assert!(e.contains("HTTP 500"), "{e}");
    assert!(e.contains("/r/[REDACTED]/mcp"), "{e}");
    assert!(!e.contains(TOKEN), "{e}");
}
