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
    pub package_network: String,
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
            package_network: "kod-packages".into(),
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
        if self.package_network.is_empty()
            || matches!(
                self.package_network.as_str(),
                "host" | "none" | "bridge" | "default"
            )
            || !self
                .package_network
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("a dedicated package-only Docker network is required");
        }
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
    pub cache: Option<String>,
}

pub fn resource_name(sha: &str) -> Result<String> {
    validate_sha(sha)?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("container name randomness: {e}"))?;
    Ok(format!(
        "kod-gate-{}-{:032x}",
        &sha[..8],
        u128::from_le_bytes(random)
    ))
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
        let mut args = self.container_args(container, false)?;
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

    pub fn container_args(&self, container: &str, preparing: bool) -> Result<Vec<String>> {
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
        args.extend(["--mount".into(), "type=volume,target=/work".into()]);
        args.push(format!("--env=KOD_SOURCE_SHA={}", self.sha));
        if preparing {
            for arg in &mut args {
                if arg == "--network=none" {
                    *arg = format!("--network={}", self.config.package_network);
                }
                if arg == "--entrypoint=/opt/kod/scan" {
                    *arg = "--entrypoint=/opt/kod/prepare".into();
                }
            }
        }
        if let Some(cache) = &self.cache {
            if cache.is_empty()
                || !cache
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                bail!("invalid prepared cache volume");
            }
            args.extend([
                "--mount".into(),
                format!(
                    "type=volume,source={cache},target=/prepared{}",
                    if preparing { "" } else { ",readonly" }
                ),
            ]);
        } else if preparing {
            bail!("preparation requires a private cache volume");
        }
        args.extend([
            "--mount".into(),
            format!("type=bind,source={path},target=/input/source.tar.gz,readonly"),
            self.config.image.clone(),
        ]);
        Ok(args)
    }

    async fn run(&self, name: &str) -> Result<()> {
        self.config.validate()?;
        self.inventory().await?;
        if self.cache.is_none() {
            bail!("scans require freshly prepared dependencies and advisories");
        }
        self.run_container(name).await
    }

    async fn inventory(&self) -> Result<()> {
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
        Ok(())
    }

    async fn run_container(&self, name: &str) -> Result<()> {
        let container = resource_name(&self.sha)?;
        let result = self
            .command(
                &self.args(name, &container)?,
                Duration::from_secs(self.config.timeout_secs),
            )
            .await;
        let cleanup = self
            .command(
                &["rm".into(), "--force".into(), "--volumes".into(), container],
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

    pub fn prepare_cache(&self) -> Result<()> {
        self.config.validate()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                self.inventory().await?;
                self.prepare_container().await
            })
    }

    async fn prepare_container(&self) -> Result<()> {
        let cache = self
            .cache
            .as_ref()
            .context("missing prepared cache volume")?;
        let container = resource_name(&self.sha)?;
        let args = self.container_args(&container, true)?;
        if !self
            .command(
                &["volume".into(), "create".into(), cache.clone()],
                Duration::from_secs(10),
            )
            .await?
        {
            bail!("creating private dependency cache failed");
        }
        let result = self
            .command(&args, Duration::from_secs(self.config.timeout_secs))
            .await;
        let cleanup = self
            .command(
                &["rm".into(), "--force".into(), "--volumes".into(), container],
                Duration::from_secs(10),
            )
            .await;
        if matches!((&result, &cleanup), (Ok(true), Ok(true))) {
            return Ok(());
        }
        let removal = self
            .command(
                &["volume".into(), "rm".into(), cache.clone()],
                Duration::from_secs(10),
            )
            .await;
        bail!(
            "dependency preparation failed: command={result:?}, container cleanup={cleanup:?}, cache cleanup={removal:?}"
        )
    }

    pub fn remove_cache(&self) -> Result<()> {
        let cache = self
            .cache
            .as_ref()
            .context("missing prepared cache volume")?;
        let success = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.command(
                &["volume".into(), "rm".into(), cache.clone()],
                Duration::from_secs(10),
            ))?;
        if !success {
            bail!("removing private dependency cache failed");
        }
        Ok(())
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
