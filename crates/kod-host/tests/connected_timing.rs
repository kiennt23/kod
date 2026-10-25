use axum::{
    Router,
    body::{Body, Bytes},
    http::Request,
    response::Response,
    routing::post,
};
use kod_host::{
    deadlines,
    service::{Admission, worker_loop},
};
use kod_llm_proxy::{
    Config, Endpoint, Limits, app_with_registry,
    config::{Budget, Price, RunSpec},
};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tower::ServiceExt;

#[tokio::test(start_paused = true)]
async fn worker_wakes_at_250ms_without_early_action() {
    // Given the real worker and an observable completion barrier.
    let (sent, mut seen) = mpsc::unbounded_channel();
    let (stop, receiver) = watch::channel(false);
    let admission = Admission::default();
    let mut tasks = JoinSet::new();
    tasks.spawn(worker_loop(
        Arc::new(move |allowed| {
            sent.send(allowed)?;
            Ok(false)
        }),
        admission,
        Arc::new(|| false),
        receiver,
        deadlines::WORKER_TICK,
    ));
    assert!(!seen.recv().await.unwrap());
    tokio::task::yield_now().await;
    // When time reaches just before, then exactly the next worker deadline.
    tokio::time::advance(Duration::from_millis(249)).await;
    tokio::task::yield_now().await;
    assert!(seen.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    // Then maintenance executes and successful startup opened admission.
    assert!(seen.recv().await.unwrap());
    stop.send(true).unwrap();
    tasks.join_next().await.unwrap().unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn model_registry_revokes_queued_and_streaming_requests_within_two_seconds() {
    // Given the real policy/audit/model gateway and a silent loopback provider.
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.db");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let upstream = Router::new().route(
        "/v1/messages",
        post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::pending::<
                        Result<Bytes, Infallible>,
                    >()))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut servers = JoinSet::new();
    // Keep virtual time stationary while real loopback I/O waits for the OS.
    servers.spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    servers.spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let config = Config {
        endpoint: Endpoint {
            name: "placeholder".into(),
            upstream: format!("http://{address}"),
            api_key: "fixture-key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into()],
        limits: Limits {
            allowed_models: vec!["fixture-model".into()],
            max_tokens: 100,
            max_body: 4096,
            max_requests: 10,
            max_concurrent: 1,
        },
        prices: HashMap::from([(
            "fixture-model".into(),
            Price {
                input: 1,
                output: 1,
                cache_write: 1,
                cache_read: 1,
            },
        )]),
        runs: HashMap::new(),
        policy: Arc::new(
            kod_policy::Policy::load(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"),
            )
            .unwrap(),
        ),
        audit: Arc::new(kod_audit::AuditLog::open(&audit_path).unwrap()),
    };
    let (router, registry) = app_with_registry(config);
    let token = "0123456789abcdef0123456789abcdef";
    let spec = RunSpec {
        id: "run-1".into(),
        ticket: "acme/app#1".into(),
        repo: "acme/app".into(),
        data_classes: vec!["placeholder".into()],
        budget: Budget {
            max_tokens: 10000,
            max_cost_micro_usd: 10000,
        },
    };
    registry.register(token, spec.clone()).unwrap();
    let request = || {
        Request::post(format!("/r/{token}/v1/messages"))
            .body(Body::from(
                r#"{"model":"fixture-model","max_tokens":10,"messages":[],"stream":true}"#,
            ))
            .unwrap()
    };
    let first = router.clone().oneshot(request()).await.unwrap();
    assert!(first.status().is_success());
    let mut requests = JoinSet::new();
    requests.spawn(async move {
        axum::body::to_bytes(first.into_body(), 4096).await.unwrap();
    });
    let second = router.clone().oneshot(request());
    let second = tokio::spawn(second);
    let db = rusqlite::Connection::open(&audit_path).unwrap();
    for attempt in 0..10000 {
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM audit WHERE record_type='prompts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if count == 2 {
            break;
        }
        assert!(
            attempt < 9999,
            "queued request never reached admission barrier"
        );
        tokio::task::yield_now().await;
    }
    // When the production registry revokes a run with a stream and queued request.
    let began = tokio::time::Instant::now();
    registry.revoke("run-1").unwrap();
    tokio::time::advance(deadlines::MODEL_CANCEL).await;
    // Then the stream ends, queue is denied, old credentials cannot reset spend.
    requests.join_next().await.unwrap().unwrap();
    assert!(!second.await.unwrap().unwrap().status().is_success());
    assert!(began.elapsed() <= deadlines::MODEL_CANCEL);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(router.oneshot(request()).await.unwrap().status(), 401);
    assert!(registry.register(token, spec).is_err());
    servers.shutdown().await;
}
