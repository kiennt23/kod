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
        Command::Serve { .. } => {
            bail!("connected serve unavailable until service assembly is implemented")
        }
        Command::Snapshot(Snapshot::Activate { config, manifest }) => {
            let config = kod_host::config::FsConfig::load(&config)?;
            kod_host::records::activate_cli(&config, &manifest)
        }
    }
}
