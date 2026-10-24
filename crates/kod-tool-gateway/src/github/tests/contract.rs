use super::*;
use crate::{
    Adapters, Gateway, Run,
    broker::Broker,
    docs::FakeDocs,
    forge::{self, Change, Collected, FakeWorkspace, Identity},
};
use std::sync::atomic::{AtomicBool, Ordering};
#[test]
fn contract_real_http() {
    let f = Fixture::start();
    let a = f.adapter();
    let base = f.permit().base;
    a.register_run(RunPermit {
        run: "contract-run".into(),
        ticket: "CONTRACT-1".into(),
        repo: "acme/app".into(),
        base: base.clone(),
    })
    .unwrap();
    forge::check(&a, &a, "acme/app", &base).unwrap();
    assert!(f.remote.lock().unwrap().refs.is_empty());
    assert_eq!(f.live(), 0);
    assert_eq!(f.remote.lock().unwrap().prs.len(), 1);
}
fn gateway(f: &Fixture, a: Arc<GitHub>, stopped: Arc<AtomicBool>) -> Arc<Gateway> {
    let (tracker, _) = kod_tracker::fake::sandbox();
    let mut issue = tracker.issue("FAKE-1").unwrap();
    issue.workflow_state = "Doing".into();
    tracker.add_issue("FAKE-1", issue);
    let workspace = Arc::new(FakeWorkspace::default());
    workspace.set(
        "run-1",
        Collected {
            changes: vec![Change {
                path: "contract.txt".into(),
                content: Some("contract\n".into()),
            }],
            test_evidence: "fixture collector: passed".into(),
        },
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Arc::new(kod_policy::Policy::load(&root.join("policy")).unwrap());
    let audit = Arc::new(kod_audit::AuditLog::open(&f.root().join("audit.db")).unwrap());
    let base = f.permit().base;
    a.register_run(RunPermit {
        run: "run-1".into(),
        ticket: "FAKE-1".into(),
        repo: "acme/app".into(),
        base: base.clone(),
    })
    .unwrap();
    let gw = Arc::new(
        Gateway::new(
            policy,
            audit,
            Adapters {
                tracker: Arc::new(tracker),
                docs: Arc::new(FakeDocs::default()),
                stopped: Arc::new(move |_| Ok(stopped.load(Ordering::SeqCst))),
                broker: a.clone(),
                forge: a,
                workspace,
            },
            Identity {
                name: "kod-agent".into(),
                email: "agent@kod.invalid".into(),
            },
        )
        .unwrap(),
    );
    gw.start_run(
        &"a".repeat(64),
        Run {
            id: "run-1".into(),
            ticket: "FAKE-1".into(),
            repo: "acme/app".into(),
            base,
            data_classes: vec!["placeholder".into()],
            pins: std::collections::BTreeMap::new(),
            linked_tickets: vec![],
            started_at: 1_700_000_000,
        },
    )
    .unwrap();
    gw
}
#[test]
fn contract_gateway_cancel_after_ref() {
    let f = Fixture::start();
    let a = Arc::new(f.adapter());
    let stopped = Arc::new(AtomicBool::new(false));
    let gw = gateway(&f, a.clone(), stopped.clone());
    let (reached, wait) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    f.remote.lock().unwrap().after_ref = Some(Box::new(move || {
        reached.send(()).unwrap();
        resume
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }));
    let worker = std::thread::spawn(move || gw.publish("run-1"));
    let barrier = wait.recv_timeout(std::time::Duration::from_secs(10));
    stopped.store(true, Ordering::SeqCst);
    let _ = release.send(());
    let result = worker.join().unwrap();
    barrier.unwrap();
    assert!(
        result
            .unwrap_err()
            .contains("the run was canceled during publish")
    );
    assert!(f.remote.lock().unwrap().refs.is_empty());
    assert_eq!(f.live(), 0);
    assert_eq!(f.requests("POST", "/repos/acme/app/pulls"), 0);
    assert_eq!(f.requests("DELETE", "/installation/token"), 2);
}
#[test]
fn contract_activation_failure_and_restart_cleanup() {
    let f = Fixture::start();
    let mut wrong = f.config();
    wrong.base_branch = "wrong-default".into();
    let a = f.adapter_with(wrong).unwrap();
    assert!(a.verify_repository().is_err());
    assert_eq!(f.live(), 0);
    drop(a);
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    a.mint("run-1", "acme/app").unwrap();
    drop(a);
    let a = f.adapter();
    a.recover().unwrap();
    a.verify_repository().unwrap();
    assert_eq!(f.live(), 0);
    assert_eq!(f.mutations(), 0);
}
#[test]
fn test_api_rejects_remote_origins() {
    let f = Fixture::start();
    for api in [
        "http://localhost:1234/",
        "https://example.com/",
        "http://127.0.0.1:1234/path",
        "http://127.0.0.1:1234/?x=1",
    ] {
        assert!(
            GitHub::with_test_api(
                f.config(),
                f.app(),
                f.root().join("leases"),
                api,
                Arc::new(|| 1_800_000_000)
            )
            .is_err()
        );
    }
    assert_eq!(f.remote.lock().unwrap().requests.len(), 0);
}
