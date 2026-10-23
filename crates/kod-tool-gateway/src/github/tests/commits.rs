use super::*;
use crate::{
    broker::Broker,
    forge::{Change, Commit, Forge, Identity},
};

#[test]
fn commit_unsupported_remote_entries_are_refused_before_writes() {
    for case in ["symlink", "gitlink", "binary", "invalid-utf8"] {
        let f = Fixture::start();
        {
            let s = f.remote.lock().unwrap();
            let repo = &s.repo;
            match case {
                "symlink" => {
                    std::os::unix::fs::symlink("untouched.txt", repo.join("bad")).unwrap();
                    objects::git(repo, &["add", "bad"], None);
                }
                "gitlink" => {
                    let base = objects::git(repo, &["rev-parse", "HEAD"], None);
                    objects::git(
                        repo,
                        &[
                            "update-index",
                            "--add",
                            "--cacheinfo",
                            "160000",
                            &base,
                            "bad",
                        ],
                        None,
                    );
                }
                "binary" => {
                    std::fs::write(repo.join("bad"), b"binary\0data").unwrap();
                    objects::git(repo, &["add", "bad"], None);
                }
                "invalid-utf8" => {
                    std::fs::write(repo.join("bad"), [255u8, 254]).unwrap();
                    objects::git(repo, &["add", "bad"], None);
                }
                _ => unreachable!(),
            }
            objects::git(repo, &["commit", "-m", "unsupported"], None);
        }
        let a = f.adapter();
        a.register_run(f.permit()).unwrap();
        let t = a.mint("run-1", "acme/app").unwrap();
        let mut c = text_commit(&f);
        c.changes.push(Change {
            path: "bad".into(),
            content: None,
        });
        assert!(a.commit(&t, &c).is_err(), "accepted {case}");
        assert_eq!(f.mutations(), 0);
    }
}
pub(super) fn text_commit(f: &Fixture) -> Commit {
    Commit {
        repo: "acme/app".into(),
        base: f.permit().base,
        changes: vec![
            Change {
                path: "bin/check".into(),
                content: Some("#!/bin/sh\nexit 1\n".into()),
            },
            Change {
                path: "new.txt".into(),
                content: Some("new\n".into()),
            },
            Change {
                path: "old.txt".into(),
                content: None,
            },
        ],
        message: "change\n".into(),
        committer: Identity {
            name: "kod-agent".into(),
            email: "agent@kod.invalid".into(),
        },
        date: 1_700_000_000,
    }
}
#[test]
fn commit_is_repeatable_and_preserves_regular_modes() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    let c = text_commit(&f);
    let first = a.commit(&t, &c).unwrap();
    assert_eq!(first, a.commit(&t, &c).unwrap());
    let repo = &f.remote.lock().unwrap().repo;
    assert!(objects::git(repo, &["ls-tree", &first, "bin/check"], None).starts_with("100755 blob"));
    assert_eq!(
        objects::git(repo, &["show", &format!("{first}:new.txt")], None),
        "new"
    );
    assert_eq!(
        objects::git(repo, &["show", &format!("{first}:untouched.txt")], None),
        "keep"
    );
    assert_eq!(
        objects::git(repo, &["ls-tree", &first, "old.txt"], None),
        ""
    );
}
#[test]
fn commit_preflight_rejects_unsafe_tail() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    for path in [
        "../escape",
        ".git/config",
        "a\\b",
        "bin",
        "new.txt",
        "bad\nname",
        "/absolute",
        "bin/check/child",
    ] {
        let mut c = text_commit(&f);
        c.changes.push(Change {
            path: path.into(),
            content: Some("bad".into()),
        });
        assert!(a.commit(&t, &c).is_err(), "accepted {path}");
        assert_eq!(f.mutations(), 0);
    }
}
#[test]
fn commit_truncated_tree_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    f.remote.lock().unwrap().truncated = true;
    assert!(a.commit(&t, &text_commit(&f)).is_err());
    assert_eq!(f.mutations(), 0);
}
