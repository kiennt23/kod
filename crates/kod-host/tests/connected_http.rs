//! Loopback HTTP checks of the real ingress and gateway; no Docker is used.
#[path = "common/connected.rs"]
mod connected;

use kod_host::acceptance::docker::fixture::Fixture;
use kod_host::config::FsConfig;
use kod_tool_gateway::Run;

async fn start() -> (connected::ConnectedFixture, Fixture) {
    let h = connected::ConnectedFixture::local();
    let config = FsConfig::load(&h.config).unwrap();
    let mut f = Fixture::new(&config).await.unwrap();
    f.start().await.unwrap();
    (h, f)
}

fn runs(f: &Fixture) -> i64 {
    kod_dispatcher::store::open(&f.config.paths.dispatcher_db)
        .unwrap()
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_signature_is_rejected_and_doubled_delivery_is_idempotent() {
    let (_h, mut f) = start().await;
    // A well-formed payload with a bad signature is refused for its signature.
    let body = f.payload("labeled", "mallory").unwrap();
    assert!(!f.webhook("bad", &body, false).await.unwrap().is_success());
    assert!(f.webhook("good", &body, true).await.unwrap().is_success());
    // A redelivery is accepted without effect; an unauthorized actor never starts a run.
    let again = f.webhook("good", &body, true).await.unwrap();
    assert!(again.is_success(), "{again}");
    assert_eq!(runs(&f), 0);
    f.shutdown().await.unwrap();
}

fn mcp(addr: std::net::SocketAddr, token: &str, host: &str) -> u16 {
    reqwest::blocking::Client::new()
        .post(format!("http://{addr}/r/{token}/mcp"))
        .header("host", host)
        .body("{}")
        .send()
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_ended_tokens_and_unlisted_hosts_are_refused() {
    let (_h, mut f) = start().await;
    let (gateway, addr) = {
        let s = f.service.as_ref().unwrap();
        (s.runtime.tool().clone(), s.gateway)
    };
    let live = "a".repeat(64);
    let listed = format!("127.0.0.1:{}", addr.port());
    let result = tokio::task::spawn_blocking(move || {
        gateway
            .start_run(
                &live,
                Run {
                    id: "run-1".into(),
                    ticket: "acme/app#7".into(),
                    repo: "acme/app".into(),
                    base: "0".repeat(40),
                    data_classes: vec!["placeholder".into()],
                    pins: Default::default(),
                    linked_tickets: vec![],
                    started_at: 0,
                },
            )
            .unwrap();
        let known = mcp(addr, &live, &listed);
        let unlisted = mcp(addr, &live, "evil.example");
        let wrong = mcp(addr, &"b".repeat(64), &listed);
        gateway.end_run("run-1").unwrap();
        let ended = mcp(addr, &live, &listed);
        (known, unlisted, wrong, ended)
    })
    .await
    .unwrap();
    assert_ne!(result.0, 404, "a live token is routed");
    assert_ne!(result.1, result.0, "an unlisted host is refused");
    assert!(result.1 >= 400);
    assert_eq!((result.2, result.3), (404, 404));
    f.shutdown().await.unwrap();
}
