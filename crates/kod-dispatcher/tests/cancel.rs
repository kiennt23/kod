mod common;

use common::*;
use kod_dispatcher::RunState;
use kod_policy::Policy;
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn a_tracker_failure_never_stops_the_stop() {
    let h = harness();
    h.running();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(
        h.count("SELECT done FROM deliveries WHERE delivery = 'd-2'"),
        1
    );
    // The ticket catches up once the tracker is back.
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(h.last_comment("FAKE-1").contains("removed by dev"));
}

#[test]
fn a_failed_stop_is_retried() {
    let h = harness();
    h.running();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    h.gw.fail_stop.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 1"),
        1
    );
}

#[test]
fn a_stop_is_never_given_up_and_raises_the_alarm_once() {
    let h = harness();
    h.running();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    // The credential goes at once, whatever the sandbox does.
    assert_eq!(h.gw.revoked(), ["run-1"]);
    for i in 1..=8 {
        h.d.work_at(NOW + 1000 * i).unwrap();
    }
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 0"),
        1
    );
    let alarms = h
        .tracker
        .comments("FAKE-1")
        .iter()
        .filter(|c| c.contains("has not confirmed the stop of run-1 after 5 attempts"))
        .count();
    assert_eq!(alarms, 1);
    let audit = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = audit
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%stop_overdue%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    h.gw.fail_stop.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 10_000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop' AND done = 1"),
        1
    );
}

#[test]
fn a_cancel_does_not_wait_behind_a_failing_event_for_its_ticket() {
    let h = harness();
    h.running();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.deliver(&label(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    assert_eq!(
        h.count("SELECT done FROM deliveries WHERE delivery = 'd-2'"),
        0,
        "the failing event must still be pending"
    );
    h.deliver(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
}

#[test]
fn a_cancel_jumps_the_queue() {
    let h = harness();
    h.running();
    // A label for another ticket is queued first, the cancel second.
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-2", "dev"))
        .unwrap();
    h.d.receive(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0], ("run-1".to_owned(), "canceled"));
    assert_eq!(
        h.count("SELECT done FROM deliveries WHERE delivery = 'd-2'"),
        0
    );
}

#[test]
fn a_label_removal_needs_neither_the_policy_nor_the_audit_log() {
    let h = harness_with(|_| Err(anyhow::anyhow!("routing.json is missing")));
    h.queued("FAKE-1");
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("start")));
    lock.execute_batch("ROLLBACK").unwrap();
    // The cancel is recorded when the log takes it.
    let h = harness();
    h.running();
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%\"kind\":\"cancel\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_run_canceled_while_starting_is_stopped() {
    let h = harness();
    h.queued("FAKE-1");
    h.sql("UPDATE runs SET state = 'canceled' WHERE seq = 1");
    h.d.work_once().unwrap();
    assert!(h.gw.calls().is_empty(), "rechecked before the start");
}

#[test]
fn removing_the_label_cancels_the_live_run() {
    let h = harness();
    h.running();
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(
        h.last_comment("FAKE-1")
            .contains("the trigger label was removed by dev")
    );
    assert!(h.d.stopped("run-1").unwrap());
    // A canceled run is never published and never moves again.
    assert!(h.d.finished("run-1", RunState::Done, "tests pass").is_err());
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("publish")));
}

#[test]
fn removing_the_label_cancels_a_queued_run_before_it_starts() {
    let h = harness();
    h.queued("FAKE-1");
    h.deliver(&unlabel(&h.tracker, "d-1", "FAKE-1", "dev"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("start")));
}

#[test]
fn an_edit_that_changes_the_ticket_stops_its_run() {
    let h = harness();
    h.running();
    // A change the run made itself (state, links) is not an edit of its inputs.
    h.tracker.add_issue("FAKE-1", {
        let mut i = h.tracker.issue("FAKE-1").unwrap();
        i.links.push("FAKE-9".into());
        i
    });
    h.deliver(&edited(&h.tracker, "d-2", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    h.tracker.add_issue("FAKE-1", {
        let mut i = h.tracker.issue("FAKE-1").unwrap();
        i.acceptance_criteria
            .push("And a third approver for refunds over 1000".into());
        i
    });
    h.deliver(&edited(&h.tracker, "d-3", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls()[1], "stop run-1");
    assert_eq!(h.workflow("FAKE-1"), "Open");
    assert!(
        h.last_comment("FAKE-1")
            .contains("back in ready at the new revision")
    );
    // Labelled again, it runs at the new revision.
    h.deliver(&label(&h.tracker, "d-4", "FAKE-1", "dev"));
    let runs = h.d.runs().unwrap();
    assert_eq!(runs[1].state, RunState::Running);
    assert_ne!(runs[0].revision, runs[1].revision);
}

#[test]
fn the_kill_switch_stops_queued_and_running_runs_and_holds() {
    let h = harness();
    h.running();
    h.queued("FAKE-2");
    h.say("c-1", "kiennt23", "/kod kill");
    assert_eq!(
        h.states(),
        [
            ("run-1".to_owned(), "canceled"),
            ("run-2".to_owned(), "canceled")
        ]
    );
    assert!(h.gw.calls().contains(&"stop_all".to_owned()));
    assert!(
        h.last_comment("FAKE-2")
            .contains("the kill switch, by kiennt23")
    );
    // Held across a restart: nothing new is admitted.
    let again = h.reopen();
    assert!(again.is_halted().unwrap());
    again
        .receive(&label(&h.tracker, "d-2", "FAKE-1", "dev"))
        .unwrap();
    while again.work_once().unwrap() {}
    assert!(
        h.last_comment("FAKE-1")
            .contains("stopped by the kill switch")
    );
    assert_eq!(h.states().len(), 2);
}

#[test]
fn the_owners_kill_switch_works_when_the_policy_fails_to_load() {
    let h = harness_with(|_| Err(anyhow::anyhow!("people.json: owner must be a login")));
    h.queued("FAKE-1");
    h.say("c-1", "KienNT23", "/kod kill");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(h.gw.calls(), ["stop_all", "stop run-1"]);
    // Nobody else's does, and resume needs the policy.
    h.say("c-2", "lead", "/kod kill");
    assert!(
        h.last_comment("FAKE-1")
            .contains("Kod did not kill for lead")
    );
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("the policy is unavailable"), "{e}");
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn a_comment_can_kill_but_never_resume() {
    let h = harness();
    h.say("c-1", "kiennt23", "/kod kill");
    assert!(h.d.is_halted().unwrap());
    h.say("c-2", "kiennt23", "/kod resume");
    let c = h.last_comment("FAKE-1");
    assert!(
        c.contains("resume is accepted only as a signed CLI request"),
        "{c}"
    );
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn a_stop_all_retry_does_not_outlive_a_resume() {
    let h = harness();
    h.running();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(h.d.signed_command(&body, &sig, NOW).is_err());
    let stop_alls = |h: &Harness| h.gw.calls().iter().filter(|c| *c == "stop_all").count();
    let tried = stop_alls(&h);
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW).unwrap();
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    // The kill's run is stopped by its own stop; the new run is not touched.
    assert_eq!(stop_alls(&h), tried);
    let calls = h.gw.calls();
    assert!(calls.contains(&"stop run-1".to_owned()));
    assert!(!calls.contains(&"stop run-2".to_owned()));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn the_owners_kill_switch_works_when_the_audit_log_and_gateway_fail() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("did not confirm it stopped the runs"), "{e}");
    // The runs are canceled and new ones refused even so, and the stop is
    // tried again.
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.d.is_halted().unwrap());
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop_all' AND done = 1"),
        1
    );
}

#[test]
fn the_kill_switch_still_stops_the_sandboxes_when_the_store_fails() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("database is locked"), "{e}");
    lock.execute_batch("ROLLBACK").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW + 1, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW + 1).unwrap();
    assert!(h.d.is_halted().unwrap());
    assert!(h.gw.calls().contains(&"stop_all".to_owned()));
}

#[test]
fn a_tech_lead_uses_the_kill_switch_through_the_policy_and_cannot_resume() {
    let h = harness_with(|dir| {
        Policy::load(&policy_with_people(
            dir,
            json!({"owner": "kiennt23", "backup": null, "tech_leads": ["Lead"]}),
        ))
    });
    h.say("c-1", "mallory", "/kod kill");
    let c = h.last_comment("FAKE-1");
    assert!(c.contains("mallory may not use the kill switch"), "{c}");
    assert!(!h.d.is_halted().unwrap());
    h.running();
    h.say("c-2", "lead", "/kod kill");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    let (body, sig) = cli("resume", "lead", NOW, b"lead's guess");
    let e = format!("{:#}", h.d.signed_command(&body, &sig, NOW).unwrap_err());
    assert!(e.contains("lead has no CLI key"), "{e}");
    assert!(h.d.is_halted().unwrap());
    let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    h.d.signed_command(&body, &sig, NOW).unwrap();
    assert!(!h.d.is_halted().unwrap());
    h.deliver(&label(&h.tracker, "d-2", "FAKE-2", "dev"));
    assert_eq!(h.states()[1], ("run-2".to_owned(), "running"));
}

#[test]
fn a_tech_lead_cancels_one_run_through_the_policy() {
    let h = harness_with(|dir| {
        Policy::load(&policy_with_people(
            dir,
            json!({"owner": "kiennt23", "backup": null, "tech_leads": ["lead"]}),
        ))
    });
    h.running();
    h.say("c-1", "mallory", "/kod cancel");
    assert_eq!(h.states(), [("run-1".to_owned(), "running")]);
    h.say("c-2", "lead", "/kod cancel");
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert!(h.last_comment("FAKE-1").contains("canceled by lead"));
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn other_comments_are_not_commands() {
    let h = harness();
    h.running();
    let w = said(&h.tracker, "c-1", "FAKE-1", "kiennt23", "please /kod kill");
    assert_eq!(h.d.receive(&w).unwrap(), kod_dispatcher::Received::Ignored);
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn a_signed_cli_request_is_checked_before_it_acts() {
    let h = harness();
    h.running();
    let try_it = |body: &[u8], sig: &str, now: i64| {
        format!("{:#}", h.d.signed_command(body, sig, now).unwrap_err())
    };
    let (body, _) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(try_it(&body, &"0".repeat(64), NOW).contains("the signature does not match"));
    let (body, sig) = cli("kill", "kiennt23", NOW, b"a guessed key");
    assert!(try_it(&body, &sig, NOW).contains("the signature does not match"));
    let (body, sig) = cli("kill", "mallory", NOW, OWNER_KEY);
    assert!(try_it(&body, &sig, NOW).contains("mallory has no CLI key"));
    let (body, sig) = cli("kill", "kiennt23", NOW - 301, OWNER_KEY);
    assert!(try_it(&body, &sig, NOW).contains("more than 300 s old"));
    assert!(!h.d.is_halted().unwrap());
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert_eq!(h.d.signed_command(&body, &sig, NOW).unwrap(), "killed");
    assert!(h.d.is_halted().unwrap());
    let (resume, rsig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
    assert_eq!(h.d.signed_command(&resume, &rsig, NOW).unwrap(), "resumed");
    // A replay of the kill is refused.
    assert!(try_it(&body, &sig, NOW).contains("already used"));
    assert!(!h.d.is_halted().unwrap());
}

#[test]
fn the_command_route_answers_by_outcome() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let h = harness();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let post = |body: Vec<u8>, sig: &str| {
        let req = Request::post("/command")
            .header("x-kod-signature", sig)
            .body(Body::from(body))
            .unwrap();
        rt.block_on(kod_dispatcher::router(h.d.clone()).oneshot(req))
            .unwrap()
            .status()
            .as_u16()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (body, sig) = cli("kill", "kiennt23", now, OWNER_KEY);
    assert_eq!(post(body.clone(), "bad"), 403);
    assert_eq!(post(body.clone(), &sig), 200);
    assert_eq!(post(body, &sig), 403, "a replay");
    assert!(h.d.is_halted().unwrap());
}

#[test]
fn owner_kill_stops_sandboxes_while_dispatcher_db_is_locked() {
    let h = harness();
    h.running();
    let lock = rusqlite::Connection::open(&h.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
    assert!(h.d.signed_command(&body, &sig, NOW).is_err());
    assert!(
        h.gw.calls().contains(&"stop_all".to_owned()),
        "{:?}",
        h.gw.calls()
    );
}

#[test]
fn pending_label_does_not_block_the_same_tickets_cancel() {
    let h = harness();
    h.running();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.d.receive(&label(&h.tracker, "d-2", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    h.d.receive(&unlabel(&h.tracker, "d-3", "FAKE-1", "dev"))
        .unwrap();
    h.d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0].1, "canceled");
    *h.flaky.fail_issue.lock().unwrap() = None;
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.states().len(), 1);
}

#[test]
fn retrying_an_old_transition_does_not_undo_a_cancel() {
    let h = harness();
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    h.running();
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "d-2", "FAKE-1", "dev"));
    assert_eq!(h.workflow("FAKE-1"), "Open");
    h.d.work_at(NOW + 1000).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
}
