use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kod_audit::{
    AuditLog,
    checkpoint::{CheckpointTarget, DirTarget},
    now,
};
use kod_policy::Policy;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::{path::PathBuf, process::ExitCode};

/// Kod's audit log. Prints one JSON line; exits non-zero on any failure.
#[derive(Parser)]
#[command(name = "kod-audit", version)]
struct Cli {
    #[arg(long, default_value = "/var/lib/kod/audit.db")]
    db: PathBuf,
    /// Write-once checkpoint directory, owned by a separate account
    #[arg(long)]
    checkpoints: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Freeze the trusted checkpoint set before taking an off-host DB snapshot
    CaptureCheckpoints {
        /// New private directory; existing directories are refused
        #[arg(long)]
        output: PathBuf,
    },
    /// Check the chain and every checkpoint
    Verify {
        /// Directory holding one policy directory per released version; when
        /// given, every retention record is checked against them too
        #[arg(long)]
        released_policies: Option<PathBuf>,
    },
    /// Verify, then write the chain head as a new checkpoint
    Checkpoint,
    /// Verify, then delete payloads whose retention has ended
    Purge {
        #[arg(long, default_value = "/etc/kod/policy")]
        policy: PathBuf,
        #[arg(long)]
        record_type: String,
    },
}

fn run(cli: Cli) -> Result<serde_json::Value> {
    let target = DirTarget {
        dir: cli.checkpoints,
    };
    Ok(match cli.command {
        Command::CaptureCheckpoints { output } => {
            let checkpoints = target.list()?;
            std::fs::DirBuilder::new().mode(0o700).create(&output)?;
            let frozen = DirTarget { dir: output };
            for cp in &checkpoints {
                frozen.put(cp)?;
            }
            json!({"ok": true, "checkpoints": checkpoints.len()})
        }
        Command::Verify { released_policies } => {
            let log = AuditLog::open_existing(&cli.db)?;
            let v = log.verify(&target)?;
            let mut out =
                json!({"ok": true, "entries": v.entries, "last_checkpoint": v.last_checkpoint});
            if let Some(dir) = released_policies {
                let released = std::fs::read_dir(&dir)
                    .with_context(|| format!("released policies {}", dir.display()))?
                    .map(|e| Policy::load(&e?.path()))
                    .collect::<Result<Vec<_>>>()?;
                out["retention_records"] = log.verify_retention(&released)?.into();
            }
            out
        }
        Command::Checkpoint => {
            let log = AuditLog::open_existing(&cli.db)?;
            json!({"ok": true, "checkpoint": log.checkpoint(&target)?})
        }
        Command::Purge {
            policy,
            record_type,
        } => {
            let log = AuditLog::open_existing(&cli.db)?;
            // Separate transactions: an entry appended in between is checked
            // by the next verify, and purge clears only payloads its own
            // retention records cover.
            log.verify(&target)?;
            let policy = Policy::load(&policy)?;
            json!({"ok": true, "payloads_cleared": log.purge(&policy, &record_type, now()?)?})
        }
    })
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", json!({"ok": false, "error": format!("{e:#}")}));
            ExitCode::FAILURE
        }
    }
}
