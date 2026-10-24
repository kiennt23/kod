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
    }
}
