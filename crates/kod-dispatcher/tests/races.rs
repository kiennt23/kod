mod common;
use common::*;
use kod_dispatcher::{Dispatcher, Gateway, RunStart};
use kod_tracker::{Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

struct Gate {
    armed: AtomicBool,
    entered: mpsc::Sender<()>,
    released: (Mutex<bool>, Condvar),
}
impl Gate {
    fn wait(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            let (lock, wake) = &self.released;
            drop(
                wake.wait_while(lock.lock().unwrap(), |done| !*done)
                    .unwrap(),
            );
        }
    }
    fn release(&self) {
        *self.released.0.lock().unwrap() = true;
        self.released.1.notify_all();
    }
}
fn gate() -> (Arc<Gate>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    (
        Arc::new(Gate {
            armed: AtomicBool::new(false),
            entered: tx,
            released: (Mutex::new(false), Condvar::new()),
        }),
        rx,
    )
}
struct SlowRevoke {
    inner: Arc<FakeGateway>,
    gate: Arc<Gate>,
}
impl Gateway for SlowRevoke {
    fn start(&self, r: &RunStart) -> anyhow::Result<()> {
        self.inner.start(r)
    }
    fn revoke(&self, r: &str) -> anyhow::Result<()> {
        self.gate.wait();
        self.inner.revoke(r)
    }
    fn stop(&self, r: &str) -> anyhow::Result<()> {
        self.inner.stop(r)
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        self.inner.stop_all()
    }
    fn publish(&self, r: &str) -> anyhow::Result<String> {
        self.inner.publish(r)
    }
    fn find_published(&self, r: &RunStart) -> anyhow::Result<Option<String>> {
        self.inner.find_published(r)
    }
}
fn open(h: &Harness, tracker: Arc<dyn Tracker>, gateway: Arc<dyn Gateway>) -> Arc<Dispatcher> {
    Arc::new(
        Dispatcher::open(
            kod_dispatcher::Config::parse(CONFIG).unwrap(),
            &h.db,
            kod_policy::Policy::load(&policy_dir()).map(Arc::new),
            Arc::new(kod_audit::AuditLog::open(&h.audit).unwrap()),
            tracker,
            gateway,
        )
        .unwrap(),
    )
}
#[test]
fn a_superseded_transition_snapshot_cannot_undo_cancel() {
    let h = harness();
    let (gate, entered) = gate();
    let d = open(
        &h,
        h.flaky.clone(),
        Arc::new(SlowRevoke {
            inner: h.gw.clone(),
            gate: gate.clone(),
        }),
    );
    h.flaky.fail_writes.store(true, Ordering::SeqCst);
    d.receive(&label(&h.tracker, "label", "FAKE-1", "dev"))
        .unwrap();
    d.work_once().unwrap();
    h.flaky.fail_writes.store(false, Ordering::SeqCst);
    h.sql("INSERT INTO followups(kind,target,arg) VALUES('revoke','run-1','')");
    gate.armed.store(true, Ordering::SeqCst);
    let worker = d.clone();
    let thread = std::thread::spawn(move || worker.work_at(NOW).unwrap());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    d.receive(&unlabel(&h.tracker, "cancel", "FAKE-1", "dev"))
        .unwrap();
    d.work_at(NOW).unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
    gate.release();
    thread.join().unwrap();
    assert_eq!(h.workflow("FAKE-1"), "Open");
}
struct PausedTracker {
    inner: Arc<Flaky>,
    gate: Arc<Gate>,
    pause_read: bool,
    fail_transition: bool,
}
impl Tracker for PausedTracker {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn get_issue(&self, k: &str) -> anyhow::Result<Issue> {
        let issue = self.inner.get_issue(k)?;
        if self.pause_read {
            self.gate.wait();
        }
        Ok(issue)
    }
    fn comment(&self, k: &str, v: &str) -> anyhow::Result<()> {
        self.inner.comment(k, v)
    }
    fn transition(&self, k: &str, v: State) -> anyhow::Result<()> {
        if !self.pause_read {
            self.gate.wait();
            if self.fail_transition && v == State::InProgress {
                anyhow::bail!("old transition failed");
            }
        }
        self.inner.transition(k, v)
    }
    fn link_pr(&self, k: &str, v: &str) -> anyhow::Result<()> {
        self.inner.link_pr(k, v)
    }
    fn create_linked(&self, k: &str, t: ReworkKind, v: &Rework) -> anyhow::Result<String> {
        self.inner.create_linked(k, t, v)
    }
    fn is_team_member(&self, k: &str) -> anyhow::Result<bool> {
        self.inner.is_team_member(k)
    }
    fn parse_webhook(&self, k: &Webhook) -> anyhow::Result<Option<Delivery>> {
        self.inner.parse_webhook(k)
    }
}
#[test]
fn an_edit_retires_an_admission_that_has_already_read_the_old_ticket() {
    let h = harness();
    let (gate, entered) = gate();
    let d = open(
        &h,
        Arc::new(PausedTracker {
            inner: h.flaky.clone(),
            gate: gate.clone(),
            pause_read: true,
            fail_transition: false,
        }),
        h.gw.clone(),
    );
    let old = kod_dispatcher::revision(&issue(&h.tracker, "FAKE-1"));
    d.receive(&label(&h.tracker, "label", "FAKE-1", "dev"))
        .unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let worker = d.clone();
    let thread = std::thread::spawn(move || worker.work_at(NOW).unwrap());
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut changed = h.tracker.issue("FAKE-1").unwrap();
    changed
        .body
        .push_str(" New requirement after admission read.");
    h.tracker.add_issue("FAKE-1", changed);
    d.receive(&edited(&h.tracker, "edit", "FAKE-1")).unwrap();
    d.receive(&label(&h.tracker, "new-label", "FAKE-1", "dev"))
        .unwrap();
    d.work_at(NOW).unwrap();
    gate.release();
    thread.join().unwrap();
    let runs = d.runs().unwrap();
    assert!(
        runs.is_empty(),
        "an edit already acknowledged must prevent stale admission: {runs:?}; old revision {old}"
    );
    d.work_once().unwrap();
    let runs = d.runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, kod_dispatcher::RunState::Running);
    assert_eq!(
        runs[0].revision,
        kod_dispatcher::revision(&issue(&h.tracker, "FAKE-1"))
    );
}

#[test]
fn an_inflight_transition_cannot_outlive_its_replacement_or_delay_stops() {
    for fail_transition in [false, true] {
        let h = harness();
        h.flaky.fail_writes.store(true, Ordering::SeqCst);
        h.running();
        let (gate, entered) = gate();
        let d = open(
            &h,
            Arc::new(PausedTracker {
                inner: h.flaky.clone(),
                gate: gate.clone(),
                pause_read: false,
                fail_transition,
            }),
            h.gw.clone(),
        );
        h.flaky.fail_writes.store(false, Ordering::SeqCst);
        gate.armed.store(true, Ordering::SeqCst);
        let worker = d.clone();
        let old = std::thread::spawn(move || worker.work_at(NOW).unwrap());
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        d.receive(&unlabel(&h.tracker, "cancel", "FAKE-1", "dev"))
            .unwrap();
        let worker = d.clone();
        let cancel = std::thread::spawn(move || worker.work_at(NOW).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !d.stopped("run-1").unwrap()
            || !h.gw.revoked().contains(&"run-1".to_owned())
            || !h.gw.calls().contains(&"stop run-1".to_owned())
        {
            if std::time::Instant::now() >= deadline {
                gate.release();
                panic!("cancellation did not revoke and stop during a blocked tracker transition");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        gate.release();
        old.join().unwrap();
        cancel.join().unwrap();
        assert_eq!(h.workflow("FAKE-1"), "Open");
        assert_eq!(
            h.count("SELECT count(*) FROM followups WHERE kind = 'transition' AND done = 0"),
            0
        );
        d.work_at(NOW + 1000).unwrap();
        assert_eq!(h.workflow("FAKE-1"), "Open");
    }
}
