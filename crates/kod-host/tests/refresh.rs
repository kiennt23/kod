//! Tracker credentials and the durable status refresh, against a loopback GitHub fake.
mod common;
use axum::{Router, body::Body, extract::State, http::Request, response::Response};
use kod_host::tracker_credentials::TrackerCredentials;
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

/// A throwaway RSA key generated per call; no private key is committed.
fn test_key() -> Vec<u8> {
    let out = std::process::Command::new("openssl")
        .args(["genrsa", "-traditional", "2048"])
        .output()
        .expect("openssl is required to generate the test key");
    assert!(out.status.success());
    out.stdout
}
const BOT: u64 = 4242;

#[derive(Default)]
struct Fake {
    bot_id: u64,
    repo_id: u64,
    returned_repo_id: Option<u64>,
    permissions: Option<Value>,
    tokens: u32,
    ttl: i64,
    now: Arc<AtomicI64>,
    revoke_status: u16,
    comments: Vec<Value>,
    next_comment: u64,
    lose_create: bool,
    create_status: Option<u16>,
    calls: Vec<(String, String, Value, String)>,
}
type Shared = Arc<Mutex<Fake>>;

fn stamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

async fn handle(State(s): State<Shared>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let host = parts.headers["host"].to_str().unwrap().to_owned();
    let auth = parts
        .headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let (method, path) = (
        parts.method.to_string(),
        parts.uri.path().replace("%5B", "[").replace("%5D", "]"),
    );
    let mut f = s.lock().unwrap();
    f.calls
        .push((method.clone(), path.clone(), body.clone(), auth));
    let base = format!("http://{host}");
    let json_ok = |v: Value, code: u16| {
        Response::builder()
            .status(code)
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap()
    };
    let issue_url = format!("{base}/repos/acme/tasks/issues/1");
    let p = path.as_str();
    match (method.as_str(), p) {
        ("GET", "/app") => json_ok(json!({"id": 5, "slug": "kod"}), 200),
        ("GET", "/app/installations/9") => json_ok(json!({"id": 9, "app_id": 5}), 200),
        ("GET", "/users/kod[bot]") => json_ok(
            json!({"id": f.bot_id, "login": "kod[bot]", "type": "Bot"}),
            200,
        ),
        ("POST", "/app/installations/9/access_tokens") => {
            f.tokens += 1;
            let expires = f.now.load(Ordering::SeqCst) + f.ttl;
            let repo = f.returned_repo_id.unwrap_or(f.repo_id);
            let permissions = f
                .permissions
                .clone()
                .unwrap_or(json!({"issues": "write", "metadata": "read"}));
            json_ok(
                json!({"token": format!("tok-{}", f.tokens), "expires_at": stamp(expires),
                    "permissions": permissions,
                    "repositories": [{"id": repo, "full_name": "acme/tasks"}]}),
                201,
            )
        }
        ("DELETE", "/installation/token") => json_ok(Value::Null, f.revoke_status),
        ("GET", "/installation/repositories") => json_ok(
            json!({"total_count": 1, "repositories": [{"id": f.returned_repo_id.unwrap_or(f.repo_id), "full_name": "acme/tasks"}]}),
            200,
        ),
        ("GET", "/repos/acme/tasks/issues/1") => json_ok(
            json!({"id": 1001, "number": 1, "repository_url": format!("{base}/repos/acme/tasks"),
                "title": "Story", "body": "story\n\n<!-- kod:issue\n{\"requirement_id\":\"REQ-1\",\"risk_class\":\"low\",\"acceptance_criteria\":[\"works\"],\"approved_versions\":[\"design\"],\"links\":[]}\n-->", "state": "open",
                "user": {"id": 1, "login": "po", "type": "User"}, "labels": []}),
            200,
        ),
        ("GET", "/repos/acme/tasks/collaborators/po/permission") => json_ok(
            json!({"permission": "write", "user": {"id": 1, "login": "po", "type": "User"}}),
            200,
        ),
        ("GET", "/repos/acme/tasks/issues/1/sub_issues") => json_ok(json!([]), 200),
        ("GET", "/repos/acme/tasks/issues/1/comments") => {
            let mut all = f.comments.clone();
            for c in &mut all {
                c["issue_url"] = issue_url.clone().into();
            }
            json_ok(Value::Array(all), 200)
        }
        ("POST", "/repos/acme/tasks/issues/1/comments") => {
            if let Some(code) = f.create_status {
                return json_ok(Value::Null, code);
            }
            f.next_comment += 1;
            let c = json!({"id": 100 + f.next_comment, "body": body["body"],
                "user": {"id": f.bot_id, "login": "kod[bot]", "type": "Bot"}});
            f.comments.push(c.clone());
            if f.lose_create {
                f.lose_create = false;
                // The comment is committed; the response dies mid-body.
                let stream = futures_util::stream::once(async {
                    Err::<axum::body::Bytes, _>(std::io::Error::other("reset"))
                });
                return Response::builder()
                    .status(201)
                    .header("content-length", "1000")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
            let mut c = c;
            c["issue_url"] = issue_url.into();
            json_ok(c, 201)
        }
        ("PATCH", p) if p.starts_with("/repos/acme/tasks/issues/comments/") => {
            let id: u64 = p.rsplit('/').next().unwrap().parse().unwrap();
            let mut hit = None;
            for c in &mut f.comments {
                if c["id"] == id {
                    c["body"] = body["body"].clone();
                    hit = Some(c.clone());
                }
            }
            match hit {
                Some(mut c) => {
                    c["issue_url"] = issue_url.into();
                    json_ok(c, 200)
                }
                None => json_ok(Value::Null, 404),
            }
        }
        _ => json_ok(Value::Null, 404),
    }
}

struct Github {
    state: Shared,
    api: String,
    _rt: std::thread::JoinHandle<()>,
}
fn github() -> Github {
    let state: Shared = Arc::new(Mutex::new(Fake {
        bot_id: BOT,
        repo_id: 77,
        ttl: 3600,
        revoke_status: 204,
        now: Arc::new(AtomicI64::new(1_800_000_000)),
        ..Fake::default()
    }));
    let app = Router::new().fallback(handle).with_state(state.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let rt = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    let addr = rx.recv().unwrap();
    Github {
        state,
        api: format!("http://{addr}/"),
        _rt: rt,
    }
}
impl Github {
    fn calls(&self) -> Vec<(String, String, Value, String)> {
        self.state.lock().unwrap().calls.clone()
    }
    fn count(&self, method: &str, path: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.0 == method && c.1 == path)
            .count()
    }
    fn clock(&self) -> Arc<AtomicI64> {
        self.state.lock().unwrap().now.clone()
    }
}

struct World {
    dir: tempfile::TempDir,
    config: Arc<kod_host::config::FsConfig>,
}
fn world() -> World {
    let dir = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = dir.path().to_path_buf();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = (*common::config(&root, &root.join("dispatcher.db"))).clone();
    std::fs::write(root.join("key.pem"), test_key()).unwrap();
    std::fs::set_permissions(root.join("key.pem"), std::fs::Permissions::from_mode(0o600)).unwrap();
    config.paths.credential_dir = root.join("cred");
    config.paths.policy_dir = root.join("policy");
    config.paths.approval_manifest = root.join("snap/manifest.json");
    config.github.app.app_id = 5;
    config.github.tracker.repo = "acme/tasks".into();
    config.github.tracker.comment_author_id = BOT;
    config.github.tracker_repository_id = 77;
    config.github.tracker_installation_id = 9;
    World {
        dir,
        config: Arc::new(config),
    }
}
fn creds(w: &World, g: &Github) -> Arc<TrackerCredentials> {
    let clock = g.clock();
    TrackerCredentials::with_api(
        &w.config,
        &g.api,
        Arc::new(move || clock.load(Ordering::SeqCst)),
    )
    .unwrap()
}
fn tracker_dir(w: &World) -> PathBuf {
    w.config.paths.credential_dir.join("tracker")
}
fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn tracker_token_is_scoped_to_the_tracker_repository_with_issues_write() {
    let (w, g) = (world(), github());
    let c = creds(&w, &g);
    c.start().unwrap();
    let mint: Vec<_> = g
        .calls()
        .into_iter()
        .filter(|c| c.1 == "/app/installations/9/access_tokens")
        .collect();
    assert_eq!(mint.len(), 1);
    assert_eq!(
        mint[0].2,
        json!({"repository_ids": [77], "permissions": {"issues": "write"}})
    );
    assert!(mint[0].3.starts_with("Bearer ey"), "App JWT, not a token");
    assert_eq!(mode(&tracker_dir(&w)), 0o700);
    assert!(c.token().unwrap() == "tok-1");
    assert!(!w.config.paths.credential_dir.join("publish").exists());
}

#[test]
fn tracker_token_renews_before_expiry_without_replacing_the_instance() {
    let (w, g) = (world(), github());
    let c = creds(&w, &g);
    c.start().unwrap();
    assert_eq!(c.token().unwrap(), "tok-1");
    g.clock().fetch_add(3600 - 120, Ordering::SeqCst);
    assert_eq!(
        c.token().unwrap(),
        "tok-2",
        "renewed inside the 5 minute margin"
    );
    assert_eq!(
        g.count("DELETE", "/installation/token"),
        1,
        "old token revoked"
    );
}

#[test]
fn tracker_credentials_refuse_the_wrong_bot_or_repository() {
    for case in 0..3 {
        let (w, g) = (world(), github());
        {
            let mut f = g.state.lock().unwrap();
            match case {
                0 => f.bot_id = BOT + 1,
                1 => f.returned_repo_id = Some(78),
                _ => {
                    f.permissions =
                        Some(json!({"issues": "write", "contents": "write", "metadata": "read"}))
                }
            }
        }
        let c = creds(&w, &g);
        assert!(c.start().is_err(), "case {case}");
        assert!(c.token().is_err(), "case {case}");
    }
}

#[test]
fn startup_revokes_a_stale_lease_and_shutdown_revokes_the_current_one() {
    let (w, g) = (world(), github());
    let first = creds(&w, &g);
    first.start().unwrap();
    let lease = tracker_dir(&w).join("lease.json");
    assert_eq!(mode(&lease), 0o600);
    drop(first); // crash: the lease stays on disk
    let second = creds(&w, &g);
    second.start().unwrap();
    assert_eq!(
        g.count("DELETE", "/installation/token"),
        1,
        "stale lease revoked at startup"
    );
    second.shutdown().unwrap();
    assert_eq!(g.count("DELETE", "/installation/token"), 2);
    assert!(!lease.exists());
    // A revoke failure retains the lease for the next startup.
    let third = creds(&w, &g);
    third.start().unwrap();
    g.state.lock().unwrap().revoke_status = 500;
    assert!(third.shutdown().is_err());
    assert!(lease.exists());
}

// ---- durable status refresh ----

use kod_host::refresh::{Timed, refresh_story, rework_records, run_loop};
use kod_status::records::{Records, Snapshot, Source};

const STORY: &str = "acme/tasks#1";

struct Rig {
    w: World,
    g: Github,
    creds: Arc<TrackerCredentials>,
}
fn rig() -> Rig {
    let (w, g) = (world(), github());
    let creds = creds(&w, &g);
    creds.start().unwrap();
    Rig { w, g, creds }
}
fn tracker(r: &Rig) -> kod_tracker::github::GitHub {
    let mapping = kod_tracker::mapping::Mapping::parse(
        "m",
        include_str!("../../../registry/trackers/github.json"),
    )
    .unwrap();
    kod_tracker::github::GitHub::with_test_api_source(
        r.w.config.github.tracker.clone(),
        mapping,
        r.creds.token_source(),
        "secret",
        &r.g.api,
    )
    .unwrap()
}
fn db(r: &Rig) -> rusqlite::Connection {
    kod_dispatcher::store::open(&r.w.config.paths.audit_db).unwrap()
}

/// Records from the tracker's story plus the dispatcher's rework rows, like the host source.
struct Rows<'a> {
    tracker: &'a kod_tracker::github::GitHub,
    db: PathBuf,
}
impl Source for Rows<'_> {
    fn load(&self, story: &str) -> anyhow::Result<Records> {
        use kod_tracker::Tracker;
        let c = kod_dispatcher::store::open(&self.db)?;
        Ok(Records {
            snapshot: Snapshot {
                story: self.tracker.get_issue(story)?,
                tickets: vec![],
                artifacts: vec![],
                pulls: vec![],
                rework: rework_records(&c, story)?,
            },
            runs: vec![],
        })
    }
}
fn owned(body: &str) -> Value {
    json!({"id": 900, "body": body, "user": {"id": BOT, "login": "kod[bot]", "type": "Bot"}})
}
fn go(r: &Rig, t: &kod_tracker::github::GitHub) -> anyhow::Result<()> {
    let audit = kod_audit::AuditLog::open(&r.w.dir.path().join("audit.log")).unwrap();
    let src = Rows {
        tracker: t,
        db: r.w.config.paths.audit_db.clone(),
    };
    refresh_story(&r.w.config, t, &src, &audit, &mut db(r), STORY)
}
fn posts(r: &Rig) -> usize {
    r.g.count("POST", "/repos/acme/tasks/issues/1/comments")
}
fn human(r: &Rig) {
    let mut f = r.g.state.lock().unwrap();
    f.comments.push(
        json!({"id": 5, "body": "human note", "user": {"id": 1, "login": "po", "type": "User"}}),
    );
}

#[test]
fn lost_status_response_after_restart_posts_once() {
    let r = rig();
    human(&r);
    r.g.state.lock().unwrap().lose_create = true;
    assert!(go(&r, &tracker(&r)).is_err(), "the response was lost");
    assert_eq!(posts(&r), 1);
    // Restart: a fresh tracker instance and connection; the token rotates meanwhile.
    r.g.clock().fetch_add(3600, Ordering::SeqCst);
    let restarted = tracker(&r);
    go(&r, &restarted).unwrap();
    go(&r, &restarted).unwrap();
    assert_eq!(posts(&r), 1, "exactly one POST across both refreshes");
    let f = r.g.state.lock().unwrap();
    assert!(
        f.comments.iter().any(|c| c["body"] == "human note"),
        "human text preserved"
    );
    drop(f);
    let outcome: String = db(&r)
        .query_row("SELECT outcome FROM tracker_write_intents WHERE kind='status' AND outcome='reconciled'", [], |x| x.get(0))
        .unwrap();
    assert_eq!(outcome, "reconciled");
    assert!(r.g.count("PATCH", "/repos/acme/tasks/issues/comments/101") >= 1);
}

#[test]
fn an_absent_or_duplicate_owned_comment_blocks_a_create() {
    let r = rig();
    r.g.state.lock().unwrap().lose_create = true;
    assert!(go(&r, &tracker(&r)).is_err());
    // The comment is not visible (absent): stay blocked, no second create.
    let kept = std::mem::take(&mut r.g.state.lock().unwrap().comments);
    assert!(go(&r, &tracker(&r)).is_err());
    assert_eq!(posts(&r), 1);
    // Two owned comments: still blocked, nothing written.
    {
        let mut f = r.g.state.lock().unwrap();
        f.comments = kept.clone();
        f.comments.push(owned(kept[0]["body"].as_str().unwrap()));
    }
    assert!(go(&r, &tracker(&r)).is_err());
    assert_eq!(posts(&r), 1);
    assert_eq!(
        r.g.count("PATCH", "/repos/acme/tasks/issues/comments/101"),
        0
    );
}

#[test]
fn a_definite_rejection_clears_uncertainty() {
    let r = rig();
    r.g.state.lock().unwrap().create_status = Some(422);
    assert!(go(&r, &tracker(&r)).is_err());
    let n: i64 = db(&r)
        .query_row(
            "SELECT count(*) FROM tracker_write_intents WHERE outcome='not_applied'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    r.g.state.lock().unwrap().create_status = None;
    go(&r, &tracker(&r)).unwrap();
    assert_eq!(posts(&r), 2, "a definite 4xx allows the retry");
}

#[test]
fn an_unavailable_rework_source_fails_the_refresh_without_writing() {
    let r = rig();
    db(&r)
        .execute(
            "INSERT INTO tracker_write_intents(operation_id,story,kind,audit_intent) VALUES('rework:x',?1,'rework',1)",
            [STORY],
        )
        .unwrap();
    assert!(
        go(&r, &tracker(&r)).is_err(),
        "an unresolved rework create has no record to show"
    );
    assert_eq!(posts(&r), 0);
    // No dispatcher database at all is also a failure, never an empty list.
    assert!(rework_records(&rusqlite::Connection::open_in_memory().unwrap(), STORY).is_err());
}

#[test]
fn a_slow_source_times_out_without_a_stale_fallback() {
    struct Slow;
    impl Source for Slow {
        fn load(&self, _: &str) -> anyhow::Result<Records> {
            std::thread::sleep(std::time::Duration::from_millis(600));
            anyhow::bail!("late")
        }
    }
    let r = rig();
    let t = tracker(&r);
    let audit = kod_audit::AuditLog::open(&r.w.dir.path().join("audit.log")).unwrap();
    let slow = Timed::new(Arc::new(Slow), std::time::Duration::from_millis(50));
    let started = std::time::Instant::now();
    let e = refresh_story(&r.w.config, &t, &slow, &audit, &mut db(&r), STORY).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_millis(400));
    assert!(format!("{e:#}").contains("timed out"));
    assert_eq!(posts(&r), 0);
}

#[test]
fn refresh_waits_for_the_gates_lock_that_activation_holds() {
    let r = rig();
    let t = tracker(&r);
    let p = kod_host::records::gates_lock_path(&r.w.config);
    let held = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&p)
        .unwrap();
    held.lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        drop(held);
    });
    let started = std::time::Instant::now();
    go(&r, &t).unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(300));
    release.join().unwrap();
}

#[test]
fn the_loop_recomputes_at_startup_then_every_interval() {
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (n2, stop2) = (n.clone(), stop.clone());
    let h = std::thread::spawn(move || {
        run_loop(
            || vec!["S-1".to_string()],
            |_| {
                n2.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("a failing story never stops the loop"))
            },
            std::time::Duration::from_millis(40),
            &stop2,
        )
    });
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(n.load(Ordering::SeqCst), 1, "immediately at startup");
    std::thread::sleep(std::time::Duration::from_millis(150));
    stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
    assert!(n.load(Ordering::SeqCst) >= 3);
}

#[test]
fn serve_with_an_invalid_config_fails_closed() {
    let d = tempfile::tempdir().unwrap();
    let bad = d.path().join("bad.json");
    std::fs::write(&bad, "{}").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kod-host"))
        .args(["serve", "--config"])
        .arg(&bad)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// ---- production workflow source and the serve loop ----

fn workflow(r: &Rig, open: Vec<&str>) -> kod_host::refresh::WorkflowSource {
    workflow_class(r, open, "design")
}
fn workflow_class(r: &Rig, open: Vec<&str>, class: &str) -> kod_host::refresh::WorkflowSource {
    let c = &r.w.config;
    drop(kod_dispatcher::store::open(&c.paths.dispatcher_db).unwrap());
    for d in [
        &c.paths.policy_dir,
        c.paths.approval_manifest.parent().unwrap(),
    ] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (n, v) in [
        ("people.json", json!({"owner": "po", "backup": "bo"})),
        ("routing.json", json!({"classes": {class: {}}})),
    ] {
        std::fs::write(c.paths.policy_dir.join(n), v.to_string()).unwrap();
        std::fs::set_permissions(
            c.paths.policy_dir.join(n),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let m = &c.paths.approval_manifest;
    let art = m.parent().unwrap().join("v1.txt");
    std::fs::write(&art, "approved design").unwrap();
    std::fs::set_permissions(&art, std::fs::Permissions::from_mode(0o600)).unwrap();
    let entry = json!({"link": "design", "revision": "v1", "path": "v1.txt", "class": class,
        "sha256": format!("{:x}", sha2::Sha256::digest("approved design")),
        "approved_by": "po", "approved_at": "2026-10-24T00:00:00Z", "approval_ref": "S-1"});
    std::fs::write(m, json!({"version": 1, "artifacts": [entry]}).to_string()).unwrap();
    std::fs::set_permissions(m, std::fs::Permissions::from_mode(0o600)).unwrap();
    let scratch = r.w.dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let open: Vec<String> = open.into_iter().map(String::from).collect();
    kod_host::refresh::WorkflowSource {
        tracker: Arc::new(tracker(r)),
        artifacts: Arc::new(
            kod_host::artifacts::Artifacts::new(
                c.paths.approval_manifest.clone(),
                &c.paths.policy_dir,
            )
            .unwrap(),
        ),
        dispatcher_db: c.paths.dispatcher_db.clone(),
        scratch,
        open: Arc::new(move || Ok(open.clone())),
    }
}
fn go_with(r: &Rig, src: &dyn Source) -> anyhow::Result<()> {
    let audit = kod_audit::AuditLog::open(&r.w.dir.path().join("audit.log")).unwrap();
    let mut c = kod_dispatcher::store::open(&r.w.config.paths.dispatcher_db).unwrap();
    refresh_story(&r.w.config, &tracker(r), src, &audit, &mut c, STORY)
}

#[test]
fn an_open_agent_pull_request_makes_the_production_source_fail_closed() {
    let r = rig();
    let src = workflow(&r, vec!["agent/acme/tasks#1-run-3"]);
    let e = go_with(&r, &src).unwrap_err();
    assert!(
        format!("{e:#}").contains("evidence is unavailable"),
        "{e:#}"
    );
    assert_eq!(posts(&r), 0);
}

#[test]
fn the_production_source_publishes_status_when_no_pull_is_open() {
    let r = rig();
    go_with(&r, &workflow(&r, vec!["agent/acme/tasks#2-run-3"])).unwrap();
    assert_eq!(posts(&r), 1);
}

#[test]
fn stories_come_from_dispatcher_runs() {
    let r = rig();
    let c = kod_dispatcher::store::open(&r.w.config.paths.dispatcher_db).unwrap();
    assert!(kod_host::refresh::stories(&c).unwrap().is_empty());
}

#[test]
fn stopping_the_refresher_does_not_wait_for_a_stuck_status_call() {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let e2 = entered.clone();
    let rf = kod_host::refresh::Refresher::spawn(
        || vec!["S".into()],
        move |_| {
            e2.store(true, Ordering::SeqCst);
            let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
            Ok(())
        },
        std::time::Duration::from_millis(10),
    );
    while !entered.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let t = std::time::Instant::now();
    rf.stop();
    assert!(t.elapsed() < std::time::Duration::from_millis(100));
    drop(tx);
}

#[test]
fn a_story_with_a_published_run_fails_closed_without_pull_evidence() {
    for state in ["publishing", "done", "needs_human"] {
        let r = rig();
        let src = workflow(&r, vec![]);
        kod_dispatcher::store::open(&r.w.config.paths.dispatcher_db)
            .unwrap()
            .execute(
                "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,?1,'v1',?2)",
                rusqlite::params![STORY, state],
            )
            .unwrap();
        if state == "needs_human" {
            // Entered publishing, then escalated: only the check_publish followup remembers it.
            kod_dispatcher::store::open(&r.w.config.paths.dispatcher_db)
                .unwrap()
                .execute(
                    "INSERT INTO followups(kind,target,arg,next_at,done) VALUES('check_publish','run-1','',0,1)",
                    [],
                )
                .unwrap();
        }
        let e = go_with(&r, &src).unwrap_err();
        assert!(
            format!("{e:#}").contains("evidence is unavailable"),
            "{e:#}"
        );
        assert_eq!(posts(&r), 0, "{state}");
    }
}

#[test]
fn a_pulls_source_error_fails_the_refresh_without_writing() {
    let r = rig();
    let mut src = workflow(&r, vec![]);
    src.open = Arc::new(|| anyhow::bail!("github unavailable"));
    assert!(go_with(&r, &src).is_err());
    assert_eq!(posts(&r), 0);
}

#[test]
fn a_blocked_story_is_logged_and_audited_only_when_the_state_changes() {
    let r = rig();
    let path = r.w.dir.path().join("blocked-audit.log");
    let audit = kod_audit::AuditLog::open(&path).unwrap();
    let count = || -> i64 {
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM audit WHERE record_type=?1 AND payload LIKE '%status_refresh_blocked%'",
                [kod_audit::RUN_AUDIT],
                |row| row.get(0),
            )
            .unwrap()
    };
    let log = kod_host::refresh::BlockedLog::default();
    let e1 = anyhow::anyhow!("rework create has no record");
    log.record(&audit, STORY, &e1);
    log.record(&audit, STORY, &e1);
    assert_eq!(count(), 1, "the same blocked state is audited once");
    log.record(&audit, STORY, &anyhow::anyhow!("another reason"));
    assert_eq!(count(), 2);
    log.clear(STORY);
    log.record(&audit, STORY, &e1);
    assert_eq!(count(), 3, "blocked again after recovering");
}

#[test]
fn another_storys_publish_attempt_does_not_block_this_story() {
    let r = rig();
    let src = workflow(&r, vec![]);
    let c = kod_dispatcher::store::open(&r.w.config.paths.dispatcher_db).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/tasks#9','v1','needs_human')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO followups(kind,target,arg,next_at,done) VALUES('check_publish','run-1','',0,1)",
        [],
    )
    .unwrap();
    drop(c);
    go_with(&r, &src).unwrap();
    assert_eq!(posts(&r), 1);
}

#[test]
fn a_data_classification_is_not_parsed_as_a_workflow_stage() {
    let r = rig();
    let src = workflow_class(&r, vec![], "placeholder");
    go_with(&r, &src).unwrap();
    assert_eq!(posts(&r), 1);
}
