use super::{ProcessExit, SpawnGate, names};
use anyhow::{Result, bail, ensure};
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "connected-test-support")]
thread_local! { static DOCKER: std::cell::RefCell<std::path::PathBuf> = std::cell::RefCell::new("docker".into()); }
/// Process-wide docker for threads without a `with_docker` scope (workers,
/// follow-ups). Set once; replaces mutating the process environment.
#[cfg(feature = "connected-test-support")]
static PROCESS_DOCKER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
#[cfg(feature = "connected-test-support")]
pub fn use_docker_for_process(path: &std::path::Path) {
    PROCESS_DOCKER.get_or_init(|| path.into());
}
#[cfg(feature = "connected-test-support")]
pub fn with_docker<T>(path: &std::path::Path, action: impl FnOnce() -> T) -> T {
    struct Reset(std::path::PathBuf);
    impl Drop for Reset {
        fn drop(&mut self) {
            DOCKER.with(|p| *p.borrow_mut() = self.0.clone());
        }
    }
    let _reset = Reset(DOCKER.with(|p| p.replace(path.into())));
    action()
}
#[cfg(feature = "connected-test-support")]
thread_local! { static CLOCK: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) }; }
#[cfg(feature = "connected-test-support")]
pub fn with_stop_clock<T>(action: impl FnOnce() -> T) -> T {
    struct Reset(Option<Duration>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CLOCK.with(|clock| clock.set(self.0));
        }
    }
    let _reset = Reset(CLOCK.with(|clock| clock.replace(Some(Duration::ZERO))));
    action()
}
fn elapsed(start: Instant) -> Duration {
    #[cfg(feature = "connected-test-support")]
    if let Some(value) = CLOCK.with(|clock| clock.get()) {
        CLOCK.with(|clock| clock.set(Some(value + Duration::from_secs(1))));
        return value;
    }
    start.elapsed()
}
fn command() -> Command {
    #[cfg(feature = "connected-test-support")]
    {
        let scoped = DOCKER.with(|p| p.borrow().clone());
        match PROCESS_DOCKER.get() {
            Some(path) if scoped.as_os_str() == "docker" => Command::new(path),
            _ => Command::new(scoped),
        }
    }
    #[cfg(not(feature = "connected-test-support"))]
    {
        Command::new("docker")
    }
}
fn query(args: &[&str], deadline: Instant) -> Result<String> {
    ensure!(Instant::now() < deadline, "Docker deadline elapsed");
    let mut child = command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing stdout"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let result = pipe.take(65537).read_to_end(&mut bytes).map(|_| bytes);
        drop(sender.send(result));
    });
    loop {
        if let Some(status) = child.try_wait()? {
            let bytes =
                receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
            ensure!(
                status.success() && bytes.len() <= 65536,
                "Docker query failed"
            );
            return Ok(String::from_utf8(bytes)?);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("Docker command timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
pub(super) fn present(name: &str, deadline: Instant) -> Result<bool> {
    Ok(!query(
        &["ps", "-aq", "--filter", &format!("name=^/{name}$")],
        deadline,
    )?
    .trim()
    .is_empty())
}
pub fn stop_owned(run: &str, targets: &[String], term: Duration, total: Duration) -> Result<()> {
    stop_process(run, targets, term, total, None)
}
fn stop_process(
    run: &str,
    targets: &[String],
    term: Duration,
    total: Duration,
    mut child: Option<&mut std::process::Child>,
) -> Result<()> {
    let owned = names(run)?;
    ensure!(
        targets.iter().all(|name| owned.contains(name)),
        "unowned name"
    );
    if total.is_zero() {
        let deadline = Instant::now() + Duration::from_secs(1);
        for name in targets {
            ensure!(
                !present(name, deadline)?,
                "container remains after stop deadline"
            );
        }
        ensure!(
            child
                .as_mut()
                .map(|process| process.try_wait())
                .transpose()?
                .flatten()
                .is_some()
                || child.is_none(),
            "launch remains after stop deadline"
        );
        return Ok(());
    }
    let start = Instant::now();
    let deadline = start + total;
    let mut signaled = std::collections::HashSet::new();
    let mut killed = std::collections::HashSet::new();
    loop {
        let elapsed = elapsed(start);
        let mut remaining = false;
        for name in targets {
            if !present(name, deadline)? {
                continue;
            }
            remaining = true;
            // The container may exit on its own (--rm) between ps and inspect/kill. A
            // failed call is fine only if a successful ps then proves absence.
            let owner = match query(
                &[
                    "inspect",
                    "--format",
                    "{{ index .Config.Labels \"kod.run\" }}",
                    name,
                ],
                deadline,
            ) {
                Ok(owner) => owner,
                Err(e) => {
                    ensure!(!present(name, deadline).map_err(|_| e)?, "inspect failed");
                    continue;
                }
            };
            ensure!(owner.trim() == run, "container owner mismatch");
            if signaled.insert(name)
                && query(&["kill", "--signal", "TERM", name], deadline).is_err()
            {
                ensure!(!present(name, deadline)?, "TERM failed");
                continue;
            }
            if elapsed >= term
                && killed.insert(name)
                && query(&["kill", "--signal", "KILL", name], deadline).is_err()
            {
                ensure!(!present(name, deadline)?, "KILL failed");
            }
        }
        let joined = match child.as_mut() {
            Some(process) => process.try_wait()?.is_some(),
            None => true,
        };
        if !remaining && joined {
            return Ok(());
        }
        if elapsed >= total || Instant::now() >= deadline {
            if let Some(process) = child.as_mut() {
                process.kill()?;
                process.wait()?;
            }
            bail!("container stop timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
pub fn launch(
    args: &[String],
    name: &str,
    wall: Duration,
    gate: &SpawnGate,
    stopped: &dyn Fn() -> Result<bool>,
) -> Result<ProcessExit> {
    let run = ["kod-agent-", "kod-collect-", "kod-export-"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .ok_or_else(|| anyhow::anyhow!("invalid container name"))?;
    ensure!(
        names(run)?.iter().any(|n| n == name),
        "invalid container name"
    );
    let mut child = gate.spawn(
        command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        stopped,
    )?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                !present(name, Instant::now() + super::ABSENCE)?,
                "container still present after CLI exit"
            );
            return Ok(ProcessExit {
                success: status.success(),
                stopped: false,
                timed_out: false,
            });
        }
        let canceled = stopped();
        if canceled.as_ref().map_or(true, |v| *v) || start.elapsed() >= wall {
            // Keep observing delayed creates until the launch CLI has exited.
            let cleanup = stop_process(
                run,
                &[name.to_owned()],
                super::TERM_GRACE,
                super::ABSENCE,
                Some(&mut child),
            );
            if cleanup.is_err() && child.try_wait()?.is_none() {
                child.kill()?;
                child.wait()?;
            }
            cleanup?;
            return Ok(ProcessExit {
                success: false,
                stopped: canceled?,
                timed_out: start.elapsed() >= wall,
            });
        }
        thread::sleep(Duration::from_millis(25));
    }
}
