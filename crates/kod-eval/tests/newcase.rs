use kod_eval::{
    case::load_case,
    newcase::{NewCase, new_case},
    workspace::git,
};
use std::{fs, path::Path};

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

fn spec(mirror: &Path, merge: &str, test_paths: &[&str]) -> NewCase {
    NewCase {
        mirror: mirror.to_path_buf(),
        merge_commit: merge.into(),
        id: "PAY-9".into(),
        language: "rust".into(),
        risk_class: "low".into(),
        test_command: "cargo test --quiet".into(),
        test_paths: test_paths.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn creates_a_case_from_a_merged_change() {
    let tmp = tempfile::tempdir().unwrap();
    let m = tmp.path().join("mirror");
    fs::create_dir_all(m.join("tests")).unwrap();
    git(&m, &["init", "-q"]).unwrap();
    fs::write(m.join("src.txt"), "v1\n").unwrap();
    let base = commit(&m, "base");
    fs::write(m.join("src.txt"), "v2\n").unwrap();
    fs::write(m.join("tests/new.sh"), "new\n").unwrap();
    let merge = commit(&m, "merge");

    let cases = tmp.path().join("cases");
    let dir = new_case(&spec(&m, &merge, &["tests"]), &cases).unwrap();
    let held = fs::read_to_string(dir.join("heldout.patch")).unwrap();
    assert!(held.contains("tests/new.sh") && !held.contains("src.txt"));

    // The ticket must be pasted in before the case loads.
    assert!(load_case(&dir).unwrap_err().to_string().contains("ticket"));
    let toml = fs::read_to_string(dir.join("case.toml")).unwrap();
    fs::write(
        dir.join("case.toml"),
        toml.replace("ticket = \"\"\"\n\"\"\"", "ticket = \"Bump it\""),
    )
    .unwrap();
    let c = load_case(&dir).unwrap();
    assert_eq!(c.base_commit, base);
    assert_eq!(c.merge_commit.as_deref(), Some(merge.as_str()));
    assert_eq!(c.test_command, "cargo test --quiet");
    assert!(c.required_tests.is_empty());
    kod_eval::newcase::set_required_tests(&dir, &["calc::adds".into()]).unwrap();
    assert_eq!(load_case(&dir).unwrap().required_tests, ["calc::adds"]);
    assert!(
        fs::read_to_string(dir.join("case.toml"))
            .unwrap()
            .contains("# Choose before the first run"),
        "comments are kept"
    );

    assert!(
        new_case(&spec(&m, &merge, &["tests"]), &cases).is_err(),
        "must not overwrite a case"
    );
    let mut other = spec(&m, &merge, &["docs"]);
    other.id = "PAY-10".into();
    assert!(
        new_case(&other, &cases).is_err(),
        "no test changes under docs/"
    );
}

#[test]
fn check_case_reports_scoring_that_did_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let m = tmp.path().join("mirror");
    fs::create_dir_all(&m).unwrap();
    git(&m, &["init", "-q"]).unwrap();
    fs::write(m.join("src.txt"), "v1\n").unwrap();
    commit(&m, "base");
    fs::write(m.join("src.txt"), "v2\n").unwrap();
    fs::write(m.join("t.txt"), "t\n").unwrap();
    let merge = commit(&m, "merge");
    let cases = tmp.path().join("cases");
    let dir = new_case(&spec(&m, &merge, &["t.txt"]), &cases).unwrap();
    let toml = fs::read_to_string(dir.join("case.toml")).unwrap();
    fs::write(
        dir.join("case.toml"),
        toml.replace("ticket = \"\"\"\n\"\"\"", "ticket = \"x\""),
    )
    .unwrap();
    // A held-back patch that can't apply stops scoring before any container starts.
    fs::write(
        dir.join("heldout.patch"),
        "diff --git a/nope.txt b/nope.txt\n--- a/nope.txt\n+++ b/nope.txt\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    let c = load_case(&dir).unwrap();
    let check = kod_eval::newcase::check_case(
        &c,
        "kod-sandbox",
        &tmp.path().join("out"),
        &tmp.path().join("STOP"),
    )
    .unwrap();
    assert!(
        check
            .problems
            .iter()
            .any(|p| p.contains("did not run for without fix")),
        "{:?}",
        check.problems
    );
    assert!(
        check
            .problems
            .iter()
            .any(|p| p.contains("did not run for with fix")),
        "{:?}",
        check.problems
    );
    assert!(check.required_tests.is_empty());
}
