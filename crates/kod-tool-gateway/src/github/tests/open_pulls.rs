use super::*;
use serde_json::json;
fn pr(n: u64, branch: &str, sha: &str, user: u64, repo: u64) -> serde_json::Value {
    json!({"number":n,"html_url":format!("https://github.com/acme/app/pull/{n}"),"state":"open","draft":false,
        "user":{"id":user},"head":{"repo":{"id":repo},"ref":branch,"sha":sha},"base":{"repo":{"id":1},"ref":"main"}})
}
#[test]
fn open_pulls_lists_only_our_bot_agent_branches_and_revokes_its_token() {
    let f = fixture::Fixture::start();
    let a = f.adapter();
    f.remote.lock().unwrap().prs = vec![
        pr(1, "agent/acme/app#7-run-3", &"a".repeat(40), 4, 1),
        pr(2, "feature/human", &"b".repeat(40), 8, 1),
        pr(3, "agent/acme/app#8-run-4", &"c".repeat(40), 4, 99),
    ];
    let open = a.open_pulls().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (
            open[0].number,
            open[0].branch.as_str(),
            open[0].head.as_str()
        ),
        (1, "agent/acme/app#7-run-3", "a".repeat(40).as_str())
    );
    assert_eq!(f.live(), 0, "the read token is revoked");
}
#[test]
fn a_provider_error_is_an_error_and_still_revokes() {
    let f = fixture::Fixture::start();
    let a = f.adapter();
    f.remote.lock().unwrap().prs = vec![pr(1, "agent/x-run-1", "short", 4, 1)];
    assert!(a.open_pulls().is_err(), "malformed head sha refuses");
    assert_eq!(f.live(), 0);
}
