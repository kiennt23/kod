//! Lifecycle scenarios: cancel, label removal, kill, restart reconciliation and kill during outages.
//! Each drives the running host through its public ingress against the real Docker sandbox and
//! observes the result in the dispatcher database, Docker and the fake forge/model.
use super::{
    fixture::Fixture,
    scenarios::{Checks, scenario},
};
use crate::{
    acceptance::{ObservationCode, Scenario},
    deadlines::MODEL_CANCEL,
};
use anyhow::{Context, Result, ensure};
use kod_eval::connected::ABSENCE;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn db(h: &Fixture) -> Result<Connection> {
    kod_dispatcher::store::open(&h.config.paths.dispatcher_db)
}
fn latest(h: &Fixture) -> Result<(String, String)> {
    let (seq, state): (i64, String) = db(h)?.query_row(
        "SELECT seq,state FROM runs ORDER BY seq DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((kod_dispatcher::run_id(seq), state))
}
fn state(h: &Fixture, run: &str) -> Result<String> {
    let seq = kod_dispatcher::store::seq_of(run).context("run id")?;
    Ok(db(h)?.query_row("SELECT state FROM runs WHERE seq=?1", [seq], |r| r.get(0))?)
}
fn run_count(h: &Fixture) -> Result<i64> {
    Ok(db(h)?.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?)
}
async fn until(limit: Duration, mut f: impl FnMut() -> Result<bool>) -> Result<bool> {
    let end = Instant::now() + limit;
    loop {
        if f()? {
            return Ok(true);
        }
        if Instant::now() >= end {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
/// Docker containers of `run` that still exist, running or not.
fn containers(run: &str) -> Result<Vec<String>> {
    let out = super::proc::output(
        std::process::Command::new("docker").args(["ps", "-a", "--format", "{{.Names}}"]),
        None,
        super::proc::SHORT,
    )?;
    ensure!(out.status.success(), "docker ps failed");
    let owned = kod_eval::connected::names(run)?;
    Ok(String::from_utf8(out.stdout)?
        .lines()
        .filter(|n| owned.iter().any(|o| o == n))
        .map(str::to_owned)
        .collect())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A `labeled` delivery that differs from every earlier one, so it is a new delivery.
async fn labeled(h: &Fixture, action: &str, nonce: &str) -> Result<()> {
    let mut v: Value = serde_json::from_slice(&h.payload(action, "kiennt23")?)?;
    v["nonce"] = json!(nonce);
    let status = h
        .webhook("lifecycle", &serde_json::to_vec(&v)?, true)
        .await?;
    ensure!(status.is_success(), "delivery {action} rejected: {status}");
    Ok(())
}
async fn cancel_comment(h: &Fixture, nonce: &str) -> Result<()> {
    let issue = h
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .issue
        .clone();
    let url = format!("{}/repos/acme/app/issues/7", h.provider.api);
    let id = u64::try_from(now())?;
    let body = json!({"action":"created","nonce":nonce,"repository":{"full_name":"acme/app"},
        "sender":{"id":9,"login":"kiennt23","type":"User"},"issue":issue,
        "comment":{"id":id,"body":"/kod cancel","issue_url":url,"user":{"id":9,"login":"kiennt23","type":"User"}}});
    let status = h
        .webhook_event(
            "issue_comment",
            "lifecycle",
            &serde_json::to_vec(&body)?,
            true,
        )
        .await?;
    ensure!(status.is_success(), "comment rejected: {status}");
    Ok(())
}
fn deliveries(h: &Fixture) -> Result<Vec<(i64, i64, Option<String>)>> {
    let c = db(h)?;
    let mut q =
        c.prepare("SELECT attempts, done, error FROM deliveries ORDER BY rowid DESC LIMIT 3")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
type Followup = (String, i64, i64, Option<String>);
fn followups(h: &Fixture) -> Result<Vec<Followup>> {
    let c = db(h)?;
    let mut q = c.prepare("SELECT kind, attempts, done, error FROM followups WHERE done=0 OR attempts>0 ORDER BY rowid DESC LIMIT 8")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
fn halted(h: &Fixture) -> Result<bool> {
    Ok(db(h)?.query_row("SELECT halted FROM control", [], |r| r.get::<_, i64>(0))? == 1)
}
async fn command(h: &Fixture, name: &str) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(40);
    loop {
        match command_once(h, name).await {
            // Resume is refused until an in-flight global stop has confirmed; retry with a new signature.
            Err(_) if name == "resume" && Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(500)).await
            }
            r => return r,
        }
    }
}
async fn command_once(h: &Fixture, name: &str) -> Result<()> {
    let status = h
        .command(json!({"command":name,"by":"kiennt23","at":now()}))
        .await?;
    // A kill whose credential cleanup is still pending answers 403 ("not complete") but has
    // halted the host and keeps retrying; the scenarios verify the effects directly.
    ensure!(
        status.is_success() || name == "kill",
        "{name} refused: {status}"
    );
    Ok(())
}
/// Holds the model request of a new run open, as a streaming response in flight.
async fn streaming_run(h: &Fixture, nonce: &str) -> Result<String> {
    let (flag, entered) = {
        let s = h
            .provider
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?;
        (s.model_blocked.clone(), s.model_entered.clone())
    };
    flag.store(true, Ordering::SeqCst);
    let before = run_count(h)?;
    let wait = entered.notified();
    tokio::pin!(wait);
    wait.as_mut().enable();
    labeled(h, "labeled", nonce).await?;
    tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .context("the agent never reached the model")?;
    let (run, state) = latest(h)?;
    ensure!(
        run_count(h)? == before + 1 && state == "running",
        "run is {state}"
    );
    Ok(run)
}
fn unblock(h: &Fixture) -> Result<()> {
    h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .model_blocked
        .store(false, Ordering::SeqCst);
    Ok(())
}
fn aborted(h: &Fixture) -> Result<usize> {
    Ok(h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .model_aborted
        .load(Ordering::SeqCst))
}
fn git_writes(h: &Fixture) -> Result<usize> {
    Ok(h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .requests
        .iter()
        .filter(|(m, p, _)| m != "GET" && (p.contains("/git/") || p.ends_with("/pulls")))
        .count())
}
fn models(h: &Fixture) -> Result<usize> {
    Ok(h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .models
        .len())
}
fn pulls_of(h: &Fixture, run: &str) -> Result<usize> {
    let s = h
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?;
    Ok(s.pulls
        .iter()
        .filter(|p| {
            p["head"]["ref"]
                .as_str()
                .is_some_and(|r| r.ends_with(&format!("-{run}")))
        })
        .count())
}
/// A fresh authorized request after a stop: the old run stays ended and a new run completes.
async fn fresh_run_completes(h: &Fixture, old: &str, nonce: &str) -> Result<bool> {
    let (run, state) = completed(h, nonce).await?;
    Ok(run != old && state == "done")
}

/// What the stop of a streaming run looks like: when the held model request was dropped, how long
/// the run's containers took to be absent, and the run's final state.
struct Stopped {
    state: String,
    revoke: Duration,
    absent: Duration,
    gone: bool,
}
async fn observe_stop(
    h: &Fixture,
    run: &str,
    since: Instant,
    base_aborted: usize,
) -> Result<Stopped> {
    let ended = until(Duration::from_secs(30), || Ok(state(h, run)? != "running")).await?;
    ensure!(ended, "run never ended");
    let seen = Instant::now();
    let revoked = until(MODEL_CANCEL, || Ok(aborted(h)? > base_aborted)).await?;
    let revoke = seen.elapsed();
    let gone = until(ABSENCE, || Ok(containers(run)?.is_empty())).await?;
    ensure!(
        revoked,
        "streaming request was not revoked within {MODEL_CANCEL:?}"
    );
    Ok(Stopped {
        state: state(h, run)?,
        revoke,
        absent: since.elapsed(),
        gone,
    })
}

fn finish(
    name: &str,
    checks: Result<Checks>,
    code: ObservationCode,
    ms: Option<Duration>,
    evidence: &Path,
) -> Scenario {
    let mut s = scenario(name, checks, evidence);
    s.observations[0].code = code;
    s.observations[0].duration_ms = ms.and_then(|d| u64::try_from(d.as_millis()).ok());
    s
}

async fn cancel(h: &Fixture) -> Result<(Checks, Duration)> {
    let base = aborted(h)?;
    let run = streaming_run(h, "cancel").await?;
    let started = Instant::now();
    cancel_comment(h, "cancel").await?;
    let s = observe_stop(h, &run, started, base).await?;
    let pr = pulls_of(h, &run)?;
    unblock(h)?;
    let fresh = fresh_run_completes(h, &run, "cancel-fresh").await?;
    Ok((
        vec![
            ("run-canceled", s.state == "canceled"),
            (
                "streaming-request-revoked-within-model-cancel",
                s.revoke <= MODEL_CANCEL,
            ),
            ("containers-absent-within-deadline", s.gone),
            ("no-pr-for-canceled-run", pr == 0),
            (
                "canceled-run-not-revived-new-run-needs-new-request",
                fresh && state(h, &run)? == "canceled",
            ),
        ],
        s.revoke,
    ))
}
async fn label_removal(h: &Fixture) -> Result<(Checks, Duration)> {
    let base = aborted(h)?;
    let run = streaming_run(h, "label").await?;
    let started = Instant::now();
    labeled(h, "unlabeled", "label").await?;
    let s = observe_stop(h, &run, started, base).await?;
    let revokes: i64 = db(h)?.query_row(
        "SELECT count(*) FROM followups WHERE kind='revoke' AND target=?1",
        [&run],
        |r| r.get(0),
    )?;
    let pr = pulls_of(h, &run)?;
    let fresh = fresh_run_completes(h, &run, "label-fresh").await?;
    Ok((
        vec![
            ("run-canceled", s.state == "canceled"),
            (
                "streaming-request-revoked-within-model-cancel",
                s.revoke <= MODEL_CANCEL,
            ),
            ("containers-absent-within-deadline", s.gone),
            ("revocation-queued", revokes > 0),
            ("no-pr-for-canceled-run", pr == 0),
            (
                "canceled-run-not-revived",
                fresh && state(h, &run)? == "canceled",
            ),
        ],
        s.revoke,
    ))
}
async fn kill_confirms(h: &Fixture) -> Result<(Checks, Duration)> {
    let base = aborted(h)?;
    let run = streaming_run(h, "kill").await?;
    let started = Instant::now();
    command(h, "kill").await?;
    let s = observe_stop(h, &run, started, base).await?;
    let before = run_count(h)?;
    labeled(h, "labeled", "kill-refused").await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let refused = run_count(h)? == before;
    command(h, "resume").await?;
    let fresh = fresh_run_completes(h, &run, "kill-fresh").await?;
    Ok((
        vec![
            ("run-canceled-by-kill", s.state == "canceled"),
            (
                "streaming-request-revoked-within-model-cancel",
                s.revoke <= MODEL_CANCEL,
            ),
            (
                "docker-confirms-absence-within-deadline",
                s.gone && s.absent <= ABSENCE + MODEL_CANCEL,
            ),
            ("halted-host-admits-nothing", refused),
            (
                "resume-needs-fresh-signed-request-then-new-run",
                fresh && state(h, &run)? == "canceled",
            ),
        ],
        s.absent,
    ))
}
async fn kill_in_outages(h: &Fixture) -> Result<(Checks, Duration)> {
    let base = aborted(h)?;
    let run = streaming_run(h, "outage").await?;
    // Tracker/forge and model upstream fail; the audit store's writer lock is held elsewhere.
    h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .fail = true;
    let audit = Connection::open(&h.config.paths.audit_db)?;
    audit.execute_batch("BEGIN IMMEDIATE")?;
    let started = Instant::now();
    let result = command(h, "kill").await;
    let stopped = observe_stop(h, &run, started, base).await;
    audit.execute_batch("ROLLBACK")?;
    h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .fail = false;
    result?;
    let s = stopped?;
    let was_halted = halted(h)?;
    command(h, "resume").await?;
    let fresh = fresh_run_completes(h, &run, "outage-fresh").await?;
    Ok((
        vec![
            ("host-halted-during-outages", was_halted),
            ("run-canceled-by-kill", s.state == "canceled"),
            (
                "streaming-request-revoked-within-model-cancel",
                s.revoke <= MODEL_CANCEL,
            ),
            ("containers-absent-within-deadline", s.gone),
            (
                "recovered-host-runs-only-fresh-requests",
                fresh && state(h, &run)? == "canceled",
            ),
        ],
        s.absent,
    ))
}

/// Stops the host, lets `edit` rewrite what a crash would have left on disk, and starts it again.
async fn restart(h: &mut Fixture, edit: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
    h.shutdown().await?;
    edit(&db(h)?)?;
    h.start().await
}
/// Leaves a run in the state a hard crash would have: the store's forward-only trigger forbids
/// a clean host from ever writing this, so it is lifted for the edit and restored.
fn set_state(c: &Connection, run: &str, state: &str) -> Result<()> {
    let seq = kod_dispatcher::store::seq_of(run).context("run id")?;
    let trigger: String = c.query_row(
        "SELECT sql FROM sqlite_master WHERE name='forward_only'",
        [],
        |r| r.get(0),
    )?;
    c.execute_batch("DROP TRIGGER forward_only")?;
    let moved = c.execute(
        "UPDATE runs SET state=?2 WHERE seq=?1",
        rusqlite::params![seq, state],
    );
    c.execute_batch(&trigger)?;
    moved?;
    Ok(())
}
/// Sends a fresh delivery and waits for the run it creates to end.
async fn completed(h: &Fixture, nonce: &str) -> Result<(String, String)> {
    unblock(h)?;
    let before = latest(h)?.0;
    labeled(h, "labeled", nonce).await?;
    let end = Instant::now() + Duration::from_secs(h.config.sandbox.wall_seconds + 30);
    loop {
        let (run, state) = latest(h)?;
        if run != before && !matches!(state.as_str(), "queued" | "running" | "publishing") {
            return Ok((run, state));
        }
        ensure!(
            Instant::now() < end,
            "fresh run never ended: latest {run} {state}, halted {:?}, runs {}, deliveries {:?}, followups {:?}",
            halted(h),
            run_count(h)?,
            deliveries(h),
            followups(h)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn completed_run(h: &Fixture, nonce: &str) -> Result<String> {
    let (run, state) = completed(h, nonce).await?;
    ensure!(state == "done", "setup run ended {state}");
    Ok(run)
}
fn forget(h: &Fixture, run: &str, branch: bool, pr: bool) -> Result<()> {
    let mut s = h
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?;
    let tail = format!("-{run}");
    if branch {
        s.refs.retain(|k, _| !k.ends_with(&tail));
    }
    if pr {
        s.pulls.retain(|p| {
            !p["head"]["ref"]
                .as_str()
                .is_some_and(|r| r.ends_with(&tail))
        });
    }
    Ok(())
}
/// Runs one run to completion, then restarts the host with it left `publishing` after `edit`
/// changed what the forge holds. Returns the run's state after recovery.
async fn crash_publishing(
    h: &mut Fixture,
    nonce: &str,
    edit: impl FnOnce(&Fixture, &str) -> Result<()>,
    lookup_fails: bool,
) -> Result<(String, String)> {
    let run = completed_run(h, nonce).await?;
    h.shutdown().await?;
    set_state(&db(h)?, &run, "publishing")?;
    edit(h, &run)?;
    fail_pulls(h, lookup_fails)?;
    h.start().await?;
    let end = Instant::now() + Duration::from_secs(30);
    while state(h, &run)? == "publishing" {
        ensure!(Instant::now() < end, "publishing run never reconciled");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    fail_pulls(h, false)?;
    let s = state(h, &run)?;
    Ok((run, s))
}
fn fail_pulls(h: &Fixture, on: bool) -> Result<()> {
    h.provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .fail_pulls = on;
    Ok(())
}
async fn restart_reconciles(h: &mut Fixture) -> Result<Checks> {
    let writes_start = git_writes(h)?;
    // Each case is an independent run, left publishing as the forge would have it after a crash.
    let (r1, crash_before_push) =
        crash_publishing(h, "r1", |h, r| forget(h, r, true, true), false).await?;
    let (r2, lost_push_response) =
        crash_publishing(h, "r2", |h, r| forget(h, r, false, true), false).await?;
    let (r3, lost_pr_response) = crash_publishing(h, "r3", |_, _| Ok(()), false).await?;
    let (r4, absent_pr) =
        crash_publishing(h, "r4", |h, r| forget(h, r, false, true), false).await?;
    let before_lookup = std::cell::Cell::new(0);
    let (r6, failed_lookup) = crash_publishing(
        h,
        "r6",
        |h, _| {
            before_lookup.set(git_writes(h)?);
            Ok(())
        },
        true,
    )
    .await?;
    let lookup_quiet = git_writes(h)? == before_lookup.get();
    // Interrupted execution: the host died while the agent waited on the model.
    let streaming = streaming_run(h, "r5").await?;
    h.shutdown().await?;
    unblock(h)?;
    set_state(&db(h)?, &streaming, "running")?;
    let (model_before, writes_before) = (models(h)?, git_writes(h)?);
    h.start().await?;
    let end = Instant::now() + Duration::from_secs(30);
    while state(h, &streaming)? == "running" {
        ensure!(Instant::now() < end, "interrupted run never retired");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let interrupted = state(h, &streaming)?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (models_after, writes_after) = (models(h)?, git_writes(h)?);
    let idle = containers(&streaming)?.is_empty();
    let all = [&r1, &r2, &r3, &r4, &r6, &streaming];
    let first: Vec<String> = all.iter().map(|r| state(h, r)).collect::<Result<_>>()?;
    // A repeated restart changes nothing: no new PR, no resumed execution.
    restart(h, |_| Ok(())).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = all
        .iter()
        .map(|r| state(h, r))
        .collect::<Result<Vec<_>>>()?
        == first
        && git_writes(h)? == writes_after
        && models(h)? == model_before;
    // Only a signed request starts work again; a forged one is refused and the run stays ended.
    let forged = h
        .command_signed(
            serde_json::to_vec(&json!({"command":"resume","by":"kiennt23","at":now()}))?,
            "forged",
        )
        .await?;
    let fresh = fresh_run_completes(h, &streaming, "r7").await?;
    Ok(vec![
        (
            "crash-before-push-needs-human",
            crash_before_push == "needs_human",
        ),
        (
            "lost-push-response-needs-human",
            lost_push_response == "needs_human",
        ),
        (
            "lost-pr-response-done-only-with-real-pr",
            lost_pr_response == "done" && pulls_of(h, &r3)? == 1,
        ),
        (
            "absent-pr-needs-human",
            absent_pr == "needs_human" && pulls_of(h, &r4)? == 0,
        ),
        (
            "failed-pr-lookup-needs-human",
            failed_lookup == "needs_human" && lookup_quiet,
        ),
        (
            "restart-never-publishes",
            writes_after == writes_before && writes_before >= writes_start,
        ),
        (
            "interrupted-execution-never-resumes",
            interrupted == "needs_human" && models_after == model_before && idle,
        ),
        ("repeated-restart-changes-nothing", repeated),
        (
            "fresh-signature-required",
            !forged.is_success() && fresh && state(h, &streaming)? == "needs_human",
        ),
    ])
}

pub async fn run(h: &mut Fixture, evidence: &Path) -> Result<Vec<Scenario>> {
    let mut out = vec![];
    let (checks, took) = match cancel(h).await {
        Ok((c, d)) => (Ok(c), Some(d)),
        Err(e) => (Err(e), None),
    };
    out.push(finish(
        "cancel-revokes-queued-and-streaming-requests",
        checks,
        ObservationCode::Revoked,
        took,
        evidence,
    ));
    unblock(h)?;
    let (checks, took) = match label_removal(h).await {
        Ok((c, d)) => (Ok(c), Some(d)),
        Err(e) => (Err(e), None),
    };
    out.push(finish(
        "label-removal-revokes-run",
        checks,
        ObservationCode::Revoked,
        took,
        evidence,
    ));
    unblock(h)?;
    let (checks, took) = match kill_confirms(h).await {
        Ok((c, d)) => (Ok(c), Some(d)),
        Err(e) => (Err(e), None),
    };
    out.push(finish(
        "kill-confirms-container-absence",
        checks,
        ObservationCode::CleanupConfirmed,
        took,
        evidence,
    ));
    unblock(h)?;
    let (checks, took) = match kill_in_outages(h).await {
        Ok((c, d)) => (Ok(c), Some(d)),
        Err(e) => (Err(e), None),
    };
    out.push(finish(
        "kill-during-dependency-outages",
        checks,
        ObservationCode::Stopped,
        took,
        evidence,
    ));
    unblock(h)?;
    let started = Instant::now();
    let checks = restart_reconciles(h).await;
    out.push(finish(
        "restart-never-resumes-and-reconciles-publish",
        checks,
        ObservationCode::Reconciled,
        Some(started.elapsed()),
        evidence,
    ));
    Ok(out)
}
