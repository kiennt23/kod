//! Building eval cases from merged changes, and checking that their held-back
//! tests actually test the ticket.
use crate::{
    case::{EndState, EvalCase},
    run,
    workspace::{git, patch_paths},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct NewCase {
    pub mirror: PathBuf,
    pub merge_commit: String,
    pub id: String,
    pub language: String,
    pub risk_class: String,
    pub test_command: String,
    /// Paths in the merged change that hold its tests. Their diff becomes heldout.patch.
    pub test_paths: Vec<String>,
}

fn quoted(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

pub fn new_case(n: &NewCase, cases_root: &Path) -> Result<PathBuf> {
    if !["low", "medium", "high"].contains(&n.risk_class.as_str()) {
        bail!("risk class must be low, medium or high");
    }
    let merge = git(
        &n.mirror,
        &["rev-parse", &format!("{}^{{commit}}", n.merge_commit)],
    )?
    .trim()
    .to_owned();
    let base = git(&n.mirror, &["rev-parse", &format!("{merge}^1")])?
        .trim()
        .to_owned();
    let mut args = vec!["diff", "--binary", "--no-renames", &base, &merge, "--"];
    args.extend(n.test_paths.iter().map(String::as_str));
    let heldout = git(&n.mirror, &args)?;
    if heldout.is_empty() {
        bail!("the change has no edits under {}", n.test_paths.join(", "));
    }
    let dir = cases_root.join(&n.id);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("heldout.patch"), heldout)?;
    let mirror = n.mirror.canonicalize()?;
    fs::write(
        dir.join("case.toml"),
        format!(
            r#"id = {id}
# Choose before the first run, and never move a case between splits.
split = "dev"
language = {language}
risk_class = {risk}
repo = {repo}
base_commit = "{base}"
merge_commit = "{merge}"
# Paste the ticket as the requester wrote it. A case with an empty ticket won't load.
ticket = """
"""
# Runs with /work read-only: keep build output under /tmp, and write a JUnit
# report to $KOD_JUNIT. See docs/eval-set.md for each language.
test_command = {test}
# Filled in by check-case.
required_tests = []
protected_paths = []

[budget]
max_turns = 50
timeout_secs = 1800
memory = "4g"
cpus = 2.0
"#,
            id = quoted(&n.id),
            language = quoted(&n.language),
            risk = quoted(&n.risk_class),
            repo = quoted(&mirror.to_string_lossy()),
            test = quoted(&n.test_command),
        ),
    )?;
    Ok(dir)
}

pub struct Check {
    pub problems: Vec<String>,
    /// For a functional case, the tests that fail or are absent without the
    /// fix and pass with it.
    pub required_tests: Vec<String>,
}

/// Checks a case by its kind; see the task description. Both scoring runs are
/// offline.
pub fn check_case(
    case: &EvalCase,
    image_prefix: &str,
    dir: &Path,
    stop_file: &Path,
) -> Result<Check> {
    if stop_file.exists() {
        bail!(
            "the kill switch is on; delete {} to check cases",
            stop_file.display()
        );
    }
    fs::create_dir_all(dir)?;
    let mut problems = Vec::new();
    if case.expected_end_state != EndState::Done {
        if git(
            &case.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", case.base_commit),
            ],
        )
        .is_err()
        {
            problems.push(format!(
                "base commit {} is not in the mirror",
                case.base_commit
            ));
        }
        if !case.required_tests.is_empty() {
            problems.push(
                "required_tests is set, but a case that isn't expected to finish is never scored"
                    .to_owned(),
            );
        }
        return Ok(Check {
            problems,
            required_tests: vec![],
        });
    }

    let merge = case
        .merge_commit
        .as_deref()
        .context("check-case needs merge_commit in case.toml")?;
    let without = run::score(
        case,
        image_prefix,
        "check",
        &dir.join("without-fix"),
        None,
        stop_file,
    )?;
    let mut args = vec![
        "diff".to_owned(),
        "--binary".into(),
        "--no-renames".into(),
        case.base_commit.clone(),
        merge.to_owned(),
        "--".into(),
        ".".into(),
    ];
    args.extend(
        patch_paths(dir, &case.heldout_patch())?
            .iter()
            .map(|p| format!(":(exclude){p}")),
    );
    let fix = dir.join("real-fix.patch");
    fs::write(
        &fix,
        git(
            &case.repo,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )?,
    )?;
    let with = run::score(
        case,
        image_prefix,
        "check",
        &dir.join("with-fix"),
        Some(&fix),
        stop_file,
    )?;
    if without.stopped || with.stopped {
        bail!("the kill switch was turned on during check-case");
    }

    for (label, sc) in [("without", &without), ("with", &with)] {
        if !sc.ran {
            problems.push(format!(
                "scoring did not run for {label} fix; see {}",
                dir.join(format!("{label}-fix/score_out")).display()
            ));
        }
    }
    if !problems.is_empty() {
        return Ok(Check {
            problems,
            required_tests: vec![],
        });
    }
    let log = dir.join("with-fix/score_out/score.log");
    if !with.exit_ok {
        problems.push(format!(
            "held-back tests fail with the real fix; see {} {}",
            log.display(),
            with.notes.join("; ")
        ));
    }
    if with.tests.is_empty() {
        problems.push(format!(
            "no JUnit report with the real fix: the test command must write $KOD_JUNIT; see {}",
            log.display()
        ));
    }
    let required: Vec<String> = with
        .tests
        .iter()
        .filter(|(name, ok)| *ok && !without.tests.iter().any(|(n, o)| n == name && *o))
        .map(|(name, _)| name.clone())
        .collect();
    if !with.tests.is_empty() && required.is_empty() {
        problems.push(
            "no held-back test fails without the fix, so they don't test the ticket".to_owned(),
        );
    }
    Ok(Check {
        problems,
        required_tests: required,
    })
}

/// Rewrites the `required_tests = ...` line in case.toml, keeping the rest of
/// the file and its comments.
pub fn set_required_tests(case_dir: &Path, tests: &[String]) -> Result<()> {
    let path = case_dir.join("case.toml");
    let text = fs::read_to_string(&path)?;
    let line = format!(
        "required_tests = {}",
        toml::Value::Array(
            tests
                .iter()
                .map(|t| toml::Value::String(t.clone()))
                .collect()
        )
    );
    let mut found = false;
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("required_tests") {
                found = true;
                line.clone()
            } else {
                l.to_owned()
            }
        })
        .collect();
    if !found {
        bail!(
            "{} has no required_tests line; add `required_tests = []` above protected_paths",
            path.display()
        );
    }
    fs::write(&path, out.join("\n") + "\n")?;
    Ok(())
}
