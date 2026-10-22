mod common;

use axum::{body::Body, http::Request};
use common::*;
use kod_dispatcher::{Received, RunState, router};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

#[test]
fn a_labelled_ticket_is_admitted_and_started() {
    let h = harness();
    h.running();
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    assert_eq!(h.workflow("FAKE-1"), "Doing");
    let run = &h.d.runs().unwrap()[0];
    assert_eq!(
        run.revision,
        kod_dispatcher::revision(&issue(&h.tracker, "FAKE-1"))
    );
    // The delegate decision is in the audit log.
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%\"delegate\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_webhook_that_is_not_genuine_is_refused_and_never_queued() {
    let h = harness();
    let mut w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    w.body = w
        .body
        .iter()
        .map(|b| if *b == b'1' { b'2' } else { *b })
        .collect();
    assert!(matches!(h.d.receive(&w).unwrap(), Received::Refused(_)));
    w.headers.clear();
    assert!(matches!(h.d.receive(&w).unwrap(), Received::Refused(_)));
    assert!(!h.d.work_once().unwrap());
    assert!(h.gw.calls().is_empty());
}

#[test]
fn a_duplicate_delivery_never_starts_a_second_run() {
    let h = harness();
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
    assert_eq!(h.d.receive(&w).unwrap(), Received::Duplicate);
    while h.d.work_once().unwrap() {}
    // After a restart too.
    let again = h.reopen();
    assert_eq!(again.receive(&w).unwrap(), Received::Duplicate);
    while again.work_once().unwrap() {}
    assert_eq!(h.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    assert_eq!(h.states().len(), 1);
}

#[test]
fn a_replay_under_a_new_unsigned_delivery_id_is_a_duplicate() {
    let h = harness();
    h.flaky.header_ids.store(true, Ordering::SeqCst);
    let mut w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    w.headers.push(("x-delivery".into(), "h-1".into()));
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
    w.headers[1].1 = "h-2".into();
    assert_eq!(h.d.receive(&w).unwrap(), Received::Duplicate);
}

#[test]
fn a_delivery_whose_write_failed_is_queued_on_the_retry() {
    let h = harness();
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(h.d.receive(&w).is_err(), "the store is busy");
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(h.d.receive(&w).unwrap(), Received::Queued);
}

#[test]
fn a_failing_delivery_backs_off_without_holding_up_other_tickets() {
    let h = harness();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    assert!(h.d.work_at(NOW).unwrap());
    assert!(
        h.d.work_at(NOW).unwrap(),
        "FAKE-2 is not stuck behind FAKE-1"
    );
    assert!(!h.d.work_at(NOW).unwrap(), "FAKE-1 waits for its backoff");
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    assert_eq!(h.d.runs().unwrap()[0].ticket, "FAKE-2");
    // Retried with growing backoff, then a dead letter, said on the ticket.
    let mut t = NOW;
    for _ in 1..kod_dispatcher::MAX_ATTEMPTS {
        t += 1000;
        assert!(h.d.work_at(t).unwrap());
    }
    assert!(!h.d.work_at(t + 1000).unwrap());
    assert_eq!(
        h.count("SELECT count(*) FROM deliveries WHERE done = 2 AND error LIKE '%timed out%'"),
        1
    );
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("gave up on an event for this ticket after 5 attempts"),
        "{c}"
    );
}

#[test]
fn a_tracker_that_is_down_never_holds_up_a_start_and_is_retried() {
    let h = harness();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    assert!(h.d.work_at(NOW).unwrap());
    assert_eq!(h.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    assert_eq!(h.workflow("FAKE-1"), "Backlog");
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Backlog", "still backing off");
    h.d.work_at(NOW + 2).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Doing");
}

#[test]
fn concurrent_admissions_respect_the_wip_limit() {
    let h = harness();
    h.d.receive(&label(&h.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| h.d.work_once().unwrap());
        }
    });
    while h.d.work_once().unwrap() {}
    assert_eq!(h.states().len(), 1, "{:?}", h.states());
    let refused = [h.last_comment("FAKE-1"), h.last_comment("FAKE-2")];
    assert!(
        refused.iter().any(|c| c.contains("WIP limit reached (1)")),
        "{refused:?}"
    );
}

#[test]
fn a_run_left_publishing_by_a_crash_is_finished_on_restart() {
    let h = harness();
    h.running();
    h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
    *h.gw.published.lock().unwrap() = Ok(Some("https://github.example/acme/app/pull/7".into()));
    h.reopen();
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert!(
        h.gw.calls()
            .contains(&"find_published run-1 FAKE-1".to_owned())
    );
    assert!(h.last_comment("FAKE-1").contains("pull/7"));
    assert_eq!(h.workflow("FAKE-1"), "Review");
}

#[test]
fn a_run_left_publishing_with_no_pr_needs_a_human_on_restart() {
    for answer in [Ok(None), Err("fake github: down".to_owned())] {
        let h = harness();
        h.running();
        h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
        *h.gw.published.lock().unwrap() = answer;
        h.reopen();
        assert_eq!(h.states(), [("run-1".to_owned(), "needs_human")]);
        assert!(h.d.stopped("run-1").unwrap());
        assert!(h.gw.calls().contains(&"stop run-1".to_owned()));
    }
}

#[test]
fn stopped_is_true_for_ended_and_unknown_runs() {
    let h = harness();
    h.running();
    assert!(!h.d.stopped("run-1").unwrap());
    assert!(h.d.stopped("run-2").unwrap());
    assert!(h.d.stopped("FAKE-1").unwrap());
    h.d.finished("run-1", RunState::Blocked, "stuck").unwrap();
    assert!(h.d.stopped("run-1").unwrap());
}

#[test]
fn admission_refusals_are_commented_with_the_reason() {
    let h = harness();
    h.deliver(&label(&h.tracker, "d-1", "FAKE-1", "mallory"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("mallory is not on the team")
    );
    h.deliver(&label(&h.tracker, "d-2", "FAKE-3", "dev"));
    let c = h.last_comment("FAKE-3");
    for why in [
        "no acceptance criteria",
        "no requirement ID",
        "links no approved document",
        "no risk class",
    ] {
        assert!(c.contains(why), "{c}");
    }
    assert!(h.states().is_empty());
    assert!(h.gw.calls().is_empty());
}

#[test]
fn wip_is_counted_and_reserved_in_the_admission() {
    let h = harness();
    h.running();
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert!(
        h.last_comment("FAKE-2").contains("WIP limit reached (1)"),
        "{}",
        h.last_comment("FAKE-2")
    );
    // A second label on a ticket with a live run is refused too.
    h.deliver(&label(&h.tracker, "d-3", "FAKE-1", "dev"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("already has a live run, run-1")
    );
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    // When run-1 ends, FAKE-2 is admitted.
    h.d.finished("run-1", RunState::NeedsHuman, "stuck")
        .unwrap();
    h.deliver(&label(&h.tracker, "d-4", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn a_policy_that_failed_to_load_refuses_every_admission() {
    let h = harness_with(|_| {
        Err(anyhow::anyhow!(
            "delegation.json: wip_limit must be a number"
        ))
    });
    h.deliver(&label(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert!(
        h.last_comment("FAKE-1")
            .contains("the policy is unavailable: delegation.json")
    );
    assert!(h.states().is_empty());
}

#[test]
fn a_finished_run_is_published_then_done() {
    let h = harness();
    h.running();
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.gw.calls()[1..], ["publish run-1", "stop run-1"]);
    assert_eq!(h.gw.revoked(), ["run-1"]);
    assert_eq!(h.states(), [("run-1".to_owned(), "done")]);
    assert_eq!(h.workflow("FAKE-1"), "Review");
    assert!(
        h.last_comment("FAKE-1")
            .contains("published https://github.example")
    );
    // Ended is final: no second finish, no second publish.
    assert!(h.d.finished("run-1", RunState::Done, "again").is_err());
    assert!(h.d.finished("run-1", RunState::Blocked, "again").is_err());
    assert_eq!(h.gw.calls().len(), 3);
}

#[test]
fn a_cancel_in_the_last_publish_window_marks_the_pr() {
    let h = harness();
    h.running();
    // The run is canceled after the gateway's last check, and the PR opens.
    let db = h.db.clone();
    *h.gw.on_publish.lock().unwrap() = Some(Box::new(move || {
        kod_dispatcher::store::open(&db)
            .unwrap()
            .execute("UPDATE runs SET state = 'canceled' WHERE seq = 1", [])
            .unwrap();
    }));
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("https://github.example/acme/app/pull/1 is not Kod's work: close it"),
        "{c}"
    );
    let audit = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = audit
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%canceled_after_publish%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_failed_publish_or_start_ends_the_run_and_says_why() {
    let h = harness();
    h.running();
    h.gw.fail_publish.store(true, Ordering::SeqCst);
    h.d.finished("run-1", RunState::Done, "tests pass").unwrap();
    // Not blocked: a failed publish is not a bounce of the work.
    assert_eq!(h.states(), [("run-1".to_owned(), "needs_human")]);
    assert_eq!(h.workflow("FAKE-1"), "Waiting");
    assert!(h.last_comment("FAKE-1").contains("publish failed"));
    h.gw.fail_start.store(true, Ordering::SeqCst);
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "needs_human"));
    assert!(h.last_comment("FAKE-2").contains("the run did not start"));
    // A half-started sandbox is stopped.
    assert_eq!(h.gw.calls().last().unwrap(), "stop run-2");
}

#[test]
fn finished_takes_only_an_ending_for_a_running_run() {
    let h = harness();
    h.running();
    for state in [RunState::Queued, RunState::Running, RunState::Canceled] {
        assert!(h.d.finished("run-1", state, "x").is_err());
    }
    assert!(h.d.finished("run-9", RunState::Done, "x").is_err());
    assert!(h.d.finished("FAKE-1", RunState::Done, "x").is_err());
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
}

#[test]
fn the_webhook_route_answers_by_outcome() {
    let h = harness();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let post = |w: &kod_tracker::Webhook| {
        let mut r = Request::post("/webhook");
        for (k, v) in &w.headers {
            r = r.header(k, v);
        }
        let req = r.body(Body::from(w.body.clone())).unwrap();
        rt.block_on(router(h.d.clone()).oneshot(req))
            .unwrap()
            .status()
            .as_u16()
    };
    let w = label(&h.tracker, "d-1", "FAKE-1", "dev");
    assert_eq!(post(&w), 202);
    assert_eq!(post(&w), 200, "a duplicate");
    let mut forged = label(&h.tracker, "d-2", "FAKE-1", "dev");
    forged.headers[0].1 = "0".repeat(64);
    assert_eq!(post(&forged), 401);
    let other = h.tracker.webhook(&serde_json::json!({"delivery": "d-3", "type": "label_added", "issue": "FAKE-1", "label": "frontend", "user": "dev"}));
    assert_eq!(post(&other), 200);
}

use kod_dispatcher::{Dispatcher, Gateway, RunStart};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

struct BlockingGateway {
    inner: Arc<FakeGateway>,
    entered: mpsc::Sender<()>,
    release: (Mutex<bool>, Condvar),
}

impl Gateway for BlockingGateway {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)?;
        let first = self
            .inner
            .calls()
            .iter()
            .filter(|c| c.starts_with("start "))
            .count()
            == 1;
        if first {
            self.entered.send(()).unwrap();
            let (lock, ready) = &self.release;
            drop(
                ready
                    .wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
        }
        Ok(())
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.inner.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.inner.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        self.inner.publish(run)
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(run)
    }
}

#[test]
fn concurrent_workers_start_a_queued_run_once() {
    let h = harness();
    let (entered, rx) = mpsc::channel();
    let gateway = Arc::new(BlockingGateway {
        inner: h.gw.clone(),
        entered,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = Arc::new(
        Dispatcher::open(
            kod_dispatcher::Config::parse(CONFIG).unwrap(),
            &h.db,
            kod_policy::Policy::load(&policy_dir()).map(Arc::new),
            Arc::new(kod_audit::AuditLog::open(&h.audit).unwrap()),
            h.flaky.clone(),
            gateway.clone(),
        )
        .unwrap(),
    );
    h.queued("FAKE-1");
    let first_d = d.clone();
    let first = std::thread::spawn(move || first_d.work_once());
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = d.work_once();
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    first.join().unwrap().unwrap();
    second.unwrap();
    assert_eq!(
        h.gw.calls()
            .iter()
            .filter(|c| c.starts_with("start "))
            .count(),
        1
    );
    assert!(!h.gw.calls().contains(&"stop run-1".to_owned()));
}

const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn a_policy_load_error_with_a_secret_is_redacted_in_the_comment() {
    let h = harness_with(|_| Err(anyhow::anyhow!("cannot read policy at {SECRET}")));
    h.deliver(&label(&h.tracker, "d-1", "FAKE-1", "dev"));
    let c = h.last_comment("FAKE-1");
    assert!(c.contains("the policy is unavailable"), "{c}");
    assert!(!c.contains(SECRET), "{c}");
}

#[test]
fn a_runner_reason_with_a_secret_is_redacted_in_the_store_and_comment() {
    let h = harness();
    h.running();
    h.d.finished(
        "run-1",
        RunState::Blocked,
        &format!("stuck, token {SECRET}"),
    )
    .unwrap();
    assert!(!h.last_comment("FAKE-1").contains(SECRET));
    assert!(!h.d.runs().unwrap()[0].reason.contains(SECRET));
}
