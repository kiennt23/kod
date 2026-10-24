mod common;
use kod_host::records::{self, Open, PullIdentity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{cell::Cell, os::unix::fs::PermissionsExt, path::Path};

fn entry(link: &str, revision: &str, text: &str) -> Value {
    json!({"link": link, "revision": revision, "sha256": format!("{:x}", Sha256::digest(text)),
        "class": "placeholder", "path": format!("{revision}.txt"),
        "approved_by": "kiennt23", "approved_at": "2026-10-24T00:00:00Z", "approval_ref": "SIGNOFF-1"})
}
fn put(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn stage(dir: &Path, revision: &str, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    put(dir, &format!("{revision}.txt"), text);
    put(
        dir,
        "manifest.json",
        &json!({"version": 1, "artifacts": [entry("design", revision, text)]}).to_string(),
    );
}
struct World {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    config: kod_host::config::FsConfig,
}
fn world() -> World {
    let dir = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = dir.path().to_path_buf();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = (*common::config(&root, &root.join("dispatcher.db"))).clone();
    config.paths.policy_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policy")
        .canonicalize()
        .unwrap();
    config.paths.approval_manifest = root.join("active/manifest.json");
    stage(&root.join("active"), "design-v3", "v3 text");
    World {
        _dir: dir,
        root,
        config,
    }
}
fn open() -> Vec<Open> {
    vec![Open {
        story: "S-1".into(),
        head: "a".repeat(40),
    }]
}
fn active_text(w: &World) -> String {
    std::fs::read_to_string(&w.config.paths.approval_manifest).unwrap()
}

#[test]
fn checks_are_invalidated_before_the_manifest_is_replaced() {
    let w = world();
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let seen = Cell::new(None);
    records::activate(
        &w.config,
        &w.root.join("staged/manifest.json"),
        &open(),
        &|head| {
            assert_eq!(head, "a".repeat(40));
            seen.set(Some(active_text(&w).contains("design-v3")));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        seen.get(),
        Some(true),
        "invalidated while v3 was still active"
    );
    assert!(active_text(&w).contains("design-v4"));
    assert!(
        w.root.join("active/design-v3.txt").exists(),
        "old revision stays readable"
    );
}

#[test]
fn a_failed_invalidation_or_bad_candidate_leaves_the_active_manifest() {
    let w = world();
    let before = active_text(&w);
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let err = records::activate(
        &w.config,
        &w.root.join("staged/manifest.json"),
        &open(),
        &|_| anyhow::bail!("github down"),
    );
    assert!(err.is_err());
    assert_eq!(active_text(&w), before);
    assert!(!w.root.join("active/design-v4.txt").exists());
    // Same revision, different content: refused before any invalidation.
    stage(&w.root.join("bad"), "design-v3", "other text");
    let called = Cell::new(false);
    let err = records::activate(
        &w.config,
        &w.root.join("bad/manifest.json"),
        &open(),
        &|_| {
            called.set(true);
            Ok(())
        },
    );
    assert!(format!("{:#}", err.unwrap_err()).contains("immutable artifact revision changed"));
    assert!(!called.get());
    assert_eq!(active_text(&w), before);
}

fn bind(db: &Path, run: &str, repo: &str) {
    let b = common::admitted(
        Path::new("/r"),
        &kod_dispatcher::RunStart {
            id: run.into(),
            ticket: "T-1".into(),
            revision: "rev".into(),
            repo: repo.into(),
        },
    )
    .binding;
    let mut c = kod_dispatcher::store::open(db).unwrap();
    let tx = c.transaction().unwrap();
    tx.execute(
        "INSERT INTO runs(ticket,revision,state) VALUES('T-1','rev','running')",
        [],
    )
    .unwrap();
    kod_dispatcher::store::bind_once(&tx, run, &serde_json::to_value(&b).unwrap()).unwrap();
    tx.commit().unwrap();
}

#[test]
fn evidence_comes_from_the_binding_and_the_returned_pull_identity() {
    let w = world();
    let db = w.root.join("dispatcher.db");
    let pr = |repo: &str| PullIdentity {
        repo: repo.into(),
        pr: 7,
        head: "b".repeat(40),
    };
    let ev =
        |run: &str, p: &PullIdentity| records::evidence(&db, run, "S-1", "team", "p1", "r1", p);
    assert!(
        ev("run-1", &pr("acme/app")).is_err(),
        "no binding leaves gates unverified"
    );
    bind(&db, "run-1", "acme/app");
    let e = ev("run-1", &pr("acme/app")).unwrap();
    assert_eq!(
        (e.pr, e.head.as_str(), e.pins.as_slice()),
        (
            7,
            "b".repeat(40).as_str(),
            ["design-v1".to_string()].as_slice()
        )
    );
    assert!(ev("run-1", &pr("acme/other")).is_err());
    assert!(
        ev(
            "run-1",
            &PullIdentity {
                repo: "acme/app".into(),
                pr: 7,
                head: "x".into()
            }
        )
        .is_err()
    );
}

#[test]
fn status_snapshot_pins_current_revisions_and_fails_on_source_errors() {
    let w = world();
    let snap = w.root.join("gate.json");
    put(
        &w.root,
        "gate.json",
        &json!({"story": "S-1", "requester": "ann", "required": ["design"], "current": {}})
            .to_string(),
    );
    let tracker = kod_gates::records::HostRecords(snap);
    let artifacts = kod_host::artifacts::Artifacts::new(
        w.config.paths.approval_manifest.clone(),
        &w.config.paths.policy_dir,
    )
    .unwrap();
    let s = records::snapshot(
        &tracker,
        &artifacts,
        "S-1",
        &|_| kod_status::Stage::Design,
        vec![],
    )
    .unwrap();
    assert_eq!(s.artifacts[0].approved.as_deref(), Some("design-v3"));
    assert!(s.rework.is_empty());
    let out = w.root.join("records.json");
    records::write_atomic(&out, &serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(
        std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        records::snapshot(
            &tracker,
            &artifacts,
            "S-2",
            &|_| kod_status::Stage::Design,
            vec![]
        )
        .is_err()
    );
}
