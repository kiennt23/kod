//! Regressions for the PR 24 review findings that were reproduced.
mod common;
use common::*;
use kod_dispatcher::Dispatcher;
use kod_policy::Policy;
use kod_tracker::fake::FakeIssue;
use std::sync::Arc;
use std::time::Duration;

fn comments_with(h: &Harness, key: &str, text: &str) -> usize {
    h.tracker
        .comments(key)
        .iter()
        .filter(|c| c.contains(text))
        .count()
}

#[test]
fn concurrent_follow_ups_post_a_comment_once() {
    let h = harness();
    *h.flaky.comment_delay.lock().unwrap() = Some(Duration::from_millis(300));
    h.sql("INSERT INTO followups (kind, target, arg, next_at) VALUES ('comment', 'FAKE-1', 'once-only', 0)");
    let ts: Vec<_> = (0..2)
        .map(|_| {
            let d = h.d.clone();
            std::thread::spawn(move || d.work_at(NOW).unwrap())
        })
        .collect();
    for t in ts {
        t.join().unwrap();
    }
    assert_eq!(comments_with(&h, "FAKE-1", "once-only"), 1);
}

#[test]
fn one_failing_follow_up_does_not_skip_the_rest() {
    let h = harness();
    h.sql(
        "INSERT INTO followups (kind, target, arg, next_at) VALUES ('stop_all', '', '1', 0);
         INSERT INTO followups (kind, target, arg, next_at) VALUES ('comment', 'FAKE-1', 'after-the-failure', 0);
         DROP TABLE control;",
    );
    assert!(h.d.work_at(NOW).is_err());
    assert_eq!(comments_with(&h, "FAKE-1", "after-the-failure"), 1);
}

#[test]
fn a_run_id_has_one_spelling() {
    let h = harness();
    h.running();
    for bad in ["run-01", "run-+1", "run-001"] {
        assert!(
            h.d.finished(bad, kod_dispatcher::RunState::Blocked, "x")
                .is_err(),
            "{bad}"
        );
    }
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
}

#[test]
fn a_publish_error_after_the_pr_exists_still_ends_done() {
    let h = harness();
    h.running();
    h.gw.fail_publish
        .store(true, std::sync::atomic::Ordering::SeqCst);
    *h.gw.published.lock().unwrap() = Ok(Some("https://github.example/acme/app/pull/9".into()));
    h.d.finished("run-1", kod_dispatcher::RunState::Done, "ok")
        .unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert!(h.last_comment("FAKE-1").contains("pull/9"));
}

#[test]
fn an_edit_cancels_only_the_run_it_compared() {
    let h = harness();
    h.running();
    let mut i = h.tracker.issue("FAKE-1").unwrap();
    i.title = "Edited title".into();
    h.tracker.add_issue("FAKE-1", i);
    let db = h.db.clone();
    // While the edit is being read, run-1 ends and run-2 takes the ticket.
    *h.flaky.on_get_issue.lock().unwrap() = Some(Box::new(move || {
        let c = kod_dispatcher::store::open(&db).unwrap();
        c.execute_batch(
            "UPDATE runs SET state = 'canceled' WHERE seq = 1;
             INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'newer', 'running');",
        )
        .unwrap();
    }));
    h.deliver(&edited(&h.tracker, "d-2", "FAKE-1"));
    assert_eq!(
        h.states(),
        [
            ("run-1".to_owned(), "canceled"),
            ("run-2".to_owned(), "running")
        ]
    );
}

#[test]
fn the_kill_switch_redacts_its_reason_like_every_other_ending() {
    let owner = "sk-abcdefghijklmnopqrstuvwxyz0123456789abcd";
    let h = harness();
    let d = Dispatcher::open(
        kod_dispatcher::Config::parse(&format!(
            r#"{{"repo": "acme/app", "owner": "{owner}", "backup": null}}"#
        ))
        .unwrap(),
        &h.dir.path().join("other.db"),
        Policy::load(&policy_dir()).map(Arc::new),
        Arc::new(kod_audit::AuditLog::open(&h.dir.path().join("other-audit.db")).unwrap()),
        h.flaky.clone(),
        h.gw.clone(),
    )
    .unwrap()
    .with_cli_key(owner, OWNER_KEY);
    let (body, sig) = cli("kill", owner, NOW, OWNER_KEY);
    let _ = FakeIssue::default();
    d.signed_command(&body, &sig, NOW).unwrap();
    // No live run here, so queue one first and kill again via a fresh body.
    let c = kod_dispatcher::store::open(&h.dir.path().join("other.db")).unwrap();
    c.execute_batch("UPDATE control SET halted = 0").unwrap();
    c.execute_batch("INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'r', 'running')")
        .unwrap();
    let (body, sig) = cli("kill", owner, NOW + 1, OWNER_KEY);
    d.signed_command(&body, &sig, NOW + 1).unwrap();
    let reason: String = c
        .query_row("SELECT reason FROM runs WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    assert!(reason.contains("kill"), "{reason}");
    assert!(!reason.contains(owner), "{reason}");
}

#[test]
fn an_edit_does_not_lose_a_label_that_has_not_started() {
    let h = harness();
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    h.d.receive(&edited(&h.tracker, "d-2", "FAKE-1")).unwrap();
    while h.d.work_once().unwrap() {}
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
}
