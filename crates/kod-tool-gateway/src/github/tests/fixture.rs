use super::super::*;
use axum::Router;
use serde_json::Value;
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, HashSet},
    os::unix::fs::PermissionsExt,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
pub(super) struct Fixture {
    pub dir: tempfile::TempDir,
    pub remote: Arc<Mutex<Remote>>,
    pub api: String,
    pub clock: Arc<AtomicI64>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[derive(Default)]
pub(super) struct Remote {
    pub requests: Vec<(String, String, Value)>,
    pub live: HashSet<String>,
    pub issued: usize,
    pub disabled: bool,
    pub widened: bool,
    pub fail_revoke: bool,
    pub lost_mint: bool,
    pub refs: BTreeMap<String, String>,
    pub prs: Vec<Value>,
    pub lost_pr: bool,
    pub hide_pr: bool,
    pub foreign: bool,
    pub ambiguous: bool,
    pub truncated: bool,
    pub lost_ref: bool,
    pub after_ref: Option<Box<dyn FnOnce() + Send>>,
    pub paginated: bool,
    pub foreign_link: bool,
    pub api: String,
    pub extra_permission: bool,
    pub app_bypass: bool,
    pub after_mint: Option<Box<dyn FnOnce() + Send>>,
    pub repo: PathBuf,
    pub public_key: PathBuf,
}
impl Fixture {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.join("leases")).unwrap();
        std::fs::set_permissions(root.join("leases"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
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
        std::fs::set_permissions(root.join("key.pem"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert!(
            std::process::Command::new("openssl")
                .args(["pkey", "-in"])
                .arg(root.join("key.pem"))
                .args(["-pubout", "-out"])
                .arg(root.join("pub.pem"))
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        let repo = root.join("git");
        super::objects::seed(&repo);
        let remote = Arc::new(Mutex::new(Remote {
            repo,
            public_key: root.join("pub.pem"),
            ..Default::default()
        }));
        let state = remote.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop, wait) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(
                    listener,
                    Router::new()
                        .fallback(super::provider::handler)
                        .with_state(state),
                )
                .with_graceful_shutdown(async {
                    let _ = wait.await;
                })
                .await
                .unwrap();
            });
        });
        let api = format!("http://{}/", rx.recv().unwrap());
        remote.lock().unwrap().api = api.clone();
        Self {
            dir,
            remote,
            api,
            clock: Arc::new(AtomicI64::new(1_800_000_000)),
            stop: Some(stop),
            thread: Some(thread),
        }
    }
    pub fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }
    pub fn config(&self) -> Config {
        Config {
            repo: "acme/app".into(),
            repository_id: 1,
            installation_id: 2,
            base_branch: "main".into(),
            guard_ruleset_id: 3,
            bot_user_id: 4,
        }
    }
    pub fn app(&self) -> AppCredentials {
        AppCredentials {
            app_id: 5,
            private_key: self.root().join("key.pem"),
        }
    }
    pub fn adapter(&self) -> GitHub {
        self.adapter_with(self.config()).unwrap()
    }
    pub fn adapter_with(&self, config: Config) -> Result<GitHub> {
        let clock = self.clock.clone();
        GitHub::with_test_api(
            config,
            self.app(),
            self.root().join("leases"),
            &self.api,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
    }
    pub fn permit(&self) -> RunPermit {
        RunPermit {
            run: "run-1".into(),
            ticket: "acme/app#7".into(),
            repo: "acme/app".into(),
            base: super::objects::git(
                &self.remote.lock().unwrap().repo,
                &["rev-parse", "HEAD"],
                None,
            ),
        }
    }
    pub fn requests(&self, method: &str, path: &str) -> usize {
        self.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .count()
    }
    pub fn mutations(&self) -> usize {
        self.remote
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, p, _)| m != "GET" && p.starts_with("/repos/"))
            .count()
    }
    pub fn live(&self) -> usize {
        self.remote.lock().unwrap().live.len()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}
