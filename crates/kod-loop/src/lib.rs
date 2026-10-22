//! kod-loop: Kod's own agent loop, run as a runner under kod-eval. The prompt
//! arrives on stdin and the work happens in the current directory. The model
//! is reached only through ANTHROPIC_BASE_URL (the Kod gateway). The trace is
//! JSONL on stderr, and stdout is one JSON outcome line.
pub mod api;
pub mod context;
pub mod mcp;
pub mod tools;

use api::{ApiError, Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The same names as kod-eval's EndState.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndState {
    Done,
    Blocked,
    NeedsHuman,
    OverBudget,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl Usage {
    fn add(&mut self, u: &Value) {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        self.input_tokens += n("input_tokens");
        self.output_tokens += n("output_tokens");
        self.cache_creation_input_tokens += n("cache_creation_input_tokens");
        self.cache_read_input_tokens += n("cache_read_input_tokens");
    }
}

/// The one line kod-loop prints on stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub end_state: EndState,
    pub summary: String,
    pub turns: u32,
    pub usage: Usage,
    pub error: Option<String>,
}

impl Outcome {
    /// The loop could not run: a person looks.
    pub fn failed(error: String) -> Outcome {
        Outcome {
            end_state: EndState::NeedsHuman,
            summary: String::new(),
            turns: 0,
            usage: Usage::default(),
            error: Some(error),
        }
    }
}

pub struct Config {
    /// The gateway, e.g. http://kod-llm-proxy:8080. Never the provider.
    pub base_url: String,
    pub model: String,
    /// Model requests allowed; reaching it ends the run over_budget.
    pub max_turns: u32,
    /// At most the gateway's KOD_MAX_TOKENS, which refuses larger requests.
    pub max_tokens: u64,
    pub workspace: PathBuf,
    /// The whole system prompt, from context::system_prompt.
    pub system: String,
    pub context_bytes: usize,
    pub command_timeout: Duration,
    /// The wait before each retry of a transient model error. Each retry is
    /// a request, so it uses a turn.
    pub retry_backoff: Vec<Duration>,
}

/// Sent when a reply stops at the output limit with no tool call.
pub const CUT_OFF: &str =
    "Your reply was cut off at the output token limit. Continue in smaller steps.";

/// The result for each tool call in a reply cut off at the output limit: its
/// input may be truncated, so it is never run.
pub const CUT_OFF_CALL: &str = "Not run: your reply was cut off at the output token limit, so this call's input may be incomplete. Resend it in smaller steps.";

pub fn run(cfg: &Config, prompt: &str, trace: &mut dyn Write) -> Outcome {
    run_with(cfg, None, prompt, trace)
}

/// `run`, with the tool gateway's tools offered beside the local ones.
pub fn run_with(
    cfg: &Config,
    mcp: Option<&mcp::Mcp>,
    prompt: &str,
    trace: &mut dyn Write,
) -> Outcome {
    let client = Client::new(&cfg.base_url);
    // Retries of the current request so far; each one is also a turn.
    let mut retries = 0;
    let mut messages = vec![json!({"role": "user", "content": prompt})];
    let mut usage = Usage::default();
    let mut turns = 0;
    let end = |end_state, summary: String, turns, usage: &Usage, error| Outcome {
        end_state,
        summary,
        turns,
        usage: usage.clone(),
        error,
    };
    let outcome = loop {
        if turns == cfg.max_turns {
            let why = format!("stopped at the turn limit ({})", cfg.max_turns);
            break end(EndState::OverBudget, why, turns, &usage, None);
        }
        turns += 1;
        context::elide(&mut messages, cfg.system.len(), cfg.context_bytes);
        let started = Instant::now();
        let reply = match client.messages(&request(cfg, mcp, &messages)) {
            Ok(r) => {
                retries = 0;
                r
            }
            Err(ApiError::Transient(e, retry_after)) => match cfg.retry_backoff.get(retries) {
                // Back to the top: the retry is a request, so the turn limit
                // applies to it like any other.
                Some(w) => {
                    retries += 1;
                    std::thread::sleep(retry_after.map_or(*w, |a| a.max(*w)));
                    continue;
                }
                None => {
                    let why = "the model call failed";
                    break end(EndState::NeedsHuman, why.into(), turns, &usage, Some(e));
                }
            },
            Err(ApiError::Budget) => {
                let why = "the gateway refused the request: the run's budget is spent";
                break end(EndState::OverBudget, why.into(), turns, &usage, None);
            }
            Err(ApiError::Failed(e)) => {
                let why = "the model call failed";
                break end(EndState::NeedsHuman, why.into(), turns, &usage, Some(e));
            }
        };
        usage.add(&reply["usage"]);
        event(
            trace,
            json!({
                "event": "model_call",
                "turn": turns,
                "gen_ai.request.model": cfg.model,
                "gen_ai.response.finish_reasons": [reply["stop_reason"]],
                "gen_ai.usage.input_tokens": reply["usage"]["input_tokens"],
                "gen_ai.usage.output_tokens": reply["usage"]["output_tokens"],
                "duration_ms": ms(started),
            }),
        );
        let content = reply["content"].as_array().cloned().unwrap_or_default();
        let calls: Vec<Value> = content
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .cloned()
            .collect();
        if !content.is_empty() {
            messages.push(json!({"role": "assistant", "content": content}));
        }
        // The stop reason is checked before anything runs. Only tool_use runs
        // tools; max_tokens is continued; only end_turn or stop_sequence with
        // no calls can end the run. Anything else (a refusal, a context
        // overflow, an unknown reason, or calls under a non-tool stop) goes to
        // a person, with no tool run.
        let stop = reply["stop_reason"].as_str().unwrap_or("");
        match (stop, calls.is_empty()) {
            ("max_tokens", false) => {
                let results: Vec<Value> = calls
                    .iter()
                    .map(|c| json!({"type": "tool_result", "tool_use_id": c["id"], "content": CUT_OFF_CALL, "is_error": true}))
                    .collect();
                messages.push(json!({"role": "user", "content": results}));
                continue;
            }
            ("max_tokens", true) => {
                messages.push(json!({"role": "user", "content": CUT_OFF}));
                continue;
            }
            ("tool_use", false) => {}
            ("end_turn" | "stop_sequence", true) => {
                let text = text_of(&content);
                if text.trim().is_empty() {
                    let why = "the model ended with an empty reply";
                    break end(EndState::NeedsHuman, why.into(), turns, &usage, None);
                }
                let state = marker(&text).unwrap_or(EndState::Done);
                break end(state, text, turns, &usage, None);
            }
            _ => {
                let text = text_of(&content);
                let why = format!(
                    "the model stopped with stop_reason {stop:?} and {} tool call(s); nothing was run: {text}",
                    calls.len()
                );
                break end(EndState::NeedsHuman, why, turns, &usage, None);
            }
        }
        let mut results = vec![];
        let mut finished = None;
        for c in &calls {
            let name = c["name"].as_str().unwrap_or("");
            let started = Instant::now();
            let r = match mcp {
                Some(m) if m.offers(name) => m.call(name, &c["input"]),
                _ => tools::call(&cfg.workspace, cfg.command_timeout, name, &c["input"]),
            };
            let mut e = json!({
                "event": "tool",
                "turn": turns,
                "gen_ai.tool.name": name,
                "is_error": r.is_error,
                "duration_ms": ms(started),
            });
            if name == "run" {
                // Only the first line: a heredoc body is file contents.
                let cmd = c["input"]["command"].as_str().unwrap_or("");
                let first = cmd.lines().next().unwrap_or("");
                e["command"] = json!(api::head(first, 200));
                e["command_bytes"] = json!(cmd.len());
            }
            event(trace, e);
            if r.finish.is_some() {
                finished = r.finish.clone();
            }
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": c["id"],
                "content": r.text,
                "is_error": r.is_error,
            }));
        }
        if let Some((state, summary)) = finished {
            break end(state, summary, turns, &usage, None);
        }
        messages.push(json!({"role": "user", "content": results}));
    };
    event(
        trace,
        json!({"event": "end", "end_state": outcome.end_state, "turns": outcome.turns, "error": outcome.error}),
    );
    outcome
}

/// Caches the tools and system prompt, and the conversation up to its last
/// block. The conversation itself is never changed here.
fn request(cfg: &Config, mcp: Option<&mcp::Mcp>, messages: &[Value]) -> Value {
    let mut msgs = messages.to_vec();
    if let Some(last) = msgs.last_mut() {
        if let Some(s) = last["content"].as_str() {
            last["content"] = json!([{"type": "text", "text": s}]);
        }
        if let Some(b) = last["content"].as_array_mut().and_then(|a| a.last_mut()) {
            b["cache_control"] = json!({"type": "ephemeral"});
        }
    }
    json!({
        "model": cfg.model,
        "max_tokens": cfg.max_tokens,
        "system": [{"type": "text", "text": cfg.system, "cache_control": {"type": "ephemeral"}}],
        "tools": all_tools(mcp),
        "messages": msgs,
    })
}

fn all_tools(mcp: Option<&mcp::Mcp>) -> Value {
    let mut all = tools::definitions();
    if let (Some(m), Some(list)) = (mcp, all.as_array_mut()) {
        list.extend(m.definitions().iter().cloned());
    }
    all
}

fn text_of(content: &[Value]) -> String {
    content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mirrors the marker rule in kod-eval's runner (kod-loop cannot depend on
/// kod-eval). The eval prompt asks for these on the last line; the same rule as
/// kod-eval's other runners.
fn marker(text: &str) -> Option<EndState> {
    match text.lines().rev().find(|l| !l.trim().is_empty())?.trim() {
        "KOD_END_STATE: blocked" => Some(EndState::Blocked),
        "KOD_END_STATE: needs_human" => Some(EndState::NeedsHuman),
        _ => None,
    }
}

fn ms(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

fn event(trace: &mut dyn Write, mut e: Value) {
    e["ts_ms"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    );
    let _ = writeln!(trace, "{e}");
}
