use anyhow::{Result, bail};
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
            kod_host::config::FsConfig::load(&config)?;
            // ponytail: service::serve is complete; the tracker credential provider is a separate slice.
            bail!("connected serve needs the tracker credential provider, which is not wired yet")
        }
        Command::Snapshot(Snapshot::Activate { config, manifest }) => {
            let config = kod_host::config::FsConfig::load(&config)?;
            kod_host::records::activate_cli(&config, &manifest)
        }
    }
}
