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

fn tracker() -> kod_tracker::fake::FakeTracker {
    use kod_tracker::fake::{FAKE_MAPPING, FakeIssue, FakeTracker};
    let t = FakeTracker::new(
        kod_tracker::mapping::Mapping::parse("m", FAKE_MAPPING).unwrap(),
        "s",
    );
    let issue = |reporter: &str, links: &[&str]| FakeIssue {
        title: "t".into(),
        body: "b".into(),
        acceptance_criteria: vec![],
        reporter: reporter.into(),
        workflow_state: "Backlog".into(),
        fields: Default::default(),
        approved_versions: vec!["design".into()],
        links: links.iter().map(|l| l.to_string()).collect(),
    };
    t.add_member("ann");
    t.add_issue("S-1", issue("ann", &["T-1", "T-2"]));
    t.add_issue("T-1", issue("ann", &[]));
    t.add_issue("T-2", issue("mallory", &[]));
    t.add_issue("S-2", issue("mallory", &[]));
    t
}
fn snap(
    w: &World,
    t: &dyn kod_tracker::Tracker,
    story: &str,
) -> anyhow::Result<kod_status::records::Snapshot> {
    let artifacts = kod_host::artifacts::Artifacts::new(
        w.config.paths.approval_manifest.clone(),
        &w.config.paths.policy_dir,
    )?;
    records::snapshot(
        t,
        &artifacts,
        story,
        &|_| kod_status::Stage::Design,
        vec![],
        vec![],
    )
}

#[test]
fn status_snapshot_pins_current_revisions_and_fails_on_source_errors() {
    let w = world();
    let t = tracker();
    let s = snap(&w, &t, "S-1").unwrap();
    assert_eq!(s.artifacts[0].approved.as_deref(), Some("design-v3"));
    let out = w.root.join("records.json");
    records::write_atomic(&out, &serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(
        std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(snap(&w, &t, "S-3").is_err(), "unknown story");
    // A linked ticket the tracker cannot read fails the whole snapshot.
    let mut broken = t.issue("S-1").unwrap();
    broken.links.push("T-404".into());
    t.add_issue("S-1", broken);
    assert!(snap(&w, &t, "S-1").is_err());
    // An approved link missing from the manifest fails closed.
    let mut unknown = t.issue("S-1").unwrap();
    unknown.links.pop();
    unknown.approved_versions = vec!["removed".into()];
    t.add_issue("S-1", unknown);
    assert!(snap(&w, &t, "S-1").is_err());
}

#[test]
fn the_status_snapshot_respects_the_tracker_audience() {
    let w = world();
    let t = tracker();
    let s = snap(&w, &t, "S-1").unwrap();
    let keys: Vec<_> = s.tickets.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["T-1"], "a ticket from outside the team is left out");
    assert!(
        snap(&w, &t, "S-2").is_err(),
        "a story from outside the team is refused"
    );
}

#[test]
fn a_conflicting_artifact_path_fails_before_any_invalidation_or_copy() {
    let w = world();
    let before = active_text(&w);
    // The staged v4 file collides with different content already in the active directory.
    put(&w.root.join("active"), "design-v4.txt", "squatter");
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let called = Cell::new(false);
    let err = records::activate(
        &w.config,
        &w.root.join("staged/manifest.json"),
        &open(),
        &|_| {
            called.set(true);
            Ok(())
        },
    );
    assert!(format!("{:#}", err.unwrap_err()).contains("already holds other content"));
    assert!(!called.get(), "nothing was invalidated");
    assert_eq!(active_text(&w), before);
    assert_eq!(
        std::fs::read_to_string(w.root.join("active/design-v4.txt")).unwrap(),
        "squatter"
    );
}

#[test]
fn a_source_error_refuses_publishes_nothing_and_invalidates_nothing() {
    let w = world();
    let before = active_text(&w);
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let invalidated = Cell::new(0);
    let err = records::activate_from(
        &w.config,
        &w.root.join("staged/manifest.json"),
        &|| Err(anyhow::anyhow!("GitHub provider status 502")),
        &|_| {
            invalidated.set(invalidated.get() + 1);
            Ok(())
        },
    );
    assert!(format!("{:#}", err.unwrap_err()).contains("open pull source failed"));
    assert_eq!((invalidated.get(), active_text(&w)), (0, before));
    assert!(!w.root.join("active/design-v4.txt").exists());
}

#[test]
fn the_live_source_heads_are_invalidated_then_published() {
    let w = world();
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let seen = std::cell::RefCell::new(vec![]);
    records::activate_from(
        &w.config,
        &w.root.join("staged/manifest.json"),
        &|| Ok(open()),
        &|head| {
            seen.borrow_mut()
                .push((head.to_owned(), active_text(&w).contains("design-v3")));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(*seen.borrow(), [("a".repeat(40), true)]);
    assert!(active_text(&w).contains("design-v4"));
}

#[test]
fn run_branches_name_their_story_and_anything_else_is_refused() {
    assert_eq!(
        records::story_of_branch("agent/acme/app#7-run-3").unwrap(),
        "acme/app#7"
    );
    for bad in ["feature/x", "agent/acme/app#7", "agent/-run-1"] {
        assert!(records::story_of_branch(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_cli_refuses_without_a_gates_config_or_token_and_publishes_nothing() {
    let w = world();
    let before = active_text(&w);
    stage(&w.root.join("staged"), "design-v4", "v4 text");
    let err = records::activate_cli(&w.config, &w.root.join("staged/manifest.json"));
    assert!(err.is_err());
    assert_eq!(active_text(&w), before);
    assert!(!w.root.join("active/design-v4.txt").exists());
    // A bad candidate is refused on its own merits first.
    stage(&w.root.join("bad"), "design-v3", "other text");
    let err = records::activate_cli(&w.config, &w.root.join("bad/manifest.json"));
    assert!(format!("{:#}", err.unwrap_err()).contains("immutable artifact revision changed"));
}

#[test]
fn a_rework_ticket_is_shown_once_as_rework_never_also_as_an_ordinary_ticket() {
    use kod_status::{Stage, records::ReworkRecord};
    use kod_tracker::State;
    let w = world();
    let t = tracker();
    let artifacts = kod_host::artifacts::Artifacts::new(
        w.config.paths.approval_manifest.clone(),
        &w.config.paths.policy_dir,
    )
    .unwrap();
    let rework = vec![ReworkRecord {
        key: "T-1".into(),
        kind: kod_tracker::ReworkKind::Amendment,
        target: Stage::Design,
        state: State::Ready,
    }];
    let s = records::snapshot(&t, &artifacts, "S-1", &|_| Stage::Design, vec![], rework).unwrap();
    assert!(
        s.tickets.is_empty(),
        "T-1 is rework, so it is not also a ticket"
    );
    assert_eq!(s.rework.len(), 1);
}
