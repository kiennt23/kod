#![allow(dead_code)]
use kod_dispatcher::{Dispatcher, RunStart, RunState};
use kod_host::{
    binding::Binding,
    config::FsConfig,
    runtime::{Admitted, Execute, ResolveAdmission, Runtime, RuntimeAdapters},
    workspace::CollectedWorkspace,
};
use kod_llm_proxy::{Config as ModelConfig, Endpoint, Limits, RunRegistry, app_with_registry};
use kod_tool_gateway::{Adapters, Gateway, docs::FakeDocs, forge::Identity, github};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const MODEL_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const MCP_TOKEN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub fn hash(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn admitted(root: &Path, start: &RunStart) -> Admitted {
    Admitted {
        binding: Binding {
            version: 1,
            run: start.id.clone(),
            ticket: start.ticket.clone(),
            revision: start.revision.clone(),
            repo: start.repo.clone(),
            base: "c".repeat(40),
            started_at: 1_800_000_000,
            data_classes: vec!["placeholder".into()],
            pins: [("design".to_owned(), "design-v1".to_owned())].into(),
            linked_tickets: vec![],
            max_tokens: 1000,
            max_cost_micro_usd: 1_000_000,
            agent_image: format!("agent@sha256:{}", "d".repeat(64)),
            collector_image: format!("collector@sha256:{}", "e".repeat(64)),
            agent_out: root.join(&start.id).join("agent_out"),
            collect_out: root.join(&start.id).join("collect_out"),
            model_token_hash: hash(MODEL_TOKEN),
            mcp_token_hash: hash(MCP_TOKEN),
        },
        prompt: "do it".into(),
        model_token: MODEL_TOKEN.into(),
        mcp_token: MCP_TOKEN.into(),
    }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub db: PathBuf,
    pub runtime: Arc<Runtime>,
    pub dispatcher: Arc<Dispatcher>,
    pub registry: RunRegistry,
    pub workspace: Arc<CollectedWorkspace>,
    pub gate: Arc<kod_eval::connected::SpawnGate>,
    pub executed: Arc<Mutex<Vec<String>>>,
}

fn config(root: &Path, db: &Path) -> Arc<FsConfig> {
    let p = |s: &str| root.join(s);
    let text = serde_json::json!({
        "paths": {"dispatcher_db": db, "audit_db": p("audit.db"), "policy_dir": "/p",
            "run_root": p("runs"), "mirror": "/m", "approval_manifest": "/a", "credential_dir": "/c"},
        "github": {
            "product": {"repo": "acme/app", "repository_id": 1, "installation_id": 1,
                "base_branch": "main", "guard_ruleset_id": 1, "bot_user_id": 1},
            "app": {"app_id": 1, "private_key": p("key.pem")},
            "tracker": {"repo": "acme/tasks", "comment_author_id": 1},
            "tracker_repository_id": 1, "tracker_installation_id": 1,
            "mapping": "/g", "webhook_secret_file": "/w", "owner_key_file": "/o",
            "backup_key_file": null, "agent_name": "kod-agent", "agent_email": "agent@kod.invalid"},
        "model": {"endpoint_name": "m", "upstream": "http://127.0.0.1:1", "api_key_file": "/k",
            "models": ["x"], "selected_model": "x", "prices": "/pr", "allowed_paths": "/v1/messages",
            "max_tokens": 1, "max_body": 1, "max_requests": 1, "max_concurrent": 1,
            "run_max_tokens": 1, "run_max_cost_micro_usd": 1},
        "networking": {"ingress_listen": "127.0.0.1:3000", "gateway_listen": "127.0.0.1:3001",
            "accepted_hosts": ["localhost"], "sandbox_network": "n", "sandbox_gateway_url": "http://h:1"},
        "sandbox": {"agent_image": "a", "collector_image": "c", "language": "rust", "memory": "1g",
            "cpus": 1, "disk": "1m", "max_turns": 1, "wall_seconds": 1},
        "dispatcher_config": "/d"
    });
    Arc::new(
        kod_host::config::strict_json::<FsConfig>(text.to_string().as_bytes())
            .expect("fixture config"),
    )
}

impl Fixture {
    /// `execute` is the agent: it receives the admitted run and the spawn gate.
    pub fn start(execute: Execute) -> Self {
        Self::build(tempfile::tempdir().unwrap(), execute)
    }
    /// A host restart: fresh in-memory state over the same databases.
    pub fn restart(self, execute: Execute) -> Self {
        let Self {
            dir,
            runtime,
            dispatcher,
            ..
        } = self;
        drop(dispatcher);
        // Workers hold the runtime until the old agent returns.
        while Arc::strong_count(&runtime) > 1 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(runtime);
        Self::build(dir, execute)
    }
    fn build(dir: tempfile::TempDir, execute: Execute) -> Self {
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let db = root.join("dispatcher.db");
        let policy = Arc::new(
            kod_policy::Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy"))
                .unwrap(),
        );
        let audit = Arc::new(kod_audit::AuditLog::open(&root.join("audit.db")).unwrap());
        let fresh = !root.join("key.pem").exists();
        assert!(
            !fresh
                || std::process::Command::new("openssl")
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
        std::fs::create_dir_all(root.join("leases")).unwrap();
        std::fs::set_permissions(root.join("leases"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let cfg = config(&root, &db);
        let github = Arc::new(
            github::GitHub::new(
                cfg.github.product.clone(),
                cfg.github.app.clone(),
                root.join("leases"),
            )
            .unwrap(),
        );
        let workspace = Arc::new(CollectedWorkspace::default());
        let (tracker, _seed) = kod_tracker::fake::sandbox();
        let tracker = Arc::new(tracker);
        let tool = Arc::new(
            Gateway::new(
                policy.clone(),
                audit.clone(),
                Adapters {
                    tracker: tracker.clone(),
                    docs: Arc::new(FakeDocs::default()),
                    stopped: kod_host::runtime::stopped_reader(db.clone()),
                    broker: github.clone(),
                    forge: github.clone(),
                    workspace: workspace.clone(),
                },
                Identity {
                    name: "kod-agent".into(),
                    email: "agent@kod.invalid".into(),
                },
            )
            .unwrap(),
        );
        let (_router, registry) = app_with_registry(ModelConfig {
            endpoint: Endpoint {
                name: "m".into(),
                upstream: "http://127.0.0.1:1".into(),
                api_key: "key".parse().unwrap(),
            },
            allowed_paths: vec!["/v1/messages".into()],
            limits: Limits {
                allowed_models: vec!["x".into()],
                max_tokens: 1000,
                max_body: 4096,
                max_requests: 100,
                max_concurrent: 2,
            },
            prices: HashMap::new(),
            runs: HashMap::new(),
            policy: policy.clone(),
            audit: audit.clone(),
        });
        let gate = Arc::new(kod_eval::connected::SpawnGate::default());
        let executed = Arc::new(Mutex::new(Vec::new()));
        let run_root = cfg.paths.run_root.clone();
        let resolve: ResolveAdmission = Arc::new(move |s| Ok(admitted(&run_root, s)));
        let runtime = Runtime::new(
            cfg,
            RuntimeAdapters {
                tool,
                model: registry.clone(),
                github,
                workspace: workspace.clone(),
                gate: gate.clone(),
                resolve,
                execute,
            },
        );
        let dispatcher = Arc::new(
            Dispatcher::open(
                kod_dispatcher::Config::parse(
                    r#"{"repo": "acme/app", "owner": "kiennt23", "backup": null}"#,
                )
                .unwrap(),
                &db,
                Ok(policy),
                audit,
                tracker,
                runtime.clone(),
            )
            .unwrap(),
        );
        runtime.attach(Arc::downgrade(&dispatcher)).unwrap();
        Self {
            dir,
            db,
            runtime,
            dispatcher,
            registry,
            workspace,
            gate,
            executed,
        }
    }
    pub fn run_start(&self, id: &str) -> RunStart {
        RunStart {
            id: id.into(),
            ticket: "acme/app#7".into(),
            revision: "v1".into(),
            repo: "acme/app".into(),
        }
    }
    /// Insert a running run row so `Gateway::start` has a durable identity.
    pub fn insert_run(&self, seq: i64, state: RunState) {
        let c = kod_dispatcher::store::open(&self.db).unwrap();
        c.execute(
            "INSERT INTO runs(seq,ticket,revision,state) VALUES(?1,'acme/app#7','v1',?2)",
            rusqlite::params![seq, state.name()],
        )
        .unwrap();
    }
}

/// Run `f` with a fake `docker` script and a virtual stop clock.
pub fn with_fake_docker<T>(script: &str, f: impl FnOnce() -> T) -> T {
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    std::fs::write(&docker, script).unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    kod_eval::connected::with_docker(&docker, || kod_eval::connected::with_stop_clock(f))
}
pub const DOCKER_ABSENT: &str = "#!/bin/sh\ncase $1 in ps) ;; esac\n";
