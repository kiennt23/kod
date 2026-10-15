//! The loop's tools. The sandbox is the boundary: paths are not confined to
//! the workspace, because `run` reaches the same files anyway.
use crate::EndState;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Most bytes of one tool result the model sees.
pub const OUTPUT_CAP: usize = 30_000;
/// Lines read_file returns when no limit is given.
pub const READ_LIMIT: u64 = 2000;
/// Larger files are read in parts with `run` (head, sed -n).
const MAX_READ: u64 = 10 << 20;
/// A command that writes more than this is killed; /tmp may be a small tmpfs.
const MAX_OUTPUT: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    /// Set by `finish`: the run ends with this state and summary.
    pub finish: Option<(EndState, String)>,
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "read_file",
            "description": "Read a UTF-8 text file. Returns numbered lines. Paths are relative to the workspace.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "First line, from 1"},
                "limit": {"type": "integer", "description": "Number of lines, default 2000"}
            }, "required": ["path"]}
        },
        {
            "name": "write_file",
            "description": "Create or overwrite a file with the given content. Creates parent directories.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }, "required": ["path", "content"]}
        },
        {
            "name": "edit_file",
            "description": "Replace old_string with new_string in a file. old_string must occur exactly once; include enough context to make it unique.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"}
            }, "required": ["path", "old_string", "new_string"]}
        },
        {
            "name": "run",
            "description": "Run a shell command (sh -c) in the workspace with no stdin. Returns stdout and stderr together, cut in the middle if long, then the exit code. Commands are killed at the time limit.",
            "input_schema": {"type": "object", "properties": {
                "command": {"type": "string"}
            }, "required": ["command"]}
        },
        {
            "name": "finish",
            "description": "End the task. state is done (the change is complete and its tests pass), blocked (it cannot be done as asked) or needs_human (a person must decide something).",
            "input_schema": {"type": "object", "properties": {
                "state": {"type": "string", "enum": ["done", "blocked", "needs_human"]},
                "summary": {"type": "string"}
            }, "required": ["state", "summary"]}
        }
    ])
}

/// Runs one tool call. Every failure is an error result for the model, never
/// a panic.
pub fn call(workspace: &Path, timeout: Duration, name: &str, input: &Value) -> ToolResult {
    let r = match name {
        "read_file" => read_file(workspace, input),
        "write_file" => write_file(workspace, input),
        "edit_file" => edit_file(workspace, input),
        "run" => run(workspace, timeout, input),
        "finish" => return finish(input),
        other => Err(format!("unknown tool {other:?}")),
    };
    let (text, is_error) = match r {
        Ok(t) => (t, false),
        Err(t) => (t, true),
    };
    ToolResult {
        text,
        is_error,
        finish: None,
    }
}

fn arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .ok_or_else(|| format!("missing string argument {key:?}"))
}

fn path(ws: &Path, input: &Value) -> Result<PathBuf, String> {
    Ok(ws.join(arg(input, "path")?))
}

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

fn read_file(ws: &Path, input: &Value) -> Result<String, String> {
    let p = path(ws, input)?;
    let len = fs::metadata(&p).map_err(io(&p))?.len();
    if len > MAX_READ {
        return Err(format!(
            "{} is {len} bytes; read parts of it with run (head, sed -n)",
            p.display()
        ));
    }
    let text = fs::read_to_string(&p).map_err(io(&p))?;
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"].as_u64().unwrap_or(READ_LIMIT) as usize;
    let out: String = text
        .lines()
        .enumerate()
        .skip(offset - 1)
        .take(limit)
        .map(|(i, l)| format!("{:>6}\t{l}\n", i + 1))
        .collect();
    if out.is_empty() {
        return Ok("(no lines in that range)".into());
    }
    Ok(clip(out.as_bytes()))
}

fn write_file(ws: &Path, input: &Value) -> Result<String, String> {
    let p = path(ws, input)?;
    let content = arg(input, "content")?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(io(dir))?;
    }
    fs::write(&p, content).map_err(io(&p))?;
    Ok(format!("wrote {} bytes to {}", content.len(), p.display()))
}

fn edit_file(ws: &Path, input: &Value) -> Result<String, String> {
    let p = path(ws, input)?;
    let (old, new) = (arg(input, "old_string")?, arg(input, "new_string")?);
    if old.is_empty() {
        return Err("old_string is empty".into());
    }
    let mut text = fs::read_to_string(&p).map_err(io(&p))?;
    let Some(i) = text.find(old) else {
        return Err(format!("old_string not found in {}", p.display()));
    };
    // Searching again one character on also catches overlapping matches.
    let next = i + old.chars().next().map_or(1, char::len_utf8);
    if text[next..].contains(old) {
        return Err(format!(
            "old_string occurs more than once in {}; include more context",
            p.display()
        ));
    }
    text.replace_range(i..i + old.len(), new);
    fs::write(&p, text).map_err(io(&p))?;
    Ok(format!("edited {}", p.display()))
}

fn run(ws: &Path, timeout: Duration, input: &Value) -> Result<String, String> {
    let command = arg(input, "command")?;
    let err = |e: std::io::Error| format!("running the command: {e}");
    let mut out = scratch().map_err(err)?;
    // One command at a time per process: the sweep below kills every child of
    // this process, which must never include another call's live command.
    let _one = RUN.lock().unwrap_or_else(|e| e.into_inner());
    become_subreaper();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(ws)
        .stdin(Stdio::null())
        .stdout(out.try_clone().map_err(err)?)
        .stderr(out.try_clone().map_err(err)?)
        // Its own process group, so a timeout kills its children too.
        .process_group(0)
        .spawn()
        .map_err(err)?;
    let pid = child.id();
    let status = wait_for(&mut child, &out, timeout);
    // Cleanup runs on every path, an early error from wait_for included.
    // Leftover background children are killed too, detached ones included
    // (sweep_orphans): they would otherwise keep changing the workspace during
    // later turns.
    // ponytail: on a normal exit the leader is already reaped here, so if its
    // group is empty and the kernel reuses the pid for a new group leader, this
    // hits that group. Negligible in a one-run sandbox; if it matters, signal
    // before reaping (try_wait, then waitid with WNOWAIT).
    let killed = kill_group(pid);
    // The leader too, in case the group signal failed, so wait() returns.
    let _ = child.kill();
    let _ = child.wait();
    let note = if killed & sweep_orphans() {
        ""
    } else {
        " [its background processes could not be killed]"
    };
    let status = status.map_err(|e| format!("{e}{note}"))? + note;
    let mut text = read_capped(&mut out).map_err(err)?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(format!("{text}{status}"))
}

/// Polls until the command exits, floods its output or runs out of time, and
/// returns its status line. The caller kills and reaps whatever is left.
fn wait_for(child: &mut Child, out: &File, timeout: Duration) -> Result<String, String> {
    let err = |e: std::io::Error| format!("running the command: {e}");
    let start = Instant::now();
    loop {
        if let Some(s) = child.try_wait().map_err(err)? {
            return Ok(match s.code() {
                Some(c) => format!("[exit code {c}]"),
                None => "[killed by a signal]".to_owned(),
            });
        }
        let flooded = out.metadata().map_err(err)?.len() > MAX_OUTPUT;
        if flooded || start.elapsed() >= timeout {
            let why = if flooded {
                "the output limit was exceeded"
            } else {
                "the command hit the time limit"
            };
            return Ok(format!(
                "[killed after {}s: {why}]",
                start.elapsed().as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

static RUN: Mutex<()> = Mutex::new(());

/// Makes this process the reaper of its orphaned descendants, so a command's
/// children that leave its process group (setsid, double fork) are reparented
/// here instead of to init, where sweep_orphans finds them.
// ponytail: Linux only (prctl). Elsewhere (macOS dev builds) only the process
// group is killed, so setsid'd descendants survive the command. On Linux a
// descendant escapes only by changing uid; the sandbox runs one uid and the
// container bounds the whole case anyway.
#[cfg(target_os = "linux")]
fn become_subreaper() {
    // SAFETY: prctl with integer arguments only sets a flag on this process.
    unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
}
#[cfg(not(target_os = "linux"))]
fn become_subreaper() {}

/// SIGKILLs and reaps every child of this process until none is left. Called
/// with RUN held after the command is reaped, so every child is a leftover of
/// a command: killing one reparents its own children here for the next pass.
/// Returns false if one could not be killed.
#[cfg(target_os = "linux")]
const SWEEP_LIMIT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
fn sweep_orphans() -> bool {
    let me = std::process::id().to_string();
    // A fork storm could refill the list forever; give up and report it.
    let deadline = Instant::now() + SWEEP_LIMIT;
    loop {
        if Instant::now() >= deadline {
            return false;
        }
        let children: Vec<libc::pid_t> = fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let pid = e.file_name().to_str()?.parse().ok()?;
                let stat = fs::read_to_string(e.path().join("stat")).ok()?;
                // The parent pid is the second field after the ")" of comm.
                let ppid = stat.rsplit_once(')')?.1.split_whitespace().nth(1)?;
                (ppid == me).then_some(pid)
            })
            .collect();
        if children.is_empty() {
            return true;
        }
        for pid in children {
            // SAFETY: kill and waitpid take integers; the pid is our child, so
            // it cannot be reused until waitpid reaps it.
            unsafe {
                if libc::kill(pid, libc::SIGKILL) != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return false;
                }
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn sweep_orphans() -> bool {
    true
}

/// SIGKILLs the command's process group (its id is the leader's pid, from
/// process_group(0)). killpg(2) needs no fork and no shell, so it works even
/// when the group holds every pid the sandbox allows. A group that is already
/// gone (ESRCH) counts as killed; any other failure returns false.
fn kill_group(pid: u32) -> bool {
    // SAFETY: killpg takes two integers and only sends a signal.
    if unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn finish(input: &Value) -> ToolResult {
    let state = match input["state"].as_str() {
        Some("done") => Some(EndState::Done),
        Some("blocked") => Some(EndState::Blocked),
        Some("needs_human") => Some(EndState::NeedsHuman),
        _ => None,
    };
    match (state, input["summary"].as_str()) {
        (Some(s), Some(summary)) => ToolResult {
            text: "finished".into(),
            is_error: false,
            finish: Some((s, summary.to_owned())),
        },
        _ => ToolResult {
            text: "finish needs state (done, blocked or needs_human) and summary".into(),
            is_error: true,
            finish: None,
        },
    }
}

/// An unlinked temp file: nothing is left behind, and a child holding it open
/// cannot block the loop the way a pipe would.
fn scratch() -> std::io::Result<File> {
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "kod-loop-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&p)?;
    fs::remove_file(&p)?;
    Ok(f)
}

/// Head and tail of the output; never reads more than OUTPUT_CAP bytes, even
/// if a background child is still writing.
fn read_capped(f: &mut File) -> std::io::Result<String> {
    let len = f.metadata()?.len();
    let half = (OUTPUT_CAP / 2) as u64;
    f.seek(SeekFrom::Start(0))?;
    if len <= OUTPUT_CAP as u64 {
        let mut b = vec![];
        f.by_ref().take(OUTPUT_CAP as u64).read_to_end(&mut b)?;
        return Ok(String::from_utf8_lossy(&b).into_owned());
    }
    let mut head = vec![];
    f.by_ref().take(half).read_to_end(&mut head)?;
    f.seek(SeekFrom::Start(len - half))?;
    let mut tail = vec![];
    f.by_ref().take(half).read_to_end(&mut tail)?;
    Ok(join(&head, len - 2 * half, &tail))
}

fn clip(b: &[u8]) -> String {
    if b.len() <= OUTPUT_CAP {
        return String::from_utf8_lossy(b).into_owned();
    }
    let half = OUTPUT_CAP / 2;
    join(
        &b[..half],
        (b.len() - 2 * half) as u64,
        &b[b.len() - half..],
    )
}

fn join(head: &[u8], omitted: u64, tail: &[u8]) -> String {
    format!(
        "{}\n[... {omitted} bytes omitted ...]\n{}",
        String::from_utf8_lossy(head),
        String::from_utf8_lossy(tail)
    )
}
