mod common;
use kod_dispatcher::{RunStart, RunState};
use kod_eval::connected::SpawnGate;
use kod_host::{artifacts::Artifacts, production};
use kod_tool_gateway::{Stopped, forge::Workspace};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, sync::Arc};

fn put(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn gate_file(path: &Path, required: &[&str]) {
    put(
        path,
        &json!({"story": "T-1", "requester": "ann", "required": required, "current": {}})
            .to_string(),
    );
}
struct World {
    dir: tempfile::TempDir,
    config: Arc<kod_host::config::FsConfig>,
    gate: std::path::PathBuf,
}
fn world() -> World {
    let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = dir.path().to_path_buf();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = (*common::config(&root, &root.join("dispatcher.db"))).clone();
    config.paths.policy_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policy")
        .canonicalize()
        .unwrap();
    config.paths.approval_manifest = root.join("active/manifest.json");
    config.paths.run_root = root.join("runs");
    config.paths.mirror = root.join("mirror");
    config.sandbox.disk = "128m".into();
    fs::create_dir_all(root.join("active")).unwrap();
    fs::set_permissions(root.join("active"), fs::Permissions::from_mode(0o700)).unwrap();
    put(&root.join("active/design-v3.txt"), "v3 text");
    let entry = json!({"link": "design", "revision": "design-v3", "sha256": format!("{:x}", Sha256::digest("v3 text")),
        "class": "placeholder", "path": "design-v3.txt", "approved_by": "kiennt23",
        "approved_at": "2026-10-24T00:00:00Z", "approval_ref": "SIGNOFF-1"});
    put(
        &root.join("active/manifest.json"),
        &json!({"version": 1, "artifacts": [entry]}).to_string(),
    );
    let gate = root.join("gate.json");
    gate_file(&gate, &["design"]);
    World {
        dir,
        config: Arc::new(config),
        gate,
    }
}
fn revision(w: &World) -> String {
    use kod_tracker::Tracker;
    kod_dispatcher::revision(
        &kod_gates::records::HostRecords(w.gate.clone())
            .get_issue("T-1")
            .unwrap(),
    )
}
fn resolver(
    w: &World,
    on_base: Arc<dyn Fn() + Send + Sync>,
) -> kod_host::runtime::ResolveAdmission {
    let artifacts = Artifacts::new(
        w.config.paths.approval_manifest.clone(),
        &w.config.paths.policy_dir,
    )
    .unwrap();
    production::resolve(
        w.config.clone(),
        production::Sources {
            tracker: Arc::new(kod_gates::records::HostRecords(w.gate.clone())),
            artifacts: Arc::new(artifacts),
            base: Arc::new(move || {
                on_base();
                Ok("c".repeat(40))
            }),
            now: Arc::new(|| 1_800_000_000),
        },
    )
}
fn start(_w: &World, revision: String) -> RunStart {
    RunStart {
        id: "run-1".into(),
        ticket: "T-1".into(),
        revision,
        repo: "acme/app".into(),
    }
}

#[test]
fn production_admission_supplies_every_binding_field() {
    let w = world();
    let a = resolver(&w, Arc::new(|| {}))(&start(&w, revision(&w))).unwrap();
    let b = &a.binding;
    b.validate(&w.config.paths.run_root).unwrap();
    assert_eq!(b.pins["design"], "design-v3");
    assert_eq!(
        (b.data_classes.as_slice(), b.base.as_str(), b.started_at),
        (
            ["placeholder".to_string()].as_slice(),
            "c".repeat(40).as_str(),
            1_800_000_000
        )
    );
    assert_eq!((b.max_tokens, b.max_cost_micro_usd), (1, 1));
    assert_eq!(
        (b.agent_image.as_str(), b.collector_image.as_str()),
        ("a", "c")
    );
    assert_eq!(
        b.model_token_hash,
        kod_host::binding::token_hash(&a.model_token)
    );
    assert_eq!(
        b.mcp_token_hash,
        kod_host::binding::token_hash(&a.mcp_token)
    );
    assert_ne!(a.model_token, a.mcp_token);
    assert!(b.linked_tickets.is_empty());
    let b2 = resolver(&w, Arc::new(|| {}))(&start(&w, revision(&w))).unwrap();
    assert_ne!(
        a.model_token, b2.model_token,
        "tokens are random per admission"
    );
}

#[test]
fn admission_refuses_a_changed_ticket_repo_or_unapproved_input() {
    let w = world();
    let r = resolver(&w, Arc::new(|| {}));
    assert!(r(&start(&w, "stale".into())).is_err());
    let mut other = start(&w, revision(&w));
    other.repo = "acme/other".into();
    assert!(r(&other).is_err());
    // The ticket changes after pins were read: the final re-check refuses.
    let gate = w.gate.clone();
    let racing = resolver(
        &w,
        Arc::new(move || gate_file(&gate, &["design", "design"])),
    );
    assert!(racing(&start(&w, revision(&w))).is_err());
    // A link without an approved snapshot entry fails closed.
    gate_file(&w.gate, &["gone"]);
    assert!(resolver(&w, Arc::new(|| {}))(&start(&w, revision(&w))).is_err());
}

const PATCH_OK: &str = "touch collect_ok; printf 'diff --git a/a b/a\\nindex aaa..bbb 100644\\n--- a/a\\n+++ b/a\\n@@ -1 +1 @@\\n-a\\n+b\\n' > diff.patch";
const EXPORT_OK: &str = r#"touch export_ok; printf '{"version":1,"base":"%s","files":[{"path":"a","mode":"100644","blob":0}]}' "$BASE" > manifest.json; printf 'b\n' > 0"#;

/// Runs the production Execute adapter against a fake docker whose collector/exporter run `collect`/`export`.
fn drive(collect: &str, export: &str) -> (anyhow::Result<RunState>, bool) {
    let mut w = world();
    let root = w.dir.path().to_path_buf();
    let mirror = root.join("mirror");
    fs::create_dir(&mirror).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&mirror)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    git(&["init", "-q"]);
    fs::write(mirror.join("a"), "a\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]).trim().to_owned();
    let dir = w.config.paths.run_root.join("run-1");
    fs::create_dir_all(&dir).unwrap();
    let docker = root.join("docker");
    fs::write(&docker, format!(r#"#!/bin/sh
[ "$1" = run ] || exit 0
for a in "$@"; do case $a in kod-agent-*) k=agent;; kod-collect-*) k=collect;; kod-export-*) k=export;; esac; done
cd '{dir}'
BASE='{base}'
case $k in
agent) printf 0 > agent_out/exit_code; printf 0 > agent_out/agent_procs; echo '{{"end_state":"done"}}' > agent_out/agent.json;;
collect) cd collect_out; {collect};;
export) cd export_out; {export};;
esac
"#, dir = dir.display())).unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    Arc::get_mut(&mut w.config).unwrap().sandbox.wall_seconds = 30;
    let mut admitted = common::admitted(&w.config.paths.run_root, &start(&w, "rev".into()));
    admitted.binding.base = base;
    let execute = production::execute(w.config.clone());
    let stopped: Stopped = Arc::new(|_| Ok(false));
    let workspace = kod_host::workspace::CollectedWorkspace::default();
    let result = kod_eval::connected::with_docker(&docker, || {
        execute(&admitted, &SpawnGate::default(), &stopped, &workspace)
    });
    (result, workspace.collected("run-1").is_ok())
}

#[test]
fn production_execute_binds_the_runner_and_gateway_urls() {
    let (result, collected) = drive(PATCH_OK, EXPORT_OK);
    assert_eq!(result.unwrap(), RunState::Done);
    assert!(collected);
    let w = world();
    let a = common::admitted(&w.config.paths.run_root, &start(&w, "rev".into()));
    let s = production::spec(&w.config, &a);
    assert_eq!(s.model_url, format!("http://h:1/r/{}", a.model_token));
    assert_eq!(s.mcp_url, format!("http://h:1/r/{}/mcp", a.mcp_token));
    assert_eq!((s.network.as_str(), s.model.as_str()), ("n", "x"));
}

#[test]
fn forged_or_failed_collection_never_reaches_done_or_a_collected_result() {
    let rename = "touch collect_ok; printf 'diff --git a/a b/b\\nrename from a\\nrename to b\\n' > diff.patch";
    for (collect, export) in [
        ("printf x > diff.patch", EXPORT_OK),
        (
            "ln -s /etc/passwd collect_ok; printf 'diff --git a/a b/a\\nindex aaa..bbb 100644\\n--- a/a\\n+++ b/a\\n@@ -1 +1 @@\\n-a\\n+b\\n' > diff.patch",
            EXPORT_OK,
        ),
        ("exit 3", EXPORT_OK),
        (rename, EXPORT_OK),
        (PATCH_OK, "true"),
        (
            PATCH_OK,
            r#"ln -s /etc/passwd export_ok; printf '{"version":1,"base":"%s","files":[{"path":"a","mode":"100644","blob":0}]}' "$BASE" > manifest.json; printf 'b\n' > 0"#,
        ),
        (
            PATCH_OK,
            r#"touch export_ok; printf '{"version":1,"base":"wrong","files":[]}' > manifest.json"#,
        ),
        (PATCH_OK, "exit 3"),
    ] {
        let (result, collected) = drive(collect, export);
        assert!(
            !matches!(result, Ok(RunState::Done)),
            "{collect} / {export}"
        );
        assert!(!collected, "{collect} / {export}");
    }
}

#[test]
fn the_mirror_base_is_the_configured_branch_head_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    let head = git(&["rev-parse", "HEAD"]);
    assert_eq!(production::mirror_base(dir.path(), "main").unwrap(), head);
    assert!(production::mirror_base(dir.path(), "absent").is_err());
    assert!(production::mirror_base(dir.path(), "--all").is_err());
    assert!(production::mirror_base(&dir.path().join("missing"), "main").is_err());
}
