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
    let policy = root.join("policy");
    assert!(
        std::process::Command::new("cp")
            .arg("-R")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"))
            .arg(&policy)
            .status()
            .unwrap()
            .success()
    );
    c.paths.policy_dir = policy.canonicalize().unwrap();
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
    let mcp = common::mcp_token("run-5");
    s.runtime
        .tool()
        .start_run(
            &mcp,
            kod_tool_gateway::Run {
                id: "run-5".into(),
                ticket: "acme/app#7".into(),
                repo: "acme/app".into(),
                base: "c".repeat(40),
                data_classes: vec!["placeholder".into()],
                pins: Default::default(),
                linked_tickets: vec![],
                started_at: 1,
            },
        )
        .unwrap();
    let live = status(s.gateway, "POST", &format!("/r/{token}/v1/messages"), None).await;
    assert_ne!(live, 401, "registered token is recognised");
    let live_mcp = status(s.gateway, "POST", &format!("/r/{mcp}/mcp"), None).await;
    assert_ne!(
        live_mcp, 401,
        "the registered token is recognised on MCP too"
    );
    s.shutdown().await.unwrap();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    assert_eq!(
        status(s.gateway, "POST", &format!("/r/{token}/v1/messages"), None).await,
        401
    );
    let denied_mcp = status(s.gateway, "POST", &format!("/r/{mcp}/mcp"), None).await;
    assert!(
        [401, 404].contains(&denied_mcp) && denied_mcp != live_mcp,
        "live {live_mcp}, after restart {denied_mcp}"
    );
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_kill_remains_live_during_dependency_outage() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    // Every dependency a kill could consult goes away: mirror source, policy files, and the
    // audit store (swapped, so the open handle's identity check fails closed).
    w.base_up.store(false, Ordering::SeqCst);
    fs::remove_dir_all(&w.config.paths.policy_dir).unwrap();
    let audit = &w.config.paths.audit_db;
    fs::rename(audit, audit.with_extension("old")).unwrap();
    put(audit, "");
    fs::set_permissions(audit, fs::Permissions::from_mode(0o600)).unwrap();
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

/// A config that passes full local validation (paths, secrets, images, model, networking).
fn bootable(w: &World) -> kod_host::config::FsConfig {
    let root = w.config.paths.dispatcher_db.parent().unwrap().to_owned();
    dir(&root.join("runs"));
    dir(&root.join("mirror"));
    put(&root.join("webhook"), "whsec");
    for db in [&w.config.paths.dispatcher_db, &root.join("audit.db")] {
        if db.exists() {
            fs::set_permissions(db, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let mut c = (*w.config).clone();
    c.paths.run_root = root.join("runs");
    c.paths.mirror = root.join("mirror");
    c.paths.audit_db = root.join("audit.db");
    c.github.mapping = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/trackers/github.json")
        .canonicalize()
        .unwrap();
    c.github.webhook_secret_file = root.join("webhook");
    c.networking.ingress_listen = "127.0.0.1:3000".parse().unwrap();
    c.networking.gateway_listen = "127.0.0.1:3001".parse().unwrap();
    c.networking.accepted_hosts = vec!["localhost".into()];
    c.networking.sandbox_network = "n".into();
    c.networking.sandbox_gateway_url = "http://h:1".into();
    c.model.upstream = "https://api.example.invalid".into();
    c.model.prices = root.join("prices.json");
    c.model.max_tokens = 100;
    c.model.max_body = 4096;
    c.model.max_requests = 10;
    c.model.max_concurrent = 2;
    c.model.run_max_tokens = 100;
    c.model.run_max_cost_micro_usd = 100;
    c.sandbox.agent_image = format!("a@sha256:{}", "a".repeat(64));
    c.sandbox.collector_image = format!("c@sha256:{}", "c".repeat(64));
    c.sandbox.memory = "8g".into();
    c.sandbox.disk = "1g".into();
    c
}

fn seed_bound_run(w: &World, seq: i64) -> String {
    let id = format!("run-{seq}");
    let c = kod_dispatcher::store::open(&w.config.paths.dispatcher_db).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(?1,'acme/app#7','v1','running')",
        [seq],
    )
    .unwrap();
    let start = kod_dispatcher::RunStart {
        id: id.clone(),
        ticket: "acme/app#7".into(),
        revision: "v1".into(),
        repo: "acme/app".into(),
    };
    let root = w.config.paths.dispatcher_db.parent().unwrap().to_owned();
    let b = serde_json::to_value(common::admitted(&root.join("runs"), &start).binding).unwrap();
    kod_dispatcher::store::bind_once(&c, &id, &b).unwrap();
    id
}

#[test]
fn a_second_serve_is_refused_before_it_starts_tracker_credentials() {
    let w = world();
    let root = w.config.paths.dispatcher_db.parent().unwrap().to_owned();
    let _first = kod_host::service::instance_lock(&root).unwrap();
    let started = AtomicBool::new(false);
    let r = kod_host::service::prepare(bootable(&w), |_| {
        started.store(true, Ordering::SeqCst);
        Ok(())
    });
    assert!(r.is_err());
    assert!(
        !started.load(Ordering::SeqCst),
        "creds.start ran before the lock"
    );
}

#[test]
fn a_config_that_parses_but_fails_validation_still_cleans_up_durable_runs() {
    let w = world();
    let id = seed_bound_run(&w, 88);
    let mut cfg = bootable(&w);
    cfg.sandbox.agent_image = "not-a-digest".into();
    assert!(cfg.validate_paths().is_ok() && cfg.validate_local().is_err());
    let started = AtomicBool::new(false);
    let r = kod_host::service::prepare(cfg, |_| {
        started.store(true, Ordering::SeqCst);
        Ok(())
    });
    assert!(r.is_err());
    assert!(!started.load(Ordering::SeqCst));
    let calls = common::docker_calls();
    assert!(
        calls
            .lines()
            .any(|l| l.contains(&format!("kod-agent-{id}"))),
        "no cleanup attempt for {id}: {calls}"
    );
}

#[test]
fn a_tracker_outage_at_boot_still_cleans_up_durable_runs() {
    let w = world();
    let id = seed_bound_run(&w, 89);
    let cfg = bootable(&w);
    cfg.validate_local().unwrap();
    let r = kod_host::service::prepare(cfg, |_| -> anyhow::Result<()> {
        anyhow::bail!("tracker unavailable")
    });
    let e = format!("{:#}", r.err().expect("must refuse"));
    assert!(e.contains("tracker unavailable"), "{e}");
    let calls = common::docker_calls();
    assert!(
        calls
            .lines()
            .any(|l| l.contains(&format!("kod-agent-{id}"))),
        "{calls}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_reports_failed_stop_and_retains_followup() {
    let w = world();
    let s = Service::start(w.config.clone(), parts(&w)).await.unwrap();
    // A bound run whose container never goes away (fake docker, run-77).
    seed_bound_run(&w, 77);
    let err = s.shutdown().await.unwrap_err();
    assert!(format!("{err:#}").contains("cleanup pending"), "{err:#}");
    // The durable identity is retained, so the next start's recovery still owns the stop.
    let c = kod_dispatcher::store::open(&w.config.paths.dispatcher_db).unwrap();
    let bound: i64 = c
        .query_row(
            "SELECT count(*) FROM run_bindings WHERE run_seq=77",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bound, 1);
    assert!(Service::start(w.config.clone(), parts(&w)).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_failure_before_recovery_still_cleans_up_durable_runs() {
    let w = world();
    let id = seed_bound_run(&w, 90);
    w.base_up.store(false, Ordering::SeqCst);
    let r = Service::start(w.config.clone(), parts(&w)).await;
    assert!(r.is_err(), "start must fail while the mirror is down");
    let calls = common::docker_calls();
    assert!(
        calls
            .lines()
            .any(|l| l.contains(&format!("kod-agent-{id}"))),
        "{calls}"
    );
}
