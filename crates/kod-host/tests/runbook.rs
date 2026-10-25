use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

const RUNBOOK: &str = include_str!("../../../docs/connected-runbook.md");

fn manifest_example() -> &'static str {
    RUNBOOK
        .split_once("<!-- tested: approval-manifest -->")
        .unwrap()
        .1
        .split_once("```json\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0
}

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn config_fixture(root: &Path) -> std::path::PathBuf {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["runs", "credentials", "mirror", "policy"] {
        let path = root.join(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in fs::read_dir(repo.join("policy")).unwrap() {
        let file = file.unwrap();
        private(
            &root.join("policy").join(file.file_name()),
            &fs::read(file.path()).unwrap(),
        );
    }
    for name in ["app.pem", "webhook", "owner", "model.key"] {
        private(&root.join(name), b"local fixture secret");
    }
    private(&root.join("approved.txt"), b"approved design\n");
    private(&root.join("manifest.json"), manifest_example().as_bytes());
    private(
        &root.join("dispatcher.json"),
        br#"{"repo":"acme/app","owner":"kiennt23","backup":null}"#,
    );
    private(
        &root.join("mapping.json"),
        &fs::read(repo.join("registry/trackers/github.json")).unwrap(),
    );
    private(
        &root.join("prices.json"),
        br#"{"model":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#,
    );
    let mut value: Value =
        serde_json::from_str(include_str!("../../../config/connected.example.json")).unwrap();
    for (key, leaf) in [
        ("dispatcher_db", "dispatcher.db"),
        ("audit_db", "audit.db"),
        ("run_root", "runs"),
        ("credential_dir", "credentials"),
        ("mirror", "mirror"),
        ("policy_dir", "policy"),
        ("approval_manifest", "manifest.json"),
        ("gates_config", "gates.json"),
    ] {
        value["paths"][key] = json!(root.join(leaf));
    }
    value["dispatcher_config"] = json!(root.join("dispatcher.json"));
    value["github"]["product"] = json!({"repo":"acme/app","repository_id":1,"installation_id":2,"base_branch":"main","guard_ruleset_id":3,"bot_user_id":4});
    value["github"]["app"] = json!({"app_id":5,"private_key":root.join("app.pem")});
    value["github"]["tracker"] = json!({"repo":"acme/tasks","comment_author_id":4});
    value["github"]["tracker_repository_id"] = json!(6);
    value["github"]["tracker_installation_id"] = json!(2);
    for (key, leaf) in [
        ("mapping", "mapping.json"),
        ("webhook_secret_file", "webhook"),
        ("owner_key_file", "owner"),
    ] {
        value["github"][key] = json!(root.join(leaf));
    }
    value["github"]["agent_name"] = json!("Fixture agent");
    value["github"]["agent_email"] = json!("fixture@example.invalid");
    value["model"] = json!({"endpoint_name":"placeholder","upstream":"https://model.example.invalid","api_key_file":root.join("model.key"),"models":["model"],"selected_model":"model","prices":root.join("prices.json"),"allowed_paths":"/v1/messages","max_tokens":128,"max_body":10000,"max_requests":10,"max_concurrent":1,"run_max_tokens":100000,"run_max_cost_micro_usd":100000});
    value["networking"]["sandbox_network"] = json!("fixture");
    value["sandbox"]["agent_image"] = json!(format!("agent@sha256:{}", "a".repeat(64)));
    value["sandbox"]["collector_image"] = json!(format!("collector@sha256:{}", "b".repeat(64)));
    value["sandbox"]["max_turns"] = json!(10);
    value["sandbox"]["wall_seconds"] = json!(60);
    let path = root.join("host.json");
    private(&path, &serde_json::to_vec(&value).unwrap());
    path
}

#[test]
fn marked_snapshot_and_staged_config_use_production_parsers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = config_fixture(&root);
    let config = kod_host::config::FsConfig::load(&path).unwrap();
    let artifacts = kod_host::artifacts::Artifacts::new(
        config.paths.approval_manifest,
        &config.paths.policy_dir,
    )
    .unwrap();
    assert_eq!(
        artifacts.current_manifest().unwrap().artifacts[0].revision,
        "design-v3"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_kod-host"))
        .args(["check", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("dispatcher.db").exists());
    assert!(!root.join("audit.db").exists());
}

#[test]
fn documented_commands_are_recognized_by_the_binary() {
    for args in [
        vec!["check", "--help"],
        vec!["serve", "--help"],
        vec!["snapshot", "activate", "--help"],
        vec!["accept", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_kod-host"))
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for mode in ["docker", "live"] {
        assert!(RUNBOOK.contains(&format!("--mode {mode}")));
    }
}

#[test]
fn internal_markdown_links_resolve() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "README.md",
        "docs/connected-runbook.md",
        "docs/kod-host-runbook.md",
        "docs/phase1-runbook.md",
        "docs/dispatcher-runbook.md",
        "docs/tool-gateway-runbook.md",
        "docs/llm-gateway-runbook.md",
        "docs/gates-runbook.md",
        "docs/status-runbook.md",
        "docs/github-tracker-runbook.md",
        "docs/architecture.md",
    ] {
        let path = root.join(name);
        let text = fs::read_to_string(&path).unwrap();
        for rest in text.split("](").skip(1) {
            let target = rest.split(')').next().unwrap();
            if target.contains("://") || target.starts_with('#') {
                continue;
            }
            let file = target.split('#').next().unwrap();
            assert!(
                path.parent().unwrap().join(file).exists(),
                "{name}: missing {target}"
            );
        }
    }
}
