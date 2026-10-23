use super::*;
use crate::{broker::Broker, forge::Forge};
use serde_json::{Value, json};

fn intent(f: &Fixture) -> Value {
    let path = std::fs::read_dir(f.root().join("leases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    let record: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    record["pr"].clone()
}

#[test]
fn definite_validation_rejections_allow_corrected_retry() {
    for (error, cause) in [
        (
            json!({"resource":"PullRequest","code":"custom","message":"No commits between main and private-ghs_secret"}),
            "no commits",
        ),
        (
            json!({"resource":"PullRequest","code":"custom","message":"Draft pull requests are not supported in this repository."}),
            "draft pull requests are not supported",
        ),
        (
            json!({"resource":"PullRequest","code":"missing_field","field":"title"}),
            "title is missing",
        ),
        (
            json!({"resource":"PullRequest","code":"invalid","field":"head","message":"ghs_secret"}),
            "head is missing or invalid",
        ),
        (json!("No commits between main and agent/run"), "no commits"),
        // Any other 4xx is still a definite refusal; it must not wedge the run.
        (
            json!({"resource":"PullRequest","code":"custom","message":"A pull request already exists for head."}),
            "request refused",
        ),
        (json!("unknown ghs_secret"), "request refused"),
    ] {
        // Given a definite rejection with an empty reconciliation result.
        let f = Fixture::start();
        let a = f.adapter();
        a.register_run(f.permit()).unwrap();
        f.remote.lock().unwrap().pr_reply = Some((
            422,
            json!({"message":"Validation Failed", "errors":[error]}),
        ));
        // When publication is rejected and retried with the provider accepting it.
        let error = super::publication::publish(&a, &f).unwrap_err().to_string();
        assert!(error.contains(cause), "{error}");
        assert!(!error.contains("ghs_secret"));
        assert!(intent(&f).is_null());
        super::publication::publish(&a, &f).unwrap();
        // Then rejection did not strand the run's idempotency guard.
        assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 2);
        assert_eq!(intent(&f)["created"], 1);
        assert_eq!(intent(&f)["blocked"], false);
    }
}

#[test]
fn ambiguous_responses_remain_uncertain_across_restart() {
    for (status, body) in [
        (503, json!({"message":"ghs_secret"})),
        (201, json!({"draft":false})),
    ] {
        // Given an ambiguous response and an empty provider list.
        let f = Fixture::start();
        let a = f.adapter();
        a.register_run(f.permit()).unwrap();
        f.remote.lock().unwrap().pr_reply = Some((status, body));
        assert!(
            super::publication::publish(&a, &f)
                .unwrap_err()
                .to_string()
                .contains("outcome unknown")
        );
        assert!(!intent(&f).is_null());
        assert!(intent(&f)["created"].is_null());
        drop(a);
        let a = f.adapter();
        a.recover().unwrap();
        // When the same run retries after restart.
        let error = super::publication::publish(&a, &f).unwrap_err().to_string();
        // Then absence never authorizes another POST.
        assert!(error.contains("outcome unknown"), "{error}");
        assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 1);
    }
}

#[test]
fn identifiable_invalid_creation_is_closed_not_orphaned() {
    for variation in [
        "draft",
        "author",
        "head_repo",
        "head",
        "base",
        "sha",
        "url",
        "schema",
    ] {
        for fail_close in [0, 1] {
            // Given a 201 that identifies a created PR but violates its expected identity.
            let f = Fixture::start();
            let a = f.adapter();
            a.register_run(f.permit()).unwrap();
            let t = a.mint("run-1", "acme/app").unwrap();
            let sha = a.commit(&t, &super::commits::text_commit(&f)).unwrap();
            let branch = f.permit().branch();
            a.set_branch(&t, "acme/app", &branch, &sha).unwrap();
            let mut response = json!({"number":27,"html_url":"https://github.com/acme/app/pull/27","state":"open","draft":true,
                "user":{"id":4},"head":{"repo":{"id":1},"ref":branch,"sha":sha},"base":{"repo":{"id":1},"ref":"main"}});
            match variation {
                "draft" => response["draft"] = json!(false),
                "author" => response["user"]["id"] = json!(9),
                "head_repo" => response["head"]["repo"]["id"] = json!(9),
                "head" => response["head"]["ref"] = json!("foreign"),
                "base" => response["base"]["ref"] = json!("foreign"),
                "sha" => response["head"]["sha"] = json!("a".repeat(40)),
                "url" => response["html_url"] = json!("https://evil.invalid/?token=ghs_secret"),
                "schema" => response = json!({"number":27}),
                _ => unreachable!(),
            }
            {
                let mut s = f.remote.lock().unwrap();
                s.pr_reply = Some((201, response));
                s.fail_close = fail_close;
            }
            // When creation fails validation.
            let error = a
                .open_draft_pr(&t, "acme/app", &branch, "title", "body")
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("#27") && !error.contains("ghs_secret"),
                "{error}"
            );
            if fail_close == 1 {
                // An unconfirmed close stays flagged across restart, and lookups refuse.
                assert!(error.contains("human review"), "{error}");
                assert_eq!(intent(&f)["blocked"], true);
                assert_eq!(intent(&f)["created"], 27);
                assert!(!intent(&f).to_string().contains("ghs_secret"));
                assert!(a.find_pr(&t, "acme/app", &branch).is_err());
                a.revoke("run-1").unwrap();
                drop(a);
                let a = f.adapter();
                a.recover().unwrap();
                super::publication::publish(&a, &f).unwrap();
            } else {
                assert!(error.contains("closed"), "{error}");
                assert!(intent(&f).is_null());
                a.open_draft_pr(&t, "acme/app", &branch, "title", "body")
                    .unwrap();
            }
            // Then the invalid PR was closed and a corrected retry published once more.
            assert_eq!(
                f.requests("PATCH", "/repos/acme/app/pulls/27"),
                1 + fail_close as usize
            );
            assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 2);
            assert_eq!(intent(&f)["created"], 1);
        }
    }
}

#[test]
fn pull_requests_reject_branches_outside_the_run_before_http() {
    // Given a live run credential.
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let token = a.mint("run-1", "acme/app").unwrap();
    let requests = f.remote.lock().unwrap().requests.len();
    // When the caller requests publication or lookup for an unauthorized branch.
    for branch in ["main", "agent/foreign"] {
        assert!(a.find_pr(&token, "acme/app", branch).is_err());
        assert!(
            a.open_draft_pr(&token, "acme/app", branch, "title", "body")
                .is_err()
        );
    }
    // Then no provider request is sent.
    assert_eq!(f.remote.lock().unwrap().requests.len(), requests);
}
