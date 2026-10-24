use kod_host::config::{FsPaths, strict_json};
#[test]
fn duplicates_unknown_and_missing_fields_are_errors() {
    assert!(strict_json::<serde_json::Value>(br#"{"model":{"x":1,"x":2}}"#).is_err());
    assert!(strict_json::<serde_json::Value>(br#"{"items":[{"x":1,"x":2}]}"#).is_err());
    assert!(strict_json::<FsPaths>(br#"{"dispatcher_db":"a","unknown":true}"#).is_err());
    assert!(strict_json::<FsPaths>(b"{}").is_err());
    assert!(strict_json::<serde_json::Value>(b"{} false").is_err());
    assert_eq!(
        strict_json::<serde_json::Value>(br#"{"a":[true,null,-2,1.25]}"#).unwrap()["a"][2],
        -2
    );
}
#[test]
fn default_deadlines_are_fixed() {
    use kod_host::deadlines::*;
    assert_eq!(
        [
            MODEL_CANCEL,
            TERM_GRACE,
            ABSENCE,
            WORKER_TICK,
            STATUS_DEBOUNCE,
            RECONCILE,
            SOURCE
        ]
        .map(|d| d.as_millis()),
        [2_000, 5_000, 10_000, 250, 1_000, 30_000, 10_000]
    );
}
#[test]
fn secret_files_must_be_owner_only_regular_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("key");
    std::fs::write(&p, b"local test secret").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(kod_host::config::private_file(&p).is_ok());
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(kod_host::config::private_file(&p).is_err());
    let link = d.path().join("link");
    symlink(&p, &link).unwrap();
    assert!(kod_host::config::private_file(&link).is_err());
}

use kod_host::config::FsConfig;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path};
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn fixture() -> (tempfile::TempDir, Value) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["runs", "credentials", "mirror", "policy"] {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in std::fs::read_dir("../../policy").unwrap() {
        let entry = entry.unwrap();
        write(
            &root.join("policy").join(entry.file_name()),
            &std::fs::read(entry.path()).unwrap(),
        );
    }
    for name in ["app.pem", "webhook", "owner", "model.key"] {
        write(&root.join(name), b"test secret");
    }
    use sha2::{Digest, Sha256};
    write(&root.join("design.txt"), b"approved design");
    let manifest = json!({"version":1,"artifacts":[{"link":"design","revision":"v1","sha256":format!("{:x}",Sha256::digest(b"approved design")),"class":"placeholder","path":"design.txt","approved_by":"kiennt23","approved_at":"2026-10-24T00:00:00Z","approval_ref":"approval-1"}]});
    write(
        &root.join("manifest.json"),
        &serde_json::to_vec(&manifest).unwrap(),
    );
    write(
        &root.join("dispatcher.json"),
        br#"{"repo":"acme/app","owner":"kiennt23","backup":null}"#,
    );
    write(
        &root.join("mapping.json"),
        &std::fs::read("../../registry/trackers/github.json").unwrap(),
    );
    write(
        &root.join("prices.json"),
        br#"{"model":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#,
    );
    let config = json!({
        "paths":{"dispatcher_db":root.join("dispatcher.db"),"audit_db":root.join("audit.db"),"policy_dir":root.join("policy"),"run_root":root.join("runs"),"mirror":root.join("mirror"),"approval_manifest":root.join("manifest.json"),"gates_config":root.join("gates.json"),"credential_dir":root.join("credentials")},
        "github":{"product":{"repo":"acme/app","repository_id":1,"installation_id":2,"base_branch":"main","guard_ruleset_id":3,"bot_user_id":4},"app":{"app_id":5,"private_key":root.join("app.pem")},"tracker":{"repo":"acme/tasks","comment_author_id":4},"tracker_repository_id":6,"tracker_installation_id":2,"mapping":root.join("mapping.json"),"webhook_secret_file":root.join("webhook"),"owner_key_file":root.join("owner"),"backup_key_file":null,"agent_name":"Agent","agent_email":"agent@example.com"},
        "model":{"endpoint_name":"approved","upstream":"https://api.example.com","api_key_file":root.join("model.key"),"models":["model"],"selected_model":"model","prices":root.join("prices.json"),"allowed_paths":"/v1/messages","max_tokens":100,"max_body":1000,"max_requests":10,"max_concurrent":1,"run_max_tokens":1000,"run_max_cost_micro_usd":1000},
        "networking":{"ingress_listen":"127.0.0.1:3000","gateway_listen":"127.0.0.1:3001","accepted_hosts":["localhost"],"sandbox_network":"kod","sandbox_gateway_url":"http://host.docker.internal:3001"},
        "sandbox":{"agent_image":format!("agent@sha256:{}","a".repeat(64)),"collector_image":format!("collector@sha256:{}","b".repeat(64)),"language":"rust","memory":"1g","cpus":1.0,"disk":"128m","max_turns":10,"wall_seconds":60},
        "dispatcher_config":root.join("dispatcher.json")
    });
    (dir, config)
}
fn load(dir: &tempfile::TempDir, value: &Value) -> anyhow::Result<FsConfig> {
    let path = dir.path().canonicalize().unwrap().join("host.json");
    write(&path, &serde_json::to_vec(value).unwrap());
    FsConfig::load(&path)
}
#[test]
fn valid_configuration_creates_no_databases_or_leases() {
    let (dir, mut value) = fixture();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    value["model"]["upstream"] = json!(format!("https://{}", listener.local_addr().unwrap()));
    load(&dir, &value).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!dir.path().join("dispatcher.db").exists());
    assert!(!dir.path().join("audit.db").exists());
    assert_eq!(
        std::fs::read_dir(dir.path().join("credentials"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn missing_nullable_backup_is_rejected() {
    let (dir, mut value) = fixture();
    value["github"]
        .as_object_mut()
        .unwrap()
        .remove("backup_key_file");
    assert!(load(&dir, &value).is_err());
}
#[test]
fn invalid_configs_never_create_databases() {
    let (dir, value) = fixture();
    for (pointer, replacement) in [
        ("/sandbox/agent_image", json!("agent:latest")),
        ("/model/run_max_tokens", json!(0)),
        ("/model/run_max_cost_micro_usd", json!(u64::MAX)),
        ("/model/models", json!(["model", "model"])),
        ("/model/upstream", json!("http://127.0.0.1:1")),
        ("/paths/audit_db", value["paths"]["dispatcher_db"].clone()),
        ("/github/product/repo", json!("acme/other")),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(load(&dir, &changed).is_err(), "{pointer}");
        assert!(!dir.path().join("dispatcher.db").exists());
        assert!(!dir.path().join("audit.db").exists());
    }
}
#[test]
fn symlink_ancestry_is_rejected() {
    let (dir, mut value) = fixture();
    let root = dir.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(root.join("credentials"), root.join("alias")).unwrap();
    value["paths"]["credential_dir"] = json!(root.join("alias"));
    assert!(load(&dir, &value).is_err());
}
#[test]
fn readable_key_is_rejected() {
    let (dir, value) = fixture();
    std::fs::set_permissions(
        dir.path().join("owner"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert!(load(&dir, &value).is_err());
}
#[test]
fn oversized_file_is_rejected() {
    let (dir, _) = fixture();
    let path = dir.path().canonicalize().unwrap().join("huge.json");
    write(&path, &vec![b' '; 1024 * 1024 + 1]);
    assert!(FsConfig::load(&path).is_err());
}

#[test]
fn nested_unknown_and_missing_fields_are_rejected() {
    let (dir, value) = fixture();
    let mut unknown = value.clone();
    unknown["github"]["app"]["unexpected"] = json!(true);
    assert!(load(&dir, &unknown).is_err());
    let mut missing = value;
    missing["sandbox"].as_object_mut().unwrap().remove("disk");
    assert!(load(&dir, &missing).is_err());
}

#[test]
fn tracker_test_transport_requires_literal_loopback() {
    let mapping =
        kod_tracker::mapping::Mapping::load(Path::new("../../registry/trackers/github.json"))
            .unwrap();
    let config = kod_tracker::github::Config {
        repo: "acme/tasks".into(),
        comment_author_id: 1,
    };
    for api in [
        "https://127.0.0.1:1234",
        "http://localhost:1234",
        "http://192.0.2.1:1234",
        "http://127.0.0.1:1234/path",
        "http://user@127.0.0.1:1234",
        "http://127.0.0.1:1234/?secret=value",
    ] {
        assert!(
            kod_tracker::github::GitHub::with_test_api(
                config.clone(),
                mapping.clone(),
                "token",
                "secret",
                api
            )
            .is_err()
        );
    }
    assert!(
        kod_tracker::github::GitHub::with_test_api(
            config,
            mapping,
            "token",
            "secret",
            "http://127.0.0.1:1234"
        )
        .is_ok()
    );
}

#[test]
fn the_example_configuration_parses_but_does_not_validate() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/connected.example.json");
    let path = std::fs::canonicalize(path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        strict_json::<FsConfig>(&bytes).is_ok(),
        "example must be schema-valid"
    );
    let error = format!(
        "{:#}",
        FsConfig::load(&path)
            .err()
            .expect("example must not validate")
    );
    assert!(error.contains("configured path is missing"), "{error}");
}
