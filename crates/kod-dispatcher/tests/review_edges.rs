mod common;

use common::*;
use kod_dispatcher::{Dispatcher, Gateway, RunStart, RunState};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

struct SlowKill {
    inner: Arc<FakeGateway>,
    first: AtomicBool,
    entered: mpsc::Sender<()>,
    release: (Mutex<bool>, Condvar),
}

impl Gateway for SlowKill {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.inner.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            let (lock, ready) = &self.release;
            drop(ready.wait_while(lock.lock().unwrap(), |v| !*v).unwrap());
            anyhow::bail!("stop_all timed out");
        }
        self.inner.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        self.inner.publish(run)
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(run)
    }
}

fn with_gateway(h: &Harness, gateway: Arc<dyn Gateway>) -> Arc<Dispatcher> {
    Arc::new(
        Dispatcher::open(
            kod_dispatcher::Config::parse(CONFIG).unwrap(),
            &h.db,
            kod_policy::Policy::load(&policy_dir()).map(Arc::new),
            Arc::new(kod_audit::AuditLog::open(&h.audit).unwrap()),
            h.flaky.clone(),
            gateway,
        )
        .unwrap()
        .with_cli_key("kiennt23", OWNER_KEY),
    )
}

#[test]
fn a_kill_finishing_after_resume_does_not_stop_new_runs() {
    let h = harness();
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    h.running();
    let kill_d = d.clone();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        kill_d.signed_command(&body, &sig, NOW)
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (body, sig) = cli("resume", "kiennt23", NOW + 1, OWNER_KEY);
    let resumed = d.signed_command(&body, &sig, NOW + 1);
    if let Err(e) = &resumed {
        assert!(
            e.to_string().contains("global stop is still in flight"),
            "{e:#}"
        );
        assert!(d.is_halted().unwrap());
    } else {
        d.receive(&label(&h.tracker, "new", "FAKE-2", "dev"))
            .unwrap();
        d.work_at(NOW + 1).unwrap();
    }
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    assert!(kill.join().unwrap().is_err());
    if resumed.is_err() {
        let (body, sig) = cli("resume", "kiennt23", NOW + 2, OWNER_KEY);
        d.signed_command(&body, &sig, NOW + 2).unwrap();
        d.receive(&label(&h.tracker, "new", "FAKE-2", "dev"))
            .unwrap();
        d.work_at(NOW + 2).unwrap();
    }
    let calls = h.gw.calls();
    let start = calls
        .iter()
        .position(|c| c.starts_with("start run-2 "))
        .unwrap();
    assert!(
        !calls[start + 1..].iter().any(|c| c == "stop_all"),
        "{calls:?}"
    );
}

#[test]
fn kill_revokes_before_waiting_for_global_stop() {
    let h = harness();
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    h.running();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let revoked = h.gw.revoked();
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    assert!(kill.join().unwrap().is_err());
    assert!(
        revoked.contains(&"run-1".to_owned()),
        "revoked when stop_all began: {revoked:?}"
    );
}

struct LostPublish(Arc<FakeGateway>);
impl Gateway for LostPublish {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.0.start(run)
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.0.revoke(run)
    }
    fn stop(&self, run: &str) -> anyhow::Result<()> {
        self.0.stop(run)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.0.stop_all()
    }
    fn publish(&self, run: &str) -> anyhow::Result<String> {
        let pr = self.0.publish(run)?;
        *self.0.published.lock().unwrap() = Ok(Some(pr));
        anyhow::bail!("the PR response was lost");
    }
    fn find_published(&self, run: &RunStart) -> anyhow::Result<Option<String>> {
        self.0.find_published(run)
    }
}

#[test]
fn a_canceled_publish_with_a_lost_response_still_marks_the_pr() {
    let h = harness();
    let d = with_gateway(&h, Arc::new(LostPublish(h.gw.clone())));
    h.running();
    let cancel_d = d.clone();
    let tracker = h.tracker.clone();
    *h.gw.on_publish.lock().unwrap() = Some(Box::new(move || {
        cancel_d
            .receive(&unlabel(&tracker, "cancel-publish", "FAKE-1", "dev"))
            .unwrap();
        cancel_d.work_at(NOW).unwrap();
    }));
    d.finished("run-1", RunState::Done, "tests pass").unwrap();
    let reopened = h.reopen();
    reopened.work_at(NOW + 1000).unwrap();
    assert!(h.gw.published.lock().unwrap().as_ref().unwrap().is_some());
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let marked: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%canceled_after_publish%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        marked, 1,
        "the canceled run's existing draft PR was never flagged"
    );
}

#[test]
fn a_canceled_publish_is_reconciled_after_a_crash_and_a_forge_outage() {
    let h = harness();
    h.running();
    h.sql("UPDATE runs SET state = 'publishing' WHERE seq = 1");
    h.sql(
        "INSERT INTO followups(kind,target,arg,next_at)
           VALUES('check_publish','run-1','',9223372036854775807)",
    );
    h.deliver(&unlabel(&h.tracker, "crash-cancel", "FAKE-1", "dev"));
    *h.gw.published.lock().unwrap() = Err("forge unavailable".into());
    let reopened = h.reopen();
    for attempt in 1..=7 {
        reopened.work_at(NOW + attempt * 1000).unwrap();
    }
    assert_eq!(
        h.count("SELECT done FROM followups WHERE kind = 'check_publish'"),
        0
    );
    *h.gw.published.lock().unwrap() = Ok(Some("https://github.example/acme/app/pull/9".into()));
    reopened.work_at(NOW + 10000).unwrap();
    assert!(
        h.last_comment("FAKE-1")
            .contains("pull/9 is not Kod's work: close it")
    );
    assert_eq!(
        h.count("SELECT done FROM followups WHERE kind = 'check_publish'"),
        1
    );
}

#[test]
fn resume_cannot_cross_an_inflight_stop_all_retry() {
    let h = harness();
    h.running();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    h.say("kill-before-retry", "kiennt23", "/kod kill");
    h.sql(&format!(
        "UPDATE followups SET next_at = {NOW} WHERE kind = 'stop_all'"
    ));
    h.gw.fail_stop_all.store(false, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    let retry_d = d.clone();
    let retry = std::thread::spawn(move || retry_d.work_at(NOW + 1000));
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (body, sig) = cli("resume", "kiennt23", NOW + 1, OWNER_KEY);
    let resumed = d.signed_command(&body, &sig, NOW + 1);
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    retry.join().unwrap().unwrap();
    assert!(
        resumed
            .unwrap_err()
            .to_string()
            .contains("global stop is still in flight")
    );
    let (body, sig) = cli("resume", "kiennt23", NOW + 2, OWNER_KEY);
    d.signed_command(&body, &sig, NOW + 2).unwrap();
    d.receive(&label(&h.tracker, "after-retry", "FAKE-2", "dev"))
        .unwrap();
    d.work_at(NOW + 2000).unwrap();
    assert_eq!(h.states()[1].1, "running");
}

struct LateStart {
    inner: Arc<FakeGateway>,
    cancel: Mutex<Option<Box<dyn Fn() + Send>>>,
    token_live: AtomicBool,
}

impl Gateway for LateStart {
    fn start(&self, run: &RunStart) -> anyhow::Result<()> {
        self.inner.start(run)?;
        if let Some(cancel) = self.cancel.lock().unwrap().take() {
            cancel();
        }
        self.token_live.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn revoke(&self, run: &str) -> anyhow::Result<()> {
        self.inner.revoke(run)?;
        self.token_live.store(false, Ordering::SeqCst);
        Ok(())
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
fn cancellation_during_start_revokes_a_token_registered_after_cancel() {
    let h = harness();
    let gateway = Arc::new(LateStart {
        inner: h.gw.clone(),
        cancel: Mutex::new(None),
        token_live: AtomicBool::new(false),
    });
    let d = with_gateway(&h, gateway.clone());
    h.queued("FAKE-1");
    let canceled = Arc::downgrade(&d);
    let tracker = h.tracker.clone();
    *gateway.cancel.lock().unwrap() = Some(Box::new(move || {
        let d = canceled.upgrade().unwrap();
        d.receive(&unlabel(&tracker, "cancel-start", "FAKE-1", "dev"))
            .unwrap();
        d.work_at(NOW).unwrap();
    }));
    d.work_at(NOW).unwrap();
    assert_eq!(h.states()[0].1, "canceled");
    assert!(
        !gateway.token_live.load(Ordering::SeqCst),
        "the start registered a live token after the first revoke"
    );
}

#[test]
fn overdue_global_stop_alert_does_not_depend_on_audit() {
    const CHILD: &str = "KOD_TEST_STOP_ALARM_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let h = harness();
        h.running();
        rusqlite::Connection::open(&h.audit)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_alarm_audit BEFORE INSERT ON audit
             BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
            )
            .unwrap();
        h.gw.fail_stop_all.store(true, Ordering::SeqCst);
        h.say("alarm-kill", "kiennt23", "/kod kill");
        for attempt in 1..=8 {
            h.d.work_at(NOW + attempt * 1000).unwrap();
        }
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "overdue_global_stop_alert_does_not_depend_on_audit",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let alerts: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        alerts
            .iter()
            .filter(|e| e["kind"] == "stop_overdue" && e["call"] == "stop_all")
            .count(),
        1,
        "{alerts:?}"
    );
}

fn hold_audit(h: &Harness) -> rusqlite::Connection {
    let lock = rusqlite::Connection::open(&h.audit).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    lock
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let end = std::time::Instant::now() + Duration::from_millis(1000);
    while !f() {
        assert!(
            std::time::Instant::now() < end,
            "{what} waited on the audit log"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_edit_cancels_the_run_when_the_tracker_is_down() {
    let h = harness();
    h.running();
    *h.flaky.fail_issue.lock().unwrap() = Some("FAKE-1".into());
    h.deliver(&edited(&h.tracker, "e-1", "FAKE-1"));
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    assert_eq!(
        h.count("SELECT count(*) FROM runs WHERE reason LIKE '%revision unknown%'"),
        1
    );
    assert_eq!(h.count("SELECT count(*) FROM deliveries WHERE done = 0"), 0);
}

#[test]
fn a_kill_queues_its_stop_all_retry_before_the_audit_append() {
    let h = harness();
    h.running();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let lock = hold_audit(&h);
    let d = h.d.clone();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    wait_for("the stop_all retry", || {
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop_all'") == 1
    });
    lock.execute_batch("ROLLBACK").unwrap();
    assert!(kill.join().unwrap().is_err());
}

#[test]
fn a_resume_waiting_on_the_audit_log_does_not_delay_a_kill() {
    let h = harness();
    h.gw.fail_stop_all.store(true, Ordering::SeqCst);
    let lock = hold_audit(&h);
    let d = h.d.clone();
    let resume = std::thread::spawn(move || {
        let (body, sig) = cli("resume", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    std::thread::sleep(Duration::from_millis(300));
    let d = h.d.clone();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW + 1, OWNER_KEY);
        d.signed_command(&body, &sig, NOW + 1)
    });
    wait_for("the kill", || {
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop_all'") == 1
    });
    lock.execute_batch("ROLLBACK").unwrap();
    // The kill landed while the resume was waiting: the resume must not
    // undo it.
    let e = format!("{:#}", resume.join().unwrap().unwrap_err());
    assert!(e.contains("a kill landed during the resume"), "{e}");
    assert!(kill.join().unwrap().is_err(), "stop_all was set to fail");
    assert!(h.d.is_halted().unwrap());
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'stop_all' AND done = 0"),
        1,
        "the kill's retry is still pending"
    );
}

#[test]
fn a_kill_during_publish_still_checks_for_the_published_pr() {
    let h = harness();
    let d = with_gateway(&h, Arc::new(LostPublish(h.gw.clone())));
    h.running();
    let kill_d = d.clone();
    *h.gw.on_publish.lock().unwrap() = Some(Box::new(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        kill_d.signed_command(&body, &sig, NOW).unwrap();
    }));
    d.finished("run-1", RunState::Done, "tests pass").unwrap();
    assert_eq!(h.states(), [("run-1".to_owned(), "canceled")]);
    h.reopen().work_at(NOW + 1000).unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM followups WHERE kind = 'check_publish' AND done = 1"),
        1
    );
    let c = rusqlite::Connection::open(&h.audit).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM audit WHERE payload LIKE '%canceled_after_publish%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_queued_run_is_not_started_while_halted() {
    let h = harness();
    h.queued("FAKE-1");
    h.sql("INSERT INTO control (id, halted, by) VALUES (1, 1, 'x') ON CONFLICT (id) DO UPDATE SET halted = 1");
    h.d.work_at(NOW).unwrap();
    assert!(!h.gw.calls().iter().any(|c| c.starts_with("start")));
    assert_eq!(h.states(), [("run-1".to_owned(), "queued")]);
}

fn halted_in_store(h: &Harness) -> bool {
    h.count("SELECT count(*) FROM control WHERE halted = 1") == 1
}

#[test]
fn a_hung_stop_all_retry_does_not_stop_a_kill_from_committing() {
    let h = harness();
    h.sql(&format!(
        "INSERT INTO followups(kind,target,arg,next_at) VALUES('stop_all','','0',{NOW})"
    ));
    let (tx, rx) = mpsc::channel();
    let gateway = Arc::new(SlowKill {
        inner: h.gw.clone(),
        first: AtomicBool::new(true),
        entered: tx,
        release: (Mutex::new(false), Condvar::new()),
    });
    let d = with_gateway(&h, gateway.clone());
    h.running();
    let retry_d = d.clone();
    let retry = std::thread::spawn(move || retry_d.work_at(NOW + 1000));
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    wait_for("the kill's store write", || {
        halted_in_store(&h) && h.count("SELECT count(*) FROM runs WHERE state = 'canceled'") == 1
    });
    *gateway.release.0.lock().unwrap() = true;
    gateway.release.1.notify_all();
    retry.join().unwrap().unwrap();
    kill.join().unwrap().unwrap();
}

#[test]
fn an_overdue_stop_alarm_waiting_on_the_audit_log_does_not_block_the_store() {
    let h = harness();
    h.running();
    h.gw.fail_stop.store(true, Ordering::SeqCst);
    h.deliver(&unlabel(&h.tracker, "u-1", "FAKE-1", "dev"));
    h.sql("UPDATE followups SET attempts = 4, next_at = 0 WHERE kind = 'stop'");
    let lock = hold_audit(&h);
    let d = h.d.clone();
    let work = std::thread::spawn(move || d.work_at(NOW));
    std::thread::sleep(Duration::from_millis(300));
    let d = h.d.clone();
    let kill = std::thread::spawn(move || {
        let (body, sig) = cli("kill", "kiennt23", NOW, OWNER_KEY);
        d.signed_command(&body, &sig, NOW)
    });
    wait_for("the kill's store write", || halted_in_store(&h));
    lock.execute_batch("ROLLBACK").unwrap();
    work.join().unwrap().unwrap();
    let _ = kill.join().unwrap();
}
