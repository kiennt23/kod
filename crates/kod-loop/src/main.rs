use anyhow::{Context, Result, bail};
use clap::Parser;
use kod_loop::{Config, Outcome, context, mcp::Mcp};
use std::{io::Read, path::PathBuf, process::ExitCode, time::Duration};

/// Kod's agent loop. Reads the prompt on stdin, works in the current
/// directory, and prints one JSON outcome line.
#[derive(Parser)]
#[command(name = "kod-loop", version)]
struct Cli {
    /// Pinned model id
    #[arg(long)]
    model: String,
    /// Model requests allowed; reaching the limit ends the run over_budget
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    max_turns: u32,
    /// max_tokens per request; at most the gateway's KOD_MAX_TOKENS
    #[arg(long, default_value_t = 8192)]
    max_tokens: u64,
    #[arg(long, default_value = "/etc/kod/kod-loop/system.md")]
    system_prompt: PathBuf,
    /// Older tool results are elided once the conversation is larger than this
    #[arg(long, default_value_t = 400_000)]
    context_bytes: usize,
    /// Each `run` command is killed after this long
    #[arg(long, default_value_t = 600)]
    command_timeout_secs: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match setup(&cli) {
        Ok((cfg, prompt, mcp)) => {
            kod_loop::run_with(&cfg, mcp.as_ref(), &prompt, &mut std::io::stderr())
        }
        Err(e) => Outcome::failed(format!("{e:#}")),
    };
    println!(
        "{}",
        serde_json::to_string(&outcome).expect("the outcome serializes")
    );
    if outcome.error.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn setup(cli: &Cli) -> Result<(Config, String, Option<Mcp>)> {
    let base = std::fs::read_to_string(&cli.system_prompt)
        .with_context(|| format!("reading {}", cli.system_prompt.display()))?;
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .context("ANTHROPIC_BASE_URL is required: the Kod gateway's URL")?;
    let mut prompt = String::new();
    std::io::stdin()
        .read_to_string(&mut prompt)
        .context("reading the prompt on stdin")?;
    if prompt.trim().is_empty() {
        bail!("empty prompt on stdin");
    }
    let workspace = std::env::current_dir().context("finding the workspace")?;
    let cfg = Config {
        system: context::system_prompt(&base, &workspace),
        base_url: base_url.trim_end_matches('/').to_owned(),
        model: cli.model.clone(),
        max_turns: cli.max_turns,
        max_tokens: cli.max_tokens,
        workspace,
        context_bytes: cli.context_bytes,
        command_timeout: Duration::from_secs(cli.command_timeout_secs),
        retry_backoff: [2, 10, 30].map(Duration::from_secs).to_vec(),
    };
    // Optional: without it the run has only its local tools. Set and
    // unreachable is a failed run, not a run without the gateway.
    let mcp = match std::env::var("KOD_MCP_URL") {
        Ok(url) => Some(Mcp::connect(&url).context("KOD_MCP_URL: the tool gateway")?),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => bail!("KOD_MCP_URL: {e}"),
    };
    Ok((cfg, prompt, mcp))
}
