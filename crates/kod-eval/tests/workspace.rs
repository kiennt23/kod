mod common;
use kod_eval::case::load_case;
use kod_eval::workspace::{
    apply, apply_heldout, extract, git, patch_paths, prepare, protected_hits,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn commit(repo: &Path, msg: &str) -> String {
    git(repo, &["add", "-A"]).unwrap();
    git(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            msg,
        ],
    )
    .unwrap();
    git(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_owned()
}

/// The base commit has src.txt and tests/old.sh. The merge changes src.txt,
/// edits tests/old.sh and adds tests/new.sh.
fn mirror(root: &Path) -> (PathBuf, String, String) {
    let m = root.join("mirror");
    fs::create_dir_all(m.join("tests")).unwrap();
    git(&m, &["init", "-q"]).unwrap();
    fs::write(m.join("src.txt"), "v1\n").unwrap();
    fs::write(m.join("tests/old.sh"), "old v1\n").unwrap();
    let base = commit(&m, "base");
    fs::write(m.join("src.txt"), "v2\n").unwrap();
    fs::write(m.join("tests/old.sh"), "old v2\n").unwrap();
    fs::write(m.join("tests/new.sh"), "new\n").unwrap();
    let merge = commit(&m, "merge");
    (m, base, merge)
}

fn heldout(root: &Path, m: &Path, base: &str, merge: &str) -> PathBuf {
    let p = root.join("heldout.patch");
    fs::write(
        &p,
        git(
            m,
            &[
                "diff",
                "--binary",
                "--no-renames",
                base,
                merge,
                "--",
                "tests",
            ],
        )
        .unwrap(),
    )
    .unwrap();
    p
}

/// The agent's edits, captured the way the entrypoint captures them.
fn agent_patch(root: &Path, m: &Path, base: &str, edit: impl Fn(&Path)) -> PathBuf {
    let scratch = root.join("scratch");
    let b = prepare(m, base, &scratch).unwrap();
    edit(&scratch);
    git(&scratch, &["add", "-A"]).unwrap();
    let p = root.join("agent.patch");
    fs::write(
        &p,
        git(
            &scratch,
            &["diff", "--cached", "--binary", "--no-renames", &b],
        )
        .unwrap(),
    )
    .unwrap();
    p
}

fn read(p: PathBuf) -> String {
    fs::read_to_string(p).unwrap()
}

#[test]
fn prepared_workspace_has_no_history() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, base, merge) = mirror(tmp.path());
    let w = tmp.path().join("w");
    prepare(&m, &base, &w).unwrap();
    assert_eq!(
        git(&w, &["rev-list", "--count", "HEAD"]).unwrap().trim(),
        "1"
    );
    assert_eq!(read(w.join("src.txt")), "v1\n");
    assert!(!w.join("tests/new.sh").exists());
    assert!(
        git(&w, &["cat-file", "-e", &merge]).is_err(),
        "the real fix must not be reachable"
    );
}

#[test]
fn extracted_tree_has_no_git_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, base, _) = mirror(tmp.path());
    let t = tmp.path().join("t");
    extract(&m, &base, &t).unwrap();
    assert_eq!(read(t.join("src.txt")), "v1\n");
    assert!(!t.join(".git").exists());
}

#[test]
fn patch_paths_lists_touched_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, base, merge) = mirror(tmp.path());
    let mut paths = patch_paths(tmp.path(), &heldout(tmp.path(), &m, &base, &merge)).unwrap();
    paths.sort();
    assert_eq!(paths, ["tests/new.sh", "tests/old.sh"]);
    let empty = tmp.path().join("empty.patch");
    fs::write(&empty, "").unwrap();
    assert!(patch_paths(tmp.path(), &empty).unwrap().is_empty());
}

#[test]
fn heldout_tests_replace_agent_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, base, merge) = mirror(tmp.path());
    let held = heldout(tmp.path(), &m, &base, &merge);
    let patch = agent_patch(tmp.path(), &m, &base, |w| {
        fs::write(w.join("src.txt"), "v2\n").unwrap();
        fs::write(w.join("tests/old.sh"), "weakened\n").unwrap();
        fs::write(w.join("tests/new.sh"), "exit 0\n").unwrap();
    });
    let w = tmp.path().join("w");
    let b = prepare(&m, &base, &w).unwrap();
    apply(&w, &patch).unwrap();
    apply_heldout(&w, &b, &held).unwrap();
    assert_eq!(read(w.join("src.txt")), "v2\n");
    assert_eq!(read(w.join("tests/old.sh")), "old v2\n");
    assert_eq!(read(w.join("tests/new.sh")), "new\n");
}

#[test]
fn heldout_never_touches_files_outside_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, base, merge) = mirror(tmp.path());
    let held = heldout(tmp.path(), &m, &base, &merge);
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("new.sh"), "keep me\n").unwrap();
    fs::write(outside.join("old.sh"), "keep me too\n").unwrap();
    let patch = agent_patch(tmp.path(), &m, &base, |w| {
        fs::remove_dir_all(w.join("tests")).unwrap();
        std::os::unix::fs::symlink(&outside, w.join("tests")).unwrap();
    });
    let w = tmp.path().join("w");
    let b = prepare(&m, &base, &w).unwrap();
    apply(&w, &patch).unwrap();
    let _ = apply_heldout(&w, &b, &held); // may refuse; must not follow the link
    assert_eq!(read(outside.join("new.sh")), "keep me\n");
    assert_eq!(read(outside.join("old.sh")), "keep me too\n");
}

#[test]
fn protected_paths_match_directories_and_exact_files() {
    let paths = ["auth/login.rs", "author.rs", "Cargo.lock"].map(String::from);
    let hits = protected_hits(&paths, &["auth/".into(), "Cargo.lock".into()]);
    assert_eq!(hits, ["auth/login.rs", "Cargo.lock"]);
}

/// `--cases evals/cases` and `--out results` are relative. git resolves a
/// relative patch path against `-C <dir>`, not our working directory.
#[test]
fn relative_case_and_patch_paths_work() {
    // Relative to the crate directory, where cargo runs integration tests.
    let tmp = tempfile::tempdir_in(".").unwrap();
    let rel = Path::new(tmp.path().file_name().unwrap());
    let (m, base, merge) = mirror(rel);
    let dir = rel.join("cases/PAY-1");
    fs::create_dir_all(&dir).unwrap();
    fs::rename(heldout(rel, &m, &base, &merge), dir.join("heldout.patch")).unwrap();
    fs::write(
        dir.join("case.toml"),
        common::case_toml("PAY-1").replace(common::SHA, &base),
    )
    .unwrap();
    let w = rel.join("w");
    let b = prepare(&m, &base, &w).unwrap();
    let mut paths = patch_paths(&w, &dir.join("heldout.patch")).unwrap();
    paths.sort();
    assert_eq!(paths, ["tests/new.sh", "tests/old.sh"]);
    let case = load_case(&dir).unwrap();
    assert!(case.dir.is_absolute());
    apply_heldout(&w, &b, &case.heldout_patch()).unwrap();
    assert_eq!(read(w.join("tests/new.sh")), "new\n");
    apply(&w, &dir.join("heldout.patch")).unwrap_err(); // already applied, so it must refuse
}
