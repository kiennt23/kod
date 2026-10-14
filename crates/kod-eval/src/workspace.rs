//! Host-side git. The host runs git only in directories it created from the
//! trusted mirror, never inside a workspace the agent has written to.
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

/// git that reads no global or system config and runs no hooks, so neither
/// the host's config nor anything in a repository can make it run a program.
fn host_git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(dir);
    c
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = host_git(dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// An absolute patch path: git resolves a relative one against `-C <dir>`.
fn patch_arg(p: &Path) -> Result<String> {
    let abs = p
        .canonicalize()
        .with_context(|| format!("resolving {}", p.display()))?;
    abs.into_os_string()
        .into_string()
        .map_err(|p| anyhow::anyhow!("non-UTF-8 path {}", Path::new(&p).display()))
}

/// Writes the tree at `commit` into `dest`, with no git metadata.
pub fn extract(mirror: &Path, commit: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let mut archive = host_git(mirror)
        .args(["archive", "--format=tar", commit])
        .stdout(Stdio::piped())
        .spawn()
        .context("running git archive")?;
    let untar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(dest)
        .stdin(archive.stdout.take().context("git archive has no stdout")?)
        .status()?;
    if !archive.wait()?.success() || !untar.success() {
        bail!("extracting {commit} from {}", mirror.display());
    }
    Ok(())
}

/// A fresh repository holding only the tree at `commit`, with no history, so
/// later commits (including the real fix) can't leak to the agent. Returns the
/// SHA of its single commit.
pub fn prepare(mirror: &Path, commit: &str, dest: &Path) -> Result<String> {
    extract(mirror, commit, dest)?;
    git(dest, &["init", "-q"])?;
    git(dest, &["add", "-A"])?;
    git(
        dest,
        &[
            "-c",
            "user.name=kod",
            "-c",
            "user.email=kod@localhost",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    )?;
    Ok(git(dest, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// Paths a patch touches, read with `git apply --numstat -z`.
pub fn patch_paths(dir: &Path, patch: &Path) -> Result<Vec<String>> {
    if std::fs::metadata(patch)?.len() == 0 {
        return Ok(vec![]);
    }
    let out = git(dir, &["apply", "--numstat", "-z", &patch_arg(patch)?])?;
    let mut paths = Vec::new();
    let mut fields = out.split('\0');
    while let Some(record) = fields.next() {
        match record.splitn(3, '\t').nth(2) {
            None => {}
            // A rename: the old and new paths follow as their own fields.
            Some("") => paths.extend(
                fields
                    .next()
                    .into_iter()
                    .chain(fields.next())
                    .map(str::to_owned),
            ),
            Some(p) => paths.push(p.to_owned()),
        }
    }
    Ok(paths)
}

pub fn apply(repo: &Path, patch: &Path) -> Result<()> {
    if std::fs::metadata(patch)?.len() == 0 {
        return Ok(());
    }
    git(repo, &["apply", "--binary", &patch_arg(patch)?]).map(|_| ())
}

/// Puts the held-back tests in place. Agent changes to those paths are
/// discarded first, so the agent can't weaken or pre-empt them. Every step goes
/// through git, which refuses to work beyond a symlink.
pub fn apply_heldout(repo: &Path, base: &str, heldout: &Path) -> Result<()> {
    git(repo, &["add", "-A"])?;
    for p in patch_paths(repo, heldout)? {
        if git(repo, &["cat-file", "-e", &format!("{base}:{p}")]).is_ok() {
            git(repo, &["checkout", base, "--", &p])?;
        } else {
            git(repo, &["rm", "-q", "-f", "--ignore-unmatch", "--", &p])?;
        }
    }
    apply(repo, heldout)
}

/// Protected entries are directory prefixes ending in "/" or exact paths.
pub fn protected_hits(paths: &[String], protected: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            protected
                .iter()
                .any(|q| *p == q || (q.ends_with('/') && p.starts_with(q.as_str())))
        })
        .cloned()
        .collect()
}
