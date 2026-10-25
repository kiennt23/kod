use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path};
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
pub fn fixture() -> (tempfile::TempDir, Value) {
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
