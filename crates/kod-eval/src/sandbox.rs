//! `docker run` arguments for the agent, collect and score containers.
use crate::case::EvalCase;
use std::path::Path;

/// Every file the entrypoint writes to /out from agent or repository output
/// is cut at this size; the run notes when a file reached it.
pub const LOG_CAP: u64 = 16 << 20;
/// /home/agent, for the runner's own state.
pub const HOME_SIZE: &str = "64m";

pub struct Mounts<'a> {
    /// Read-only binds as (host path, container path). Agent: the prepared
    /// workspace at /seed and the prompt at /in. Collect: the base tree at /in
    /// and the agent's output at /agent. Score: the patched workspace at
    /// /work, so the tests can't change it.
    pub read_only: &'a [(&'a Path, &'a str)],
    pub out: &'a Path,
    /// Writable tmpfs mounts, each of the case's disk budget. Any tmpfs also
    /// makes the root filesystem read-only and adds a /home/agent tmpfs of
    /// HOME_SIZE, so nothing the agent or the tests run can write to the host
    /// disk. tmpfs counts against the memory limit.
    pub tmpfs: &'a [&'a str],
}

pub fn image(case: &EvalCase, prefix: &str) -> String {
    format!("{prefix}-{}", case.language)
}

/// The entrypoint runs as root with only the capabilities it needs to hand
/// directories to the agent user and back. The agent user gets none of them.
pub fn run_args(
    name: &str,
    run_id: &str,
    case: &EvalCase,
    network: &str,
    m: &Mounts,
    host_ids: (u32, u32),
    env: &[(String, String)],
) -> Vec<String> {
    let b = &case.budget;
    let mut a: Vec<String> = [
        "run",
        "--rm",
        "--name",
        name,
        "--label",
        &format!("kod.run={run_id}"),
        "--network",
        network,
        "--workdir",
        "/work",
        "--memory",
        &b.memory,
        "--memory-swap",
        &b.memory,
        "--cpus",
        &b.cpus.to_string(),
        "--pids-limit",
        "512",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "CHOWN",
        "--cap-add",
        "SETUID",
        "--cap-add",
        "SETGID",
        "--cap-add",
        "DAC_OVERRIDE",
        "--cap-add",
        "KILL",
        "--security-opt",
        "no-new-privileges",
        "-e",
        &format!("KOD_HOST_UID={}", host_ids.0),
        "-e",
        &format!("KOD_HOST_GID={}", host_ids.1),
        "-e",
        &format!("KOD_LOG_CAP={LOG_CAP}"),
        "-v",
        &format!("{}:/out", m.out.display()),
    ]
    .map(String::from)
    .to_vec();
    for (host, path) in m.read_only {
        a.extend(["-v".into(), format!("{}:{path}:ro", host.display())]);
    }
    if !m.tmpfs.is_empty() {
        a.push("--read-only".into());
        for path in m.tmpfs {
            a.extend(["--tmpfs".into(), format!("{path}:size={},exec", b.disk)]);
        }
        a.extend([
            "--tmpfs".into(),
            format!("/home/agent:size={HOME_SIZE},exec"),
        ]);
    }
    for (k, v) in env {
        a.extend(["-e".into(), format!("{k}={v}")]);
    }
    a
}
