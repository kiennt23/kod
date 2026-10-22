use crate::{Scanner, validate_sha};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanConfig {
    pub image: String,
    pub timeout_secs: u64,
    pub memory_mb: u64,
    pub commands: BTreeMap<String, Vec<String>>,
}

impl ScanConfig {
    pub fn version(&self) -> Result<String> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(self)?)
        ))
    }
    pub fn rust(image: &str) -> Self {
        Self {
            image: image.into(),
            timeout_secs: 1800,
            memory_mb: 8192,
            commands: [
                (
                    "tests".into(),
                    vec![
                        "/usr/local/cargo/bin/cargo",
                        "test",
                        "--all",
                        "--locked",
                        "--offline",
                    ],
                ),
                (
                    "sast".into(),
                    vec![
                        "/usr/local/cargo/bin/cargo",
                        "clippy",
                        "--all-targets",
                        "--locked",
                        "--offline",
                        "--",
                        "-D",
                        "warnings",
                    ],
                ),
                (
                    "dependency-scan".into(),
                    vec![
                        "/usr/local/cargo/bin/cargo",
                        "deny",
                        "--offline",
                        "--locked",
                        "--config",
                        "/opt/kod/deny.toml",
                        "check",
                    ],
                ),
                (
                    "secret-scan".into(),
                    vec![
                        "/usr/local/bin/gitleaks",
                        "dir",
                        "/work/src",
                        "--config",
                        "/opt/kod/gitleaks.toml",
                        "--redact",
                        "--ignore-gitleaks-allow",
                        "--no-banner",
                    ],
                ),
            ]
            .into_iter()
            .map(|(key, args)| (key, args.into_iter().map(String::from).collect()))
            .collect(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let digest = self
            .image
            .rsplit_once("@sha256:")
            .map(|(_, h)| h)
            .or_else(|| self.image.strip_prefix("sha256:"));
        if !digest.is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }) {
            bail!("scanner image must be pinned by sha256 digest");
        }
        if self.timeout_secs == 0
            || self.timeout_secs > 3600
            || !(64..=32768).contains(&self.memory_mb)
        {
            bail!("scan timeout or memory limit out of range");
        }
        if self.commands.len() != 4
            || crate::CHECKS[3..].iter().any(|name| {
                self.commands.get(*name).is_none_or(|args| {
                    args.is_empty()
                        || !args[0].starts_with('/')
                        || args.iter().any(|a| a.contains('\0'))
                })
            })
        {
            bail!("four scans with absolute tool paths are required");
        }
        Ok(())
    }
}

pub struct DockerScanner {
    pub config: ScanConfig,
    pub archive: PathBuf,
    pub sha: String,
    pub docker: PathBuf,
}

impl DockerScanner {
    async fn command(&self, args: &[String], timeout: Duration) -> Result<bool> {
        let mut child = Command::new(&self.docker)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting trusted Docker CLI")?;
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => Ok(status?.success()),
            Err(_) => {
                child.kill().await?;
                bail!("scan timed out");
            }
        }
    }

    fn args(&self, name: &str, container: &str) -> Result<Vec<String>> {
        let path = self.archive.canonicalize()?;
        let path = path.to_str().context("non-UTF-8 archive path")?;
        if path.contains([',', '\n', '\r']) {
            bail!("invalid archive mount path");
        }
        let mut args: Vec<String> = [
            "run",
            "--name",
            container,
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user=10001:10001",
            "--pids-limit=256",
            "--cpus=2",
            "--log-driver=none",
            "--tmpfs=/tmp:rw,nosuid,nodev,size=128m,uid=10001,gid=10001",
            "--workdir=/work",
            "--entrypoint=/opt/kod/scan",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        args.push(format!("--memory={}m", self.config.memory_mb));
        args.push(format!("--memory-swap={}m", self.config.memory_mb));
        args.push(format!(
            "--tmpfs=/work:rw,nosuid,nodev,exec,size={}m,uid=10001,gid=10001",
            self.config.memory_mb
        ));
        args.extend([
            "--mount".into(),
            format!("type=bind,source={path},target=/input/source.tar.gz,readonly"),
            self.config.image.clone(),
        ]);
        args.extend(
            self.config
                .commands
                .get(name)
                .context("unknown scan")?
                .iter()
                .cloned(),
        );
        Ok(args)
    }

    async fn run(&self, name: &str) -> Result<()> {
        self.config.validate()?;
        let output = Command::new(&self.docker)
            .args(["info", "--format", "{{json .Labels}}"])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(Duration::from_secs(10), output)
            .await
            .context("Docker inventory timeout")??;
        let labels: Vec<String> =
            serde_json::from_slice(&output.stdout).context("Docker daemon labels unavailable")?;
        if !output.status.success()
            || !labels.iter().any(|s| s == "kod.role=gate")
            || labels
                .iter()
                .any(|s| s.starts_with("kod.role=") && s != "kod.role=gate")
        {
            bail!("Docker daemon must have the exclusive kod.role=gate label");
        }
        self.run_container(name).await
    }

    async fn run_container(&self, name: &str) -> Result<()> {
        let id = tempfile::Builder::new().prefix("kod-gate-").tempdir()?;
        let container = id
            .path()
            .file_name()
            .and_then(|s| s.to_str())
            .context("container name")?;
        let result = self
            .command(
                &self.args(name, container)?,
                Duration::from_secs(self.config.timeout_secs),
            )
            .await;
        let cleanup = self
            .command(
                &["rm".into(), "--force".into(), container.into()],
                Duration::from_secs(10),
            )
            .await;
        match cleanup {
            Ok(true) => {}
            Ok(false) => bail!("scan container cleanup failed"),
            Err(e) => bail!("cannot verify scan container cleanup: {e:#}"),
        }
        match result {
            Ok(true) => Ok(()),
            Ok(false) => bail!("{name} failed (nonzero scanner exit)"),
            Err(e) => Err(e),
        }
    }
}

impl Scanner for DockerScanner {
    fn version(&self) -> Result<String> {
        self.config.version()
    }
    fn scan(&self, name: &str, sha: &str) -> Result<()> {
        validate_sha(sha)?;
        if sha != self.sha {
            bail!("archive SHA does not match scan target");
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.run(name))
    }
}

#[cfg(test)]
#[path = "scanner_tests.rs"]
mod tests;
