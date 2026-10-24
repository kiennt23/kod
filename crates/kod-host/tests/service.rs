mod common;
use kod_dispatcher::RunState;
use kod_host::service::{Parts, Service, production_parts};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn put(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

const OWNER_KEY: &[u8] = b"owner-secret-key";

struct World {
    _dir: tempfile::TempDir,
    config: Arc<kod_host::config::FsConfig>,
    trace: Arc<Mutex<Vec<String>>>,
    base_up: Arc<AtomicBool>,
}
fn world() -> World {
    common::init();
    let tmp = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().to_path_buf();
    dir(&root);
    let mut c = (*common::config(&root, &root.join("dispatcher.db"))).clone();
    c.paths.policy_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policy")
        .canonicalize()
        .unwrap();
    c.paths.approval_manifest = root.join("active/manifest.json");
    c.paths.credential_dir = root.join("cred");
    dir(&c.paths.credential_dir);
    dir(&root.join("active"));
    put(&root.join("active/d.txt"), "v3 text");
    let entry = serde_json::json!({"link": "design", "revision": "design-v3",
        "sha256": format!("{:x}", Sha256::digest("v3 text")), "class": "placeholder",
        "path": "d.txt", "approved_by": "kiennt23", "approved_at": "2026-10-24T00:00:00Z",
        "approval_ref": "S-1"});
    put(
        &root.join("active/manifest.json"),
        &serde_json::json!({"version": 1, "artifacts": [entry]}).to_string(),
    );
    assert!(
        std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out"
            ])
            .arg(root.join("key.pem"))
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(root.join("key.pem"), fs::Permissions::from_mode(0o600)).unwrap();
    put(&root.join("api.key"), "sk-test");
    put(
        &root.join("prices.json"),
        r#"{"x":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#,
    );
    put(
        &root.join("owner.key"),
        std::str::from_utf8(OWNER_KEY).unwrap(),
    );
    put(
        &root.join("dispatcher.json"),
        r#"{"repo":"acme/app","owner":"kiennt23","backup":null}"#,
    );
    c.model.api_key_file = root.join("api.key");
    c.model.prices = root.join("prices.json");
    c.model.endpoint_name = "placeholder".into();
    c.model.max_body = 4096;
    c.model.max_tokens = 100;
    c.model.max_requests = 10;
    c.model.max_concurrent = 2;
    c.github.owner_key_file = root.join("owner.key");
    c.dispatcher_config = root.join("dispatcher.json");
    c.networking.ingress_listen = "127.0.0.1:0".parse().unwrap();
    c.networking.gateway_listen = "127.0.0.1:0".parse().unwrap();
    World {
        _dir: tmp,
        config: Arc::new(c),
        trace: Arc::default(),
        base_up: Arc::new(AtomicBool::new(true)),
    }
}
fn parts(w: &World) -> Parts {
    let (tracker, _seed) = kod_tracker::fake::sandbox();
    let mut p = production_parts(&w.config, Arc::new(tracker)).unwrap();
    let up = w.base_up.clone();
    p.base = Arc::new(move || {
        anyhow::ensure!(up.load(Ordering::SeqCst), "mirror unavailable");
        Ok("c".repeat(40))
    });
    let t = w.trace.clone();
    p.trace = Arc::new(move |s| t.lock().unwrap().push(s.to_owned()));
    p
}
async fn status(addr: std::net::SocketAddr, method: &str, path: &str, auth: Option<&str>) -> u16 {
    let mut r = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("http://{addr}{path}"))
        .body("{}");
    if let Some(a) = auth {
        r = r.bearer_auth(a).header("x-api-key", a);
    }
    r.send().await.unwrap().status().as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_order_opens_admission_last() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    assert_eq!(
        *w.trace.lock().unwrap(),
        ["lock", "recover", "dispatcher", "bind", "ready"]
    );
    assert!(s.is_ready());
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_stop_refuses_start_and_never_opens() {
    let w = world();
    let root = w.config.paths.dispatcher_db.parent().unwrap().to_owned();
    // A bound run whose container never goes away (fake docker, run-77).
    let c = kod_dispatcher::store::open(&w.config.paths.dispatcher_db).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(77,'acme/app#7','v1',?1)",
        [RunState::Running.name()],
    )
    .unwrap();
    let start = kod_dispatcher::RunStart {
        id: "run-77".into(),
        ticket: "acme/app#7".into(),
        revision: "v1".into(),
        repo: "acme/app".into(),
    };
    let b = serde_json::to_value(common::admitted(&root.join("runs"), &start).binding).unwrap();
    let tx = c.unchecked_transaction().unwrap();
    kod_dispatcher::store::bind_once(&tx, "run-77", &b).unwrap();
    tx.commit().unwrap();
    drop(c);
    assert!(Service::start(w.config.clone(), parts(&w)).await.is_err());
    let t = w.trace.lock().unwrap().clone();
    assert!(
        !t.contains(&"bind".to_owned()) && !t.contains(&"ready".to_owned()),
        "{t:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_process_is_refused() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    assert!(Service::start(w.config.clone(), parts(&w)).await.is_err());
    assert!(s.is_ready());
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_are_split_between_listeners() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    for p in ["/r/abc/mcp", "/v1/messages"] {
        assert_eq!(
            status(s.ingress, "POST", p, Some("t")).await,
            404,
            "ingress {p}"
        );
    }
    for p in ["/webhook", "/command"] {
        // The gateway answers a control path exactly like any unknown path (model fallback).
        assert_eq!(
            status(s.gateway, "POST", p, None).await,
            status(s.gateway, "POST", "/nope", None).await,
            "gateway {p}"
        );
        assert_ne!(
            status(s.ingress, "POST", p, None).await,
            status(s.ingress, "POST", "/nope", None).await,
            "ingress {p}"
        );
    }
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn old_token_is_denied_after_restart() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    let token = common::model_token("run-5");
    s.registry
        .register(
            &token,
            kod_llm_proxy::config::RunSpec {
                id: "run-5".into(),
                ticket: "acme/app#7".into(),
                repo: "acme/app".into(),
                data_classes: vec!["placeholder".into()],
                budget: kod_llm_proxy::config::Budget {
                    max_tokens: 10,
                    max_cost_micro_usd: 10,
                },
            },
        )
        .unwrap();
    let live = status(s.gateway, "POST", &format!("/r/{token}/v1/messages"), None).await;
    assert_ne!(live, 401, "registered token is recognised");
    let denied_mcp = status(s.gateway, "POST", &format!("/r/{token}/mcp"), None).await;
    s.shutdown().await.unwrap();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    assert_eq!(
        status(s.gateway, "POST", &format!("/r/{token}/v1/messages"), None).await,
        401
    );
    assert_eq!(
        status(s.gateway, "POST", &format!("/r/{token}/mcp"), None).await,
        denied_mcp
    );
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_kill_works_during_a_source_outage() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    w.base_up.store(false, Ordering::SeqCst);
    let body = serde_json::json!({"by": "kiennt23", "at": kod_host::service::system_now(), "command": "kill"}).to_string();
    let sig = kod_dispatcher::command::sign(OWNER_KEY, body.as_bytes());
    let r = reqwest::Client::new()
        .post(format!("http://{}/command", s.ingress))
        .header("x-kod-signature", sig)
        .body(body)
        .send()
        .await
        .unwrap();
    // The gateway stop may still be pending (it is polled); the kill itself must have landed.
    let ok = r.status().is_success();
    let text = r.text().await.unwrap();
    assert!(
        ok || text.contains("the kill switch is not complete"),
        "{text}"
    );
    assert!(s.dispatcher.is_halted().unwrap());
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_closes_admission_then_cleans_then_exits() {
    let w = world();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (cfg, p) = (w.config.clone(), parts(&w));
    let h = tokio::spawn(kod_host::service::serve(cfg, p, async {
        rx.await.unwrap()
    }));
    while !w.trace.lock().unwrap().contains(&"ready".to_owned()) {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tx.send(()).unwrap();
    h.await.unwrap().unwrap();
    assert_eq!(
        w.trace.lock().unwrap()[5..],
        [
            "close-admission",
            "stop-worker",
            "quiesce",
            "revoke",
            "stop",
            "stop-listeners"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn model_spend_is_durable_across_restart() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    // The sink is installed exactly once, by the host.
    assert!(
        s.registry
            .set_reservation_sink(Arc::new(kod_host::runtime::model_sink::DurableSink(
                w.config.paths.dispatcher_db.clone()
            )))
            .is_err()
    );
    let c = kod_dispatcher::store::open(&w.config.paths.dispatcher_db).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(5,'acme/app#7','v1','running')",
        [],
    )
    .unwrap();
    let start = kod_dispatcher::RunStart {
        id: "run-5".into(),
        ticket: "acme/app#7".into(),
        revision: "v1".into(),
        repo: "acme/app".into(),
    };
    let root = w.config.paths.dispatcher_db.parent().unwrap().to_owned();
    let b = serde_json::to_value(common::admitted(&root.join("runs"), &start).binding).unwrap();
    kod_dispatcher::store::bind_once(&c, "run-5", &b).unwrap();
    drop(c);
    let token = common::model_token("run-5");
    s.registry
        .register(
            &token,
            kod_llm_proxy::config::RunSpec {
                id: "run-5".into(),
                ticket: "acme/app#7".into(),
                repo: "acme/app".into(),
                data_classes: vec!["placeholder".into()],
                budget: kod_llm_proxy::config::Budget {
                    max_tokens: 100000,
                    max_cost_micro_usd: 100000000,
                },
            },
        )
        .unwrap();
    // Reserve through the installed sink via the registry's own path.
    let r = reqwest::Client::new()
        .post(format!("http://{}/r/{token}/v1/messages", s.gateway))
        .header("content-type", "application/json")
        .body(r#"{"model":"x","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    let (st, body) = (r.status().as_u16(), r.text().await.unwrap());
    eprintln!("MODEL {st} {body}");
    assert_ne!(st, 401, "{body}");
    s.shutdown().await.unwrap();
    let c = kod_dispatcher::store::open(&w.config.paths.dispatcher_db).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM model_requests WHERE run_seq=5",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
