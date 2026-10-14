mod common;
use kod_eval::{
    case::EndState,
    runner::{PROXY_URL, Runner},
};

fn claude() -> Runner {
    Runner::ClaudeCode {
        model: "claude-sonnet-5-5".into(),
    }
}

#[test]
fn claude_success_is_done_with_cost() {
    let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"Added the flag.","total_cost_usd":0.42}"#;
    assert_eq!(claude().end_state(0, out), (EndState::Done, Some(0.42)));
}

#[test]
fn claude_max_turns_is_over_budget() {
    let out =
        r#"{"type":"result","subtype":"error_max_turns","is_error":true,"total_cost_usd":1.5}"#;
    assert_eq!(claude().end_state(1, out).0, EndState::OverBudget);
}

#[test]
fn claude_blocked_marker_is_blocked() {
    let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"The ticket contradicts the API.\nKOD_END_STATE: blocked"}"#;
    assert_eq!(claude().end_state(0, out).0, EndState::Blocked);
}

#[test]
fn claude_errors_and_unreadable_output_need_a_human() {
    assert_eq!(claude().end_state(1, "panic").0, EndState::NeedsHuman);
    let out = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
    assert_eq!(claude().end_state(1, out).0, EndState::NeedsHuman);
}

#[test]
fn script_end_states() {
    let s = Runner::Script {
        command: "true".into(),
    };
    assert_eq!(s.end_state(0, "").0, EndState::Done);
    assert_eq!(s.end_state(1, "").0, EndState::NeedsHuman);
    assert_eq!(
        s.end_state(0, "x\nKOD_END_STATE: needs_human\n").0,
        EndState::NeedsHuman
    );
}

#[test]
fn claude_command_pins_model_and_turns() {
    let cmd = claude().command(&common::case("PAY-1")).join(" ");
    assert!(cmd.starts_with("claude -p --output-format json"));
    assert!(cmd.contains("--max-turns 40"));
    assert!(cmd.contains("--model claude-sonnet-5-5"));
}

#[test]
fn claude_env_points_at_the_proxy_with_a_dummy_key() {
    let env = claude().env();
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("ANTHROPIC_BASE_URL"), Some(PROXY_URL));
    assert_eq!(get("ANTHROPIC_API_KEY"), Some("sk-kod-dummy"));
}
