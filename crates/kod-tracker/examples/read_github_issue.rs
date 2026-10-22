use anyhow::{Context, Result};
use kod_tracker::{
    Tracker,
    github::{Config, GitHub},
    mapping::Mapping,
};

fn execute() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let repo = args
        .next()
        .context("usage: read_github_issue OWNER/REPO NUMBER")?;
    let number = args.next().context("missing issue number")?;
    anyhow::ensure!(args.next().is_none(), "unexpected arguments");
    let token = std::env::var("KOD_TRACKER_TOKEN").context("missing KOD_TRACKER_TOKEN")?;
    let secret = std::env::var("KOD_TRACKER_WEBHOOK_SECRET")
        .context("missing KOD_TRACKER_WEBHOOK_SECRET")?;
    let comment_author_id = std::env::var("KOD_TRACKER_COMMENT_AUTHOR_ID")
        .context("missing KOD_TRACKER_COMMENT_AUTHOR_ID")?
        .parse()
        .context("invalid comment author ID")?;
    let mapping = Mapping::parse(
        "GitHub registry",
        include_str!("../../../registry/trackers/github.json"),
    )?;
    let github = GitHub::new(
        Config {
            repo: repo.clone(),
            comment_author_id,
        },
        mapping,
        &token,
        &secret,
    )?;
    let issue = github.get_issue(&format!("{repo}#{number}"))?;
    println!(
        "{} {} {}",
        issue.key,
        issue.state.name(),
        issue.title.escape_debug()
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    match execute() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
