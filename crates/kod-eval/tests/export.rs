use kod_eval::{export::export_index, workspace::git};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
fn stage(repo: &std::path::Path) {
    git(repo, &["add", "-A"]).unwrap();
}
#[test]
fn index_exports_exact_blobs_and_preserves_executable_mode() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]).unwrap();
    fs::write(repo.join("keep"), "before").unwrap();
    fs::set_permissions(repo.join("keep"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(repo.join("delete"), "delete").unwrap();
    stage(repo);
    git(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "base",
        ],
    )
    .unwrap();
    fs::write(repo.join("keep"), "after").unwrap();
    fs::remove_file(repo.join("delete")).unwrap();
    for (name, body) in [("new", "new"), ("foo", "one"), ("0:foo", "two")] {
        fs::write(repo.join(name), body).unwrap();
    }
    stage(repo);
    let out = tempfile::tempdir().unwrap();
    export_index(repo, out.path(), &"a".repeat(40)).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(out.path().join("manifest.json")).unwrap()).unwrap();
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    for (name, body) in [
        ("keep", "after"),
        ("new", "new"),
        ("foo", "one"),
        ("0:foo", "two"),
    ] {
        let entry = files.iter().find(|e| e["path"] == name).unwrap();
        assert_eq!(
            fs::read_to_string(out.path().join(entry["blob"].to_string())).unwrap(),
            body
        );
        assert_eq!(
            entry["mode"],
            if name == "keep" { "100755" } else { "100644" }
        );
    }
    assert_eq!(
        files.iter().find(|e| e["path"] == "delete").unwrap()["deleted"],
        true
    );
}
#[test]
fn staged_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]).unwrap();
    git(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    )
    .unwrap();
    symlink("/etc/passwd", repo.join("link")).unwrap();
    stage(repo);
    let out = tempfile::tempdir().unwrap();
    assert!(export_index(repo, out.path(), &"a".repeat(40)).is_err());
    assert!(!out.path().join("manifest.json").exists());
}
