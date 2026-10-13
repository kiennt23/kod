//! `docker run` arguments for the agent, collect and score containers.
use crate::case::EvalCase;
use std::path::Path;

pub struct Mounts<'a> {
    pub work: &'a Path,
    /// True for collect and score, so nothing in those containers can change
    /// the workspace, including the held-back tests.
    pub work_read_only: bool,
    pub out: &'a Path,
    /// Read-only input at /in: the prompt for agent, the base tree for collect.
    pub input: Option<&'a Path>,
    /// Writable scratch at /tmp for build output. Only score gets it.
    pub scratch: Option<&'a Path>,
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
    let ro = if m.work_read_only { ":ro" } else { "" };
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
        "-v",
        &format!("{}:/work{ro}", m.work.display()),
        "-v",
        &format!("{}:/out", m.out.display()),
    ]
    .map(String::from)
    .to_vec();
    if let Some(input) = m.input {
        a.extend(["-v".into(), format!("{}:/in:ro", input.display())]);
    }
    if let Some(scratch) = m.scratch {
        a.extend(["-v".into(), format!("{}:/tmp", scratch.display())]);
    }
    for (k, v) in env {
        a.extend(["-e".into(), format!("{k}={v}")]);
    }
    a
}
