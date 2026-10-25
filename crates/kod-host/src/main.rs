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
    Accept {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long, value_enum)]
        mode: kod_host::acceptance::Mode,
    },
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
        Command::Accept {
            config,
            evidence,
            mode,
        } => {
            let result = (|| -> Result<i32> {
                let config = kod_host::config::FsConfig::parse(&config)?;
                let runtime = tokio::runtime::Runtime::new()?;
                let report =
                    runtime.block_on(kod_host::acceptance::run(&config, &evidence, mode))?;
                Ok(report.exit_code())
            })();
            let code = match result {
                Ok(code) => code,
                Err(_) => {
                    eprintln!("acceptance blocked: configuration or evidence unavailable");
                    2
                }
            };
            std::process::exit(code);
        }
        Command::Check { config } => {
            kod_host::config::FsConfig::load(&config)?;
            println!("configuration valid");
            Ok(())
        }
        Command::Serve { config } => {
            let config = kod_host::config::FsConfig::parse(&config)?;
            let prepared = kod_host::service::prepare(config, |config| {
                let creds = kod_host::tracker_credentials::TrackerCredentials::new(config)?;
                let mapping = kod_tracker::mapping::Mapping::load(&config.github.mapping)?;
                let secret = kod_host::config::read_private(&config.github.webhook_secret_file)?;
                creds.start()?;
                Ok((creds, mapping, secret))
            })?;
            let (creds, mapping, secret) = prepared.extra;
            let config = prepared.config;
            let run = async {
                let tracker = kod_tracker::github::GitHub::with_token_source(
                    config.github.tracker.clone(),
                    mapping,
                    creds.token_source(),
                    std::str::from_utf8(&secret)?.trim(),
                )?;
                let tracker = std::sync::Arc::new(tracker);
                let parts = kod_host::service::cleanup_on_error(
                    &config,
                    kod_host::service::production_parts(&config, tracker.clone()),
                )
                .await?;
                kod_host::service::serve_refreshing(
                    config.clone(),
                    parts,
                    tracker,
                    prepared.lock,
                    async {
                        let term = tokio::signal::unix::signal(
                            tokio::signal::unix::SignalKind::terminate(),
                        );
                        // No handler means shut down at once (fail closed), not panic.
                        if let Ok(mut term) = term {
                            tokio::select! {
                                _ = tokio::signal::ctrl_c() => {}
                                _ = term.recv() => {}
                            }
                        }
                    },
                )
                .await
            };
            let runtime = tokio::runtime::Runtime::new()?;
            let result = runtime.block_on(run);
            // A stuck blocking tick must not keep the process (or the tracker revoke) waiting.
            kod_host::service::exit_sequence(
                runtime,
                result,
                std::time::Duration::from_secs(5),
                || creds.shutdown(),
            )
        }
        Command::Snapshot(Snapshot::Activate { config, manifest }) => {
            let config = kod_host::config::FsConfig::load(&config)?;
            kod_host::records::activate_cli(&config, &manifest)
        }
    }
}
