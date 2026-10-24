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
    until_ok("recover_local", || f.runtime.recover_local());
    // The old ID's model token is tombstoned: it can never be registered again.
    assert!(
        f.registry
            .register(
                &model_token("run-1"),
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
    assert!(f.runtime.start(&f.run_start("run-1")).is_err());
    assert!(ready.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn start_cancel_after_admission_before_registration() {
    let (execute, ready, _release) = blocked();
    let db = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
    let hook_db = db.clone();
    let f = Fixture::start_hooked(
        execute,
        Arc::new(move |_| {
            // The owner cancels while admission is in progress.
            let path = hook_db.lock().unwrap().clone().unwrap();
            rusqlite::Connection::open(path)
                .unwrap()
                .execute("UPDATE runs SET state='canceled' WHERE seq=1", [])
                .unwrap();
        }),
    );
    *db.lock().unwrap() = Some(f.db.clone());
    f.insert_run(1, RunState::Running);
    // Canceled before the first live check passes or at the next barrier: never executes.
    assert!(f.runtime.start(&f.run_start("run-1")).is_err());
    assert!(ready.recv_timeout(Duration::from_millis(200)).is_err());
    until_ok("stop", || f.runtime.stop("run-1"));
    until_ok("revoke", || f.runtime.revoke("run-1"));
    assert!(!f.runtime.cleanup_pending());
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
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel::<()>();
    let (spawned, outcome) = mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    let execute: kod_host::runtime::Execute = Arc::new(move |_, gate, _, _| {
        entered.send(()).ok();
        wait.lock().unwrap().recv().ok();
        // The agent tries to launch after shutdown began.
        let mut c = std::process::Command::new("true");
        spawned.send(gate.spawn(&mut c, || Ok(false)).is_err()).ok();
        Ok(RunState::NeedsHuman)
    });
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    // A worker is inside the run; shutdown retires it durably and closes the gate.
    f.runtime.quiesce().unwrap();
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let state: String = c
        .query_row("SELECT state FROM runs WHERE seq=1", [], |r| r.get(0))
        .unwrap();
    assert_ne!(state, "running");
    release.send(()).unwrap();
    assert!(
        outcome.recv_timeout(Duration::from_secs(5)).unwrap(),
        "launch after shutdown must be refused"
    );
}

#[test]
fn done_requires_trusted_collection() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::Done));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    // No trusted collection: the run must not become Done, and never reaches publish.
    assert_eq!(wait_left_running(&f, 1), "needs_human");
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let why: String = c
        .query_row("SELECT reason FROM runs WHERE seq=1", [], |r| r.get(0))
        .unwrap();
    assert!(why.contains("no trusted collected result"), "{why}");
}

#[test]
fn a_trusted_collection_lets_done_reach_publish() {
    let execute: kod_host::runtime::Execute = Arc::new(|a, _, _, ws| {
        ws.insert_for_test(
            &a.binding.run,
            kod_tool_gateway::forge::Collected {
                changes: vec![],
                test_evidence: "ok".into(),
            },
        )?;
        Ok(RunState::Done)
    });
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    wait_left_running(&f, 1);
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let why: String = c
        .query_row("SELECT reason FROM runs WHERE seq=1", [], |r| r.get(0))
        .unwrap();
    // The collection check passed; publish was attempted (and failed: no forge in this fixture).
    assert!(why.contains("publish failed"), "{why}");
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
                &model_token("run-1"),
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
    f.insert_run(77, RunState::Running);
    f.runtime.start(&f.run_start("run-77")).unwrap();
    wait_left_running(&f, 77);
    with_clock(|| {
        assert!(f.runtime.stop("run-77").is_err());
        assert!(f.runtime.cleanup_pending());
        // A second attempt still reports pending; nothing clears the debt.
        assert!(f.runtime.stop("run-77").is_err());
    });
    assert!(f.runtime.cleanup_pending());
    let calls = std::fs::read_to_string(FAKE_ARGV.get().unwrap()).unwrap();
    assert!(calls.find("kill --signal TERM").unwrap() < calls.find("kill --signal KILL").unwrap());
    // Pending cleanup blocks new admissions.
    f.insert_run(2, RunState::Running);
    assert!(f.runtime.start(&f.run_start("run-2")).is_err());
    // The unconfirmed stop stays visible as a pending follow-up in the reopened DB.
    let c = kod_dispatcher::store::open(&f.db).unwrap();
    let pending: i64 = c
        .query_row(
            "SELECT count(*) FROM followups WHERE kind='stop' AND target='run-77' AND done=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(pending >= 1);
}

/// Retry `f` (a dispatcher-style follow-up) until it succeeds; fails the test at the deadline.
fn until_ok(what: &str, mut f: impl FnMut() -> anyhow::Result<()>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match f() {
            Ok(()) => return,
            Err(e) if std::time::Instant::now() > deadline => panic!("{what}: {e:#}"),
            Err(_) => std::thread::yield_now(),
        }
    }
}

/// Wait (blocking on the database, not on time) until the run leaves `running`.
fn wait_left_running(f: &Fixture, seq: i64) -> String {
    let c = rusqlite::Connection::open(&f.db).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let s: String = c
            .query_row("SELECT state FROM runs WHERE seq=?1", [seq], |r| r.get(0))
            .unwrap();
        if s != "running" && s != "publishing" {
            return s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run never left running"
        );
        std::thread::yield_now();
    }
}

#[test]
fn normal_revoke_then_stop_clears_debt_and_admits_the_next_run() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::NeedsHuman));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    wait_left_running(&f, 1);
    // The dispatcher retries revoke and stop as separate follow-ups.
    until_ok("stop", || f.runtime.stop("run-1"));
    until_ok("revoke", || f.runtime.revoke("run-1"));
    assert!(
        !f.runtime.cleanup_pending(),
        "debt must clear once both are confirmed"
    );
    f.insert_run(2, RunState::Running);
    f.runtime.start(&f.run_start("run-2")).unwrap();
}

#[test]
fn never_bound_slots_do_not_hold_debt_forever() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::NeedsHuman));
    let f = Fixture::start(execute);
    kod_dispatcher::check_absent_run(f.runtime.as_ref(), "run-999").unwrap();
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    wait_left_running(&f, 1);
    until_ok("stop", || f.runtime.stop("run-1"));
    until_ok("revoke", || f.runtime.revoke("run-1"));
    assert!(!f.runtime.cleanup_pending());
}

#[test]
fn a_reaped_failed_global_revocation_never_clears_debt() {
    let execute: kod_host::runtime::Execute = Arc::new(|_, _, _, _| Ok(RunState::NeedsHuman));
    let f = Fixture::start(execute);
    f.insert_run(1, RunState::Running);
    f.runtime.start(&f.run_start("run-1")).unwrap();
    wait_left_running(&f, 1);
    until_ok("revoke", || f.runtime.revoke("run-1"));
    // A failing global revocation: first call spawns the worker, later calls reap it.
    loop {
        let e = f
            .runtime
            .revoke_all_with(|| anyhow::bail!("remote down"))
            .unwrap_err();
        if format!("{e:#}").contains("remote down") {
            break; // the failed worker has been reaped; no worker handle remains
        }
        std::thread::yield_now();
    }
    // The run's own stop completing must not erase the failed global obligation.
    until_ok("stop", || f.runtime.stop("run-1"));
    assert!(f.runtime.cleanup_pending());
    // Only a successful global revocation clears it.
    until_ok("global", || f.runtime.revoke_all_with(|| Ok(())));
    until_ok("stop", || f.runtime.stop("run-1"));
    assert!(!f.runtime.cleanup_pending());
}
