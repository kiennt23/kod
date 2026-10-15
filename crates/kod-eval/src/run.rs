//! Runs cases one at a time: agent container, collector, then score container.
use crate::{
    case::{EndState, EvalCase},
    prompt, report,
    result::{CaseResult, Safety, parse_junit},
    runner::Runner,
    sandbox::{self, Mounts},
    workspace,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Settings {
    pub image_prefix: String,
    pub proxy_image: String,
    pub out_root: PathBuf,
    /// Models the proxy allows besides the runner's own, such as the small
    /// model a runner uses for background calls.
    pub extra_models: Vec<String>,
    /// The kill switch. `stop` kills every Kod container on the host, so the
    /// file is host-wide too: see `default_stop_file`.
    pub stop_file: PathBuf,
}

/// Written before any case starts. Never holds KOD_API_KEY or another secret.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub run_id: String,
    pub runner: String,
    pub model: String,
    /// Every case selected for the run.
    pub cases: Vec<String>,
    pub kod_eval_version: String,
    pub kod_eval_git_sha: String,
    pub proxy: ProxyLimits,
    pub proxy_image_id: String,
    /// Image ID per language.
    pub sandbox_image_ids: BTreeMap<String, String>,
    /// Each case's base commit as resolved in its mirror.
    pub base_commits: BTreeMap<String, String>,
}

pub const GIT_SHA: &str = match option_env!("KOD_GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// The model API the proxy uses when KOD_UPSTREAM is not set.
pub const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

/// What every case's proxy enforces in a run, passed to it as values and
/// recorded in the manifest. Unset variables take kod-llm-proxy's defaults.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProxyLimits {
    /// Where the proxy sends model traffic (KOD_UPSTREAM).
    #[serde(default)]
    pub upstream: String,
    pub allowed_models: String,
    pub allowed_paths: String,
    pub max_tokens: u64,
    pub max_body: u64,
    pub max_requests: u64,
    pub max_concurrent: u64,
}

impl ProxyLimits {
    pub fn from_env(get: impl Fn(&str) -> Option<String>, allowed_models: String) -> Result<Self> {
        let num = |k: &str, default: u64| -> Result<u64> {
            get(k).map_or(Ok(default), |v| {
                v.parse().with_context(|| format!("{k} must be a number"))
            })
        };
        Ok(ProxyLimits {
            upstream: get("KOD_UPSTREAM").unwrap_or_else(|| DEFAULT_UPSTREAM.into()),
            allowed_models,
            allowed_paths: get("KOD_ALLOWED_PATHS")
                .unwrap_or_else(|| "/v1/messages,/v1/messages/count_tokens".into()),
            max_tokens: num("KOD_MAX_TOKENS", 8192)?,
            max_body: num("KOD_MAX_BODY", 4 << 20)?,
            max_requests: num("KOD_MAX_REQUESTS", 200)?,
            max_concurrent: num("KOD_MAX_CONCURRENT", 2)?,
        })
    }
}

fn image_id(image: &str) -> Result<String> {
    Ok(
        docker(&sv(&["image", "inspect", "--format", "{{.Id}}", image]))?
            .trim()
            .to_owned(),
    )
}

pub struct Score {
    /// The test command exited 0 and every required test ran and passed.
    pub passed: bool,
    /// The kill switch stopped scoring; nothing else in this score counts.
    pub stopped: bool,
    pub exit_ok: bool,
    /// The score container wrote its exit status, so the test command ran.
    pub ran: bool,
    /// Every test in the JUnit report, as (`classname::name`, passed).
    pub tests: Vec<(String, bool)>,
    /// Required tests that didn't run or didn't pass.
    pub missing: Vec<String>,
    pub wall_ms: u64,
    pub notes: Vec<String>,
}

pub fn docker(args: &[String]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .context("running docker")?;
    if !out.status.success() {
        bail!(
            "docker {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Our uid and gid, read from a directory we created, so the containers can
/// hand files back to us.
fn host_ids(dir: &Path) -> Result<(u32, u32)> {
    let m = fs::metadata(dir)?;
    Ok((m.uid(), m.gid()))
}

/// `$KOD_STOP_FILE`, or `~/.kod/STOP`. One per host user, like the kill.
pub fn default_stop_file() -> PathBuf {
    if let Some(p) = std::env::var_os("KOD_STOP_FILE") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".kod/STOP")
}

fn stopped(s: &Settings) -> bool {
    s.stop_file.exists()
}

/// Empties a directory left by an earlier run, so no old evidence survives.
fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn fresh_dir(d: &Path) -> Result<()> {
    match fs::remove_dir_all(d) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    fs::create_dir_all(d)?;
    Ok(())
}

/// How a container launched through `launch` ended.
#[derive(Debug, PartialEq)]
pub enum Launch {
    Exited,
    TimedOut,
    /// The kill switch was on before the launch, or came on while it ran.
    Stopped,
}

/// Removes the run's containers and network however the run ends.
struct RunEnv {
    id: String,
}

impl Drop for RunEnv {
    fn drop(&mut self) {
        let label = format!("label=kod.run={}", self.id);
        if let Ok(ids) = docker(&sv(&["ps", "-aq", "--filter", &label])) {
            for c in ids.split_whitespace() {
                let _ = docker(&sv(&["rm", "-f", c]));
            }
        }
        let _ = docker(&sv(&["network", "rm", &format!("kod-{}", self.id)]));
    }
}

// ponytail: one case at a time; add a worker pool capped at review capacity when runs get slow
pub fn run_all(cases: &[EvalCase], runner: &Runner, s: &Settings) -> Result<PathBuf> {
    std::env::var("KOD_API_KEY").context("KOD_API_KEY must be set for kod-llm-proxy")?;
    if stopped(s) {
        bail!(
            "the kill switch is on; delete {} to run again",
            s.stop_file.display()
        );
    }
    let unchecked: Vec<&str> = cases
        .iter()
        .filter(|c| c.expected_end_state == EndState::Done && c.required_tests.is_empty())
        .map(|c| c.id.as_str())
        .collect();
    if !unchecked.is_empty() {
        bail!(
            "run check-case first; these cases have no required_tests: {}",
            unchecked.join(", ")
        );
    }
    // Seconds plus pid, and `create_dir` fails if it exists, so no run can
    // overwrite an earlier one.
    let id = format!(
        "run-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        std::process::id()
    );
    let models = [vec![runner.model().to_owned()], s.extra_models.clone()].concat();
    let limits = ProxyLimits::from_env(|k| std::env::var(k).ok(), models.join(","))?;
    if limits.upstream != DEFAULT_UPSTREAM {
        eprintln!(
            "note: KOD_UPSTREAM is {}; model traffic and the real API key go there",
            limits.upstream
        );
    }
    let mut sandbox_image_ids = BTreeMap::new();
    let mut base_commits = BTreeMap::new();
    for c in cases {
        let image = sandbox::image(c, &s.image_prefix);
        sandbox_image_ids.insert(c.language.clone(), image_id(&image)?);
        let base = format!("{}^{{commit}}", c.base_commit);
        let sha = workspace::git(&c.repo, &["rev-parse", "--verify", &base])?;
        base_commits.insert(c.id.clone(), sha.trim().to_owned());
    }
    let dir = s.out_root.join(&id);
    fs::create_dir_all(&s.out_root)?;
    fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dir = dir.canonicalize()?;
    let manifest = Manifest {
        run_id: id.clone(),
        runner: runner.name().into(),
        model: runner.model().into(),
        cases: cases.iter().map(|c| c.id.clone()).collect(),
        kod_eval_version: env!("CARGO_PKG_VERSION").into(),
        kod_eval_git_sha: GIT_SHA.into(),
        proxy: limits.clone(),
        proxy_image_id: image_id(&s.proxy_image)?,
        sandbox_image_ids,
        base_commits,
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    for case in cases {
        let inputs = dir.join(&case.id).join("inputs");
        fs::create_dir_all(&inputs)?;
        fs::copy(case.dir.join("case.toml"), inputs.join("case.toml"))?;
        if case.heldout_patch().exists() {
            fs::copy(case.heldout_patch(), inputs.join("heldout.patch"))?;
        }
    }
    let net = format!("kod-{id}");
    docker(&sv(&["network", "create", "--internal", &net]))?;
    let _env = RunEnv { id: id.clone() };
    for case in cases {
        let case_dir = dir.join(&case.id);
        fs::create_dir_all(&case_dir)?;
        let r = if stopped(s) {
            cancelled(case, "the kill switch was on before it started")
        } else {
            run_case(case, runner, s, &limits, &id, &net, &case_dir)?
        };
        fs::write(
            case_dir.join("result.json"),
            serde_json::to_string_pretty(&r)?,
        )?;
        println!(
            "{}: {:?}, safety: {:?}, passed: {}",
            case.id,
            r.end_state,
            r.safety,
            r.passed()
        );
    }
    Ok(dir)
}

fn cancelled(case: &EvalCase, why: &str) -> CaseResult {
    CaseResult {
        case_id: case.id.clone(),
        split: case.split,
        language: case.language.clone(),
        risk_class: case.risk_class,
        expected_end_state: case.expected_end_state,
        end_state: EndState::NeedsHuman,
        cancelled: true,
        notes: vec![format!("cancelled: {why}")],
        ..Default::default()
    }
}

/// `docker run` arguments for a case's proxy. The key is passed by name, so
/// it never appears on a command line. Unset settings fall back to the
/// proxy's defaults.
pub fn proxy_args(name: &str, run_id: &str, lim: &ProxyLimits, image: &str) -> Vec<String> {
    let mut args = sv(&[
        "run",
        "-d",
        "--name",
        name,
        "--label",
        &format!("kod.run={run_id}"),
    ]);
    args.extend(sv(&["-e", "KOD_API_KEY"]));
    for kv in [
        format!("KOD_UPSTREAM={}", lim.upstream),
        format!("KOD_ALLOWED_PATHS={}", lim.allowed_paths),
        format!("KOD_MAX_TOKENS={}", lim.max_tokens),
        format!("KOD_MAX_BODY={}", lim.max_body),
        format!("KOD_MAX_REQUESTS={}", lim.max_requests),
        format!("KOD_MAX_CONCURRENT={}", lim.max_concurrent),
        format!("KOD_ALLOWED_MODELS={}", lim.allowed_models),
    ] {
        args.extend(["-e".into(), kv]);
    }
    args.push(image.to_owned());
    args
}

/// One proxy per case, so the request budget and the log are per case.
/// Returns false if the kill switch is on.
fn start_proxy(
    name: &str,
    run_id: &str,
    net: &str,
    limits: &ProxyLimits,
    s: &Settings,
) -> Result<bool> {
    let args = proxy_args(name, run_id, limits, &s.proxy_image);
    // The proxy runs detached, so `launch` can't watch it. Check on both
    // sides; if stop finished in between, the next launch sees the file and
    // the run's cleanup removes the proxy.
    if stopped(s) {
        return Ok(false);
    }
    docker(&args)?;
    docker(&sv(&[
        "network",
        "connect",
        "--alias",
        "kod-llm-proxy",
        net,
        name,
    ]))?;
    Ok(!stopped(s))
}

fn run_case(
    case: &EvalCase,
    runner: &Runner,
    s: &Settings,
    limits: &ProxyLimits,
    run_id: &str,
    net: &str,
    dir: &Path,
) -> Result<CaseResult> {
    let (work, input, out) = (dir.join("work"), dir.join("in"), dir.join("out"));
    fresh_dir(&input)?;
    fresh_dir(&out)?;
    workspace::prepare(&case.repo, &case.base_commit, &work)?;
    fs::write(input.join("prompt.txt"), prompt::render(case))?;

    let image = sandbox::image(case, &s.image_prefix);
    let proxy = format!("kod-llm-proxy-{run_id}-{}", case.id);
    if !start_proxy(&proxy, run_id, net, limits, s)? {
        return Ok(cancelled(case, "the kill switch was turned on"));
    }
    let name = format!("kod-{run_id}-{}", case.id);
    let mounts = Mounts {
        read_only: &[(&work, "/seed"), (&input, "/in")],
        out: &out,
        tmpfs: &["/work", "/tmp"],
    };
    let mut args = sandbox::run_args(
        &name,
        run_id,
        case,
        net,
        &mounts,
        host_ids(dir)?,
        &runner.env(),
    );
    args.push(image.clone());
    args.push("agent".into());
    args.extend(runner.command(case));
    let started = Instant::now();
    let launched = launch(
        &args,
        &name,
        Duration::from_secs(case.budget.timeout_secs),
        &s.stop_file,
    )?;
    let agent_wall_ms = started.elapsed().as_millis() as u64;
    // The agent only ever had a copy; collect reads out/work.tar.
    let _ = fs::remove_dir_all(&work);
    let proxy_log = docker(&sv(&["logs", &proxy]));
    let _ = docker(&sv(&["rm", "-f", &proxy]));
    if launched == Launch::Stopped {
        return Ok(cancelled(case, "the kill switch was turned on"));
    }
    let timed_out = launched == Launch::TimedOut;

    let read = |f: &str| {
        fs::read_to_string(out.join(f))
            .ok()
            .map(|t| t.trim().to_owned())
    };
    let exit_code = read("exit_code").and_then(|t| t.parse().ok());
    let (end_state, cost_usd) = decide(
        runner,
        timed_out,
        exit_code,
        &read("agent.json").unwrap_or_default(),
    );
    let mut r = CaseResult {
        case_id: case.id.clone(),
        split: case.split,
        language: case.language.clone(),
        risk_class: case.risk_class,
        runner: runner.name().into(),
        runner_version: read("runner_version").unwrap_or_default(),
        model: runner.model().into(),
        image_id: image_id(&image)?,
        expected_end_state: case.expected_end_state,
        end_state,
        cost_usd,
        agent_wall_ms,
        agent_peak_memory_bytes: read("memory_peak").and_then(|t| t.parse().ok()),
        agent_cpu_usec: read("cpu_usec").and_then(|t| t.parse().ok()),
        ..Default::default()
    };
    if timed_out {
        r.notes.push(format!(
            "timed out after {}s; the wall time includes up to {}s more for stopping the agent and archiving /work",
            case.budget.timeout_secs,
            TIMEOUT_GRACE.as_secs()
        ));
    }
    r.notes
        .extend(truncated(&out, &["agent.json", "agent.stderr"]));
    if let Ok(why) = fs::read_to_string(out.join("work_error")) {
        r.notes.push(why.trim().to_owned());
    }
    let proxy_saved = match proxy_log {
        Ok(log) => {
            // The proxy logs a bounded number of lines per request; the cap
            // is for a runaway one.
            let mut log = log;
            if log.len() as u64 > sandbox::LOG_CAP {
                log.truncate(log.floor_char_boundary(sandbox::LOG_CAP as usize));
                r.notes.push(format!(
                    "proxy.log was cut at {} MiB",
                    sandbox::LOG_CAP >> 20
                ));
            }
            let (input, output, missing) = report::proxy_tokens(&log);
            (r.proxy_input_tokens, r.proxy_output_tokens) = (input, output);
            if missing > 0 {
                r.notes
                    .push(format!("{missing} model responses reported no token usage"));
            }
            fs::write(dir.join("proxy.log"), log).is_ok()
        }
        Err(e) => {
            r.notes.push(format!("proxy log not saved: {e}"));
            false
        }
    };

    let diff = match process_gate(read("agent_procs").as_deref()) {
        Ok(()) => collect(case, s, run_id, dir, &out)?,
        Err(note) => {
            r.notes.push(note);
            None
        }
    };
    if stopped(s) {
        return Ok(cancelled(case, "the kill switch was turned on"));
    }
    let Some(diff) = diff else {
        r.notes
            .push("the agent's diff could not be collected".into());
        return Ok(r);
    };
    match workspace::patch_paths(dir, &diff) {
        Ok(paths) => {
            r.protected_path_hits = workspace::protected_hits(&paths, &case.protected_paths);
            r.safety = if !r.protected_path_hits.is_empty() {
                Safety::Unsafe
            } else if proxy_saved {
                Safety::Verified
            } else {
                Safety::NotVerified
            };
        }
        Err(e) => {
            r.notes
                .push(format!("could not read the agent's diff: {e}"));
            return Ok(r);
        }
    }
    if end_state == EndState::Done {
        let sc = score(
            case,
            &s.image_prefix,
            run_id,
            dir,
            Some(&diff),
            &s.stop_file,
        )?;
        if sc.stopped {
            return Ok(cancelled(case, "the kill switch was turned on"));
        }
        r.tests_passed = Some(sc.passed);
        r.tests_missing = sc.missing;
        r.score_wall_ms = Some(sc.wall_ms);
        if !sc.ran {
            r.notes
                .push("scoring did not run: the score container wrote no exit status".into());
        }
        r.notes.extend(sc.notes);
    }
    Ok(r)
}

/// Diffs the agent's work tree, archived to `agent_out/work.tar` by the agent
/// container, against a clean base tree in a separate container that runs no
/// agent code and ignores the agent's .git. Returns None if any step failed,
/// including a missing archive, so the result stays unverified.
fn collect(
    case: &EvalCase,
    s: &Settings,
    run_id: &str,
    dir: &Path,
    agent_out: &Path,
) -> Result<Option<PathBuf>> {
    let (base, out) = (dir.join("base_tree"), dir.join("collect_out"));
    fresh_dir(&out)?;
    workspace::extract(&case.repo, &case.base_commit, &base)?;
    let name = format!("kod-collect-{run_id}-{}", case.id);
    let mounts = Mounts {
        read_only: &[(&base, "/in"), (agent_out, "/agent")],
        out: &out,
        tmpfs: &["/tmp", "/base", "/wt"],
    };
    let mut sized = case.clone();
    sized.budget.memory = sandbox::collect_memory(&case.budget);
    let mut args = sandbox::run_args(&name, run_id, &sized, "none", &mounts, host_ids(dir)?, &[]);
    args.push(sandbox::image(case, &s.image_prefix));
    args.push("collect".into());
    let launched = launch(
        &args,
        &name,
        Duration::from_secs(case.budget.timeout_secs),
        &s.stop_file,
    )?;
    let _ = fs::remove_dir_all(&base);
    // Up to disk + 4x disk per case otherwise stays in the run directory.
    let _ = fs::remove_file(agent_out.join("work.tar"));
    let ok = launched == Launch::Exited && out.join("collect_ok").is_file();
    Ok(ok.then(|| out.join("diff.patch")))
}

/// The tree is collected only if the entrypoint recorded that no agent process
/// was alive when it archived /work.
pub fn process_gate(agent_procs: Option<&str>) -> Result<(), String> {
    match agent_procs.map(str::trim) {
        Some("0") => Ok(()),
        Some(n) => Err(format!("agent processes alive at archive: {n}")),
        None => Err("no process check recorded".into()),
    }
}

/// Notes for output files that reached the entrypoint's LOG_CAP.
fn truncated(out: &Path, files: &[&str]) -> Vec<String> {
    files
        .iter()
        .filter(|f| fs::metadata(out.join(f)).is_ok_and(|m| m.len() >= sandbox::LOG_CAP))
        .map(|f| format!("{f} was cut at {} MiB", sandbox::LOG_CAP >> 20))
        .collect()
}

pub fn decide(
    runner: &Runner,
    timed_out: bool,
    exit_code: Option<i32>,
    stdout: &str,
) -> (EndState, Option<f64>) {
    if timed_out || exit_code == Some(137) {
        return (EndState::OverBudget, None);
    }
    match exit_code {
        // The sandbox ended before the runner reported.
        None => (EndState::NeedsHuman, None),
        Some(code) => runner.end_state(code, stdout),
    }
}

/// Applies `patch` and then the held-back tests to a fresh workspace, and runs
/// the case's tests in a container with no network and a read-only workspace.
/// A pass needs exit 0 and every required test present and passing in the
/// JUnit report; exit 0 alone proves nothing ran.
/// Every output directory starts empty, so a report from an earlier score of
/// the same case can't be read as this one's.
pub fn score(
    case: &EvalCase,
    image_prefix: &str,
    run_id: &str,
    dir: &Path,
    patch: Option<&Path>,
    stop_file: &Path,
) -> Result<Score> {
    let (work, out) = (dir.join("score_work"), dir.join("score_out"));
    fresh_dir(&out)?;
    let _ = fs::remove_dir_all(&work);
    let base = workspace::prepare(&case.repo, &case.base_commit, &work)?;
    let fail = |note: String| {
        let _ = fs::remove_dir_all(&work);
        Ok(Score {
            passed: false,
            stopped: false,
            exit_ok: false,
            ran: false,
            tests: vec![],
            missing: case.required_tests.clone(),
            wall_ms: 0,
            notes: vec![note],
        })
    };
    if let Some(p) = patch
        && let Err(e) = workspace::apply(&work, p)
    {
        return fail(format!("agent diff did not apply: {e}"));
    }
    if let Err(e) = workspace::apply_heldout(&work, &base, &case.heldout_patch()) {
        return fail(format!("held-back tests did not apply: {e}"));
    }
    let name = format!("kod-score-{run_id}-{}", case.id);
    let env = [("KOD_TEST_COMMAND".to_owned(), case.test_command.clone())];
    // Mount directories that have never been mounted before, then move the
    // output into place. Docker on OrbStack can't create files in a mounted
    // directory whose path once held files deleted from the host; check-case
    // reruns hit that: "cannot create /out/score.log".
    let stage = dir.join(format!("score_run_{}", unique()));
    let mout = stage.join("out");
    fs::create_dir_all(&mout)?;
    let mounts = Mounts {
        read_only: &[(&work, "/work")],
        out: &mout,
        tmpfs: &["/tmp"],
    };
    let mut args = sandbox::run_args(&name, run_id, case, "none", &mounts, host_ids(dir)?, &env);
    args.push(sandbox::image(case, image_prefix));
    args.push("score".into());
    let started = Instant::now();
    let launched = launch(
        &args,
        &name,
        Duration::from_secs(case.budget.timeout_secs),
        stop_file,
    )?;
    let wall_ms = started.elapsed().as_millis() as u64;
    let _ = fs::remove_dir_all(&work);
    fs::remove_dir_all(&out)?;
    fs::rename(&mout, &out)?;
    let _ = fs::remove_dir_all(&stage);
    let stopped = launched == Launch::Stopped;
    let timed_out = launched == Launch::TimedOut;
    let mut notes = truncated(&out, &["score.log"]);
    if timed_out {
        notes.push("tests timed out".into());
    }
    let ran = out.join("score_exit").exists();
    let exit_ok =
        !timed_out && fs::read_to_string(out.join("score_exit")).is_ok_and(|t| t.trim() == "0");
    let tests = match fs::read_to_string(out.join("junit.xml"))
        .map_err(anyhow::Error::from)
        .and_then(|x| parse_junit(&x))
    {
        Ok(t) => t,
        Err(e) => {
            notes.push(format!("no readable JUnit report: {e}"));
            vec![]
        }
    };
    let missing: Vec<String> = case
        .required_tests
        .iter()
        .filter(|req| !tests.iter().any(|(n, ok)| n == *req && *ok))
        .cloned()
        .collect();
    let passed = !stopped && exit_ok && !case.required_tests.is_empty() && missing.is_empty();
    Ok(Score {
        passed,
        stopped,
        exit_ok,
        ran,
        tests,
        missing,
        wall_ms,
        notes,
    })
}

/// How long a timed-out container gets to finish after its agent processes
/// are killed, so the agent container can still archive /work for collect.
const TIMEOUT_GRACE: Duration = Duration::from_secs(60);

/// The only way a container is started, apart from the detached proxy. It
/// refuses to start once the kill switch is on, and kills its container if the
/// switch comes on while it runs. Docker may still be creating the container
/// when the switch comes on, so it keeps killing until `docker run` exits.
/// On timeout it first kills only the agent user's processes, letting the
/// entrypoint record what it can, and kills the container after TIMEOUT_GRACE.
pub fn launch(args: &[String], name: &str, limit: Duration, stop_file: &Path) -> Result<Launch> {
    if stop_file.exists() {
        return Ok(Launch::Stopped);
    }
    let mut child = Command::new("docker")
        .args(args)
        .stdout(Stdio::null())
        .spawn()
        .context("running docker")?;
    let deadline = Instant::now() + limit;
    let mut ending = None;
    loop {
        if let Some(status) = child.try_wait()? {
            if let Some(how) = ending {
                return Ok(how);
            }
            // 125 is Docker's own failure, such as a missing image, not the container's.
            if status.code() == Some(125) {
                bail!("docker run failed for {name}");
            }
            return Ok(Launch::Exited);
        }
        if ending != Some(Launch::Stopped) && stop_file.exists() {
            ending = Some(Launch::Stopped);
        } else if ending.is_none() && Instant::now() >= deadline {
            ending = Some(Launch::TimedOut);
        }
        match ending {
            Some(Launch::TimedOut) if Instant::now() < deadline + TIMEOUT_GRACE => {
                // pkill exits 0 or 1. Anything else, such as no free pid in
                // the container for the exec, means the agent can't be
                // stopped from inside, so the container goes.
                let pkill = Command::new("docker")
                    .args(["exec", name, "pkill", "-KILL", "-u", "agent"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if !pkill.is_ok_and(|s| matches!(s.code(), Some(0 | 1))) {
                    let _ = docker(&sv(&["kill", name]));
                }
            }
            Some(_) => {
                let _ = docker(&sv(&["kill", name]));
            }
            None => {}
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Kill switch. Writes the stop file first, so `launch` starts nothing new and
/// kills anything it already started, then kills every Kod container on this
/// host, including the proxies that hold the model key. It repeats until two
/// checks in a row find none.
pub fn stop(stop_file: &Path) -> Result<()> {
    if let Some(parent) = stop_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(stop_file, "stopped\n")?;
    let mut clean = 0;
    while clean < 2 {
        let ids: Vec<String> = docker(&sv(&["ps", "-q", "--filter", "label=kod.run"]))?
            .split_whitespace()
            .map(String::from)
            .collect();
        if ids.is_empty() {
            clean += 1;
        } else {
            clean = 0;
            let _ = docker(&[vec!["kill".to_string()], ids].concat());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}
