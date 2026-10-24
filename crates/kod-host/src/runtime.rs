use crate::{binding::Binding, config::FsConfig, workspace::CollectedWorkspace};
use anyhow::{Context, Result, ensure};
use kod_dispatcher::{Dispatcher, RunStart, RunState};
use kod_eval::connected::SpawnGate;
use kod_tool_gateway::{Stopped, broker::Broker, forge::Workspace, github};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

mod cleanup;
pub mod model_sink;
mod start;

pub type ResolveAdmission = Arc<dyn Fn(&RunStart) -> Result<Admitted> + Send + Sync>;
pub type Execute = Arc<
    dyn Fn(&Admitted, &SpawnGate, &Stopped, &CollectedWorkspace) -> Result<RunState> + Send + Sync,
>;
pub struct Admitted {
    pub binding: Binding,
    pub prompt: String,
    pub model_token: String,
    pub mcp_token: String,
}

/// Test hook run at registration barriers.
#[cfg(feature = "test-support")]
pub type BarrierHook = Arc<dyn Fn(&str) + Send + Sync>;

pub struct RuntimeAdapters {
    pub tool: Arc<kod_tool_gateway::Gateway>,
    pub model: kod_llm_proxy::RunRegistry,
    pub github: Arc<github::GitHub>,
    pub workspace: Arc<CollectedWorkspace>,
    pub gate: Arc<SpawnGate>,
    pub resolve: ResolveAdmission,
    pub execute: Execute,
}

pub struct Runtime {
    config: Arc<FsConfig>,
    adapters: RuntimeAdapters,
    weak: Weak<Runtime>,
    dispatcher: OnceLock<Weak<Dispatcher>>,
    slots: Mutex<HashMap<String, Arc<RunSlot>>>,
    global_cleanup: Mutex<Option<JoinHandle<Result<()>>>>,
    cleanup_debt: AtomicBool,
    /// False from the first `revoke_all` until a global revocation joined Ok.
    global_confirmed: AtomicBool,
    /// Test hook run at named points between registration and spawn.
    #[cfg(feature = "test-support")]
    barrier: Mutex<Option<BarrierHook>>,
}

#[derive(Default)]
struct RunSlot {
    canceled: AtomicBool,
    execution_done: AtomicBool,
    stop_confirmed: AtomicBool,
    cancel_at: Mutex<Option<Instant>>,
    setup: Mutex<Option<std::result::Result<(), String>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    cleanup: Mutex<Option<JoinHandle<Result<()>>>>,
    cleanup_confirmed: AtomicBool,
}

impl RunSlot {
    fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        self.cancel_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(Instant::now);
    }
    fn run_if_live<T>(
        &self,
        run: &str,
        stopped: &Stopped,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.live(run, stopped)?;
        f()
    }
    fn live(&self, run: &str, stopped: &Stopped) -> Result<()> {
        ensure!(
            !self.canceled.load(Ordering::SeqCst) && !stopped(run)?,
            "run canceled before spawn"
        );
        Ok(())
    }
}

pub fn stopped_reader(path: PathBuf) -> Stopped {
    Arc::new(move |run| {
        let c = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.busy_timeout(Duration::from_millis(50))?;
        kod_dispatcher::store::stopped(&c, run)
    })
}

pub fn background(work: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new()
        .name("kod-connected-run".into())
        .spawn(work)?)
}

pub fn notify_finished(
    dispatcher: &Weak<Dispatcher>,
    run: &str,
    state: RunState,
    reason: &str,
) -> Result<()> {
    dispatcher
        .upgrade()
        .context("dispatcher detached")?
        .finished(run, state, reason)
}

impl Runtime {
    pub fn new(config: Arc<FsConfig>, adapters: RuntimeAdapters) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            config,
            adapters,
            weak: weak.clone(),
            dispatcher: OnceLock::new(),
            slots: Mutex::default(),
            global_cleanup: Mutex::default(),
            cleanup_debt: AtomicBool::new(false),
            global_confirmed: AtomicBool::new(true),
            #[cfg(feature = "test-support")]
            barrier: Mutex::default(),
        })
    }
    pub fn attach(&self, dispatcher: Weak<Dispatcher>) -> Result<()> {
        ensure!(dispatcher.upgrade().is_some(), "dispatcher detached");
        self.dispatcher
            .set(dispatcher)
            .map_err(|_| anyhow::anyhow!("dispatcher already attached"))
    }
    fn slot(&self, run: &str) -> Arc<RunSlot> {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(run.into())
            .or_default()
            .clone()
    }
    fn bound(&self, run: &str) -> Result<bool> {
        let Some(seq) = kod_dispatcher::store::seq_of(run) else {
            return Ok(false);
        };
        let c = rusqlite::Connection::open_with_flags(
            &self.config.paths.dispatcher_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_bindings WHERE run_seq=?1)",
            [seq],
            |r| r.get(0),
        )?)
    }
    fn binding(&self, run: &str) -> Result<Binding> {
        let b = Binding::load(&self.config.paths.dispatcher_db, run)?;
        b.validate(&self.config.paths.run_root)?;
        Ok(b)
    }
    /// Run `hook(point)` at "github", "model", "tool" (after that registration)
    /// and "worker" (just before the agent would spawn).
    #[cfg(feature = "test-support")]
    pub fn set_barrier(&self, hook: BarrierHook) {
        *self.barrier.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }
    fn barrier(&self, _point: &str) {
        #[cfg(feature = "test-support")]
        if let Some(hook) = self
            .barrier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            hook(_point);
        }
    }
    pub fn cleanup_pending(&self) -> bool {
        self.cleanup_debt.load(Ordering::Acquire)
    }
    pub fn quiesce(&self) -> Result<()> {
        self.adapters.gate.close();
        let mut c = kod_dispatcher::store::open(&self.config.paths.dispatcher_db)?;
        kod_dispatcher::store::retire_active(&mut c, "host shutting down")
    }
}

impl kod_dispatcher::Gateway for Runtime {
    fn start(&self, start: &RunStart) -> Result<()> {
        self.start_registered(start)
    }
    fn revoke(&self, run: &str) -> Result<()> {
        self.revoke_registered(run)
    }
    fn stop(&self, run: &str) -> Result<()> {
        self.stop_registered(run)
    }
    fn stop_all(&self) -> Result<()> {
        self.stop_everything()
    }
    fn publish(&self, run: &str) -> Result<String> {
        self.binding(run)?;
        self.adapters.workspace.collected(run)?;
        self.adapters
            .tool
            .publish(run)
            .map(|p| p.pr)
            .map_err(anyhow::Error::msg)
    }
    fn find_published(&self, start: &RunStart) -> Result<Option<String>> {
        let b = self.binding(&start.id)?;
        ensure!(
            (&b.ticket, &b.revision, &b.repo) == (&start.ticket, &start.revision, &start.repo),
            "publication binding identity changed"
        );
        self.adapters.github.register_run(b.permit())?;
        self.adapters
            .tool
            .published(&start.repo, &start.ticket, &start.id)
            .map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::cleanup::poll_cleanup;
    use super::*;
    #[test]
    fn cancel_before_spawn_never_creates_a_child() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("dispatcher.db");
        let c = kod_dispatcher::store::open(&db).unwrap();
        c.execute(
            "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#7','v1','running')",
            [],
        )
        .unwrap();
        let slot = Arc::new(RunSlot::default());
        let gate = Arc::new(SpawnGate::default());
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let worker_slot = slot.clone();
        let marker = dir.path().join("spawned");
        let output = marker.clone();
        let stopped = stopped_reader(db);
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            worker_slot.run_if_live("run-1", &stopped, || {
                let mut command = std::process::Command::new("touch");
                command.arg(output);
                let mut child = gate.spawn(&mut command, || stopped("run-1"))?;
                ensure!(child.wait()?.success(), "child failed");
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        slot.cancel();
        c.execute("UPDATE runs SET state='canceled' WHERE seq=1", [])
            .unwrap();
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn durable_stop_alone_also_prevents_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("dispatcher.db");
        let c = kod_dispatcher::store::open(&db).unwrap();
        c.execute(
            "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#7','v1','canceled')",
            [],
        )
        .unwrap();
        let marker = dir.path().join("spawned");
        let mut command = std::process::Command::new("touch");
        command.arg(&marker);
        let stopped = stopped_reader(db);
        assert!(
            SpawnGate::default()
                .spawn(&mut command, || stopped("run-1"))
                .is_err()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn silent_remote_revocation_does_not_block_the_stop_caller() {
        let cell = Mutex::new(None);
        let (release, wait) = std::sync::mpsc::channel();
        let mut process = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let began = Instant::now();
        assert!(
            poll_cleanup(&cell, move || {
                wait.recv()?;
                Ok(())
            })
            .is_err()
        );
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(!cell.lock().unwrap().as_ref().unwrap().is_finished());
        process.kill().unwrap();
        assert!(!process.wait().unwrap().success());
        assert!(began.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        let worker = cell.lock().unwrap().take().unwrap();
        worker.join().unwrap().unwrap();
    }
}
