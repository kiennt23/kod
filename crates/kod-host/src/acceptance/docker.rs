//! Deterministic upstream simulation around the production connected composition.
pub mod fixture;
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
use super::{DOCKER_SCENARIOS, Execution, Observation, ObservationCode, Outcome, Scenario, Spend};
use crate::config::FsConfig;
use anyhow::{Context, Result, ensure};
use fixture::Fixture;
use std::path::Path;

pub fn preflight(config: &FsConfig) -> Result<()> {
    ensure!(
        config.github.product.repo == "acme/app" && config.github.tracker.repo == "acme/app",
        "fixture repository required"
    );
    ensure!(
        config.model.upstream == "https://connected-fixture.invalid",
        "fixture upstream required"
    );
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
        ensure!(
            crate::config::read_private(path)? == expected,
            "fixture credential required"
        );
    }
    config.validate_local()?;
    Ok(())
}
fn observed(name: &str, ok: bool, observation: Observation) -> Scenario {
    Scenario {
        name: name.into(),
        outcome: if ok { Outcome::Pass } else { Outcome::Fail },
        observations: vec![observation],
    }
}
pub async fn run(config: &FsConfig, evidence: &Path) -> Result<Execution> {
    preflight(config)?;
    let mut fixture = Fixture::new(config).await?;
    fixture.start().await?;
    let result = conformance(&mut fixture, evidence).await;
    let cleanup = fixture.shutdown().await;
    let mut execution = result?;
    cleanup?;
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
    execution.spend = Spend {
        requests: u64::try_from(state.models.len())?,
        tokens: u64::try_from(state.models.len())?
            .checked_mul(15)
            .context("usage overflow")?,
        cost_micro_usd: 0,
    };
    for name in DOCKER_SCENARIOS {
        if !execution.scenarios.iter().any(|s| s.name == *name) {
            execution.scenarios.push(Scenario {
                name: (*name).into(),
                outcome: Outcome::Blocked,
                observations: vec![],
            });
        }
    }
    Ok(execution)
}
// The provider guard is dropped explicitly before the next await; clippy cannot see through drop().
#[allow(clippy::await_holding_lock)]
async fn conformance(h: &mut Fixture, evidence: &Path) -> Result<Execution> {
    let mut execution = Execution::default();
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
        let exported = crate::workspace::read_export(
            &h.config.paths.run_root.join(&run).join("export_out"),
            &crate::production::mirror_base(&remote.repo, "main")?,
        )?;
        same = !exported.changes.is_empty()
            && exported.changes.iter().all(|change| match &change.content {
                Some(text) => provider::git(
                    &remote.repo,
                    &["show", &format!("{head}:{}", change.path)],
                    None,
                )
                .is_ok_and(|actual| actual == text.trim_end_matches('\n')),
                None => false,
            });
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
    execution
        .scenarios
        .extend(scenarios::run(h, evidence).await?);
    Ok(execution)
}
