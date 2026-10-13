use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, Uri},
    routing::{get, post},
};
use kod_llm_proxy::{Config, Limits, app};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const MODEL: &str = "claude-test-1";

async fn serve(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

fn limits() -> Limits {
    Limits {
        allowed_models: vec![MODEL.into()],
        max_tokens: 1000,
        max_body: 4096,
        max_requests: 100,
        max_concurrent: 2,
    }
}

/// A fake upstream that echoes what it received, behind the proxy.
async fn proxy_with(limits: Limits) -> String {
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
    serve(app(Config {
        upstream,
        api_key: "real-key".parse().unwrap(),
        allowed_paths: vec!["/v1/messages".into()],
        limits,
    }))
    .await
}

async fn proxy() -> String {
    proxy_with(limits()).await
}

fn msg(model: &str, max_tokens: u64) -> String {
    format!(r#"{{"model":"{model}","max_tokens":{max_tokens},"messages":[]}}"#)
}

async fn post_msg(p: &str, body: String) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{p}/v1/messages"))
        .body(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn forwards_with_the_real_key_and_drops_client_credentials() {
    let p = proxy().await;
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
    let p = proxy().await;
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
    let p = proxy().await;
    let mut s = TcpStream::connect(p.trim_start_matches("http://"))
        .await
        .unwrap();
    s.write_all(b"GET /v1/messages/../files HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    assert!(buf.starts_with("HTTP/1.1 403"), "{buf}");
}

#[tokio::test]
async fn refuses_models_not_on_the_list() {
    let p = proxy().await;
    assert_eq!(
        post_msg(&p, msg("claude-expensive-9", 100)).await.status(),
        403
    );
}

#[tokio::test]
async fn refuses_missing_or_oversized_max_tokens() {
    let p = proxy().await;
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
    let p = proxy().await;
    let big = format!(
        r#"{{"model":"{MODEL}","max_tokens":10,"pad":"{}"}}"#,
        "x".repeat(5000)
    );
    assert_eq!(post_msg(&p, big).await.status(), 413);
}

#[tokio::test]
async fn stops_forwarding_after_the_request_budget() {
    let p = proxy_with(Limits {
        max_requests: 2,
        ..limits()
    })
    .await;
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
    let p = serve(app(Config {
        upstream,
        api_key: "k".parse().unwrap(),
        allowed_paths: vec!["/v1/messages".into()],
        limits: Limits {
            max_concurrent: 1,
            ..limits()
        },
    }))
    .await;
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
