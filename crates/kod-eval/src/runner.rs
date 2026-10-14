//! Runner adapters: build the command, set the model endpoint, and map the
//! runner's output to an end state (the runner adapter contract in the spec).
use crate::case::{EndState, EvalCase};

/// The proxy's alias on each run's internal network.
pub const PROXY_URL: &str = "http://kod-llm-proxy:8080";

pub enum Runner {
    /// Claude Code headless: the phase 0 baseline.
    ClaudeCode { model: String },
    /// A shell command. Used by the end-to-end tests.
    Script { command: String },
}

impl Runner {
    pub fn name(&self) -> &str {
        match self {
            Runner::ClaudeCode { .. } => "claude-code",
            Runner::Script { .. } => "script",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Runner::ClaudeCode { model } => model,
            Runner::Script { .. } => "none",
        }
    }

    /// Runs as the unprivileged agent user. The prompt arrives on stdin.
    pub fn command(&self, case: &EvalCase) -> Vec<String> {
        match self {
            Runner::ClaudeCode { model } => [
                "claude",
                "-p",
                "--output-format",
                "json",
                "--max-turns",
                &case.budget.max_turns.to_string(),
                "--model",
                model,
                // Safe only because the sandbox is the boundary.
                "--dangerously-skip-permissions",
            ]
            .map(String::from)
            .to_vec(),
            Runner::Script { command } => vec!["sh".into(), "-c".into(), command.clone()],
        }
    }

    pub fn env(&self) -> Vec<(String, String)> {
        match self {
            Runner::ClaudeCode { .. } => [
                ("ANTHROPIC_BASE_URL", PROXY_URL),
                // The real key lives only in kod-llm-proxy.
                ("ANTHROPIC_API_KEY", "sk-kod-dummy"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .to_vec(),
            Runner::Script { .. } => vec![],
        }
    }

    pub fn end_state(&self, exit_code: i32, stdout: &str) -> (EndState, Option<f64>) {
        match self {
            Runner::ClaudeCode { .. } => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
                    return (EndState::NeedsHuman, None);
                };
                let state = if v["subtype"] == "error_max_turns" {
                    EndState::OverBudget
                } else if v["is_error"].as_bool() != Some(false) {
                    EndState::NeedsHuman
                } else {
                    marker(v["result"].as_str().unwrap_or("")).unwrap_or(EndState::Done)
                };
                (state, v["total_cost_usd"].as_f64())
            }
            Runner::Script { .. } => {
                let fallback = if exit_code == 0 {
                    EndState::Done
                } else {
                    EndState::NeedsHuman
                };
                (marker(stdout).unwrap_or(fallback), None)
            }
        }
    }
}

/// The agent reports blocked or needs-human on its last line.
fn marker(text: &str) -> Option<EndState> {
    match text.lines().rev().find(|l| !l.trim().is_empty())?.trim() {
        "KOD_END_STATE: blocked" => Some(EndState::Blocked),
        "KOD_END_STATE: needs_human" => Some(EndState::NeedsHuman),
        _ => None,
    }
}
