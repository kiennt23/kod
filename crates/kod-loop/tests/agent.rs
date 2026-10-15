mod common;
use common::*;
use kod_loop::{CUT_OFF, CUT_OFF_CALL, Config, EndState, context::ELIDED};
use serde_json::{Value, json};
use std::fs;

#[test]
fn edits_a_file_then_finishes() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        msg(
            json!([
                text("Writing it."),
                tool(
                    "t1",
                    "write_file",
                    json!({"path": "a.txt", "content": "hi\n"})
                )
            ]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t2",
                "finish",
                json!({"state": "done", "summary": "wrote a.txt"})
            )]),
            "tool_use",
        ),
    ]);
    let (o, trace) = go(&config(&url, ws.path()), "the ticket");
    assert_eq!(
        (o.end_state, o.summary.as_str(), o.turns, o.error.clone()),
        (EndState::Done, "wrote a.txt", 2, None)
    );
    assert_eq!(
        (
            o.usage.input_tokens,
            o.usage.output_tokens,
            o.usage.cache_read_input_tokens
        ),
        (20, 10, 6)
    );
    assert_eq!(fs::read_to_string(ws.path().join("a.txt")).unwrap(), "hi\n");

    let seen = &m.lock().unwrap().seen;
    let (headers, first) = &seen[0];
    // The gateway holds the key; the loop never sends one.
    assert!(headers.get("x-api-key").is_none() && headers.get("authorization").is_none());
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(first["model"], "m");
    assert_eq!(first["max_tokens"], 1000);
    assert_eq!(first["system"][0]["text"], "SYSTEM");
    assert_eq!(first["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(first["tools"].as_array().unwrap().len(), 5);
    assert_eq!(first["messages"][0]["content"][0]["text"], "the ticket");
    let second = &seen[1].1;
    assert_eq!(
        second["messages"][0]["content"], "the ticket",
        "only the last message carries cache_control"
    );
    let r = &second["messages"][2]["content"][0];
    assert_eq!(r["type"], "tool_result");
    assert_eq!(r["tool_use_id"], "t1");
    assert_eq!(r["is_error"], false);
    assert_eq!(r["cache_control"]["type"], "ephemeral");

    let events: Vec<&str> = trace.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["model_call", "tool", "model_call", "tool", "end"]);
    assert_eq!(trace[0]["gen_ai.request.model"], "m");
    assert_eq!(trace[0]["gen_ai.usage.input_tokens"], 10);
    assert_eq!(trace[1]["gen_ai.tool.name"], "write_file");
    assert_eq!(trace[4]["end_state"], "done");
    assert!(
        !trace.iter().any(|e| e.to_string().contains("hi\\n")),
        "no file contents in the trace"
    );
}

#[test]
fn a_reply_without_tool_calls_ends_the_run() {
    for (reply, want) in [
        (json!([text("All done.")]), EndState::Done),
        (
            json!([text("Cannot do this.\nKOD_END_STATE: blocked")]),
            EndState::Blocked,
        ),
        (
            json!([text("KOD_END_STATE: needs_human\n")]),
            EndState::NeedsHuman,
        ),
        (json!([]), EndState::NeedsHuman),
        (json!([text("  ")]), EndState::NeedsHuman),
    ] {
        let ws = tempfile::tempdir().unwrap();
        let (url, _m) = model(vec![msg(reply.clone(), "end_turn")]);
        let (o, _) = go(&config(&url, ws.path()), "t");
        assert_eq!(o.end_state, want, "{reply}");
        assert_eq!(o.error, None, "{reply}");
    }
}

#[test]
fn the_turn_limit_ends_over_budget() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(
        (0..5)
            .map(|i| {
                msg(
                    json!([tool(&format!("t{i}"), "run", json!({"command": "true"}))]),
                    "tool_use",
                )
            })
            .collect(),
    );
    let cfg = Config {
        max_turns: 3,
        ..config(&url, ws.path())
    };
    let (o, _) = go(&cfg, "t");
    assert_eq!(
        (o.end_state, o.turns, o.error),
        (EndState::OverBudget, 3, None)
    );
    assert_eq!(bodies(&m).len(), 3);
}

#[test]
fn a_reply_cut_at_max_tokens_is_continued_not_taken_as_done() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        msg(json!([text("The plan: first I will")]), "max_tokens"),
        msg(
            json!([tool(
                "t1",
                "finish",
                json!({"state": "done", "summary": "ok"})
            )]),
            "tool_use",
        ),
    ]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.turns), (EndState::Done, 2));
    let b = bodies(&m);
    let last = b[1]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["text"], CUT_OFF);
}

#[test]
fn bad_tool_calls_get_error_results_and_the_loop_goes_on() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        msg(
            json!([
                tool("t1", "rm_rf", json!({})),
                tool("t2", "read_file", json!({"path": 7})),
                tool(
                    "t3",
                    "finish",
                    json!({"state": "over_budget", "summary": "x"})
                )
            ]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t4",
                "finish",
                json!({"state": "blocked", "summary": "gave up"})
            )]),
            "tool_use",
        ),
    ]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.turns), (EndState::Blocked, 2));
    let b = bodies(&m);
    let results = b[1]["messages"][2]["content"].as_array().unwrap().clone();
    assert_eq!(results.len(), 3);
    for (r, id) in results.iter().zip(["t1", "t2", "t3"]) {
        assert_eq!(r["tool_use_id"], id);
        assert_eq!(r["is_error"], true, "{r}");
    }
}

#[test]
fn finish_takes_effect_after_the_other_calls_in_its_reply() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![msg(
        json!([
            tool("t1", "finish", json!({"state": "done", "summary": "ok"})),
            tool("t2", "write_file", json!({"path": "b.txt", "content": "x"}))
        ]),
        "tool_use",
    )]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::Done);
    assert!(ws.path().join("b.txt").exists());
    assert_eq!(bodies(&m).len(), 1);
}

#[test]
fn the_gateway_budget_refusal_ends_over_budget_without_retry() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![(429, Value::Null)]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.error), (EndState::OverBudget, None));
    assert_eq!(bodies(&m).len(), 1);
}

#[test]
fn transient_errors_are_retried() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        (529, error_body("overloaded_error")),
        (429, error_body("rate_limit_error")),
        (500, Value::Null),
        msg(json!([text("done")]), "end_turn"),
    ]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.turns), (EndState::Done, 4), "{:?}", o.error);
    assert_eq!(bodies(&m).len(), 4);
}

#[test]
fn errors_that_outlast_the_retries_need_a_human() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![(503, Value::Null); 5]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("503"));
    assert_eq!(bodies(&m).len(), 4, "one try and three retries");
}

#[test]
fn refusals_are_not_retried() {
    for status in [400, 403, 404, 413] {
        let ws = tempfile::tempdir().unwrap();
        let (url, m) = model(vec![
            (status, Value::Null),
            msg(json!([text("done")]), "end_turn"),
        ]);
        let (o, _) = go(&config(&url, ws.path()), "t");
        assert_eq!(o.end_state, EndState::NeedsHuman, "{status}");
        assert!(o.error.unwrap().contains(&status.to_string()));
        assert_eq!(bodies(&m).len(), 1, "{status}");
    }
}

#[test]
fn a_success_that_is_not_a_message_needs_a_human() {
    let ws = tempfile::tempdir().unwrap();
    let (url, _m) = model(vec![(200, json!({"hello": 1}))]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("not a message"));
}

#[test]
fn a_tool_call_cut_at_max_tokens_is_not_run() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        msg(
            json!([tool(
                "t1",
                "write_file",
                json!({"path": "half.txt", "content": "first ha"})
            )]),
            "max_tokens",
        ),
        msg(
            json!([tool(
                "t2",
                "finish",
                json!({"state": "done", "summary": "ok"})
            )]),
            "tool_use",
        ),
    ]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.turns), (EndState::Done, 2));
    assert!(
        !ws.path().join("half.txt").exists(),
        "a cut-off call never runs"
    );
    let b = bodies(&m);
    let r = &b[1]["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(r["tool_use_id"], "t1");
    assert_eq!(r["is_error"], true);
    assert_eq!(r["content"], CUT_OFF_CALL);
}

#[test]
fn only_end_turn_or_stop_sequence_can_end_done() {
    for stop in ["refusal", "model_context_window_exceeded", "something_new"] {
        let ws = tempfile::tempdir().unwrap();
        let (url, _m) = model(vec![msg(json!([text("I can't help with that.")]), stop)]);
        let (o, _) = go(&config(&url, ws.path()), "t");
        assert_eq!(o.end_state, EndState::NeedsHuman, "{stop}");
        assert!(o.summary.contains(stop), "{}", o.summary);
    }
    // Tool calls under any stop other than tool_use never run, and a finish
    // among them can't end the run done.
    for stop in [
        "refusal",
        "model_context_window_exceeded",
        "something_new",
        "end_turn",
    ] {
        let ws = tempfile::tempdir().unwrap();
        let (url, m) = model(vec![msg(
            json!([
                tool("t1", "write_file", json!({"path": "w.txt", "content": "x"})),
                tool("t2", "finish", json!({"state": "done", "summary": "ok"}))
            ]),
            stop,
        )]);
        let (o, _) = go(&config(&url, ws.path()), "t");
        assert_eq!(o.end_state, EndState::NeedsHuman, "{stop}");
        assert!(!ws.path().join("w.txt").exists(), "{stop}: a tool ran");
        assert_eq!(bodies(&m).len(), 1, "{stop}");
    }
    let ws = tempfile::tempdir().unwrap();
    let (url, _m) = model(vec![msg(json!([text("Done.")]), "stop_sequence")]);
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::Done);
}

#[test]
fn a_retry_uses_a_turn() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        (503, Value::Null),
        msg(json!([text("done")]), "end_turn"),
    ]);
    let cfg = Config {
        max_turns: 1,
        ..config(&url, ws.path())
    };
    let (o, _) = go(&cfg, "t");
    assert_eq!((o.end_state, o.turns), (EndState::OverBudget, 1));
    assert_eq!(bodies(&m).len(), 1, "no request beyond the turn limit");
}

#[test]
fn retry_after_is_honoured_when_longer_than_the_backoff() {
    let ws = tempfile::tempdir().unwrap();
    let mut busy = error_body("rate_limit_error");
    busy["retry_after"] = json!(1);
    let (url, m) = model(vec![(429, busy), msg(json!([text("done")]), "end_turn")]);
    let start = std::time::Instant::now();
    // config() sets every backoff to zero.
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!(o.end_state, EndState::Done, "{:?}", o.error);
    assert!(start.elapsed() >= std::time::Duration::from_secs(1));
    assert_eq!(bodies(&m).len(), 2);
}

#[test]
fn the_trace_keeps_only_the_start_of_a_command() {
    let ws = tempfile::tempdir().unwrap();
    let long = format!("echo {}", "s".repeat(1000));
    let (url, _m) = model(vec![
        msg(
            json!([tool("t1", "run", json!({"command": long}))]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t2",
                "finish",
                json!({"state": "done", "summary": "ok"})
            )]),
            "tool_use",
        ),
    ]);
    let (_, trace) = go(&config(&url, ws.path()), "t");
    assert_eq!(trace[1]["command"].as_str().unwrap().chars().count(), 200);
    assert_eq!(trace[1]["command_bytes"], long.len());
}

#[test]
fn an_unreachable_gateway_needs_a_human() {
    let ws = tempfile::tempdir().unwrap();
    let (o, trace) = go(&config("http://127.0.0.1:1", ws.path()), "t");
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.is_some());
    assert_eq!(trace.last().unwrap()["event"], "end");
}

#[test]
fn old_tool_results_are_elided_once_the_context_is_large() {
    let ws = tempfile::tempdir().unwrap();
    let (url, m) = model(vec![
        msg(
            json!([tool("t1", "run", json!({"command": "printf '%020000d' 0"}))]),
            "tool_use",
        ),
        msg(
            json!([tool("t2", "run", json!({"command": "true"}))]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t3",
                "finish",
                json!({"state": "done", "summary": "ok"})
            )]),
            "tool_use",
        ),
    ]);
    let cfg = Config {
        context_bytes: 5000,
        ..config(&url, ws.path())
    };
    let (o, _) = go(&cfg, "t");
    assert_eq!(o.end_state, EndState::Done);
    let b = bodies(&m);
    let big = b[1]["messages"][2]["content"][0]["content"]
        .as_str()
        .unwrap();
    assert!(big.len() > 20_000, "the newest result is sent whole");
    assert_eq!(b[2]["messages"][2]["content"][0]["content"], ELIDED);
}

#[test]
fn the_trace_cuts_a_command_at_its_first_line() {
    let ws = tempfile::tempdir().unwrap();
    let cmd = "cat > f.txt <<'EOF'\nSECRET-BODY\nEOF";
    let (url, _m) = model(vec![
        msg(
            json!([tool("t1", "run", json!({"command": cmd}))]),
            "tool_use",
        ),
        msg(
            json!([tool(
                "t2",
                "finish",
                json!({"state": "done", "summary": "ok"})
            )]),
            "tool_use",
        ),
    ]);
    let (_, trace) = go(&config(&url, ws.path()), "t");
    assert_eq!(trace[1]["command"], "cat > f.txt <<'EOF'");
    assert_eq!(trace[1]["command_bytes"], cmd.len());
    assert!(!trace.iter().any(|e| e.to_string().contains("SECRET-BODY")));
}

#[test]
fn a_failed_body_read_is_retried_not_taken_as_budget() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let good = msg(json!([text("done")]), "end_turn").1.to_string();
    std::thread::spawn(move || {
        for i in 0..2 {
            let (mut s, _) = l.accept().unwrap();
            // Read the request head and body in one go (it is small).
            let mut buf = [0; 65536];
            let _ = s.read(&mut buf);
            let reply = if i == 0 {
                // Declares 100 bytes, sends 5, then closes: the body read fails.
                "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 100\r\n\r\nshort".to_string()
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{good}",
                    good.len()
                )
            };
            let _ = s.write_all(reply.as_bytes());
        }
    });
    let ws = tempfile::tempdir().unwrap();
    let (o, _) = go(&config(&url, ws.path()), "t");
    assert_eq!((o.end_state, o.turns), (EndState::Done, 2), "{:?}", o.error);
}
