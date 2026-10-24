use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Check {
        #[arg(long)]
        config: PathBuf,
    },
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(subcommand)]
    Snapshot(Snapshot),
}
#[derive(Subcommand)]
enum Snapshot {
    Activate {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
    },
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Check { config } => {
            kod_host::config::FsConfig::load(&config)?;
            println!("configuration valid");
            Ok(())
        }
        Command::Serve { config } => {
            let config = std::sync::Arc::new(kod_host::config::FsConfig::load(&config)?);
            let creds = kod_host::tracker_credentials::TrackerCredentials::new(&config)?;
            let mapping = kod_tracker::mapping::Mapping::load(&config.github.mapping)?;
            let secret = kod_host::config::read_private(&config.github.webhook_secret_file)?;
            creds.start()?;
            let run = async {
                let tracker = kod_tracker::github::GitHub::with_token_source(
                    config.github.tracker.clone(),
                    mapping,
                    creds.token_source(),
                    std::str::from_utf8(&secret)?.trim(),
                )?;
                let parts =
                    kod_host::service::production_parts(&config, std::sync::Arc::new(tracker))?;
                kod_host::service::serve(config.clone(), parts, async {
                    let mut term =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("SIGTERM handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                })
                .await
            };
            let result = tokio::runtime::Runtime::new()?.block_on(run);
            let revoked = creds.shutdown();
            result.and(revoked)
        }
        Command::Snapshot(Snapshot::Activate { config, manifest }) => {
            let config = kod_host::config::FsConfig::load(&config)?;
            kod_host::records::activate_cli(&config, &manifest)
        }
    }
}
