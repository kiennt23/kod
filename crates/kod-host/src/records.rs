//! Authoritative gate evidence, status snapshots and snapshot activation.
use crate::{artifacts::Artifacts, binding::Binding, config::FsConfig};
use anyhow::{Context, Result, ensure};
use kod_gates::Evidence;
use kod_status::{
    Stage,
    records::{Artifact as StatusArtifact, Pull, Snapshot},
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
/// Rework stays empty (unknown) because the tracker cannot report its kind or target.
pub fn snapshot(
    tracker: &dyn Tracker,
    artifacts: &Artifacts,
    story: &str,
    stage_of: &dyn Fn(&str) -> Stage,
    pulls: Vec<Pull>,
) -> Result<Snapshot> {
    let issue = tracker.get_issue(story)?;
    ensure!(issue.key == story, "tracker returned another story");
    let tickets = issue
        .links
        .iter()
        .map(|k| tracker.get_issue(k))
        .collect::<Result<Vec<_>>>()?;
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
        rework: vec![],
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

fn lock(path: PathBuf) -> Result<std::fs::File> {
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

/// An open PR whose checks depend on the active approvals.
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
    let stories: BTreeSet<_> = open.iter().map(|o| o.story.as_str()).collect();
    let mut _held = vec![];
    for s in stories {
        ensure!(
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "unsafe story key"
        );
        _held.push(lock(suffix(&format!(".refresh.{s}.lock")))?);
    }
    for o in open {
        invalidate(&o.head)?;
    }
    let (from, to) = (
        candidate.parent().context("no staged directory")?,
        active.parent().context("no active directory")?,
    );
    for a in &manifest.artifacts {
        let dest = to.join(&a.path);
        if dest.exists() {
            ensure!(
                std::fs::read(&dest)? == std::fs::read(from.join(&a.path))?,
                "artifact path already holds other content"
            );
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
