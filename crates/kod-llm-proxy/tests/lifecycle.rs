mod common;
use common::*;
use kod_llm_proxy::{Config, Endpoint, ReservationSink, app_with_registry, budget::Spend};
use std::{collections::HashMap, sync::Arc};

fn config(dir: &tempfile::TempDir) -> Config {
    Config {
        endpoint: Endpoint {
            name: ENDPOINT.into(),
            upstream: "http://127.0.0.1:1".into(),
            api_key: "key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into()],
        limits: limits(),
        prices: HashMap::from([(MODEL.into(), PRICE)]),
        runs: HashMap::new(),
        policy: Arc::new(kod_policy::Policy::load(&policy_dir()).unwrap()),
        audit: Arc::new(kod_audit::AuditLog::open(&dir.path().join("audit.db")).unwrap()),
    }
}
#[test]
fn registration_is_bound_and_revocation_is_permanent() {
    let dir = tempfile::tempdir().unwrap();
    let (_, registry) = app_with_registry(config(&dir));
    let spec = run(&[CLASS], budget());
    registry.register(TOKEN, spec.clone()).unwrap();
    registry.register(TOKEN, spec.clone()).unwrap();
    assert!(registry.register(&"a".repeat(32), spec.clone()).is_err());
    let mut changed = spec.clone();
    changed.ticket = "other".into();
    assert!(registry.register(TOKEN, changed).is_err());
    registry.revoke("r1").unwrap();
    registry.revoke("r1").unwrap();
    assert!(registry.register(TOKEN, spec).is_err());
}

#[tokio::test]
async fn revocation_ends_silent_stream_and_queued_call() {
    use axum::{
        Router,
        body::{Body, Bytes},
        response::Response,
        routing::post,
    };
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::pending::<
                        Result<Bytes, Infallible>,
                    >()))
                    .unwrap()
            }
        }),
    ))
    .await;
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    cfg.limits.max_concurrent = 1;
    let (router, registry) = app_with_registry(cfg);
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    let first = post_msg(&url, msg(MODEL, 10)).await;
    let first = tokio::spawn(first.bytes());
    let next_url = url.clone();
    let second = tokio::spawn(async move { post_msg(&next_url, msg(MODEL, 10)).await });
    // The prompt audit is before semaphore acquisition, proving the second request queued.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if entries(&dir.path().join("audit.db"))
                .iter()
                .filter(|(kind, _)| kind == "prompts")
                .count()
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    registry.revoke("r1").unwrap();
    tokio::time::timeout(kod_llm_proxy::MODEL_CANCEL_DEADLINE, first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !tokio::time::timeout(kod_llm_proxy::MODEL_CANCEL_DEADLINE, second)
            .await
            .unwrap()
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 401);
}

#[tokio::test]
async fn revocation_cancels_upstream_before_headers() {
    use axum::{Router, routing::post};
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(move || {
            signal.notify_one();
            std::future::pending::<String>()
        }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    let request = tokio::spawn(async move { post_msg(&url, msg(MODEL, 10)).await });
    entered.notified().await;
    registry.revoke("r1").unwrap();
    assert!(
        !tokio::time::timeout(kod_llm_proxy::MODEL_CANCEL_DEADLINE, request)
            .await
            .unwrap()
            .unwrap()
            .status()
            .is_success()
    );
}

#[tokio::test]
async fn duplicate_registration_does_not_reset_spend() {
    use axum::{Router, routing::post};
    let dir = tempfile::tempdir().unwrap();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(|| async { r#"{"usage":{"input_tokens":4000,"output_tokens":10}}"# }),
    ))
    .await;
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    let mut spec = run(&[CLASS], budget());
    spec.budget.max_tokens = 8000;
    registry.register(TOKEN, spec.clone()).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 200);
    registry.register(TOKEN, spec).unwrap();
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 403);
}

#[derive(Default)]
struct Sink {
    records: std::sync::Mutex<Vec<(String, String, Spend)>>,
    fail: bool,
}
impl ReservationSink for Sink {
    fn reserve(&self, _: &str, request: &str, spend: Spend) -> anyhow::Result<()> {
        self.records
            .lock()
            .unwrap()
            .push(("reserve".into(), request.into(), spend));
        if self.fail {
            anyhow::bail!("disk failure");
        }
        Ok(())
    }
    fn settle(&self, _: &str, request: &str, spend: Spend) -> anyhow::Result<()> {
        self.records
            .lock()
            .unwrap()
            .push(("settle".into(), request.into(), spend));
        Ok(())
    }
}

#[tokio::test]
async fn durable_reservation_precedes_dispatch_and_settlement() {
    use axum::{Router, routing::post};
    let sink = Arc::new(Sink::default());
    let seen = sink.clone();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(move || {
            assert_eq!(seen.records.lock().unwrap()[0].0, "reserve");
            async { r#"{"usage":{"input_tokens":10,"output_tokens":10}}"# }
        }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry.set_reservation_sink(sink.clone()).unwrap();
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    post_msg(&url, msg(MODEL, 10)).await.bytes().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if sink.records.lock().unwrap().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let records = sink.records.lock().unwrap();
    assert_eq!(records[0].1, records[1].1);
    assert_eq!(
        records[1].2,
        Spend {
            tokens: 20,
            pico_usd: 20
        }
    );
}

#[tokio::test]
async fn failed_reservation_never_dispatches() {
    use axum::{Router, routing::post};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { "{}" }
        }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry
        .set_reservation_sink(Arc::new(Sink {
            fail: true,
            ..Sink::default()
        }))
        .unwrap();
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 503);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 401);
}

#[tokio::test]
async fn unknown_usage_keeps_durable_charge_and_blocks_reuse() {
    use axum::{Router, routing::post};
    let sink = Arc::new(Sink::default());
    let upstream = serve(Router::new().route("/v1/messages", post(|| async { "{}" }))).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry.set_reservation_sink(sink.clone()).unwrap();
    let mut spec = run(&[CLASS], budget());
    spec.budget.max_tokens = 8000;
    registry.register(TOKEN, spec).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    post_msg(&url, msg(MODEL, 10)).await.bytes().await.unwrap();
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 403);
    assert_eq!(sink.records.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn overrun_is_durably_settled_and_halts_run() {
    use axum::{Router, routing::post};
    let sink = Arc::new(Sink::default());
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(|| async { r#"{"usage":{"input_tokens":9000,"output_tokens":10}}"# }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry.set_reservation_sink(sink.clone()).unwrap();
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    post_msg(&url, msg(MODEL, 10)).await.bytes().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if entries(&dir.path().join("audit.db"))
                .iter()
                .any(|(kind, _)| kind == "model_outputs")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 403);
    assert_eq!(sink.records.lock().unwrap()[1].2.tokens, 9010);
}

struct FailedSettlement;
impl ReservationSink for FailedSettlement {
    fn reserve(&self, _: &str, _: &str, _: Spend) -> anyhow::Result<()> {
        Ok(())
    }
    fn settle(&self, _: &str, _: &str, _: Spend) -> anyhow::Result<()> {
        anyhow::bail!("disk unavailable")
    }
}
#[tokio::test]
async fn failed_settlement_revokes_the_run() {
    use axum::{Router, routing::post};
    let upstream = serve(Router::new().route(
        "/v1/messages",
        post(|| async { r#"{"usage":{"input_tokens":10,"output_tokens":10}}"# }),
    ))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir);
    cfg.endpoint.upstream = upstream;
    let (router, registry) = app_with_registry(cfg);
    registry
        .set_reservation_sink(Arc::new(FailedSettlement))
        .unwrap();
    registry.register(TOKEN, run(&[CLASS], budget())).unwrap();
    let url = format!("{}/r/{TOKEN}", serve(router).await);
    post_msg(&url, msg(MODEL, 10)).await.bytes().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if entries(&dir.path().join("audit.db"))
                .iter()
                .any(|(kind, _)| kind == "model_outputs")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 401);
}
