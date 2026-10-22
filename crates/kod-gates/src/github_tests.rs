use super::*;
use axum::{Router, body::Bytes, extract::State, http::StatusCode, response::IntoResponse};
use std::{collections::VecDeque, sync::Arc};

type Request = (String, String, Value);
struct Server {
    github: GitHub,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
type Reply = (u16, Value);
type Shared = (Arc<Mutex<VecDeque<Reply>>>, Arc<Mutex<Vec<Request>>>);

fn server(replies: Vec<Reply>) -> Server {
    let requests = Arc::new(Mutex::new(vec![]));
    let state = (
        Arc::new(Mutex::new(VecDeque::from(replies))),
        requests.clone(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let app = Router::new()
                    .fallback(
                        async |State((replies, requests)): State<Shared>,
                               method: axum::http::Method,
                               uri: axum::http::Uri,
                               body: Bytes| {
                            let body = if body.is_empty() {
                                Value::Null
                            } else {
                                serde_json::from_slice(&body).unwrap()
                            };
                            requests.lock().unwrap().push((
                                method.to_string(),
                                uri.to_string(),
                                body,
                            ));
                            let (status, body) = replies
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((500, json!({})));
                            (StatusCode::from_u16(status).unwrap(), axum::Json(body))
                                .into_response()
                        },
                    )
                    .with_state(state);
                axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                    .with_graceful_shutdown(async {
                        stopped.await.unwrap();
                    })
                    .await
                    .unwrap();
            });
    });
    Server {
        github: GitHub::with_api("acme/app", "private-test-token", &url).unwrap(),
        requests,
        stop: Some(stop),
        thread: Some(thread),
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn pull(files: usize) -> Value {
    json!({"number": 42, "head": {"sha": SHA}, "base": {"sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "repo": {"full_name": "acme/app"}},
        "state": "open", "user": {"login": "author", "type": "User"}, "changed_files": files})
}

#[test]
fn changed_paths_include_rename_sources_and_every_page() {
    let first: Vec<_> = (0..100)
        .map(|n| json!({"filename": format!("src/{n}.rs")}))
        .collect();
    let s = server(vec![
        (200, pull(101)),
        (200, json!(first)),
        (
            200,
            json!([{"filename": "renamed.yml", "previous_filename": ".github/workflows/ci.yml"}]),
        ),
        (
            200,
            json!([{"id": 1, "user": {"login": "robot[bot]", "type": "Bot"}, "commit_id": SHA, "state": "APPROVED"}]),
        ),
    ]);
    let pr = s.github.pull(42).unwrap();
    assert_eq!(pr.paths.len(), 102);
    assert!(pr.paths.contains(&".github/workflows/ci.yml".into()));
    assert!(!pr.reviews[0].human);
    assert!(
        s.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, url, _)| url.ends_with("files?per_page=100&page=2"))
    );
}

#[test]
fn incomplete_paths_and_unknown_review_states_fail_closed() {
    let s = server(vec![(200, pull(2)), (200, json!([{"filename": "one.rs"}]))]);
    assert!(s.github.pull(42).is_err());
    let s = server(vec![
        (200, pull(0)),
        (200, json!([])),
        (
            200,
            json!([{"id": 1,
        "user": {"login": "reviewer", "type": "User"}, "commit_id": SHA, "state": "SURPRISE"}]),
        ),
    ]);
    assert!(s.github.pull(42).is_err());
}

#[test]
fn truncated_github_file_lists_never_pass() {
    let mut replies = vec![(200, pull(3001))];
    for page in 0..30 {
        replies.push((
            200,
            json!(
                (0..100)
                    .map(|n| json!({"filename": format!("{page}-{n}.rs")}))
                    .collect::<Vec<_>>()
            ),
        ));
    }
    let s = server(replies);
    assert!(
        s.github
            .pull(42)
            .unwrap_err()
            .to_string()
            .contains("truncated")
    );
}

#[test]
fn completion_updates_the_started_check_and_does_not_create_another() {
    let s = server(vec![(201, json!({"id": 17})), (200, json!({"id": 17}))]);
    let mut check = Check {
        name: "inputs-current".into(),
        conclusion: Conclusion::Pending,
        summary: "Checking".into(),
    };
    s.github.report(SHA, &check).unwrap();
    check.conclusion = Conclusion::Success;
    s.github.report(SHA, &check).unwrap();
    let requests = s.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "POST");
    assert_eq!(requests[0].2["head_sha"], SHA);
    assert_eq!(requests[1].0, "PATCH");
    assert!(requests[1].1.ends_with("check-runs/17"));
    assert_eq!(requests[1].2["conclusion"], "success");
}

#[test]
fn errors_never_echo_response_bodies_or_credentials() {
    let s = server(vec![(403, json!({"error": "private-test-token"}))]);
    let error = format!("{:#}", s.github.pull(42).unwrap_err());
    assert!(error.contains("403"));
    assert!(!error.contains("private-test-token"));
}

#[test]
fn a_completed_check_without_a_started_generation_is_refused() {
    let s = server(vec![]);
    assert!(
        s.github
            .report(
                SHA,
                &Check {
                    name: "tests".into(),
                    conclusion: Conclusion::Success,
                    summary: "Passed".into()
                }
            )
            .is_err()
    );
    assert!(s.requests.lock().unwrap().is_empty());
}

#[test]
fn reviewer_write_permissions_are_read_from_github() {
    for (permission, expected) in [("read", false), ("write", true)] {
        let s = server(vec![
            (200, pull(0)),
            (200, json!([])),
            (
                200,
                json!([{"id": 1,
            "user": {"login": "reviewer", "type": "User"}, "commit_id": SHA, "state": "APPROVED"}]),
            ),
            (200, json!({"permission": permission})),
        ]);
        let pr = s.github.pull(42).unwrap();
        assert_eq!(pr.reviews[0].can_approve, expected);
        assert!(
            s.requests
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .ends_with("collaborators/reviewer/permission")
        );
    }
}
