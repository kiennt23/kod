use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use kod_audit::AuditLog;
use kod_gates::{
    Conclusion, Config, Evidence, Gates,
    github::GitHub,
    records::HostRecords,
    scanner::{DockerScanner, ScanConfig, resource_name},
};
use kod_policy::Policy;
use serde::Deserialize;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::OnceLock,
};

struct DownloadScanner<'a> {
    github: &'a GitHub,
    config: ScanConfig,
    docker: PathBuf,
    archive: OnceLock<std::result::Result<tempfile::TempDir, String>>,
    prepared: OnceLock<std::result::Result<DockerScanner, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_same_archive_is_downloaded_once_for_all_scans() {
        let cache = OnceLock::new();
        let calls = std::cell::Cell::new(0);
        for _ in 0..4 {
            let dir = cached_archive(
                &cache,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                |_, out| {
                    calls.set(calls.get() + 1);
                    out.write_all(b"immutable source")?;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                std::fs::read(dir.path().join("source.tar.gz")).unwrap(),
                b"immutable source"
            );
        }
        assert_eq!(calls.get(), 1);
    }
}

fn cached_archive<'a>(
    cache: &'a OnceLock<std::result::Result<tempfile::TempDir, String>>,
    sha: &str,
    download: impl FnOnce(&str, &mut dyn Write) -> Result<()>,
) -> Result<&'a tempfile::TempDir> {
    let result = cache.get_or_init(|| {
        (|| -> Result<tempfile::TempDir> {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("source.tar.gz");
            let mut file = std::fs::File::create(&path)?;
            download(sha, &mut file)?;
            file.flush()?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))?;
            Ok(dir)
        })()
        .map_err(|e| format!("{e:#}"))
    });
    result.as_ref().map_err(|e| anyhow::anyhow!("{e}"))
}
impl kod_gates::Scanner for DownloadScanner<'_> {
    fn version(&self) -> Result<String> {
        self.config.version()
    }
    fn scan(&self, name: &str, sha: &str) -> Result<()> {
        let prepared = self.prepared.get_or_init(|| {
            (|| -> Result<DockerScanner> {
                let dir =
                    cached_archive(&self.archive, sha, |sha, out| self.github.archive(sha, out))?;
                let scanner = DockerScanner {
                    config: self.config.clone(),
                    archive: dir.path().join("source.tar.gz"),
                    sha: sha.into(),
                    docker: self.docker.clone(),
                    cache: Some(resource_name(sha)?),
                };
                scanner.prepare_cache()?;
                Ok(scanner)
            })()
            .map_err(|e| format!("{e:#}"))
        });
        let scanner = prepared.as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
        kod_gates::Scanner::scan(scanner, name, sha)
    }
    fn finish(&self) -> Result<()> {
        if let Some(Ok(scanner)) = self.prepared.get() {
            scanner.remove_cache()?;
        }
        Ok(())
    }
}

#[derive(Parser)]
#[command(
    version,
    about = "Trusted gate runner; all configuration and records must live outside PR workspaces"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        scans: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        records: PathBuf,
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        audit: PathBuf,
        #[arg(long)]
        docker: PathBuf,
        /// Host-verified merge_group binding; queue groups must contain one PR.
        #[arg(long)]
        candidate: Option<PathBuf>,
    },
    Ruleset {
        #[arg(long)]
        app_id: u64,
    },
    ScanConfig {
        #[arg(long)]
        image: String,
    },
    RecordBypass {
        #[arg(long)]
        audit: PathBuf,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        delivery: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        sha: String,
        #[arg(long)]
        ruleset: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    pr: u64,
    head: String,
    sha: String,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .context("invalid trusted configuration or evidence")
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{}", kod_audit::redact::redact(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::Run {
            config,
            scans,
            evidence,
            records,
            policy,
            audit,
            docker,
            candidate,
        } => {
            use std::os::unix::fs::OpenOptionsExt;
            let mut lock_path = audit.as_os_str().to_owned();
            lock_path.push(".gates.lock");
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(PathBuf::from(lock_path))?;
            lock.lock()?;
            let config: Config = read(&config)?;
            config.validate()?;
            let scans: ScanConfig = read(&scans)?;
            scans.validate()?;
            if !docker.is_absolute() {
                bail!("Docker executable must be an absolute trusted path");
            }
            let evidence: Evidence = read(&evidence)?;
            let target = match candidate {
                Some(path) => {
                    let candidate: Candidate = read(&path)?;
                    if candidate.pr != evidence.pr || candidate.head != evidence.head {
                        bail!("merge candidate does not bind this PR head");
                    }
                    candidate.sha
                }
                None => evidence.head.clone(),
            };
            kod_gates::validate_sha(&target)?;
            let policy = Policy::load(&policy)?;
            let audit = AuditLog::open(&audit)?;
            let records = HostRecords(records);
            let token = std::env::var("KOD_GITHUB_TOKEN")
                .context("KOD_GITHUB_TOKEN must be a dedicated GitHub App installation token")?;
            let github = GitHub::new(&config.repo, &token)?;
            let scanner = DownloadScanner {
                github: &github,
                config: scans,
                docker,
                archive: OnceLock::new(),
                prepared: OnceLock::new(),
            };
            let gates = Gates {
                config: &config,
                policy: &policy,
                audit: &audit,
                tracker: &records,
                artifacts: &records,
                forge: &github,
                scanner: &scanner,
            };
            let checks = gates.run(&evidence, &target)?;
            serde_json::to_writer(std::io::stdout().lock(), &checks)?;
            Ok(checks.iter().all(|c| c.conclusion == Conclusion::Success))
        }
        Command::Ruleset { app_id } => {
            serde_json::to_writer_pretty(std::io::stdout().lock(), &kod_gates::ruleset(app_id)?)?;
            Ok(true)
        }
        Command::ScanConfig { image } => {
            let config = ScanConfig::rust(&image);
            config.validate()?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &config)?;
            Ok(true)
        }
        Command::RecordBypass {
            audit,
            repo,
            delivery,
            actor,
            sha,
            ruleset,
        } => {
            kod_gates::record_bypass(
                &AuditLog::open(&audit)?,
                &repo,
                &delivery,
                &actor,
                &sha,
                ruleset,
            )?;
            Ok(true)
        }
    }
}
