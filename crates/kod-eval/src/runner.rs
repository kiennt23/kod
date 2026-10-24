//! Runner adapters: build the command, set the model endpoint, and map the
//! runner's output to an end state (the runner adapter contract in the spec).
use crate::case::{EndState, EvalCase};

/// The gateway's alias on each run's internal network. Each case's base URL
/// adds `/r/<token>`, which tells the gateway which case is calling.
pub const PROXY_URL: &str = "http://kod-llm-proxy:8080";

pub enum Runner {
    /// Claude Code headless: the phase 0 baseline.
    ClaudeCode { model: String },
    /// Our loop (crates/kod-loop), built into every sandbox image.
    /// `max_tokens` follows the proxy's cap, which refuses larger requests.
    KodLoop { model: String, max_tokens: u64 },
    /// A shell command. Used by the end-to-end tests.
    Script { command: String },
}

impl Runner {
    pub fn name(&self) -> &str {
        match self {
            Runner::ClaudeCode { .. } => "claude-code",
            Runner::KodLoop { .. } => "kod-loop",
            Runner::Script { .. } => "script",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Runner::ClaudeCode { model } => model,
            Runner::KodLoop { model, .. } => model,
            Runner::Script { .. } => "none",
        }
    }

    /// Runs as the unprivileged agent user. The prompt arrives on stdin.
    pub fn command(&self, case: &EvalCase) -> Vec<String> {
        self.command_with_turns(case.budget.max_turns)
    }

    pub fn command_with_turns(&self, max_turns: u32) -> Vec<String> {
        match self {
            Runner::ClaudeCode { model } => [
                "claude",
                "-p",
                "--output-format",
                "json",
                "--max-turns",
                &max_turns.to_string(),
                "--model",
                model,
                // Safe only because the sandbox is the boundary.
                "--dangerously-skip-permissions",
            ]
            .map(String::from)
            .to_vec(),
            Runner::KodLoop { model, max_tokens } => [
                "kod-loop",
                "--model",
                model,
                "--max-turns",
                &max_turns.to_string(),
                "--max-tokens",
                &max_tokens.to_string(),
            ]
            .map(String::from)
            .to_vec(),
            Runner::Script { command } => vec!["sh".into(), "-c".into(), command.clone()],
        }
    }

    /// `base_url` is the case's gateway URL, `PROXY_URL/r/<token>`. A script
    /// gets it too, so a script's model calls meet the same budget.
    pub fn env(&self, base_url: &str) -> Vec<(String, String)> {
        match self {
            Runner::ClaudeCode { .. } => [
                ("ANTHROPIC_BASE_URL", base_url),
                // The real key lives only in kod-llm-proxy.
                ("ANTHROPIC_API_KEY", "sk-kod-dummy"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
                // The gateway forwards no anthropic-beta value it has not
                // been told it can price and see.
                ("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .to_vec(),
            // No key at all: the gateway adds the real one.
            Runner::KodLoop { .. } | Runner::Script { .. } => {
                vec![("ANTHROPIC_BASE_URL".into(), base_url.into())]
            }
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
            Runner::KodLoop { .. } => {
                // Untrusted, like all agent output: only one clean outcome line
                // from a clean exit counts. Safety never depends on it.
                let state = serde_json::from_str::<serde_json::Value>(stdout)
                    .ok()
                    .filter(|_| exit_code == 0)
                    .and_then(|v| serde_json::from_value(v["end_state"].clone()).ok())
                    .unwrap_or(EndState::NeedsHuman);
                (state, None)
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
