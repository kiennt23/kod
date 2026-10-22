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
fn claude_env_points_at_the_gateway_with_a_dummy_key() {
    let base = format!("{PROXY_URL}/r/t1");
    let env = claude().env(&base);
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("ANTHROPIC_BASE_URL"), Some(base.as_str()));
    assert_eq!(get("ANTHROPIC_API_KEY"), Some("sk-kod-dummy"));
    assert_eq!(get("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS"), Some("1"));
}

fn kod_loop() -> Runner {
    Runner::KodLoop {
        model: "claude-sonnet-5-5".into(),
        max_tokens: 4096,
    }
}

#[test]
fn kod_loop_command_and_env() {
    let r = kod_loop();
    assert_eq!((r.name(), r.model()), ("kod-loop", "claude-sonnet-5-5"));
    assert_eq!(
        r.command(&common::case("c1")),
        [
            "kod-loop",
            "--model",
            "claude-sonnet-5-5",
            "--max-turns",
            "40",
            "--max-tokens",
            "4096"
        ]
    );
    // The base URL only: no key, not even a dummy one. A script gets the
    // same, so its model calls meet the same gateway budget.
    let base = format!("{PROXY_URL}/r/t1");
    let script = Runner::Script {
        command: "true".into(),
    };
    for r in [r, script] {
        assert_eq!(
            r.env(&base),
            vec![("ANTHROPIC_BASE_URL".to_owned(), base.clone())]
        );
    }
}

#[test]
fn kod_loop_end_state_reads_the_outcome_line() {
    use kod_loop::EndState as Loop;
    for s in [
        Loop::Done,
        Loop::Blocked,
        Loop::NeedsHuman,
        Loop::OverBudget,
    ] {
        let line = serde_json::to_string(&kod_loop::Outcome {
            end_state: s,
            summary: "x".into(),
            turns: 1,
            usage: kod_loop::Usage::default(),
            error: None,
        })
        .unwrap();
        let (got, cost) = kod_loop().end_state(0, &line);
        assert_eq!(
            serde_json::to_value(got).unwrap(),
            serde_json::to_value(s).unwrap(),
            "{line}"
        );
        assert_eq!(cost, None);
    }
}

#[test]
fn kod_loop_garbled_or_failed_output_needs_a_human() {
    let done = r#"{"end_state":"done","summary":"","turns":1,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"error":null}"#;
    let twice = format!("{done}\n{done}");
    for (code, out) in [
        (0, ""),
        (0, "not json"),
        (0, twice.as_str()),
        (0, r#"{"end_state":"finished"}"#),
        (0, r#"{"end_state":"done""#),
        (0, "[]"),
        (1, done),
        (137, done),
    ] {
        assert_eq!(
            kod_loop().end_state(code, out).0,
            EndState::NeedsHuman,
            "{code} {out}"
        );
    }
}
