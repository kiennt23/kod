use crate::workspace::{CollectedWorkspace, read_export, supported_patch};
use anyhow::{Result, ensure};
use kod_dispatcher::RunState;
use kod_eval::{
    case::EndState,
    connected::{self, ConnectedSpec, SpawnGate, regular_bytes},
};
use std::{os::unix::fs::MetadataExt, time::Duration};
pub fn ready_to_collect(
    container_success: bool,
    exit: Option<&str>,
    procs: Option<&str>,
) -> Result<()> {
    ensure!(
        container_success && exit.map(str::trim) == Some("0"),
        "agent did not exit successfully"
    );
    kod_eval::run::process_gate(procs).map_err(anyhow::Error::msg)
}
pub fn run(
    spec: &ConnectedSpec,
    gate: &SpawnGate,
    stopped: &dyn Fn() -> Result<bool>,
    workspace: &CollectedWorkspace,
) -> Result<RunState> {
    let names = connected::names(&spec.run)?;
    {
        use std::os::unix::fs::DirBuilderExt;
        // Exclusive and private: a leftover or planted run directory is refused, not reused.
        std::fs::DirBuilder::new().mode(0o700).create(&spec.dir)?;
    }
    for name in ["seed", "in", "agent_out"] {
        std::fs::create_dir(spec.dir.join(name))?;
    }
    kod_eval::workspace::extract(&spec.mirror, &spec.base, &spec.dir.join("seed"))?;
    std::fs::write(spec.dir.join("in/prompt.txt"), &spec.prompt)?;
    let meta = std::fs::metadata(&spec.dir)?;
    let args = connected::agent_args(spec, (meta.uid(), meta.gid()))?;
    let exit = connected::launch(
        &args,
        &names[0],
        Duration::from_secs(spec.budget.timeout_secs),
        gate,
        stopped,
    )?;
    if exit.stopped {
        return Ok(RunState::Canceled);
    }
    if exit.timed_out {
        return Ok(RunState::OverBudget);
    }
    ensure!(exit.success, "agent container failed");
    let output = spec.dir.join("agent_out");
    let code = String::from_utf8(regular_bytes(&output.join("exit_code"), 64)?)?;
    let procs = String::from_utf8(regular_bytes(&output.join("agent_procs"), 64)?)?;
    ready_to_collect(exit.success, Some(&code), Some(&procs))?;
    let stdout = String::from_utf8(regular_bytes(&output.join("agent.json"), 16 << 20)?)?;
    let runner = kod_eval::runner::Runner::KodLoop {
        model: spec.model.clone(),
        max_tokens: spec.max_tokens,
    };
    match runner.end_state(0, &stdout).0 {
        EndState::Blocked => return Ok(RunState::Blocked),
        EndState::NeedsHuman => return Ok(RunState::NeedsHuman),
        EndState::OverBudget => return Ok(RunState::OverBudget),
        EndState::Done => {}
    }
    let collected = connected::collect(spec, gate, stopped)?;
    supported_patch(&regular_bytes(&collected.join("diff.patch"), 32 << 20)?)?;
    let exported = connected::export(spec, gate, stopped)?;
    ensure!(!stopped()?, "run stopped before collection insert");
    workspace.insert(&spec.run, read_export(&exported, &spec.base)?)?;
    Ok(RunState::Done)
}
