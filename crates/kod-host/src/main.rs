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
        /// Published status records naming open PR heads whose checks must be invalidated.
        #[arg(long)]
        records: Option<PathBuf>,
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
        Command::Snapshot(Snapshot::Activate {
            config,
            manifest,
            records,
        }) => {
            let config = kod_host::config::FsConfig::load(&config)?;
            let mut open = vec![];
            if let Some(path) = records {
                let s: kod_status::records::Snapshot =
                    kod_host::config::strict_json(&std::fs::read(path)?)?;
                for p in s
                    .pulls
                    .iter()
                    .filter(|p| matches!(p.state, kod_status::records::PrState::Open))
                {
                    open.push(kod_host::records::Open {
                        story: s.story.key.clone(),
                        head: p.head.clone(),
                    });
                }
            }
            // Invalidating GitHub checks needs the Task 7 GitHub assembly; until then any open head refuses.
            kod_host::records::activate(&config, &manifest, &open, &|_| {
                bail!("check invalidation requires the connected service assembly")
            })?;
            println!("snapshot activated");
            Ok(())
        }
    }
}
