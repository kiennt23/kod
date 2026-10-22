use anyhow::Result;
use clap::{Parser, Subcommand};
use kod_status::records::HostRecords;
use std::{io::Write, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(version, about = "Kod workflow status from trusted host records")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Status {
        story: String,
        #[arg(long)]
        records: PathBuf,
        #[arg(long)]
        dispatcher: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Status {
            story,
            records,
            dispatcher,
            json,
        } => {
            let source = HostRecords {
                snapshot: records,
                dispatcher,
            };
            let view = kod_status::load(&source, &story)?;
            let mut out = std::io::stdout().lock();
            if json {
                let value = serde_json::to_value(&view)?;
                serde_json::to_writer_pretty(&mut out, &kod_audit::redact::redact_value(&value))?;
                writeln!(out)?;
            } else {
                out.write_all(view.render().as_bytes())?;
            }
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", kod_audit::redact::redact(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}
