//! Publishing through the production Execute adapter against a recording loopback GitHub:
//! nothing untrusted reaches a Forge write, and a restart mid-publish adopts or escalates, never reruns.
mod common;
use axum::{Router, body::Body, extract::State, http::Request, response::Response};
use common::{Fixture, Setup};
use kod_dispatcher::{Gateway, RunState};
use kod_host::production;
use kod_tool_gateway::forge::Workspace;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const EXPECTED: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const PATCH_OK: &str = "touch collect_ok; printf 'diff --git a/a b/a\\nindex aaa..bbb 100644\\n--- a/a\\n+++ b/a\\n@@ -1 +1 @@\\n-a\\n+b\\n' > diff.patch";
const EXPORT_OK: &str = r#"touch export_ok; printf '{"version":1,"base":"%s","files":[{"path":"a","mode":"100644","blob":0}]}' "$BASE" > manifest.json; printf 'b\n' > 0"#;

#[derive(Default)]
struct Fake {
    calls: Vec<String>,
    prs: Vec<Value>,
    hold: bool,
    entered: bool,
}
type Shared = Arc<Mutex<Fake>>;

async fn handle(State(s): State<Shared>, req: Request<Body>) -> Response {
    let key = format!("{} {}", req.method(), req.uri().path());
    let (hold, reply) = {
        let mut f = s.lock().unwrap();
        f.calls.push(key.clone());
        let mint = key.ends_with("/access_tokens");
        f.entered |= mint;
        let exp = chrono::DateTime::from_timestamp(1_800_003_600, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let reply = match key.as_str() {
            _ if mint => (
                201,
                json!({"token": "ghs_x", "expires_at": exp,
                "permissions": {"contents": "write", "pull_requests": "write", "metadata": "read"},
                "repositories": [{"id": 1, "full_name": "acme/app"}]}),
            ),
            "GET /repos/acme/app" => (
                200,
                json!({"id": 1, "full_name": "acme/app", "default_branch": "main"}),
            ),
            "GET /repos/acme/app/rulesets/1" => (
                200,
                json!({"id": 1, "target": "branch", "enforcement": "active",
                "bypass_actors": [], "conditions": {"ref_name": {"include": ["~ALL"],
                "exclude": ["refs/heads/agent/*", "refs/heads/agent/**/*"]}},
                "rules": [{"type": "creation"}, {"type": "update"}, {"type": "deletion"}]}),
            ),
            "GET /repos/acme/app/pulls" => (200, Value::Array(f.prs.clone())),
            "DELETE /installation/token" => (204, Value::Null),
            _ => (404, json!({})),
        };
        (mint && f.hold, reply)
    };
    while hold && s.lock().unwrap().hold {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let body = if reply.0 == 204 {
        Body::empty()
    } else {
        Body::from(reply.1.to_string())
    };
    Response::builder()
        .status(reply.0)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

fn github() -> (Shared, String) {
    let state: Shared = Shared::default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let api = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().fallback(handle).with_state(state.clone());
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let l = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(l, app).await.unwrap();
        })
    });
    (state, api)
}

static NEXT: AtomicI64 = AtomicI64::new(100);

struct World {
    f: Fixture,
    fake: Shared,
    seq: i64,
}
impl World {
    fn run(&self) -> String {
        format!("run-{}", self.seq)
    }
    fn state(&self) -> (String, String) {
        rusqlite::Connection::open(&self.f.db)
            .unwrap()
            .query_row(
                "SELECT state,reason FROM runs WHERE seq=?1",
                [self.seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
    fn wait(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let (s, _) = self.state();
            if s != "running" && s != "publishing" {
                return s;
            }
            assert!(Instant::now() < deadline, "run never settled");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn calls(&self) -> Vec<String> {
        self.fake.lock().unwrap().calls.clone()
    }
}

fn setup_dir() -> (tempfile::TempDir, Arc<kod_host::config::FsConfig>) {
    let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = dir.path().to_path_buf();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let mut c = (*common::config(&root, &root.join("dispatcher.db"))).clone();
    c.paths.mirror = root.join("mirror");
    c.sandbox.disk = "128m".into();
    c.sandbox.wall_seconds = 30;
    (dir, Arc::new(c))
}

fn mirror(root: &Path) -> String {
    let m = root.join("mirror");
    fs::create_dir(&m).unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "-A"],
        &["commit", "-qm", "base"],
    ] {
        if args[0] == "add" {
            fs::write(m.join("a"), "a\n").unwrap();
        }
        let ok = Command::new("git")
            .arg("-C")
            .arg(&m)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok);
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(&m)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A host over the production Execute adapter whose collector/exporter run the given shell.
fn production_world(collect: &str, export: &str) -> World {
    let (dir, cfg) = setup_dir();
    let seq = NEXT.fetch_add(1, Ordering::SeqCst);
    let root = dir.path().canonicalize().unwrap();
    let base = mirror(&root);
    let rundir = root.join("runs").join(format!("run-{seq}"));
    fs::create_dir_all(&rundir).unwrap();
    common::docker_case(
        &format!("run-{seq}"),
        &format!(
            r#"cd '{dir}'
BASE='{base}'
case $1 in
agent) printf 0 > agent_out/exit_code; printf 0 > agent_out/agent_procs; echo '{{"end_state":"done"}}' > agent_out/agent.json;;
collect) cd collect_out; {collect};;
export) cd export_out; {export};;
esac
"#,
            dir = rundir.display()
        ),
    );
    let (fake, api) = github();
    let f = Fixture::start_with(
        dir,
        production::execute(cfg.clone()),
        Setup {
            config: Some(cfg),
            api: Some(api),
            base: Some(base),
        },
    );
    f.insert_run(seq, RunState::Running);
    World { f, fake, seq }
}

#[test]
fn nothing_untrusted_reaches_a_forge_write_or_done() {
    let rename = "touch collect_ok; printf 'diff --git a/a b/b\\nrename from a\\nrename to b\\n' > diff.patch";
    for (collect, export) in [
        ("printf x > diff.patch", EXPORT_OK), // missing marker
        (
            "ln -s /etc/passwd collect_ok; printf '' > diff.patch",
            EXPORT_OK,
        ), // forged marker
        ("exit 3", EXPORT_OK),                // collector exit
        (rename, EXPORT_OK),                  // unsupported patch
        (PATCH_OK, "true"),                   // missing export marker
        (
            PATCH_OK,
            "ln -s /etc/passwd export_ok; cp /dev/null manifest.json",
        ), // forged export marker
        (
            PATCH_OK,
            r#"touch export_ok; printf '{"version":1,"base":"wrong","files":[]}' > manifest.json"#,
        ), // wrong base
        (PATCH_OK, "exit 3"),
    ] {
        let w = production_world(collect, export);
        w.f.runtime.start(&w.f.run_start(&w.run())).unwrap();
        assert_eq!(w.wait(), "needs_human", "{collect} / {export}");
        assert!(
            w.f.workspace.collected(&w.run()).is_err(),
            "{collect} / {export}"
        );
        let posts: Vec<_> = w
            .calls()
            .into_iter()
            .filter(|c| !c.starts_with("GET "))
            .collect();
        assert!(
            posts.is_empty() && w.calls().is_empty(),
            "{collect} / {export}: {:?}",
            w.calls()
        );
    }
}

#[test]
fn a_valid_collection_does_reach_the_forge() {
    // Positive control: proves the recorder would have seen the requests the cases above never made.
    let w = production_world(PATCH_OK, EXPORT_OK);
    w.f.runtime.start(&w.f.run_start(&w.run())).unwrap();
    w.wait();
    assert!(
        w.calls()
            .iter()
            .any(|c| c.starts_with("POST ") && c.ends_with("/access_tokens")),
        "{:?}",
        w.calls()
    );
}

fn seed(dir: &Path, seq: i64) {
    let root = dir.canonicalize().unwrap();
    let run = format!("run-{seq}");
    let c = kod_dispatcher::store::open(&root.join("dispatcher.db")).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(?1,'acme/app#7','v1','running')",
        [seq],
    )
    .unwrap();
    let start = kod_dispatcher::RunStart {
        id: run.clone(),
        ticket: "acme/app#7".into(),
        revision: "v1".into(),
        repo: "acme/app".into(),
    };
    let b = common::admitted(&root.join("runs"), &start).binding;
    kod_dispatcher::store::bind_once(&c, &run, &serde_json::to_value(&b).unwrap()).unwrap();
    c.execute("INSERT INTO model_requests(run_seq,request_id,reserved_tokens,reserved_cost) VALUES(?1,'r1','10','20')", [seq]).unwrap();
    c.execute("UPDATE runs SET state='publishing' WHERE seq=?1", [seq])
        .unwrap();
    let leases = root.join("leases");
    fs::create_dir_all(&leases).unwrap();
    fs::set_permissions(&leases, fs::Permissions::from_mode(0o700)).unwrap();
    let file = leases.join(format!("{:x}.json", Sha256::digest(run.as_bytes())));
    fs::write(
        &file,
        json!({"version": 1, "generation": 0, "permit": b.permit(), "tokens": [], "pending": null,
        "expected": EXPECTED, "input": null, "pr": null, "updated_at": 1_800_000_000})
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
}

fn spend(db: &Path) -> Vec<(String, String, Option<String>)> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut s = c
        .prepare("SELECT reserved_tokens,reserved_cost,settled_usage FROM model_requests")
        .unwrap();
    s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn restart_publishing(pr: bool) -> (World, Arc<AtomicUsize>) {
    let (dir, cfg) = setup_dir();
    let seq = NEXT.fetch_add(1, Ordering::SeqCst);
    seed(dir.path(), seq);
    let (fake, api) = github();
    if pr {
        let branch = format!("agent/acme/app#7-run-{seq}");
        fake.lock().unwrap().prs.push(json!({"number": 1, "html_url": "https://github.com/acme/app/pull/1",
            "state": "open", "draft": true, "user": {"id": 1}, "head": {"repo": {"id": 1}, "ref": branch, "sha": EXPECTED},
            "base": {"repo": {"id": 1}, "ref": "main"}}));
    }
    let reruns = Arc::new(AtomicUsize::new(0));
    let r = reruns.clone();
    let execute: kod_host::runtime::Execute = Arc::new(move |_, _, _, _| {
        r.fetch_add(1, Ordering::SeqCst);
        Ok(RunState::Done)
    });
    let f = Fixture::start_with(
        dir,
        execute,
        Setup {
            config: Some(cfg),
            api: Some(api),
            base: None,
        },
    );
    (World { f, fake, seq }, reruns)
}

#[test]
fn startup_adopts_the_exact_pr_without_a_rerun_or_spend_reset() {
    let (w, reruns) = restart_publishing(true);
    assert_eq!(w.state().0, "done", "{:?}", w.state());
    assert_eq!(reruns.load(Ordering::SeqCst), 0);
    assert_eq!(spend(&w.f.db), vec![("10".into(), "20".into(), None)]);
    assert!(
        w.calls()
            .iter()
            .all(|c| !c.starts_with("POST ") || c.ends_with("/access_tokens")),
        "{:?}",
        w.calls()
    );
}

#[test]
fn startup_without_a_pr_escalates_without_a_rerun_or_spend_reset() {
    let (w, reruns) = restart_publishing(false);
    let (state, why) = w.state();
    assert_eq!(state, "needs_human", "{why}");
    assert!(why.contains("no draft PR"), "{why}");
    assert_eq!(reruns.load(Ordering::SeqCst), 0);
    assert_eq!(spend(&w.f.db), vec![("10".into(), "20".into(), None)]);
}

#[test]
fn sigterm_during_publish_cleans_up_and_never_finishes_the_run() {
    let w = production_world(PATCH_OK, EXPORT_OK);
    w.fake.lock().unwrap().hold = true;
    w.f.runtime.start(&w.f.run_start(&w.run())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !w.fake.lock().unwrap().entered {
        assert!(Instant::now() < deadline, "publish never reached GitHub");
        std::thread::sleep(Duration::from_millis(10));
    }
    // What `Service::shutdown` does, in order, while the mint is still in flight.
    w.f.runtime.quiesce().unwrap();
    let rt = w.f.runtime.clone();
    let cleanup = std::thread::spawn(move || {
        // Shutdown retries each step until confirmed.
        let until = Instant::now() + Duration::from_secs(15);
        let step = |f: &dyn Fn() -> anyhow::Result<()>| loop {
            match f() {
                Ok(()) => break,
                Err(e) => assert!(Instant::now() < until, "cleanup never confirmed: {e:#}"),
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        step(&|| rt.revoke_all());
        step(&|| Gateway::stop_all(rt.as_ref()));
    });
    std::thread::sleep(Duration::from_millis(200));
    w.fake.lock().unwrap().hold = false;
    cleanup.join().unwrap();
    let (state, why) = w.state();
    assert_ne!(state, "done", "{why}");
    assert!(
        w.calls().iter().all(|c| c != "POST /repos/acme/app/pulls"),
        "{:?}",
        w.calls()
    );
}
