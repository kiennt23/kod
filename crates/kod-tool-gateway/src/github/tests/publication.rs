use super::*;
use crate::{broker::Broker, forge::Forge};

#[test]
fn pr_paginated_existing() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    publish(&a, &f).unwrap();
    f.remote.lock().unwrap().paginated = true;
    assert!(publish(&a, &f).is_ok());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
    assert_eq!(f.requests("GET", "/repos/acme/app/pulls"), 3);
}
#[test]
fn pr_foreign_pagination_is_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    publish(&a, &f).unwrap();
    {
        let mut s = f.remote.lock().unwrap();
        s.paginated = true;
        s.foreign_link = true;
    }
    assert!(publish(&a, &f).is_err());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
}
pub(super) fn publish(a: &GitHub, f: &Fixture) -> Result<String> {
    let t = a.mint("run-1", "acme/app")?;
    let result = (|| {
        let sha = a.commit(&t, &super::commits::text_commit(f))?;
        a.set_branch(&t, "acme/app", &f.permit().branch(), &sha)?;
        a.open_draft_pr(&t, "acme/app", &f.permit().branch(), "change", "evidence")
    })();
    a.revoke("run-1")?;
    result
}
#[test]
fn pr_lost_reply_survives_restart_without_second_post() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    {
        let mut s = f.remote.lock().unwrap();
        s.lost_pr = true;
        s.hide_pr = true;
    }
    assert!(publish(&a, &f).is_err());
    drop(a);
    f.remote.lock().unwrap().hide_pr = false;
    let a = f.adapter();
    a.recover().unwrap();
    a.register_run(f.permit()).unwrap();
    assert!(publish(&a, &f).is_ok());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
    assert_eq!(f.remote.lock().unwrap().prs.len(), 1);
}
#[test]
fn pr_hidden_uncertain_blocks_retry() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    {
        let mut s = f.remote.lock().unwrap();
        s.lost_pr = true;
        s.hide_pr = true;
    }
    assert!(publish(&a, &f).is_err());
    assert!(publish(&a, &f).is_err());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
}
#[test]
fn pr_foreign_author() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    publish(&a, &f).unwrap();
    f.remote.lock().unwrap().foreign = true;
    assert!(publish(&a, &f).is_err());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
}
#[test]
fn pr_ambiguous_candidates() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    publish(&a, &f).unwrap();
    f.remote.lock().unwrap().ambiguous = true;
    assert!(publish(&a, &f).is_err());
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
}
#[test]
fn refs_wrong_run() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    let sha = a.commit(&t, &super::commits::text_commit(&f)).unwrap();
    let before = f.remote.lock().unwrap().requests.len();
    for branch in ["main", "agent/foreign", "refs/tags/v1"] {
        assert!(a.set_branch(&t, "acme/app", branch, &sha).is_err());
        assert!(a.delete_branch(&t, "acme/app", branch).is_err());
    }
    assert_eq!(before, f.remote.lock().unwrap().requests.len());
}
#[test]
fn refs_divergent() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    let sha = a.commit(&t, &super::commits::text_commit(&f)).unwrap();
    let branch = f.permit().branch();
    f.remote.lock().unwrap().refs.insert(branch, "a".repeat(40));
    assert!(
        a.set_branch(&t, "acme/app", &f.permit().branch(), &sha)
            .is_err()
    );
    assert!(
        a.delete_branch(&t, "acme/app", &f.permit().branch())
            .is_err()
    );
    assert_eq!(f.requests("POST", "/repos/acme/app/git/refs"), 0);
}
#[test]
fn refs_lost_create_reply_and_delete_twice() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    let sha = a.commit(&t, &super::commits::text_commit(&f)).unwrap();
    f.remote.lock().unwrap().lost_ref = true;
    a.set_branch(&t, "acme/app", &f.permit().branch(), &sha)
        .unwrap();
    a.delete_branch(&t, "acme/app", &f.permit().branch())
        .unwrap();
    a.delete_branch(&t, "acme/app", &f.permit().branch())
        .unwrap();
    assert!(f.remote.lock().unwrap().refs.is_empty());
    assert_eq!(f.requests("POST", "/repos/acme/app/git/refs"), 1);
}
