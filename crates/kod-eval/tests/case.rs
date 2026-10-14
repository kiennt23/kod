mod common;
use common::{SHA, case_toml};
use kod_eval::case::{EndState, Split, load_case, load_cases, parse_size};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn write_case(root: &Path, dir: &str, toml: &str, heldout: Option<&str>) -> PathBuf {
    let d = root.join(dir);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("case.toml"), toml).unwrap();
    if let Some(h) = heldout {
        fs::write(d.join("heldout.patch"), h).unwrap();
    }
    d
}

#[test]
fn loads_a_valid_case() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(
        tmp.path(),
        "PAY-1",
        &case_toml("PAY-1"),
        Some("diff --git a/t b/t\n"),
    );
    let c = load_case(&d).unwrap();
    assert_eq!(c.id, "PAY-1");
    assert_eq!(c.split, Split::Dev);
    assert_eq!(c.base_commit, SHA);
    assert_eq!(c.expected_end_state, EndState::Done);
    assert_eq!(
        c.heldout_patch(),
        d.canonicalize().unwrap().join("heldout.patch")
    );
}

#[test]
fn rejects_unknown_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(
        tmp.path(),
        "PAY-1",
        &format!("foo = 1\n{}", case_toml("PAY-1")),
        Some("x"),
    );
    assert!(load_case(&d).is_err());
}

#[test]
fn rejects_short_shas() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(
        tmp.path(),
        "PAY-1",
        &case_toml("PAY-1").replace(SHA, "abc123"),
        Some("x"),
    );
    assert!(
        load_case(&d)
            .unwrap_err()
            .to_string()
            .contains("base_commit")
    );
}

#[test]
fn rejects_an_empty_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = case_toml("PAY-1").replace("Add a --verbose flag", " ");
    let d = write_case(tmp.path(), "PAY-1", &toml, Some("x"));
    assert!(load_case(&d).unwrap_err().to_string().contains("ticket"));
}

#[test]
fn a_case_expected_to_finish_needs_heldout_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(tmp.path(), "PAY-1", &case_toml("PAY-1"), None);
    assert!(
        load_case(&d)
            .unwrap_err()
            .to_string()
            .contains("heldout.patch")
    );
}

#[test]
fn a_blocked_case_needs_no_heldout_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = format!("expected_end_state = \"blocked\"\n{}", case_toml("PAY-1"));
    let d = write_case(tmp.path(), "PAY-1", &toml, None);
    assert_eq!(load_case(&d).unwrap().expected_end_state, EndState::Blocked);
}

#[test]
fn directory_name_must_equal_id() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(tmp.path(), "other", &case_toml("PAY-1"), Some("x"));
    assert!(load_case(&d).is_err());
}

#[test]
fn load_cases_sorts_by_id() {
    let tmp = tempfile::tempdir().unwrap();
    write_case(tmp.path(), "B", &case_toml("B"), Some("x"));
    write_case(tmp.path(), "A", &case_toml("A"), Some("x"));
    let ids: Vec<_> = load_cases(tmp.path())
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, ["A", "B"]);
}

#[test]
fn disk_defaults_to_2g_and_must_be_below_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let d = write_case(tmp.path(), "PAY-1", &case_toml("PAY-1"), Some("x"));
    assert_eq!(load_case(&d).unwrap().budget.disk, "2g");
    for (memory, disk) in [("4g", "4g"), ("1g", "2g"), ("512m", "")] {
        let toml = case_toml("PAY-1").replace(
            "memory = \"4g\"",
            &format!("memory = \"{memory}\"\ndisk = \"{disk}\""),
        );
        let d = write_case(tmp.path(), "PAY-1", &toml, Some("x"));
        let err = load_case(&d).unwrap_err().to_string();
        assert!(err.contains("disk"), "{memory}/{disk}: {err}");
    }
}

#[test]
fn sizes_parse_like_docker() {
    assert_eq!(parse_size("32m"), Some(32 << 20));
    assert_eq!(parse_size("4G"), Some(4 << 30));
    assert_eq!(parse_size("1024"), Some(1024));
    for bad in ["", "m", "1.5g", "2gb", "-1m", "1t"] {
        assert_eq!(parse_size(bad), None, "{bad}");
    }
}
