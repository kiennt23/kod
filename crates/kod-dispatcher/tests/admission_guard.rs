mod common;

use common::*;
use kod_audit::AuditLog;
use kod_dispatcher::{Config, Dispatcher};
use kod_policy::Policy;
use kod_tracker::fake;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Guarded {
    d: Dispatcher,
    open: Arc<AtomicBool>,
    tracker: Arc<fake::FakeTracker>,
    gw: Arc<FakeGateway>,
    _dir: tempfile::TempDir,
}

fn guarded(open: bool) -> Guarded {
    let dir = tempfile::tempdir().unwrap();
    let (t, _) = fake::sandbox();
    let tracker = Arc::new(t);
    let gw = Arc::new(FakeGateway::default());
    let flag = Arc::new(AtomicBool::new(open));
    let seen = flag.clone();
    let d = Dispatcher::open(
        Config::parse(CONFIG).unwrap(),
        &dir.path().join("dispatcher.db"),
        Policy::load(&policy_dir()).map(Arc::new),
        Arc::new(AuditLog::open(&dir.path().join("audit.db")).unwrap()),
        tracker.clone(),
        gw.clone(),
    )
    .unwrap()
    .with_admission_guard(Arc::new(move || seen.load(Ordering::SeqCst)));
    Guarded {
        d,
        open: flag,
        tracker,
        gw,
        _dir: dir,
    }
}

#[test]
fn a_closed_guard_keeps_the_label_pending_and_starts_nothing() {
    let g = guarded(false);
    g.d.receive(&label(&g.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    for _ in 0..3 {
        g.d.work_once_with_admission(true).unwrap();
    }
    assert!(g.gw.calls().is_empty());
    assert!(g.d.runs().unwrap().is_empty());
    g.open.store(true, Ordering::SeqCst);
    while g.d.work_once().unwrap() {}
    assert_eq!(g.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
}

#[test]
fn an_unready_worker_still_cancels_a_running_run() {
    let g = guarded(true);
    g.d.receive(&label(&g.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    while g.d.work_once().unwrap() {}
    assert_eq!(g.gw.calls(), ["start run-1 FAKE-1 acme/app"]);
    g.open.store(false, Ordering::SeqCst);
    g.d.receive(&label(&g.tracker, "d-2", "FAKE-2", "dev")).ok();
    g.d.receive(&unlabel(&g.tracker, "d-3", "FAKE-1", "dev"))
        .unwrap();
    for _ in 0..5 {
        g.d.work_once_with_admission(false).unwrap();
    }
    assert!(
        g.gw.calls().iter().any(|c| c == "stop run-1"),
        "{:?}",
        g.gw.calls()
    );
    assert_eq!(
        g.gw.calls()
            .iter()
            .filter(|c| c.starts_with("start"))
            .count(),
        1
    );
}

#[test]
fn a_false_admission_argument_starts_no_queued_run() {
    let g = guarded(true);
    g.d.receive(&label(&g.tracker, "d-1", "FAKE-1", "dev"))
        .unwrap();
    for _ in 0..3 {
        g.d.work_once_with_admission(false).unwrap();
    }
    assert!(g.gw.calls().is_empty());
}
