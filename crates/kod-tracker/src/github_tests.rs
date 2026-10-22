use super::*;
use crate::contract::{Sandbox, check};
use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State as AppState},
    http::HeaderMap as AxumHeaders,
    routing::any,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

const REPO: &str = "acme/app";
const MAPPING: &str = include_str!("../../../registry/trackers/github.json");

fn issue(number: u64) -> Value {
    json!({"id": 1000 + number, "number": number, "repository_url": "https://api.github.com/repos/acme/app", "title": "Export invoices", "body": format!("A CSV export\n\n{META_START}{}{}", json!({"requirement_id": "REQ-7", "risk_class": "low", "acceptance_criteria": ["Downloads CSV"], "approved_versions": ["https://docs.example/design?rev=3"], "links": []}), META_END), "state": "open", "user": {"id": 1, "login": "po", "type": "User"}, "labels": [{"name": "product"}]})
}

#[derive(Default)]
struct Store {
    issues: BTreeMap<u64, Value>,
    children: BTreeMap<u64, Vec<u64>>,
    comments: BTreeMap<u64, Vec<Value>>,
    calls: Vec<(Method, String, Value)>,
    lose_create: bool,
    hidden_comments: bool,
    fail_link: bool,
}

type Shared = Arc<Mutex<Store>>;

async fn api(
    AppState(state): AppState<Shared>,
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    method: Method,
    headers: AxumHeaders,
    bytes: Bytes,
) -> (StatusCode, axum::Json<Value>) {
    assert_eq!(headers["authorization"], "Bearer test-token");
    assert_eq!(headers["x-github-api-version"], "2026-03-10");
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let mut s = state.lock().unwrap();
    s.calls.push((method.clone(), path.clone(), body.clone()));
    let ok = |v| (StatusCode::OK, axum::Json(v));
    let missing = || {
        (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"message": "not found"})),
        )
    };
    let parts: Vec<_> = path.split('/').collect();
    if parts.first() == Some(&"collaborators") {
        return match parts.get(1).copied() {
            Some("dev") => ok(
                json!({"permission": "write", "user": {"id": 2, "login": "dev", "type": "User"}}),
            ),
            Some("reader") => ok(
                json!({"permission": "read", "user": {"id": 3, "login": "reader", "type": "User"}}),
            ),
            Some("outage") => (StatusCode::FORBIDDEN, axum::Json(Value::Null)),
            _ => missing(),
        };
    }
    if path == "issues" && method == Method::POST {
        let n = s.issues.keys().last().copied().unwrap_or(0) + 1;
        let mut v = issue(n);
        v["title"] = body["title"].clone();
        v["body"] = body["body"].clone();
        s.issues.insert(n, v.clone());
        return (StatusCode::CREATED, axum::Json(v));
    }
    if parts.get(1) == Some(&"comments") && method == Method::PATCH {
        let id = parts.get(2).unwrap().parse::<u64>().unwrap();
        for comments in s.comments.values_mut() {
            if let Some(c) = comments.iter_mut().find(|c| c["id"] == id) {
                c["body"] = body["body"].clone();
                return ok(c.clone());
            }
        }
        return missing();
    }
    let Some(n) = parts.get(1).and_then(|n| n.parse::<u64>().ok()) else {
        return missing();
    };
    if !s.issues.contains_key(&n) {
        return missing();
    }
    if parts.len() == 2 {
        if method == Method::PATCH {
            s.issues.get_mut(&n).unwrap()["state"] = body["state"].clone();
        }
        return ok(s.issues[&n].clone());
    }
    if parts[2] == "sub_issues" {
        if method == Method::POST {
            if s.fail_link {
                return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(Value::Null));
            }
            let id = body["sub_issue_id"].as_u64().unwrap();
            let child = s.issues.values().find(|v| v["id"] == id).unwrap()["number"]
                .as_u64()
                .unwrap();
            s.children.entry(n).or_default().push(child);
            return (StatusCode::CREATED, axum::Json(s.issues[&child].clone()));
        }
        let children: Vec<_> = s
            .children
            .get(&n)
            .into_iter()
            .flatten()
            .map(|n| s.issues[n].clone())
            .collect();
        return ok(page(children, &query));
    }
    if parts[2] == "labels" {
        let labels = s.issues.get_mut(&n).unwrap()["labels"]
            .as_array_mut()
            .unwrap();
        if method == Method::POST {
            for name in body["labels"].as_array().unwrap() {
                if !labels.iter().any(|v| v["name"] == *name) {
                    labels.push(json!({"name": name}));
                }
            }
        }
        if method == Method::DELETE {
            labels.retain(|v| v["name"] != parts[3]);
        }
        return ok(Value::Array(labels.clone()));
    }
    if parts[2] == "comments" {
        if method == Method::POST {
            let id = u64::try_from(s.comments.values().map(Vec::len).sum::<usize>()).unwrap() + 1;
            let comment = json!({"id": id, "body": body["body"], "issue_url": format!("https://api.github.com/repos/{REPO}/issues/{n}"), "user": {"id": 99, "login": "kod[bot]", "type": "Bot"}});
            s.comments.entry(n).or_default().push(comment.clone());
            if s.lose_create {
                s.lose_create = false;
                return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(Value::Null));
            }
            return (StatusCode::CREATED, axum::Json(comment));
        }
        return ok(page(
            if s.hidden_comments {
                vec![]
            } else {
                s.comments.get(&n).cloned().unwrap_or_default()
            },
            &query,
        ));
    }
    missing()
}

fn page(values: Vec<Value>, query: &HashMap<String, String>) -> Value {
    let page = query.get("page").unwrap().parse::<usize>().unwrap();
    Value::Array(
        values
            .into_iter()
            .skip((page - 1) * 100)
            .take(100)
            .collect(),
    )
}

struct Harness {
    github: GitHub,
    store: Shared,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn new() -> Self {
        let store = Arc::new(Mutex::new(Store::default()));
        store.lock().unwrap().issues.insert(1, issue(1));
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let state = store.clone();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    port_tx.send(listener.local_addr().unwrap().port()).unwrap();
                    let app = Router::new()
                        .route("/repos/acme/app/{*path}", any(api))
                        .with_state(state);
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            stopped.await.unwrap();
                        })
                        .await
                        .unwrap();
                });
        });
        let api = format!("http://127.0.0.1:{}", port_rx.recv().unwrap());
        let github = GitHub::with_api(
            Config {
                repo: REPO.into(),
                comment_author_id: 99,
            },
            Mapping::parse("fixture", MAPPING).unwrap(),
            "test-token",
            "webhook-secret",
            &api,
        )
        .unwrap();
        Self {
            github,
            store,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn webhook(&self, value: &Value) -> Webhook {
        let body = serde_json::to_vec(value).unwrap();
        let signature: String = hmac_sha256(b"webhook-secret", &body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Webhook {
            headers: vec![
                ("X-Hub-Signature-256".into(), format!("sha256={signature}")),
                ("X-GitHub-Event".into(), "ignored-by-design".into()),
            ],
            body,
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            assert!(stop.send(()).is_ok());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn payload(action: &str) -> Value {
    let mut value = recorded(action);
    value["repository"]["full_name"] = REPO.into();
    value["issue"]["repository_url"] = format!("https://api.github.com/repos/{REPO}").into();
    value["sender"] = json!({"id": 2, "login": "dev", "type": "User"});
    if let Some(label) = value.get_mut("label") {
        label["name"] = "kod".into();
    }
    if let Some(comment) = value.get_mut("comment") {
        comment["issue_url"] = format!("https://api.github.com/repos/{REPO}/issues/1").into();
    }
    value
}

fn recorded(action: &str) -> Value {
    serde_json::from_str(match action {
        "labeled" => include_str!("../tests/fixtures/github/labeled.json"),
        "unlabeled" => include_str!("../tests/fixtures/github/unlabeled.json"),
        "edited" => include_str!("../tests/fixtures/github/edited.json"),
        "created" => include_str!("../tests/fixtures/github/comment.json"),
        _ => panic!("unknown recorded event"),
    })
    .unwrap()
}

#[test]
fn recorded_github_payloads_parse_without_changing_their_identity() {
    let h = Harness::new();
    let mut mapping: Value = serde_json::from_str(MAPPING).unwrap();
    mapping["trigger_label"] = "bug".into();
    let github = GitHub::new(
        Config {
            repo: "Codertocat/Hello-World".into(),
            comment_author_id: 99,
        },
        Mapping::parse("recorded examples", &mapping.to_string()).unwrap(),
        "test-token",
        "webhook-secret",
    )
    .unwrap();
    for (action, expected) in [
        ("labeled", "labeled"),
        ("unlabeled", "unlabeled"),
        ("edited", "edited"),
        ("created", "commented"),
    ] {
        let event = github
            .parse_webhook(&h.webhook(&recorded(action)))
            .unwrap()
            .unwrap();
        match (expected, event.event) {
            ("labeled", TaskEvent::Labeled { issue, by })
            | ("unlabeled", TaskEvent::Unlabeled { issue, by }) => {
                assert_eq!(issue, "Codertocat/Hello-World#1");
                assert_eq!(by, "Codertocat");
            }
            ("edited", TaskEvent::Edited { issue }) => {
                assert_eq!(issue, "Codertocat/Hello-World#1")
            }
            ("commented", TaskEvent::Commented { issue, author, .. }) => {
                assert_eq!(issue, "Codertocat/Hello-World#1");
                assert_eq!(author, "Codertocat");
            }
            other => panic!("unexpected recorded event: {other:?}"),
        }
    }
}

#[test]
fn github_passes_the_shared_contract_over_http() {
    let h = Harness::new();
    let labeled = payload("labeled");
    let mut commented = payload("created");
    commented["comment"] = json!({"id": 7, "body": "@kod use v2", "issue_url": "https://api.github.com/repos/acme/app/issues/1", "user": {"id": 2, "login": "dev", "type": "User"}});
    let trigger = h.webhook(&labeled);
    let comment = h.webhook(&commented);
    let untrigger = h.webhook(&payload("unlabeled"));
    let edited = h.webhook(&payload("edited"));
    let mut tampered = trigger.clone();
    tampered.body.push(b' ');
    let mut wrong_signature = trigger.clone();
    wrong_signature.headers[0].1.replace_range(7..8, "z");
    let mut unsigned = trigger.clone();
    unsigned.headers.clear();
    let mut unidentified = labeled.clone();
    unidentified["issue"].as_object_mut().unwrap().remove("id");
    let mut ignored = labeled.clone();
    ignored["label"]["name"] = "other".into();
    let mut other_unlabel = payload("unlabeled");
    other_unlabel["label"]["name"] = "other".into();
    let sandbox = Sandbox {
        issue: h.github.get_issue("acme/app#1").unwrap(),
        transitions: vec![
            State::Ready,
            State::InProgress,
            State::Blocked,
            State::InProgress,
            State::InReview,
            State::Done,
        ],
        missing: "acme/app#404".into(),
        member: "dev".into(),
        outsider: "outsider".into(),
        trigger_event: h.github.parse_webhook(&trigger).unwrap().unwrap(),
        trigger,
        tampered,
        wrong_signature,
        unsigned,
        unidentified: h.webhook(&unidentified),
        comment_event: h.github.parse_webhook(&comment).unwrap().unwrap(),
        comment,
        ignored: h.webhook(&ignored),
        untrigger_event: h.github.parse_webhook(&untrigger).unwrap().unwrap(),
        untrigger,
        other_unlabel: h.webhook(&other_unlabel),
        edited_event: h.github.parse_webhook(&edited).unwrap().unwrap(),
        edited,
    };
    check(&h.github, &sandbox);
    let store = h.store.lock().unwrap();
    assert!(
        store.issues[&1]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "product")
    );
    assert_eq!(store.children[&1], vec![2]);
}

#[test]
fn webhook_identity_is_bound_to_the_body_not_unsigned_headers() {
    let h = Harness::new();
    let mut w = h.webhook(&payload("labeled"));
    let first = h.github.parse_webhook(&w).unwrap().unwrap();
    assert_eq!(
        first.event,
        TaskEvent::Labeled {
            issue: "acme/app#1".into(),
            by: "dev".into()
        }
    );
    w.headers
        .push(("X-GitHub-Delivery".into(), "forged-delivery".into()));
    w.headers[1].1 = "issue_comment".into();
    assert_eq!(h.github.parse_webhook(&w).unwrap(), Some(first));
    let mut value = payload("unlabeled");
    value.as_object_mut().unwrap().remove("sender");
    assert!(h.github.parse_webhook(&h.webhook(&value)).is_err());
    value = payload("labeled");
    value["repository"]["full_name"] = "other/repo".into();
    assert!(h.github.parse_webhook(&h.webhook(&value)).is_err());
    value = payload("labeled");
    value["issue"]["pull_request"] = json!({"url": "https://example.test"});
    assert!(
        h.github
            .parse_webhook(&h.webhook(&value))
            .unwrap()
            .is_none()
    );
}

#[test]
fn metadata_and_workflow_conflicts_fail_closed() {
    let h = Harness::new();
    h.store.lock().unwrap().issues.get_mut(&1).unwrap()["body"] = Value::Null;
    let issue = h.github.get_issue("acme/app#1").unwrap();
    assert_eq!((issue.requirement_id, issue.risk_class), (None, None));
    for text in [
        format!("{META_START}{{\"risk_class\":\"low\",\"risk_class\":\"high\"}}{META_END}"),
        format!("{META_START}{{\"links\":[\"other/repo#1\"]}}{META_END}"),
        format!("{META_START}{{\"unknown\":true}}{META_END}"),
        "<!-- kod:issue invalid -->".into(),
    ] {
        h.store.lock().unwrap().issues.get_mut(&1).unwrap()["body"] = text.into();
        assert!(h.github.get_issue("acme/app#1").is_err());
    }
    h.store.lock().unwrap().issues.get_mut(&1).unwrap()["body"] = Value::Null;
    h.store.lock().unwrap().issues.get_mut(&1).unwrap()["labels"] =
        json!([{"name":"kod:blocked"}, {"name":"kod:in-review"}]);
    assert!(h.github.get_issue("acme/app#1").is_err());
    h.store.lock().unwrap().issues.get_mut(&1).unwrap()["labels"] = json!([{"name":"kod:paused"}]);
    assert!(h.github.get_issue("acme/app#1").is_err());
    for key in ["acme/app#01", "other/repo#1", "acme/app#0", "acme/app#../1"] {
        assert!(h.github.get_issue(key).is_err());
    }
    assert!(!h.github.is_team_member("reader").unwrap());
    assert!(!h.github.is_team_member("kod[bot]").unwrap());
    assert!(h.github.is_team_member("outage").is_err());
}

#[test]
fn status_comment_paginates_and_preserves_human_comments() {
    let h = Harness::new();
    let human = json!({"id": 1, "body": STATUS, "issue_url": h.github.issue_url(1), "user": {"id": 1, "login":"po", "type":"User"}});
    h.store.lock().unwrap().comments.insert(1, vec![human; 100]);
    h.github
        .status_comment("acme/app#1", "first ``` @all https://evil.example")
        .unwrap();
    assert!(
        h.store.lock().unwrap().comments[&1][100]["body"]
            .as_str()
            .unwrap()
            .contains("````text\n")
    );
    h.github.status_comment("acme/app#1", "second").unwrap();
    let s = h.store.lock().unwrap();
    assert_eq!(s.comments[&1].len(), 101);
    assert_eq!(s.comments[&1][0]["body"], STATUS);
    assert!(
        s.comments[&1][100]["body"]
            .as_str()
            .unwrap()
            .contains("second")
    );
    assert_eq!(
        s.calls
            .iter()
            .filter(|(m, p, _)| *m == Method::POST && p.ends_with("/comments"))
            .count(),
        1
    );
}

#[test]
fn uncertain_status_creates_are_reconciled_instead_of_duplicated() {
    let h = Harness::new();
    h.store.lock().unwrap().lose_create = true;
    assert!(h.github.status_comment("acme/app#1", "first").is_err());
    h.store.lock().unwrap().hidden_comments = true;
    assert!(h.github.status_comment("acme/app#1", "second").is_err());
    assert_eq!(h.store.lock().unwrap().comments[&1].len(), 1);
    h.store.lock().unwrap().hidden_comments = false;
    h.github.status_comment("acme/app#1", "third").unwrap();
    assert_eq!(h.store.lock().unwrap().comments[&1].len(), 1);
}

#[test]
fn partial_rework_failure_reports_the_created_key() {
    let h = Harness::new();
    h.store.lock().unwrap().fail_link = true;
    let fields = Rework {
        stage: "build".into(),
        artifact: "design".into(),
        version: "v3".into(),
        text: "Clarify limit".into(),
        evidence: vec!["run-1".into()],
    };
    let error = h
        .github
        .create_linked("acme/app#1", ReworkKind::Question, &fields)
        .unwrap_err();
    assert!(format!("{error:#}").contains("created acme/app#2 but linking failed"));
    assert!(
        h.github
            .link_pr("acme/app#1", "https://evil.example/other/repo/pull/1")
            .is_err()
    );
}

#[test]
fn ordinary_comments_cannot_forge_the_owned_status_namespace() {
    let h = Harness::new();
    assert!(h.github.comment("acme/app#1", STATUS).is_err());
    assert!(h.store.lock().unwrap().calls.is_empty());
    h.github
        .link_pr("acme/app#1", "https://github.com/other/repo/pull/1")
        .unwrap();
}

#[test]
fn bot_events_duplicate_signatures_and_invalid_json_are_not_instructions() {
    let h = Harness::new();
    assert!(
        h.github
            .parse_webhook(&h.webhook(&json!({"zen": "Keep it logically awesome.", "hook_id": 1})))
            .unwrap()
            .is_none()
    );
    let mut value = payload("created");
    value["comment"]["user"] = json!({"id": 99, "login": "kod[bot]", "type": "Bot"});
    assert!(
        h.github
            .parse_webhook(&h.webhook(&value))
            .unwrap()
            .is_none()
    );
    let mut w = h.webhook(&payload("labeled"));
    w.headers.push(w.headers[0].clone());
    assert!(h.github.parse_webhook(&w).is_err());
    assert!(h.github.parse_webhook(&h.webhook(&Value::Null)).is_err());
    let mut value = payload("labeled");
    value["sender"] = json!({"id": 99, "login": "kod[bot]", "type": "Bot"});
    assert!(
        h.github
            .parse_webhook(&h.webhook(&value))
            .unwrap()
            .is_none()
    );
}
