use kod_gates::Artifacts as _;
use kod_host::artifacts::Artifacts;
use kod_tool_gateway::docs::Docs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{os::unix::fs::PermissionsExt, path::Path};

fn policy() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policy")
        .canonicalize()
        .unwrap()
}
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
fn manifest(dir: &Path, artifacts: Vec<Value>) {
    put(
        dir,
        "manifest.json",
        &json!({"version": 1, "artifacts": artifacts}).to_string(),
    );
}
fn root() -> tempfile::TempDir {
    let dir = tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    put(dir.path(), "design-v3.txt", "v3 text");
    manifest(dir.path(), vec![entry("design", "design-v3", "v3 text")]);
    dir
}
fn load(dir: &Path) -> anyhow::Result<Artifacts> {
    Artifacts::new(dir.join("manifest.json"), &policy())
}
fn refuses(dir: &Path, artifacts: Vec<Value>, why: &str) {
    manifest(dir, artifacts);
    let error = format!("{:#}", load(dir).err().expect("must refuse"));
    assert!(error.contains(why), "{why}: {error}");
}

#[test]
fn loads_a_valid_manifest_and_pins_revisions() {
    let dir = root();
    let a = load(dir.path()).unwrap();
    let (pins, classes) = a.pins(&["design".into()]).unwrap();
    assert_eq!(pins["design"], "design-v3");
    assert_eq!(classes, ["placeholder"]);
    assert_eq!(a.read("design-v3").unwrap().text, "v3 text");
    assert!(a.pins(&["unknown".into()]).is_err());
    assert!(a.read("design-v9").is_err());
}

#[test]
fn rejects_unapproved_or_unrecorded_entries() {
    let dir = root();
    let good = entry("design", "design-v3", "v3 text");
    let with = |k: &str, v: Value| {
        let mut e = good.clone();
        e[k] = v;
        vec![e]
    };
    refuses(
        dir.path(),
        with("approved_by", json!("mallory")),
        "unknown artifact approver",
    );
    refuses(
        dir.path(),
        with("class", json!("secret")),
        "unknown artifact class",
    );
    refuses(
        dir.path(),
        with("approval_ref", json!("  ")),
        "missing recorded approval reference",
    );
    refuses(
        dir.path(),
        with("sha256", json!("0".repeat(64))),
        "content hash mismatch",
    );
    refuses(
        dir.path(),
        with("path", json!("../escape.txt")),
        "escapes snapshot root",
    );
    refuses(
        dir.path(),
        with("path", json!("/etc/passwd")),
        "escapes snapshot root",
    );
    refuses(
        dir.path(),
        with("approved_at", json!("yesterday")),
        "invalid approval timestamp",
    );
    refuses(dir.path(), vec![good.clone(), good.clone()], "duplicate");
}

#[test]
fn rejects_symlinks_and_non_owner_only_files() {
    let dir = root();
    std::fs::write(dir.path().join("real.txt"), "v3 text").unwrap();
    std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();
    let mut e = entry("design", "design-v3", "v3 text");
    e["path"] = json!("link.txt");
    refuses(dir.path(), vec![e], "");
    let mut e = entry("design", "design-v3", "v3 text");
    e["path"] = json!("design-v3.txt");
    manifest(dir.path(), vec![e]);
    std::fs::set_permissions(
        dir.path().join("design-v3.txt"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(
        load(dir.path()).is_err(),
        "group/world readable artifact accepted"
    );
    std::fs::set_permissions(
        dir.path().join("design-v3.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(
        dir.path().join("manifest.json"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(load(dir.path()).is_err(), "writable manifest accepted");
}

#[test]
fn a_revision_cannot_be_replaced_with_different_content() {
    let dir = root();
    let a = load(dir.path()).unwrap();
    put(dir.path(), "design-v3.txt", "swapped");
    manifest(dir.path(), vec![entry("design", "design-v3", "swapped")]);
    let error = format!("{:#}", a.current_manifest().err().unwrap());
    assert!(
        error.contains("immutable artifact revision changed"),
        "{error}"
    );
    assert_eq!(a.read("design-v3").unwrap().text, "v3 text");
}

#[test]
fn advancing_a_link_keeps_old_pins_readable_and_makes_them_stale() {
    let dir = root();
    let a = load(dir.path()).unwrap();
    let (pins, _) = a.pins(&["design".into()]).unwrap();
    put(dir.path(), "design-v4.txt", "v4 text");
    manifest(dir.path(), vec![entry("design", "design-v4", "v4 text")]);
    assert_eq!(a.current("design").unwrap(), "design-v4");
    // The admitted v3 stays readable, byte for byte; it is no longer current.
    assert_eq!(a.read(&pins["design"]).unwrap().text, "v3 text");
    let stale = kod_gates::check_pins(&["design-v3".into()], &["design".into()], &a);
    assert!(stale.is_err());
    // A removed link fails closed.
    manifest(dir.path(), vec![entry("other", "other-v1", "v3 text")]);
    put(dir.path(), "other-v1.txt", "v3 text");
    assert!(a.current("design").is_err());
}
