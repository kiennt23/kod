//! Deterministic upstream simulation around the production connected composition.
pub mod fixture;
mod lifecycle;
pub mod proc;
pub mod provider;
mod scenarios;

/// The scenario being executed when an error occurred; it is the one reported as failed.
#[derive(Debug)]
pub struct Stage(pub &'static str);
impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "scenario {}", self.0)
    }
}
use super::{
    DOCKER_SCENARIOS, Execution, Observation, ObservationCode, Outcome, Scenario, SuiteBudget,
};
use crate::config::FsConfig;
use anyhow::{Context, Result, ensure};
use fixture::Fixture;
use std::{path::Path, sync::Arc};

/// A preflight failure reduced to a fixed code; the underlying error (which may echo supplied
/// values such as a malformed policy) is never carried into stderr or the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocked(pub &'static str);
impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Blocked {}

pub fn preflight(config: &FsConfig) -> std::result::Result<(), Blocked> {
    let check = |ok: bool, code| if ok { Ok(()) } else { Err(Blocked(code)) };
    check(
        config.github.product.repo == "acme/app" && config.github.tracker.repo == "acme/app",
        "fixture_repository",
    )?;
    check(
        config.model.upstream == "https://connected-fixture.invalid",
        "fixture_upstream",
    )?;
    for (path, expected) in [
        (
            &config.model.api_key_file,
            b"connected-fixture-key".as_slice(),
        ),
        (
            &config.github.webhook_secret_file,
            b"connected-fixture-webhook".as_slice(),
        ),
        (
            &config.github.owner_key_file,
            b"connected-fixture-owner".as_slice(),
        ),
    ] {
        check(
            crate::config::read_private(path).is_ok_and(|v| v == expected),
            "fixture_credential",
        )?;
    }
    // Only the fixed text of our own digest rule is matched; nothing from the error is kept.
    config.validate_local().map_err(|e| {
        Blocked(if format!("{e:#}").contains("exact sha256 digest") {
            "image_digest"
        } else {
            "local_validation"
        })
    })
}
fn observed(name: &str, ok: bool, observation: Observation) -> Scenario {
    Scenario {
        name: name.into(),
        outcome: if ok { Outcome::Pass } else { Outcome::Fail },
        observations: vec![observation],
    }
}
/// Best-effort provenance knowable in Docker mode: the kod commit under test and the sandbox image ids.
pub fn provenance(config: &FsConfig) -> (Option<String>, Vec<String>) {
    let out = |cmd: &str, args: &[&str]| {
        proc::output(
            std::process::Command::new(cmd).args(args),
            None,
            std::time::Duration::from_secs(10),
        )
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
    };
    let commit = out(
        "git",
        &["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"],
    )
    .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut digests: Vec<String> = [&config.sandbox.agent_image, &config.sandbox.collector_image]
        .into_iter()
        .filter_map(|i| out("docker", &["image", "inspect", "--format", "{{.Id}}", i]))
        .collect();
    digests.sort();
    digests.dedup();
    (commit, digests)
}
const SUITE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);
const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// The suite stopped at its deadline; carries only our own text (scenario names).
#[derive(Debug)]
pub struct SuiteTimeout(pub String);
impl std::fmt::Display for SuiteTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SuiteTimeout {}

/// Test-support override so the CLI can be exercised with a short deadline.
fn deadline() -> std::time::Duration {
    std::env::var("KOD_ACCEPTANCE_SUITE_DEADLINE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(SUITE_DEADLINE, std::time::Duration::from_millis)
}

/// Always returns what was accumulated, with the error (if any) beside it: a failed or timed-out
/// suite must not lose completed scenarios, external writes or spend.
pub async fn run(
    config: &FsConfig,
    evidence: &Path,
    suite: Arc<SuiteBudget>,
) -> (Execution, Result<()>) {
    let mut execution = Execution::default();
    let result = suite_run(config, evidence, &suite, deadline(), &mut execution).await;
    // Unfinished scenarios stay explicit blockers; the caller marks the failing one.
    for name in DOCKER_SCENARIOS {
        if !execution.scenarios.iter().any(|s| s.name == *name) {
            execution.scenarios.push(Scenario {
                name: (*name).into(),
                outcome: Outcome::Blocked,
                observations: vec![],
            });
        }
    }
    execution.spend = suite.consumed().unwrap_or_default();
    (execution, result)
}
async fn suite_run(
    config: &FsConfig,
    evidence: &Path,
    suite: &Arc<SuiteBudget>,
    limit: std::time::Duration,
    execution: &mut Execution,
) -> Result<()> {
    preflight(config)?;
    let mut fixture: Option<Fixture> = None;
    // The deadline covers setup too: a hung fixture start must fail fast like a hung scenario.
    let result = {
        let body = async {
            let f = fixture.insert(Fixture::new(config).await?);
            f.suite = Some(suite.clone());
            f.start().await?;
            conformance(f, evidence, execution).await
        };
        match tokio::time::timeout(limit, body).await {
            Ok(r) => r,
            Err(_) => Err(SuiteTimeout(format!(
                "docker suite exceeded {}ms; scenarios completed before the hang: [{}]",
                limit.as_millis(),
                execution
                    .scenarios
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .into()),
        }
    };
    let Some(mut fixture) = fixture else {
        return result;
    };
    let cleanup = match tokio::time::timeout(SHUTDOWN_DEADLINE, fixture.shutdown()).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("fixture shutdown timed out")),
    };
    // Recorded even when a scenario errored, so a budget stop is independently observable.
    let received = fixture
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("provider lock"))?
        .models
        .len();
    fixture::private_write(
        &evidence.join("provider-observations.json"),
        &serde_json::to_vec(&serde_json::json!({"model_requests_received": received}))?,
    )?;
    let state = fixture
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("provider lock"))?;
    execution.external_writes = u64::try_from(
        state
            .requests
            .iter()
            .filter(|(method, path, _)| method != "GET" && path.starts_with("/repos/"))
            .count(),
    )?;
    drop(state);
    result?;
    cleanup
}
// The provider guard is dropped explicitly before the next await; clippy cannot see through drop().
#[allow(clippy::await_holding_lock)]
async fn conformance(h: &mut Fixture, evidence: &Path, execution: &mut Execution) -> Result<()> {
    let stage = |n: &'static str| Stage(n);
    // A valid-shaped payload, so the rejection can only be for its signature.
    let shaped = h.payload("labeled", "kiennt23")?;
    let wrong = h
        .webhook("invalid-signature", &shaped, false)
        .await
        .context(stage("connected-ingress-safety"))?;
    let status = h.delivery("conformance", "labeled", "kiennt23").await?;
    ensure!(status.is_success(), "delivery rejected: {status}");
    ensure!(
        h.delivery("conformance", "labeled", "kiennt23")
            .await?
            .is_success(),
        "duplicate rejected"
    );
    let (run, state) = h
        .terminal()
        .await
        .context(stage("draft-pr-story-agent-trailers"))?;
    let audit = h.audit()?;
    fixture::private_write(
        &evidence.join("audit-observations.json"),
        &serde_json::to_vec(&audit)?,
    )?;
    let c = kod_dispatcher::store::open(&h.config.paths.dispatcher_db)?;
    let count: i64 = c.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?;
    execution.scenarios.push(observed(
        "connected-ingress-safety",
        !wrong.is_success() && count == 1,
        Observation {
            code: ObservationCode::Denied,
            count: Some(u64::try_from(count)?),
            ..Default::default()
        },
    ));
    let records: Vec<(u64, serde_json::Value)> = audit
        .iter()
        .filter_map(|(seq, p)| Some((*seq, serde_json::from_str(p).ok()?)))
        .collect();
    let decision = |target: &str, allow: bool| {
        records.iter().find(|(_, v)| {
            v["kind"] == "policy_decision"
                && v["input"]["target"]["id"] == target
                && v["decision"]["allow"] == allow
        })
    };
    // The read must have been allowed and then actually succeeded.
    let successful = decision("design-v1", true).and_then(|_| {
        records.iter().find(|(_, v)| {
            v["kind"] == "tool_result" && v["tool"] == "read_doc" && v["result"] == "ok"
        })
    });
    execution.scenarios.push(observed(
        "mcp-tool-call",
        successful.is_some(),
        Observation {
            code: ObservationCode::Audited,
            audit_sequence: successful.map(|x| x.0),
            ..Default::default()
        },
    ));
    let denied = decision("planted-secret", false);
    execution.scenarios.push(observed(
        "planted-secret-denied-and-audited",
        denied.is_some(),
        Observation {
            code: ObservationCode::Denied,
            audit_sequence: denied.map(|x| x.0),
            ..Default::default()
        },
    ));
    let remote = h
        .provider
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("provider lock"))?;
    let pr = remote.pulls.first();
    let head = pr.and_then(|p| p["head"]["sha"].as_str());
    let mut tree = None;
    let mut same = false;
    let mut trailers = false;
    if let Some(head) = head {
        tree = Some(provider::git(
            &remote.repo,
            &["rev-parse", &format!("{head}^{{tree}}")],
            None,
        )?);
        let message = provider::git(&remote.repo, &["log", "-1", "--format=%B", head], None)?;
        trailers = message.contains("Kod-Ticket: github:acme/app#7")
            && message.contains(&format!("Kod-Run: {run}"))
            && message.contains("Co-Authored-By: kod-agent <agent@kod.invalid>");
        let export_dir = h.config.paths.run_root.join(&run).join("export_out");
        let base = crate::production::mirror_base(&remote.repo, "main")?;
        let exported = crate::workspace::read_export(&export_dir, &base)?;
        same = !exported.changes.is_empty()
            && published_tree_matches(&remote.repo, &base, head, &export_dir)?;
        fixture::private_write(
            &evidence.join("published-objects.json"),
            &serde_json::to_vec(
                &serde_json::json!({"pr":pr,"head":head,"tree":tree,"message":message,"changes":exported.changes.iter().map(|v|&v.path).collect::<Vec<_>>()}),
            )?,
        )?;
    }
    execution.scenarios.push(observed(
        "draft-pr-story-agent-trailers",
        state == "done" && same && trailers && pr.is_some_and(|p| p["draft"] == true),
        Observation {
            code: ObservationCode::Published,
            head_sha: head.map(str::to_owned),
            tree_sha: tree,
            ..Default::default()
        },
    ));
    drop(remote);
    // Test support (feature-gated build only): simulate a scenario that never finishes.
    if std::env::var_os("KOD_ACCEPTANCE_TEST_HANG").is_some() {
        std::future::pending::<()>().await;
    }
    execution
        .scenarios
        .extend(scenarios::run(h, evidence).await?);
    execution
        .scenarios
        .extend(lifecycle::run(h, evidence).await?);
    Ok(())
}

/// True only when the published head differs from `base` by EXACTLY the collected export: the
/// same set of paths (so no extra unapproved file), deletions as deletions, and identical file
/// modes and blob contents.
pub fn published_tree_matches(repo: &Path, base: &str, head: &str, export: &Path) -> Result<bool> {
    use std::collections::BTreeMap;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(export.join("manifest.json"))?)?;
    // path -> None (deleted) or Some((mode, git blob sha of the expected bytes))
    let mut expected: BTreeMap<String, Option<(String, String)>> = BTreeMap::new();
    for (i, f) in manifest["files"]
        .as_array()
        .context("files")?
        .iter()
        .enumerate()
    {
        let path = f["path"].as_str().context("path")?.to_owned();
        let entry = if f["deleted"] == true {
            None
        } else {
            let bytes = std::fs::read(export.join(i.to_string()))?;
            let sha = provider::git(repo, &["hash-object", "--stdin"], Some(&bytes))?;
            Some((f["mode"].as_str().context("mode")?.to_owned(), sha))
        };
        expected.insert(path, entry);
    }
    let raw = provider::git(
        repo,
        &[
            "diff-tree",
            "-r",
            "--no-renames",
            "--no-abbrev",
            "-z",
            base,
            head,
        ],
        None,
    )?;
    let mut actual: BTreeMap<String, Option<(String, String)>> = BTreeMap::new();
    let mut fields = raw.split('\0');
    while let Some(meta) = fields.next().filter(|m| !m.is_empty()) {
        let path = fields.next().context("diff path")?.to_owned();
        let m: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        ensure!(m.len() == 5, "unexpected diff-tree record");
        actual.insert(
            path,
            (m[4] != "D").then(|| (m[1].to_owned(), m[3].to_owned())),
        );
    }
    Ok(actual == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// base commit + a head commit on top of it, plus an export dir describing the expected change.
    fn repo_and_export(
        extra: Option<(&str, &str)>,
        mode: &str,
    ) -> (tempfile::TempDir, String, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let g = |a: &[&str], i: Option<&[u8]>| provider::git(dir.path(), a, i).unwrap();
        g(&["init", "-b", "main"], None);
        std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
        std::fs::write(dir.path().join("old.txt"), "old\n").unwrap();
        g(&["add", "."], None);
        g(&["commit", "-m", "base"], None);
        let base = g(&["rev-parse", "HEAD"], None);
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        std::fs::remove_file(dir.path().join("old.txt")).unwrap();
        if let Some((p, t)) = extra {
            std::fs::write(dir.path().join(p), t).unwrap();
        }
        g(&["add", "-A"], None);
        g(&["update-index", "--chmod=+x", "new.txt"], None);
        g(&["commit", "-m", "head"], None);
        let head = g(&["rev-parse", "HEAD"], None);
        let export = tempfile::tempdir().unwrap();
        std::fs::write(export.path().join("0"), "new\n").unwrap();
        std::fs::write(
            export.path().join("manifest.json"),
            serde_json::json!({"files":[
                {"path":"new.txt","mode":mode,"blob":0},
                {"path":"old.txt","deleted":true}]})
            .to_string(),
        )
        .unwrap();
        (dir, base, head, export)
    }
    #[test]
    fn published_tree_must_equal_the_export_exactly() {
        let (r, base, head, e) = repo_and_export(None, "100755");
        assert!(published_tree_matches(r.path(), &base, &head, e.path()).unwrap());
        // An extra unapproved file in the published tree.
        let (r, base, head, e) = repo_and_export(Some(("evil.txt", "x")), "100755");
        assert!(!published_tree_matches(r.path(), &base, &head, e.path()).unwrap());
        // A wrong file mode.
        let (r, base, head, e) = repo_and_export(None, "100644");
        assert!(!published_tree_matches(r.path(), &base, &head, e.path()).unwrap());
        // A deletion the export does not contain (published head deletes old.txt, export keeps it).
        let (r, base, head, e) = repo_and_export(None, "100755");
        std::fs::write(
            e.path().join("manifest.json"),
            serde_json::json!({"files":[{"path":"new.txt","mode":"100755","blob":0}]}).to_string(),
        )
        .unwrap();
        assert!(!published_tree_matches(r.path(), &base, &head, e.path()).unwrap());
    }
}
