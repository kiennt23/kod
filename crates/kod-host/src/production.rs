//! The two production connections the runtime is assembled from: admission
//! (tracker + approved snapshot -> immutable `Binding`) and execution
//! (`Admitted` -> Task 4's `runner::run`).
use crate::{
    artifacts::Artifacts,
    binding::{Binding, token_hash},
    config::FsConfig,
    runner,
    runtime::{Admitted, Execute, ResolveAdmission},
};
use anyhow::{Context, Result, ensure};
use kod_eval::connected::ConnectedSpec;
use kod_tracker::Tracker;
use std::sync::Arc;

/// Trusted inputs admission reads afresh at every start.
pub struct Sources {
    pub tracker: Arc<dyn Tracker>,
    pub artifacts: Arc<Artifacts>,
    /// The product repository's base branch head, from the host mirror.
    pub base: Arc<dyn Fn() -> Result<String> + Send + Sync>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// 256 random bits as hex. Never derived from run identity.
fn token() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Head commit of `branch` in the host mirror; any git failure or non-SHA output is an error.
pub fn mirror_base(mirror: &std::path::Path, branch: &str) -> Result<String> {
    ensure!(
        !branch.is_empty() && !branch.starts_with('-') && !branch.chars().any(char::is_control),
        "invalid base branch"
    );
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(mirror)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("refs/heads/{branch}^{{commit}}"))
        .env_remove("GIT_DIR")
        .output()
        .context("git unavailable")?;
    ensure!(out.status.success(), "mirror base unavailable");
    let sha = String::from_utf8(out.stdout)?.trim().to_owned();
    kod_gates::validate_sha(&sha)?;
    Ok(sha)
}

pub fn resolve(config: Arc<FsConfig>, s: Sources) -> ResolveAdmission {
    Arc::new(move |start| {
        ensure!(
            start.repo == config.github.product.repo,
            "run repository is not the configured product repository"
        );
        let issue = s.tracker.get_issue(&start.ticket)?;
        ensure!(
            issue.key == start.ticket && kod_dispatcher::revision(&issue) == start.revision,
            "ticket changed since the run was queued"
        );
        let (pins, data_classes) = s.artifacts.pins(&issue.approved_versions)?;
        let base = (s.base)()?;
        kod_gates::validate_sha(&base)?;
        let dir = config.paths.run_root.join(&start.id);
        let (model_token, mcp_token) = (token()?, token()?);
        let binding = Binding {
            version: 1,
            run: start.id.clone(),
            ticket: start.ticket.clone(),
            revision: start.revision.clone(),
            repo: start.repo.clone(),
            base,
            started_at: (s.now)(),
            data_classes,
            pins,
            linked_tickets: issue.links.clone(), // revision-pinned with the issue
            max_tokens: config.model.run_max_tokens,
            max_cost_micro_usd: config.model.run_max_cost_micro_usd,
            agent_image: config.sandbox.agent_image.clone(),
            collector_image: config.sandbox.collector_image.clone(),
            agent_out: dir.join("agent_out"),
            collect_out: dir.join("collect_out"),
            model_token_hash: token_hash(&model_token),
            mcp_token_hash: token_hash(&mcp_token),
        };
        // Re-check the admitted issue after every other read: nothing above may have gone stale.
        let again = s.tracker.get_issue(&start.ticket)?;
        ensure!(
            kod_dispatcher::revision(&again) == start.revision,
            "ticket changed during admission"
        );
        let prompt = format!(
            "{}\n\n{}\n\nAcceptance criteria:\n{}",
            issue.title,
            issue.body,
            issue.acceptance_criteria.join("\n")
        );
        Ok(Admitted {
            binding,
            prompt,
            model_token,
            mcp_token,
        })
    })
}

pub fn spec(config: &FsConfig, a: &Admitted) -> ConnectedSpec {
    let b = &a.binding;
    let url = config.networking.sandbox_gateway_url.trim_end_matches('/');
    ConnectedSpec {
        run: b.run.clone(),
        base: b.base.clone(),
        mirror: config.paths.mirror.clone(),
        dir: config.paths.run_root.join(&b.run),
        agent_image: b.agent_image.clone(),
        collector_image: b.collector_image.clone(),
        network: config.networking.sandbox_network.clone(),
        model: config.model.selected_model.clone(),
        max_tokens: b.max_tokens,
        budget: kod_eval::case::Budget {
            max_turns: config.sandbox.max_turns,
            timeout_secs: config.sandbox.wall_seconds,
            memory: config.sandbox.memory.clone(),
            disk: config.sandbox.disk.clone(),
            cpus: config.sandbox.cpus,
        },
        prompt: a.prompt.clone(),
        model_url: format!("{url}/r/{}", a.model_token),
        mcp_url: format!("{url}/r/{}/mcp", a.mcp_token),
    }
}

/// Task 4's runner behind the runtime's `Execute`. Only `runner::run` can insert a collection.
pub fn execute(config: Arc<FsConfig>) -> Execute {
    Arc::new(move |a, gate, stopped, workspace| {
        let run = a.binding.run.clone();
        let stop = || stopped(&run);
        runner::run(&spec(&config, a), gate, &stop, workspace)
            .with_context(|| format!("connected run {run}"))
    })
}
