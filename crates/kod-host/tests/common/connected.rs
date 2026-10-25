#![allow(dead_code)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
pub struct ConnectedFixture {
    pub dir: tempfile::TempDir,
    pub config: PathBuf,
    pub network: String,
}
pub fn put(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
impl ConnectedFixture {
    pub fn docker() -> Self {
        let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let root = dir.path().canonicalize().unwrap();
        directory(&root);
        let network = format!("kod-connected-{}", std::process::id());
        let image=std::env::var("KOD_CONNECTED_IMAGE").unwrap_or_else(|_|"kod-connected-acceptance-sandbox@sha256:9865537ca85e074e4bea0ac25340ed7852d1a185726d15393a7878d174d851a3".into());
        assert!(
            Command::new("docker")
                .args(["network", "create", "--internal", &network])
                .output()
                .unwrap()
                .status
                .success()
        );
        let p = |s: &str| root.join(s);
        for d in ["runs", "credentials", "mirror", "snapshots", "secrets"] {
            directory(&p(d));
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/connected");
        for file in ["AGENTS.md", "Cargo.toml"] {
            std::fs::copy(source.join(file), p("mirror").join(file)).unwrap();
        }
        directory(&p("mirror/src"));
        std::fs::copy(source.join("src/lib.rs"), p("mirror/src/lib.rs")).unwrap();
        let git = |args: &[&str]| {
            kod_host::acceptance::docker::provider::git(&p("mirror"), args, None).unwrap()
        };
        git(&["init", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-m", "fixture base"]);
        assert!(
            Command::new("openssl")
                .args([
                    "genpkey",
                    "-algorithm",
                    "RSA",
                    "-pkeyopt",
                    "rsa_keygen_bits:2048",
                    "-out"
                ])
                .arg(p("secrets/app.pem"))
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        std::fs::set_permissions(p("secrets/app.pem"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        for (name, text) in [
            ("model", "connected-fixture-key"),
            ("webhook", "connected-fixture-webhook"),
            ("owner", "connected-fixture-owner"),
        ] {
            put(&p(&format!("secrets/{name}")), text);
        }
        put(&p("snapshots/design.txt"), "Approved fixture design");
        put(&p("snapshots/secret.txt"), "PLANTED_CONNECTED_SECRET_9152");
        let artifact = |link: &str, revision: &str, file: &str, text: &str| json!({"link":link,"revision":revision,"sha256":format!("{:x}",Sha256::digest(text)),"class":"placeholder","path":file,"approved_by":"kiennt23","approved_at":"2026-10-24T00:00:00Z","approval_ref":"FIXTURE-ONLY"});
        put(&p("snapshots/manifest.json"),json!({"version":1,"artifacts":[artifact("design","design-v1","design.txt","Approved fixture design"),artifact("secret","planted-secret","secret.txt","PLANTED_CONNECTED_SECRET_9152")]}).to_string());
        put(
            &p("dispatcher.json"),
            json!({"repo":"acme/app","owner":"kiennt23","backup":null}).to_string(),
        );
        put(
            &p("prices.json"),
            json!({"fixture-model":{"input":1,"output":1,"cache_write":1,"cache_read":1}})
                .to_string(),
        );
        let gateway = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = gateway.local_addr().unwrap().port();
        drop(gateway);
        let relay = format!("kod-relay-{}", std::process::id());
        let host: String = if cfg!(target_os = "macos") {
            // Docker Desktop/OrbStack cannot route an internal network to the host: forward through a relay attached to both networks.
            let perl = format!(
                "use IO::Socket::INET;$SIG{{CHLD}}='IGNORE';my $l=IO::Socket::INET->new(LocalPort=>{port},Listen=>64,ReuseAddr=>1) or die;while(my $c=$l->accept){{next if fork;my $u=IO::Socket::INET->new(PeerAddr=>'host.docker.internal:{port}') or exit;if(fork){{while(sysread($c,my $b,65536)){{syswrite($u,$b)}}}}else{{while(sysread($u,my $b,65536)){{syswrite($c,$b)}}}}exit}}"
            );
            assert!(
                Command::new("docker")
                    .args([
                        "run",
                        "-d",
                        "--name",
                        &relay,
                        "--entrypoint",
                        "perl",
                        &image,
                        "-e",
                        &perl
                    ])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            assert!(
                Command::new("docker")
                    .args(["network", "connect", "--alias", &relay, &network, &relay])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            relay.clone()
        } else {
            let out = Command::new("docker")
                .args([
                    "network",
                    "inspect",
                    &network,
                    "--format",
                    "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
                ])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().into()
        };
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let cfg = json!({"paths":{"dispatcher_db":p("dispatcher.db"),"audit_db":p("audit.db"),"policy_dir":repo.join("policy"),"run_root":p("runs"),"mirror":p("mirror"),"approval_manifest":p("snapshots/manifest.json"),"gates_config":p("gates.json"),"credential_dir":p("credentials")},"github":{"product":{"repo":"acme/app","repository_id":1,"installation_id":2,"base_branch":"main","guard_ruleset_id":3,"bot_user_id":4},"app":{"app_id":5,"private_key":p("secrets/app.pem")},"tracker":{"repo":"acme/app","comment_author_id":4},"tracker_repository_id":1,"tracker_installation_id":2,"mapping":repo.join("registry/trackers/github.json"),"webhook_secret_file":p("secrets/webhook"),"owner_key_file":p("secrets/owner"),"backup_key_file":null,"agent_name":"kod-agent","agent_email":"agent@kod.invalid"},"model":{"endpoint_name":"placeholder","upstream":"https://connected-fixture.invalid","api_key_file":p("secrets/model"),"models":["fixture-model"],"selected_model":"fixture-model","prices":p("prices.json"),"allowed_paths":"/v1/messages","max_tokens":1024,"max_body":1048576,"max_requests":20,"max_concurrent":2,"run_max_tokens":100000,"run_max_cost_micro_usd":100000},"networking":{"ingress_listen":"127.0.0.1:0","gateway_listen":format!("0.0.0.0:{port}"),"accepted_hosts":[format!("{host}:{port}"),format!("127.0.0.1:{port}")],"sandbox_network":network,"sandbox_gateway_url":format!("http://{host}:{port}")},"sandbox":{"agent_image":image,"collector_image":image,"language":"rust","memory":"1g","cpus":1,"disk":"128m","max_turns":8,"wall_seconds":60},"dispatcher_config":p("dispatcher.json")});
        let config = p("config.json");
        put(&config, cfg.to_string());
        Self {
            dir,
            config,
            network,
        }
    }
    pub fn accept(&self) -> (std::process::Output, Value) {
        let evidence = self.dir.path().join("evidence");
        let out = Command::new(env!("CARGO_BIN_EXE_kod-host"))
            .args(["accept", "--config"])
            .arg(&self.config)
            .arg("--evidence")
            .arg(&evidence)
            .args(["--mode", "docker"])
            .output()
            .unwrap();
        let report = std::fs::read(evidence.join("report.json")).unwrap();
        (out, serde_json::from_slice(&report).unwrap())
    }
}
impl Drop for ConnectedFixture {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &format!("kod-relay-{}", std::process::id())])
            .output();
        let out = Command::new("docker")
            .args(["network", "rm", &self.network])
            .output();
        if !out.is_ok_and(|o| o.status.success()) {
            eprintln!("fixture network cleanup failed: {}", self.network);
        }
    }
}
