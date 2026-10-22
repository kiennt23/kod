mod common;

use anyhow::Result;
use common::*;
use kod_tool_gateway::{
    broker::{Broker, Secret},
    forge::{self, Change, Collected, Commit, FakeGitHub, Forge},
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

fn nothing_published(h: &Harness) {
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
    assert!(h.github.prs().is_empty());
}

#[test]
fn publish_pushes_the_collected_change_and_opens_a_draft_pr() {
    let h = harness();
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(p.branch, "agent/FAKE-1-run-1");
    assert_eq!(
        h.github.branch("acme/app", &p.branch),
        Some(p.commit.clone())
    );
    let c = h.github.commit_of(&p.commit).unwrap();
    assert_eq!(c.base, BASE);
    assert_eq!(c.changes, collected("src/refund.rs").changes);
    assert_eq!(c.committer.name, "kod-agent");
    assert_eq!(c.date, 1_700_000_000);
    let title = &h.tracker.issue("FAKE-1").unwrap().title;
    assert_eq!(
        c.message,
        format!(
            "{title}\n\nKod-Ticket: fake:FAKE-1\nKod-Run: run-1\nCo-Authored-By: kod-agent <kod-agent@users.noreply.example>\n"
        )
    );
    let prs = h.github.prs();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].draft);
    assert_eq!(prs[0].url, p.pr);
    assert_eq!(prs[0].title, format!("FAKE-1: {title}"));
    for line in [
        "Ticket: fake:FAKE-1",
        "Requirement: REQ-7",
        "Run: run-1",
        "- https://docs.example/brief?rev=3",
        "- src/refund.rs\n- old.rs",
        "Test evidence:\ncargo test: 12 passed",
    ] {
        assert!(prs[0].body.contains(line), "{line} not in {}", prs[0].body);
    }
    assert_eq!(h.tracker.prs("FAKE-1"), std::slice::from_ref(&p.pr));
}

#[test]
fn the_sandbox_cannot_publish() {
    let h = harness();
    let e =
        h.gw.call(
            TOKEN,
            "publish",
            &json!({"files": [{"path": "x", "content": "y"}]}),
        )
        .unwrap_err();
    assert_eq!(e, "unknown tool \"publish\"");
    nothing_published(&h);
    assert_eq!(h.github.minted().len(), 0);
}

#[test]
fn publish_needs_a_live_run_and_a_collected_change() {
    let h = harness();
    assert_eq!(h.gw.publish("run-9").unwrap_err(), "unknown run");
    h.workspace.set(
        "run-1",
        Collected {
            changes: vec![],
            test_evidence: String::new(),
        },
    );
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "the change must have 1 to 1000 files, it has 0"
    );
    let twice = Collected {
        changes: vec![collected("a.rs").changes[0].clone(); 2],
        test_evidence: String::new(),
    };
    h.workspace.set("run-1", twice);
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "a.rs is in the change twice"
    );
    let big = Collected {
        changes: vec![Change {
            path: "big.bin".into(),
            content: Some("x".repeat(8 * 1024 * 1024 + 1)),
        }],
        test_evidence: String::new(),
    };
    h.workspace.set("run-1", big);
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "the change is over 8388608 bytes"
    );
    nothing_published(&h);
}

#[test]
fn publish_is_refused_for_paths_the_policy_protects() {
    let h = harness();
    h.workspace.set("run-1", collected("policy/agent.rego"));
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    nothing_published(&h);
}

#[test]
fn a_run_admitted_for_restricted_docs_cannot_publish_to_an_uncleared_repo() {
    let h = harness();
    h.gw.end_run("run-1").unwrap();
    let mut r = run();
    r.data_classes.push("customer-pii".into());
    h.gw.start_run(TOKEN, r).unwrap();
    h.docs
        .add_classed(LINKED_DOC, "a payroll export", "customer-pii");
    h.gw.call(TOKEN, "read_doc", &json!({"id": LINKED_DOC}))
        .unwrap();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused by policy"), "{e}");
    let (_, input) = h.decisions().pop().unwrap();
    assert_eq!(
        input["run"]["data_classes"],
        json!(["placeholder", "customer-pii"])
    );
    nothing_published(&h);
}

#[test]
fn a_retry_after_a_lost_pr_response_finds_the_pr() {
    let h = harness();
    h.github.lose_next_pr();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("connection reset"), "{e}");
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(h.github.prs().len(), 1);
    assert_eq!(h.github.prs()[0].url, p.pr);
}

#[test]
fn every_publish_credential_is_revoked_when_it_returns() {
    let h = harness();
    h.gw.publish("run-1").unwrap();
    h.github.fail_next_pr("rate limited");
    h.gw.publish("run-1").unwrap();
    h.github.fail_next_pr("rate limited");
    let mut fresh = run();
    fresh.id = "run-2".into();
    h.workspace.set("run-2", collected("src/a.rs"));
    h.gw.start_run(&"2".repeat(64), fresh).unwrap();
    assert!(h.gw.publish("run-2").is_err());
    assert_eq!(h.github.minted().len(), 3);
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn ending_runs_revokes_their_credentials() {
    let h = harness();
    // A credential that outlived its publish, as if the revoke had failed.
    let token = h.github.mint("run-1", "acme/app").unwrap();
    h.gw.end_run("run-1").unwrap();
    assert!(h.github.live_tokens().is_empty());
    assert!(h.github.set_branch(&token, "acme/app", "x", BASE).is_err());
    h.github.mint("run-2", "acme/app").unwrap();
    h.gw.end_all().unwrap();
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn ending_a_never_started_run_succeeds_without_revoking_another_run() {
    let h = harness();
    let token = h.github.mint("run-1", "acme/app").unwrap();
    h.gw.end_run("never-started").unwrap();
    h.gw.end_run("never-started").unwrap();
    assert!(h.gw.run(TOKEN).is_some());
    h.github
        .set_branch(&token, "acme/app", "unaffected", BASE)
        .unwrap();
}

#[test]
fn credentials_never_reach_the_reply_or_the_audit_log() {
    let h = harness();
    let leak = "ghs_0123456789abcdef0123456789abcdef";
    h.github
        .fail_next_pr(&format!("token {leak} is rate limited"));
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("rate limited"), "{e}");
    h.github
        .fail_mint(&format!("could not mint for url /r/{TOKEN}/mcp ({leak})"));
    let e2 = h.gw.publish("run-1").unwrap_err();
    for text in [e, e2, h.raw_log()] {
        assert!(!text.contains(leak), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("ghs_fake"), "{text}");
    }
}

#[test]
fn a_cancel_before_the_push_stops_publish() {
    let h = harness();
    let runs = h.runs.clone();
    h.github.on_commit(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
    });
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("canceled during publish"), "{e}");
    nothing_published(&h);
}

#[test]
fn a_cancel_after_the_push_deletes_the_branch_and_opens_no_pr() {
    let h = harness();
    let runs = h.runs.clone();
    h.github.on_set_branch(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
    });
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.contains("canceled during publish"), "{e}");
    nothing_published(&h);
    assert!(h.tracker.prs("FAKE-1").is_empty());
}

#[test]
fn a_canceled_run_is_not_published() {
    let h = harness();
    h.stop("run-1");
    assert_eq!(
        h.gw.publish("run-1").unwrap_err(),
        "refused: the run is canceled"
    );
    nothing_published(&h);
    assert_eq!(h.github.minted().len(), 0);
}

#[test]
fn publish_is_refused_when_the_intent_cannot_be_recorded() {
    let h = harness();
    // The policy decision is written; the intent row is refused.
    let db = rusqlite::Connection::open(&h.audit).unwrap();
    db.execute_batch(
        "CREATE TRIGGER no_intent BEFORE INSERT ON audit
         WHEN json_extract(NEW.payload, '$.kind') = 'tool_call'
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();
    let e = h.gw.publish("run-1").unwrap_err();
    assert!(e.starts_with("refused: the audit write failed"), "{e}");
    assert_eq!(h.decisions().len(), 1);
    assert_eq!(h.github.minted().len(), 0);
    nothing_published(&h);
}

#[test]
fn published_finds_the_pr_of_a_run_that_died_mid_publish() {
    let h = harness();
    assert_eq!(h.gw.published("acme/app", "FAKE-1", "run-1").unwrap(), None);
    let p = h.gw.publish("run-1").unwrap();
    assert_eq!(
        h.gw.published("acme/app", "FAKE-1", "run-1").unwrap(),
        Some(p.pr)
    );
    assert!(h.github.live_tokens().is_empty());
}

#[test]
fn the_fake_forge_meets_the_contract() {
    let gh = FakeGitHub::default();
    gh.add_repo("acme/app", BASE);
    forge::check(&gh, &gh, "acme/app", BASE).unwrap();
}

/// A forge that dates each commit with the clock, as `git commit` does when
/// nothing pins the date.
#[derive(Default)]
struct ClockDated(FakeGitHub, AtomicU64);

impl Broker for ClockDated {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        self.0.mint(run, repo)
    }
    fn revoke(&self, run: &str) -> Result<()> {
        self.0.revoke(run)
    }
    fn revoke_all(&self) -> Result<()> {
        self.0.revoke_all()
    }
}

impl Forge for ClockDated {
    fn commit(&self, token: &Secret, commit: &Commit) -> Result<String> {
        let mut c = commit.clone();
        c.date = 1_700_000_000 + self.1.fetch_add(1, Ordering::SeqCst) as i64;
        self.0.commit(token, &c)
    }
    fn set_branch(&self, t: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()> {
        self.0.set_branch(t, repo, branch, sha)
    }
    fn delete_branch(&self, t: &Secret, repo: &str, branch: &str) -> Result<()> {
        self.0.delete_branch(t, repo, branch)
    }
    fn find_pr(&self, t: &Secret, repo: &str, branch: &str) -> Result<Option<String>> {
        self.0.find_pr(t, repo, branch)
    }
    fn open_draft_pr(
        &self,
        t: &Secret,
        r: &str,
        b: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        self.0.open_draft_pr(t, r, b, title, body)
    }
}

#[test]
fn the_contract_catches_a_forge_that_dates_commits_by_the_clock() {
    let f = ClockDated::default();
    f.0.add_repo("acme/app", BASE);
    let e = forge::check(&f, &f, "acme/app", BASE).unwrap_err();
    assert!(format!("{e:#}").contains("got a new SHA"), "{e:#}");
}

#[test]
fn cancel_revocation_does_not_prevent_branch_cleanup() {
    let h = harness();
    let runs = h.runs.clone();
    let gw = h.gw.clone();
    h.github.on_set_branch(move || {
        runs.stopped.lock().unwrap().insert("run-1".into());
        gw.end_run("run-1").unwrap();
    });
    assert!(h.gw.publish("run-1").is_err());
    assert_eq!(h.github.branch("acme/app", "agent/FAKE-1-run-1"), None);
}
