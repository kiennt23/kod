//! A revoked run's queued and streaming model calls end within the cancel deadline.
use kod_llm_proxy::{
    Config, Endpoint, Limits, MODEL_CANCEL_DEADLINE, app_with_registry,
    config::{Budget, Price, RunSpec},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const BODY: &str = r#"{"model":"claude-test-1","max_tokens":10,"messages":[]}"#;

/// Answers every request with SSE headers and then stays silent forever.
async fn silent_upstream(connections: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                            transfer-encoding: chunked\r\n\r\n";
                let _ = socket.write_all(head.as_bytes()).await;
                std::future::pending::<()>().await;
            });
        }
    });
    format!("http://{addr}")
}

fn audited_prompts(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .and_then(|c| {
            c.query_row(
                "SELECT count(*) FROM audit WHERE record_type='prompts'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn queued_and_streaming_calls_cancel_within_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    let connections = Arc::new(AtomicUsize::new(0));
    let (router, registry) = app_with_registry(Config {
        endpoint: Endpoint {
            name: "placeholder".into(),
            upstream: silent_upstream(connections.clone()).await,
            api_key: "key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into()],
        limits: Limits {
            allowed_models: vec!["claude-test-1".into()],
            max_tokens: 1000,
            max_body: 4096,
            max_requests: 100,
            max_concurrent: 1,
        },
        prices: HashMap::from([(
            "claude-test-1".to_owned(),
            Price {
                input: 1,
                output: 1,
                cache_write: 1,
                cache_read: 1,
            },
        )]),
        runs: HashMap::new(),
        policy: Arc::new(
            kod_policy::Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"))
                .unwrap(),
        ),
        audit: Arc::new(kod_audit::AuditLog::open(&db).unwrap()),
    });
    registry
        .register(
            TOKEN,
            RunSpec {
                id: "r1".into(),
                ticket: "KOD-1".into(),
                repo: "acme/app".into(),
                data_classes: vec!["placeholder".into()],
                budget: Budget {
                    max_tokens: 1_000_000_000,
                    max_cost_micro_usd: 1_000_000,
                },
            },
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/r/{TOKEN}/v1/messages",
        listener.local_addr().unwrap()
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let post = |url: String| async move {
        // Reading the body keeps the stream (and its concurrency slot) open.
        let response = reqwest::Client::new().post(url).body(BODY).send().await?;
        let ok = response.status().is_success();
        response.bytes().await.map(|_| ok)
    };
    // The first call streams (holds the only slot); the second queues behind it.
    let streaming = tokio::spawn(post(url.clone()));
    let queued = tokio::spawn(post(url.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while audited_prompts(&db) < 2 || connections.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both calls reached the gateway");
    registry.revoke("r1").unwrap();
    for call in [streaming, queued] {
        // Ending in an error or a refusal both count; hanging does not.
        let _ended = tokio::time::timeout(MODEL_CANCEL_DEADLINE, call)
            .await
            .expect("call ended within the cancel deadline")
            .unwrap();
    }
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "the queued call never reached upstream"
    );
    assert!(!post(url).await.unwrap_or(false));
}
