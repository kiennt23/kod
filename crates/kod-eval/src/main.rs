use anyhow::{Result, bail};
use clap::Parser;
use kod_eval::{case, newcase, report, run, runner::Runner};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kod-eval",
    about = "Run Kod eval cases in sandboxes and report the results"
)]
#[allow(clippy::large_enum_variant)] // parsed once at startup
enum Cli {
    /// Create a case from a merged change
    NewCase {
        #[arg(long)]
        mirror: PathBuf,
        #[arg(long)]
        merge_commit: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        language: String,
        #[arg(long)]
        risk_class: String,
        #[arg(long)]
        test_command: String,
        /// A path holding the change's tests; repeat for several
        #[arg(long = "test-path", required = true)]
        test_paths: Vec<String>,
        #[arg(long, default_value = "evals/cases")]
        cases: PathBuf,
    },
    /// Check a case by its kind, and record a functional case's required tests
    CheckCase {
        case_dir: PathBuf,
        #[arg(long, default_value = "kod-sandbox")]
        image_prefix: String,
        #[arg(long, default_value = "results")]
        out: PathBuf,
    },
    /// Run eval cases and score them
    Run {
        #[arg(long, default_value = "evals/cases")]
        cases: PathBuf,
        /// dev, heldout or all. Held-out cases are a gate: never tune on them.
        #[arg(long, default_value = "dev")]
        split: String,
        /// Only this case id
        #[arg(long)]
        only: Option<String>,
        /// claude-code or script
        #[arg(long, default_value = "claude-code")]
        runner: String,
        /// Pinned model id; required for claude-code
        #[arg(long)]
        model: Option<String>,
        /// Shell command; required for the script runner
        #[arg(long)]
        script: Option<String>,
        #[arg(long, default_value = "kod-sandbox")]
        image_prefix: String,
        #[arg(long, default_value = "kod-llm-proxy")]
        proxy_image: String,
        #[arg(long, default_value = "results")]
        out: PathBuf,
        /// Another model the proxy may forward to, such as the runner's small
        /// background model. Repeatable.
        #[arg(long)]
        extra_model: Vec<String>,
    },
    /// Kill switch: no new containers start, and every Kod container on this
    /// host is killed. Delete the stop file ($KOD_STOP_FILE, or ~/.kod/STOP)
    /// to run again.
    Stop,
    /// Summarize a run directory; exits 1 if any case is unsafe or not verified
    Report { run_dir: PathBuf },
}

fn main() -> Result<()> {
    match Cli::parse() {
        Cli::Run {
            cases,
            split,
            only,
            runner,
            model,
            script,
            image_prefix,
            proxy_image,
            out,
            extra_model,
        } => {
            let runner = match (runner.as_str(), model, script) {
                ("claude-code", Some(model), _) => Runner::ClaudeCode { model },
                ("claude-code", None, _) => bail!("--model is required: pin the model id"),
                ("script", _, Some(command)) => Runner::Script { command },
                ("script", _, None) => bail!("--script is required for the script runner"),
                (other, ..) => bail!("unknown runner {other}"),
            };
            let cases: Vec<_> = case::load_cases(&cases)?
                .into_iter()
                .filter(|c| match split.as_str() {
                    "all" => true,
                    "dev" => c.split == case::Split::Dev,
                    "heldout" => c.split == case::Split::Heldout,
                    _ => false,
                })
                .filter(|c| only.as_deref().is_none_or(|id| c.id == id))
                .collect();
            if cases.is_empty() {
                bail!("no cases match --split {split} and --only");
            }
            let settings = run::Settings {
                image_prefix,
                proxy_image,
                out_root: out,
                extra_models: extra_model,
                stop_file: run::default_stop_file(),
            };
            let dir = run::run_all(&cases, &runner, &settings)?;
            println!("results: {}", dir.display());
        }
        Cli::NewCase {
            mirror,
            merge_commit,
            id,
            language,
            risk_class,
            test_command,
            test_paths,
            cases,
        } => {
            let n = newcase::NewCase {
                mirror,
                merge_commit,
                id,
                language,
                risk_class,
                test_command,
                test_paths,
            };
            let dir = newcase::new_case(&n, &cases)?;
            println!(
                "created {}: paste the ticket into case.toml, then run check-case",
                dir.display()
            );
        }
        Cli::CheckCase {
            case_dir,
            image_prefix,
            out,
        } => {
            let c = case::load_case(&case_dir)?;
            let check = newcase::check_case(
                &c,
                &image_prefix,
                &out.join(format!("check-{}", c.id)),
                &run::default_stop_file(),
            )?;
            if !check.problems.is_empty() {
                for p in &check.problems {
                    eprintln!("{}: {p}", c.id);
                }
                std::process::exit(1);
            }
            if c.expected_end_state == case::EndState::Done {
                newcase::set_required_tests(&case_dir, &check.required_tests)?;
                println!(
                    "{}: ok, required tests: {}",
                    c.id,
                    check.required_tests.join(", ")
                );
            } else {
                println!("{}: ok", c.id);
            }
        }
        Cli::Stop => run::stop(&run::default_stop_file())?,
        Cli::Report { run_dir } => {
            if !report::write(&run_dir)? {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
