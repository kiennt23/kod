use anyhow::{Result, ensure};
use std::{
    path::PathBuf,
    process::{Child, Command},
    sync::Mutex,
};
/// Pinned equal to `kod_host::deadlines` by a kod-host test (kod-eval cannot depend on kod-host).
pub const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
pub const ABSENCE: std::time::Duration = std::time::Duration::from_secs(10);
#[derive(Default)]
pub struct SpawnGate(Mutex<bool>);
impl SpawnGate {
    pub fn close(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }
    pub fn spawn(
        &self,
        command: &mut Command,
        stopped: impl FnOnce() -> Result<bool>,
    ) -> Result<Child> {
        let closed = self.0.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(!*closed && !stopped()?, "launch admission closed");
        Ok(command.spawn()?)
    }
}
pub struct ConnectedSpec {
    pub run: String,
    pub base: String,
    pub mirror: PathBuf,
    pub dir: PathBuf,
    pub agent_image: String,
    pub collector_image: String,
    pub network: String,
    pub model: String,
    pub max_tokens: u64,
    pub budget: crate::case::Budget,
    pub prompt: String,
    pub model_url: String,
    pub mcp_url: String,
}
pub struct ProcessExit {
    pub success: bool,
    pub stopped: bool,
    pub timed_out: bool,
}

mod collection;
mod process;
pub use collection::{collect, export, regular_bytes};
pub use process::{launch, stop_owned};
#[cfg(feature = "connected-test-support")]
pub use process::{with_docker, with_stop_clock};

pub fn names(run: &str) -> Result<Vec<String>> {
    let number = run
        .strip_prefix("run-")
        .ok_or_else(|| anyhow::anyhow!("invalid run"))?;
    let seq: u64 = number.parse()?;
    ensure!(seq > 0 && seq.to_string() == number, "invalid run");
    Ok(["agent", "collect", "export"]
        .map(|kind| format!("kod-{kind}-{run}"))
        .to_vec())
}

pub fn agent_args(spec: &ConnectedSpec, host_ids: (u32, u32)) -> Result<Vec<String>> {
    let names = names(&spec.run)?;
    let seed = spec.dir.join("seed");
    let input = spec.dir.join("in");
    let out = spec.dir.join("agent_out");
    let runner = crate::runner::Runner::KodLoop {
        model: spec.model.clone(),
        max_tokens: spec.max_tokens,
    };
    let mut env = runner.env(&spec.model_url);
    env.push(("KOD_MCP_URL".into(), spec.mcp_url.clone()));
    let mut args = crate::sandbox::budget_args(
        &names[0],
        &spec.run,
        &spec.budget,
        &spec.network,
        &crate::sandbox::Mounts {
            read_only: &[(&seed, "/seed"), (&input, "/in")],
            out: &out,
            tmpfs: &["/work", "/tmp"],
        },
        host_ids,
        &env,
    );
    args.extend([spec.agent_image.clone(), "agent".into()]);
    args.extend(runner.command_with_turns(spec.budget.max_turns));
    Ok(args)
}
