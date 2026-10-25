use super::*;
mod authentication;
mod commits;
mod contract;
mod fixture;
mod live;
mod objects;
mod open_pulls;
mod pr_outcomes_review;
mod provider;
mod publication;
mod recovery;
mod review_regressions;
mod storage_review;
use fixture::Fixture;

#[test]
fn registration_native_issue_key() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    assert_eq!(f.permit().branch(), "agent/acme/app#7-run-1");
    a.register_run(f.permit()).unwrap();
    assert_eq!(f.remote.lock().unwrap().requests.len(), 0);
}
#[test]
fn registration_conflict() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let mut p = f.permit();
    p.base = "a".repeat(40);
    assert!(a.register_run(p).is_err());
    assert_eq!(f.mutations(), 0);
}

#[test]
fn registration_branch_collision_is_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    let mut p = f.permit();
    p.ticket = "a".into();
    p.run = "b-c".into();
    a.register_run(p.clone()).unwrap();
    p.ticket = "a-b".into();
    p.run = "c".into();
    assert!(a.register_run(p).is_err());
    assert_eq!(f.mutations(), 0);
}

#[test]
fn constructor_rejects_untrusted_inputs_before_http() {
    let c = Config {
        repo: "acme/../other".into(),
        repository_id: 1,
        installation_id: 2,
        base_branch: "main".into(),
        guard_ruleset_id: 3,
        bot_user_id: 4,
    };
    assert!(
        GitHub::new(
            c,
            AppCredentials {
                app_id: 1,
                private_key: "/missing".into()
            },
            "/missing"
        )
        .is_err()
    );
}
