//! Authoritative gate evidence, status snapshots and snapshot activation.
use crate::{artifacts::Artifacts, binding::Binding, config::FsConfig};
use anyhow::{Context, Result, ensure};
use kod_gates::Evidence;
use kod_status::{
    Stage,
    records::{Artifact as StatusArtifact, Pull, ReworkRecord, Snapshot},
};
use kod_tracker::Tracker;
use std::{
    collections::BTreeSet,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The provider-returned identity of a pull request. Never parsed from its description.
pub struct PullIdentity {
    pub repo: String,
    pub pr: u64,
    pub head: String,
}

/// Evidence from the immutable pre-publish binding plus the returned PR identity.
/// A missing or mismatched binding is an error, which leaves the gates unverified.
pub fn evidence(
    db: &Path,
    run: &str,
    story: &str,
    team: &str,
    policy_version: &str,
    runner_config_version: &str,
    pr: &PullIdentity,
) -> Result<Evidence> {
    let b = Binding::load(db, run).context("no immutable binding for this run")?;
    ensure!(
        b.repo == pr.repo,
        "pull request belongs to another repository"
    );
    kod_gates::validate_sha(&pr.head)?;
    ensure!(
        pr.pr > 0 && !story.is_empty() && !team.is_empty() && !b.pins.is_empty(),
        "incomplete evidence"
    );
    Ok(Evidence {
        repo: b.repo,
        pr: pr.pr,
        head: pr.head.clone(),
        run: b.run,
        story: story.into(),
        team: team.into(),
        pins: b.pins.values().cloned().collect(),
        policy_version: policy_version.into(),
        runner_config_version: runner_config_version.into(),
    })
}

/// Fresh tracker reads; any required source error fails the whole snapshot.
/// Audience is the tracker team: a story whose reporter is outside it is refused, and an
/// out-of-audience linked ticket is left out (unknown to the status view).
/// `rework` comes from the dispatcher's records: kod-status renders an empty list as "none"
/// (kod-status/src/lib.rs:160-161), so this function never invents one.
/// Artifact `draft` and `in_review` are unknown here, not "none"; the snapshot carries the
/// approved revision only.
pub fn snapshot(
    tracker: &dyn Tracker,
    artifacts: &Artifacts,
    story: &str,
    stage_of: &dyn Fn(&str) -> Stage,
    pulls: Vec<Pull>,
    rework: Vec<ReworkRecord>,
) -> Result<Snapshot> {
    let issue = tracker.get_issue(story)?;
    ensure!(issue.key == story, "tracker returned another story");
    ensure!(
        tracker.is_team_member(&issue.reporter)?,
        "story reporter is outside the tracker audience"
    );
    let mut tickets = vec![];
    for key in issue
        .links
        .iter()
        .filter(|k| !rework.iter().any(|r| &r.key == *k))
    {
        let t = tracker.get_issue(key)?;
        if tracker.is_team_member(&t.reporter)? {
            tickets.push(t);
        }
    }
    let (pins, _) = artifacts.pins(&issue.approved_versions)?;
    let artifacts = pins
        .into_iter()
        .map(|(link, revision)| StatusArtifact {
            stage: stage_of(&link),
            link,
            approved: Some(revision),
            draft: None,
            in_review: false,
        })
        .collect();
    Ok(Snapshot {
        story: issue,
        tickets,
        artifacts,
        pulls,
        rework,
    })
}

/// Write-then-rename, owner-only.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn lock(path: PathBuf) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    f.lock()?;
    Ok(f)
}

/// The file-name form of a story key: `[A-Za-z0-9_-]` stay, every other byte becomes `%XX`, so
/// `acme/app#1` can never escape the directory and activation and refresh agree on the name.
pub(crate) fn story_lock_name(story: &str) -> Result<String> {
    ensure!(!story.is_empty(), "unsafe story key");
    Ok(story
        .bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect())
}

/// An open PR whose checks depend on the active approvals.
#[derive(Clone)]
pub struct Open {
    pub story: String,
    pub head: String,
}

/// Validate the candidate, take the gates lock and per-story refresh locks, invalidate every
/// open head, then publish. Any failure before the final rename leaves the active manifest as is.
pub fn activate(
    config: &FsConfig,
    candidate: &Path,
    open: &[Open],
    invalidate: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    activate_from(config, candidate, &|| Ok(open.to_vec()), invalidate)
}

/// As `activate`, but the open heads are read live under the gates lock, so no check can be
/// reported for a head the read missed. A source error refuses before anything changes.
pub fn activate_from(
    config: &FsConfig,
    candidate: &Path,
    source: &dyn Fn() -> Result<Vec<Open>>,
    invalidate: &dyn Fn(&str) -> Result<()>,
) -> Result<()> {
    let active = &config.paths.approval_manifest;
    let artifacts = Artifacts::new(active.clone(), &config.paths.policy_dir)?;
    // The same instance remembers every active revision, so a replaced revision is refused.
    let manifest = artifacts.validate(candidate)?;
    let suffix = |s: &str| {
        let mut p = config.paths.audit_db.as_os_str().to_owned();
        p.push(s);
        PathBuf::from(p)
    };
    let _gates = lock(suffix(".gates.lock"))?;
    let open = source().context("open pull source failed")?;
    let stories: BTreeSet<_> = open.iter().map(|o| o.story.as_str()).collect();
    let mut _held = vec![];
    for s in stories {
        _held.push(lock(suffix(&format!(
            ".refresh.{}.lock",
            story_lock_name(s)?
        )))?);
    }
    let (from, to) = (
        candidate.parent().context("no staged directory")?,
        active.parent().context("no active directory")?,
    );
    // Refuse conflicting destinations before any check is invalidated or any file copied.
    for a in &manifest.artifacts {
        let dest = to.join(&a.path);
        ensure!(
            !dest.exists() || std::fs::read(&dest)? == std::fs::read(from.join(&a.path))?,
            "artifact path already holds other content"
        );
    }
    for o in &open {
        invalidate(&o.head)?;
    }
    for a in &manifest.artifacts {
        let dest = to.join(&a.path);
        if dest.exists() {
            continue;
        }
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = std::fs::read(from.join(&a.path))?;
        write_atomic(&dest, &bytes)?;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
    }
    write_atomic(active, &std::fs::read(candidate)?)?;
    Ok(())
}

/// `agent/{ticket}-run-{n}` is the host's run branch; the ticket is the story key.
pub fn story_of_branch(branch: &str) -> Result<String> {
    let ticket = branch
        .strip_prefix("agent/")
        .and_then(|b| b.rsplit_once("-run-"))
        .map(|(t, _)| t)
        .filter(|t| !t.is_empty())
        .with_context(|| format!("open pull branch {branch} is not a run branch"))?;
    Ok(ticket.to_owned())
}

/// The CLI entry. Open heads come from a live read of the product repository; checks are
/// invalidated through the gates report path (token from `KOD_GITHUB_TOKEN`, as `kod-gates run`)
/// before the manifest is replaced. Any source or invalidation error refuses.
pub fn activate_cli(config: &FsConfig, candidate: &Path) -> Result<()> {
    Artifacts::new(
        config.paths.approval_manifest.clone(),
        &config.paths.policy_dir,
    )?
    .validate(candidate)?;
    let gates: kod_gates::Config = serde_json::from_slice(
        &std::fs::read(&config.paths.gates_config).context("reading gates configuration")?,
    )
    .context("invalid gates configuration")?;
    gates.validate()?;
    let token = std::env::var("KOD_GITHUB_TOKEN")
        .context("KOD_GITHUB_TOKEN must be a dedicated GitHub App installation token")?;
    let forge = kod_gates::github::GitHub::new(&gates.repo, &token)?;
    let audit = kod_audit::AuditLog::open(&config.paths.audit_db)?;
    let dir = config.paths.credential_dir.join("activate");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let product = kod_tool_gateway::github::GitHub::new(
        config.github.product.clone(),
        config.github.app.clone(),
        &dir,
    )?;
    activate_from(config, candidate, &|| open_from(&product), &|head| {
        kod_gates::invalidate(&gates, &audit, &forge, head)
    })
    .context("snapshot activation refused; nothing was published")
}

fn open_from(product: &kod_tool_gateway::github::GitHub) -> Result<Vec<Open>> {
    product
        .open_pulls()?
        .into_iter()
        .map(|p| {
            Ok(Open {
                story: story_of_branch(&p.branch)?,
                head: p.head,
            })
        })
        .collect()
}
