mod common;

use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, Uri},
    response::IntoResponse,
    routing::{get, post},
};
use common::{Gateway, MODEL, Setup, gateway, limits, msg, post_msg, serve};
use kod_llm_proxy::{Limits, USAGE_FIELDS, Usage};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// A fake upstream that echoes what it received, behind the gateway.
async fn proxy_with(limits: Limits) -> Gateway {
    let upstream = Router::new()
        .route(
            "/v1/messages",
            post(|h: HeaderMap, uri: Uri, body: Bytes| async move {
                let key = h
                    .get("x-api-key")
                    .map_or("", |v| v.to_str().unwrap())
                    .to_owned();
                format!(
                    "key={key} auth={} query={} body={}",
                    h.contains_key("authorization"),
                    uri.query().unwrap_or(""),
                    String::from_utf8_lossy(&body)
                )
            }),
        )
        .route("/v1/files", get(|| async { "LEAK" }));
    let upstream = serve(upstream).await;
    gateway(
        &upstream,
        Setup {
            limits,
            ..Setup::default()
        },
    )
    .await
}

async fn proxy() -> Gateway {
    proxy_with(limits()).await
}

#[tokio::test]
async fn forwards_with_the_real_key_and_drops_client_credentials() {
    let g = proxy().await;
    let p = g.url.clone();
    let body = msg(MODEL, 100);
    let res = reqwest::Client::new()
        .post(format!("{p}/v1/messages?beta=true"))
        .header("x-api-key", "dummy")
        .header("authorization", "Bearer dummy")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.text().await.unwrap(),
        format!("key=real-key auth=false query=beta=true body={body}")
    );
}

#[tokio::test]
async fn refuses_other_endpoints() {
    let g = proxy().await;
    let p = g.url.clone();
    assert_eq!(
        reqwest::get(format!("{p}/v1/files"))
            .await
            .unwrap()
            .status(),
        403
    );
}

#[tokio::test]
async fn refuses_dot_dot_paths() {
    // Sent raw, because HTTP clients normalize "..".
    let g = proxy().await;
    let p = g.url.clone();
    let (host, prefix) = p.trim_start_matches("http://").split_once('/').unwrap();
    let mut s = TcpStream::connect(host).await.unwrap();
    let req = format!(
        "GET /{prefix}/v1/messages/../files HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    assert!(buf.starts_with("HTTP/1.1 403"), "{buf}");
}

#[tokio::test]
async fn refuses_models_not_on_the_list() {
    let g = proxy().await;
    let p = g.url.clone();
    assert_eq!(
        post_msg(&p, msg("claude-expensive-9", 100)).await.status(),
        403
    );
}

#[tokio::test]
async fn refuses_missing_or_oversized_max_tokens() {
    let g = proxy().await;
    let p = g.url.clone();
    assert_eq!(post_msg(&p, msg(MODEL, 1001)).await.status(), 400);
    assert_eq!(
        post_msg(&p, format!(r#"{{"model":"{MODEL}","messages":[]}}"#))
            .await
            .status(),
        400
    );
    assert_eq!(post_msg(&p, "not json".into()).await.status(), 400);
}

#[tokio::test]
async fn refuses_oversized_bodies() {
    let g = proxy().await;
    let p = g.url.clone();
    let big = format!(
        r#"{{"model":"{MODEL}","max_tokens":10,"pad":"{}"}}"#,
        "x".repeat(5000)
    );
    assert_eq!(post_msg(&p, big).await.status(), 413);
}

#[tokio::test]
async fn stops_forwarding_after_the_request_budget() {
    let g = proxy_with(Limits {
        max_requests: 2,
        ..limits()
    })
    .await;
    let p = g.url.clone();
    assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 200);
    assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 200);
    assert_eq!(post_msg(&p, msg(MODEL, 10)).await.status(), 429);
    // Refused requests count too, so a script can't probe for free.
    assert_eq!(
        post_msg(&p, msg("claude-expensive-9", 10)).await.status(),
        429
    );
}

#[tokio::test]
async fn limits_concurrent_requests() {
    let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (n, m) = (now.clone(), max.clone());
    let slow = Router::new().route(
        "/v1/messages",
        post(move || {
            let (n, m) = (n.clone(), m.clone());
            async move {
                let cur = n.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(200)).await;
                n.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );
    let upstream = serve(slow).await;
    let g = gateway(
        &upstream,
        Setup {
            limits: Limits {
                max_concurrent: 1,
                ..limits()
            },
            ..Setup::default()
        },
    )
    .await;
    let p = g.url.clone();
    let calls: Vec<_> = (0..3)
        .map(|_| tokio::spawn(post_msg_owned(p.clone())))
        .collect();
    for c in calls {
        assert_eq!(c.await.unwrap(), 200);
    }
    assert_eq!(max.load(Ordering::SeqCst), 1);
}

async fn post_msg_owned(p: String) -> u16 {
    post_msg(&p, msg(MODEL, 10)).await.status().as_u16()
}

/// The client holds the key, so it must not follow a redirect to another host.
#[tokio::test]
async fn does_not_follow_upstream_redirects() {
    let stolen = Arc::new(AtomicUsize::new(0));
    let seen = stolen.clone();
    let thief = serve(Router::new().fallback(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async { "stolen" }
    }))
    .await;
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(move || async move {
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", format!("{thief}/steal"))],
            )
        }),
    ))
    .await;
    let g = gateway(&upstream, Setup::default()).await;
    let p = g.url.clone();
    // The test's own client must not follow it either, or it would reach the thief.
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .post(format!("{p}/v1/messages"))
        .body(msg(MODEL, 100))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 307);
    assert_eq!(stolen.load(Ordering::SeqCst), 0);
}

const SSE: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1,\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":4}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":34}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

fn usage_of(sse: bool, body: &str, chunk: usize) -> serde_json::Value {
    let mut u = Usage::new(sse);
    for c in body.as_bytes().chunks(chunk) {
        u.feed(c);
    }
    serde_json::Value::Object(u.finish())
}

#[test]
fn reads_usage_from_json_and_event_streams_in_any_chunking() {
    let json =
        r#"{"id":"m","usage":{"input_tokens":7,"output_tokens":9,"cache_read_input_tokens":2}}"#;
    for chunk in [1, 5, 4096] {
        let u = usage_of(false, json, chunk);
        assert_eq!(u["input_tokens"], 7);
        assert_eq!(u["output_tokens"], 9);
        assert_eq!(u["cache_read_input_tokens"], 2);
        assert!(
            u["cache_creation_input_tokens"].is_null(),
            "missing is null, not 0"
        );
        let u = usage_of(true, SSE, chunk);
        assert_eq!(
            u,
            serde_json::json!({"input_tokens": 12, "output_tokens": 34,
                "cache_creation_input_tokens": 3, "cache_read_input_tokens": 4})
        );
    }
    for (sse, body) in [
        (false, r#"{"error":{}}"#),
        (false, "{trunc"),
        (true, "data: {}\n"),
    ] {
        let u = usage_of(sse, body, 3);
        assert!(USAGE_FIELDS.iter().all(|k| u[k].is_null()), "{u}");
    }
    // A stream cut before message_stop has a real input count, but its output
    // count is unknown, not message_start's placeholder.
    let cut = &SSE[..SSE.find("event: message_stop").unwrap()];
    let u = usage_of(true, cut, 7);
    assert_eq!(u["input_tokens"], 12);
    assert!(u["output_tokens"].is_null(), "{u}");
}

/// Starts the gateway binary against `upstream` with the repo's policy, the
/// fixture's prices and run, and an audit log in a temporary directory. Waits
/// until it answers, and returns the run's URL, the process, its stdout lines
/// and the directory.
async fn spawn_proxy(
    upstream: &str,
    env: &[(&str, &str)],
) -> (
    String,
    tokio::process::Child,
    tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let (prices, runs) = (dir.path().join("prices.json"), dir.path().join("runs.json"));
    std::fs::write(&prices, common::PRICES_JSON).unwrap();
    std::fs::write(&runs, common::RUNS_JSON).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_kod-llm-proxy"))
        .env("KOD_ENDPOINT", common::ENDPOINT)
        .env("KOD_UPSTREAM", upstream)
        .env("KOD_API_KEY", "real-key")
        .env("KOD_ALLOWED_MODELS", MODEL)
        .env("KOD_LISTEN", format!("127.0.0.1:{port}"))
        .env("KOD_PRICES", &prices)
        .env("KOD_RUNS", &runs)
        .env("KOD_POLICY_DIR", common::policy_dir())
        .env("KOD_AUDIT_DB", dir.path().join("audit.db"))
        .envs(env.iter().copied())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let url = format!("http://127.0.0.1:{port}/r/{}", common::TOKEN);
    (url, child, lines, dir)
}

/// Log lines until the proxy has been quiet for half a second.
async fn drain(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) -> Vec<serde_json::Value> {
    let mut out = vec![];
    while let Ok(Ok(Some(l))) =
        tokio::time::timeout(Duration::from_millis(500), lines.next_line()).await
    {
        out.push(serde_json::from_str(&l).unwrap());
    }
    out
}

fn sse_upstream() -> Router {
    Router::new().route(
        "/v1/messages",
        post(|body: String| async move {
            if body.contains("fail") {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    [("content-type", "application/json")],
                    r#"{"error":{}}"#,
                )
                    .into_response()
            } else {
                ([("content-type", "text/event-stream")], SSE).into_response()
            }
        }),
    )
}

/// The proxy binary passes an event stream through unchanged and logs one
/// usage line once the stream has ended.
#[tokio::test]
async fn logs_usage_after_a_stream_and_passes_it_through_unchanged() {
    let upstream = serve(sse_upstream()).await;
    let (p, _child, mut lines, _dir) = spawn_proxy(&upstream, &[]).await;
    let body = reqwest::Client::new()
        .post(format!("{p}/v1/messages"))
        .body(msg(MODEL, 10))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, SSE);
    let events = drain(&mut lines).await;
    let names: Vec<_> = events.iter().map(|v| v["event"].clone()).collect();
    assert_eq!(names, ["ready", "allow", "usage"]);
    let v = &events[2];
    assert_eq!(
        (v["input_tokens"].clone(), v["output_tokens"].clone()),
        (12.into(), 34.into())
    );
    assert_eq!(v["status"], 200);
}

/// The log is bounded by the request budget: refused paths count against it,
/// only the first request past it is logged, logged paths are cut short, and
/// error responses get no usage line.
#[tokio::test]
async fn log_lines_are_bounded_by_the_request_budget() {
    let upstream = serve(sse_upstream()).await;
    let (p, _child, mut lines, _dir) = spawn_proxy(&upstream, &[("KOD_MAX_REQUESTS", "3")]).await;
    let c = reqwest::Client::new();
    let long = format!("{p}/v1/{}", "x".repeat(10_000));
    assert_eq!(c.post(&long).send().await.unwrap().status(), 403);
    let ok = c
        .post(format!("{p}/v1/messages"))
        .body(msg(MODEL, 10))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    ok.text().await.unwrap();
    let fail = format!(
        r#"{{"model":"{MODEL}","max_tokens":10,"messages":[{{"role":"user","content":"fail"}}]}}"#
    );
    let res = c
        .post(format!("{p}/v1/messages"))
        .body(fail)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
    res.text().await.unwrap();
    for _ in 0..20 {
        assert_eq!(c.post(&long).send().await.unwrap().status(), 429);
    }
    let events = drain(&mut lines).await;
    let names: Vec<_> = events
        .iter()
        .map(|v| v["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["ready", "deny", "allow", "usage", "allow", "over_budget"],
        "{events:?}"
    );
    for v in &events {
        assert!(v.to_string().len() < 1024, "{v}");
    }
}
