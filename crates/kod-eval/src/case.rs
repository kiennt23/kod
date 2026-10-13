//! Eval case format: one directory per case, `evals/cases/<id>/`, holding
//! `case.toml` and, for cases expected to finish, `heldout.patch`.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    #[default]
    Dev,
    Heldout,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum RiskClass {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndState {
    #[default]
    Done,
    Blocked,
    NeedsHuman,
    OverBudget,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub max_turns: u32,
    pub timeout_secs: u64,
    /// Docker memory limit, e.g. "4g". Swap is disabled.
    pub memory: String,
    pub cpus: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub id: String,
    pub split: Split,
    /// Names the sandbox image: `<image prefix>-<language>`.
    pub language: String,
    pub risk_class: RiskClass,
    /// Sanitized mirror on the eval host. Only the tree at `base_commit` reaches the agent.
    pub repo: PathBuf,
    pub base_commit: String,
    /// The real merge commit. Only `check-case` uses it; the agent never sees it.
    pub merge_commit: Option<String>,
    pub ticket: String,
    /// Runs in the score container with /work read-only. It must keep build
    /// output under /tmp and write a JUnit report to $KOD_JUNIT.
    pub test_command: String,
    /// Held-back tests that must run and pass, as `classname::name` from the
    /// JUnit report. `check-case` fills this in; `run` refuses a case expected
    /// to finish while it is empty.
    #[serde(default)]
    pub required_tests: Vec<String>,
    /// Directory prefixes ending in "/", or exact file paths.
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub expected_end_state: EndState,
    pub budget: Budget,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl EvalCase {
    pub fn heldout_patch(&self) -> PathBuf {
        self.dir.join("heldout.patch")
    }
}

pub fn load_case(dir: &Path) -> Result<EvalCase> {
    let path = dir.join("case.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut case: EvalCase =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    case.dir = dir.to_path_buf();
    validate(&case)?;
    Ok(case)
}

pub fn load_cases(root: &Path) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    for entry in std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let path = entry?.path();
        if path.join("case.toml").is_file() {
            cases.push(load_case(&path)?);
        }
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cases)
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate(c: &EvalCase) -> Result<()> {
    let id = &c.id;
    // The id names directories and containers.
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        bail!("{id}: id may only use letters, digits, '.', '_' and '-'");
    }
    if c.dir.file_name().and_then(|n| n.to_str()) != Some(id.as_str()) {
        bail!("{id}: the directory name must equal the case id");
    }
    if c.language.is_empty()
        || !c
            .language
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        bail!("{id}: language must be lowercase letters and digits");
    }
    if !is_sha(&c.base_commit) {
        bail!("{id}: base_commit must be a full 40-character SHA");
    }
    if c.merge_commit.as_deref().is_some_and(|m| !is_sha(m)) {
        bail!("{id}: merge_commit must be a full 40-character SHA");
    }
    if c.ticket.trim().is_empty() {
        bail!("{id}: ticket is empty");
    }
    if c.test_command.trim().is_empty() {
        bail!("{id}: test_command is empty");
    }
    if c.budget.max_turns == 0 || c.budget.timeout_secs == 0 || c.budget.cpus <= 0.0 {
        bail!("{id}: budget values must be positive");
    }
    if c.expected_end_state == EndState::Done {
        let len = std::fs::metadata(c.heldout_patch())
            .map(|m| m.len())
            .unwrap_or(0);
        if len == 0 {
            bail!("{id}: a case expected to finish needs a non-empty heldout.patch");
        }
    }
    Ok(())
}
