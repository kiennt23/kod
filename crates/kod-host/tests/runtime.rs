mod common;
use common::*;
use kod_dispatcher::{Gateway, RunState};
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn stopped_reader_uses_the_durable_row_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.db");
    let c = kod_dispatcher::store::open(&path).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#7','v1','running')",
        [],
    )
    .unwrap();
    let stopped = kod_host::runtime::stopped_reader(path);
    assert!(!stopped("run-1").unwrap());
    c.execute("UPDATE runs SET state='canceled' WHERE seq=1", [])
        .unwrap();
    assert!(stopped("run-1").unwrap());
    assert!(stopped("run-99").unwrap());
    assert!(stopped("run-01").unwrap());
    drop(c);
    std::fs::remove_file(dir.path().join("dispatcher.db")).unwrap();
    assert!(stopped("run-1").is_err());
}

#[test]
fn background_start_returns_before_agent_completion() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let join = kod_host::runtime::background(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        done_tx.send(()).unwrap();
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(done_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    join.join().unwrap();
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
}

/// An agent that blocks until released, reporting that it started.
fn blocked() -> (
    kod_host::runtime::Execute,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel::<()>();
    let wait = std::sync::Mutex::new(wait);
    let execute: kod_host::runtime::Execute = Arc::new(move |_, _, _, _| {
        entered.send(()).ok();
        wait.lock().unwrap().recv().ok();
        Ok(RunState::NeedsHuman)
    });
    (execute, ready, release)
}

#[test]
fn start_binds_registers_and_returns_while_agent_runs() {
    let (execute, ready, release) = blocked();
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM run_bindings WHERE run_seq=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "binding precedes spawn");
    release.send(()).ok();
}

#[test]
fn recovery_does_not_resume() {
    let (execute, ready, release) = blocked();
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).ok();
    // Restart: startup recovery retires the run, so start is refused.
    let (execute, ready, _release) = blocked();
    let f = f.restart(execute);
    assert!(f.runtime.start(&f.run_start("run-1")).is_err());
    assert!(ready.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn existing_binding_refuses_start() {
    let (execute, ready, _release) = blocked();
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    // A previous host bound the run but never finished it.
    let root = f.dir.path().canonicalize().unwrap().join("runs");
    let b = admitted(&root, &f.run_start("run-1")).binding;
    let mut c = kod_dispatcher::store::open(&f.db).unwrap();
    let tx = c.transaction().unwrap();
    kod_dispatcher::store::bind_once(&tx, "run-1", &serde_json::to_value(&b).unwrap()).unwrap();
    tx.commit().unwrap();
    let err = f
        .runtime
        .start(&f.run_start("run-1"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("old execution cannot resume"), "{err}");
    assert!(ready.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn start_cancel_before_registration() {
    let (execute, ready, _release) = blocked();
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Canceled);
    assert!(f.runtime.start(&f.run_start("run-1")).is_err());
    assert!(ready.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn shutdown_closes_launch_gate() {
    let (execute, _ready, _release) = blocked();
    let f = Fixture::start(execute);
    f.runtime.quiesce().unwrap();
    let mut c = std::process::Command::new("true");
    assert!(f.gate.spawn(&mut c, || Ok(false)).is_err());
}

#[test]
fn done_requires_trusted_collection() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::Done));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    // No trusted collection: the run must not become Done.
    let c = rusqlite::Connection::open(&f.db).unwrap();
    for _ in 0..100 {
        let s: String = c
            .query_row("SELECT state FROM runs WHERE seq=1", [], |r| r.get(0))
            .unwrap();
        if s != "running" {
            assert_eq!(s, "needs_human");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("run never left running");
}

#[test]
fn absent_run_contract() {
    let (execute, _ready, _release) = blocked();
    let f = Fixture::start(execute);
    kod_dispatcher::check_absent_run(f.runtime.as_ref(), "run-999").unwrap();
    kod_dispatcher::check_absent_run(f.runtime.as_ref(), "run-999").unwrap();
}

#[test]
fn finished_run_credentials_are_revoked() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::NeedsHuman));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    let c = rusqlite::Connection::open(&f.db).unwrap();
    for _ in 0..100 {
        let s: String = c
            .query_row("SELECT state FROM runs WHERE seq=1", [], |r| r.get(0))
            .unwrap();
        if s != "running" {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Revoked tokens cannot be registered again.
    assert!(
        f.registry
            .register(
                MODEL_TOKEN,
                kod_llm_proxy::config::RunSpec {
                    id: "run-1".into(),
                    ticket: "acme/app#7".into(),
                    repo: "acme/app".into(),
                    data_classes: vec!["placeholder".into()],
                    budget: kod_llm_proxy::config::Budget {
                        max_tokens: 1,
                        max_cost_micro_usd: 1
                    },
                }
            )
            .is_err()
    );
}

#[test]
fn stop_timeout_stays_pending() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::NeedsHuman));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    let log = f.dir.path().join("argv");
    let stuck = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase $1 in ps) echo container;; inspect) echo run-1;; kill) exit 0;; esac\n",
        log.display()
    );
    with_fake_docker(&stuck, || {
        assert!(f.runtime.stop("run-1").is_err());
        assert!(f.runtime.cleanup_pending());
        // A second attempt still reports pending; nothing clears the debt.
        assert!(f.runtime.stop("run-1").is_err());
    });
    assert!(f.runtime.cleanup_pending());
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.find("kill --signal TERM").unwrap() < calls.find("kill --signal KILL").unwrap());
    // Pending cleanup blocks new admissions.
    f.insert_run(2, RunState::Running);
    assert!(f.runtime.start(&f.run_start("run-2")).is_err());
}
