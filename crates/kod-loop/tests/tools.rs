use kod_loop::{
    EndState,
    tools::{self, OUTPUT_CAP, ToolResult},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn call(ws: &Path, name: &str, input: Value) -> ToolResult {
    tools::call(ws, Duration::from_secs(30), name, &input)
}

#[test]
fn definitions_list_the_five_tools_in_order() {
    let names: Vec<String> = tools::definitions()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        ["read_file", "write_file", "edit_file", "run", "finish"]
    );
}

#[test]
fn write_then_read_numbers_lines() {
    let d = tempfile::tempdir().unwrap();
    let w = call(
        d.path(),
        "write_file",
        json!({"path": "src/a.txt", "content": "one\ntwo\nthree\n"}),
    );
    assert!(!w.is_error, "{}", w.text);
    let r = call(
        d.path(),
        "read_file",
        json!({"path": "src/a.txt", "offset": 2, "limit": 1}),
    );
    assert_eq!(r.text, "     2\ttwo\n");
    let all = call(d.path(), "read_file", json!({"path": "src/a.txt"}));
    assert_eq!(all.text, "     1\tone\n     2\ttwo\n     3\tthree\n");
}

#[test]
fn edit_replaces_exactly_one_match() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    fs::write(&f, "a b a").unwrap();
    let e = |old: &str, new: &str| {
        call(
            d.path(),
            "edit_file",
            json!({"path": "a.txt", "old_string": old, "new_string": new}),
        )
    };
    assert!(!e("b", "c").is_error);
    assert_eq!(fs::read_to_string(&f).unwrap(), "a c a");
    for (old, why) in [("a", "more than once"), ("zzz", "not found"), ("", "empty")] {
        let r = e(old, "x");
        assert!(r.is_error && r.text.contains(why), "{old:?}: {}", r.text);
    }
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        "a c a",
        "errors change nothing"
    );
    // Overlapping matches are ambiguous too.
    fs::write(&f, "aaa").unwrap();
    assert!(e("aa", "b").is_error);
}

#[test]
fn read_errors_are_tool_errors() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("bin"), [0xff, 0xfe, 0x00]).unwrap();
    fs::create_dir(d.path().join("dir")).unwrap();
    for p in ["missing.txt", "bin", "dir"] {
        let r = call(d.path(), "read_file", json!({"path": p}));
        assert!(r.is_error, "{p}: {}", r.text);
    }
}

#[test]
fn malformed_and_unknown_calls_are_errors_not_panics() {
    let d = tempfile::tempdir().unwrap();
    for (name, input) in [
        ("read_file", json!({})),
        ("read_file", json!({"path": 7})),
        ("write_file", json!({"path": "x"})),
        ("edit_file", json!(null)),
        ("edit_file", json!("a string")),
        ("run", json!({"cmd": "ls"})),
        ("finish", json!({"state": "over_budget", "summary": "x"})),
        ("finish", json!({"state": "done"})),
        ("finish", json!([])),
        ("rm_rf", json!({})),
        ("", json!({})),
    ] {
        let r = call(d.path(), name, input.clone());
        assert!(r.is_error, "{name} {input}: {}", r.text);
        assert_eq!(r.finish, None, "{name} {input}");
    }
}

#[test]
fn finish_reports_the_state() {
    let d = tempfile::tempdir().unwrap();
    for (s, want) in [
        ("done", EndState::Done),
        ("blocked", EndState::Blocked),
        ("needs_human", EndState::NeedsHuman),
    ] {
        let r = call(d.path(), "finish", json!({"state": s, "summary": "why"}));
        assert!(!r.is_error);
        assert_eq!(r.finish, Some((want, "why".to_owned())));
    }
}

#[test]
fn run_reports_output_and_exit_code_in_the_workspace() {
    let d = tempfile::tempdir().unwrap();
    let r = call(
        d.path(),
        "run",
        json!({"command": "touch made.txt; echo out; echo err >&2; exit 3"}),
    );
    assert!(!r.is_error);
    assert!(
        r.text.contains("out") && r.text.contains("err"),
        "{}",
        r.text
    );
    assert!(r.text.ends_with("[exit code 3]"), "{}", r.text);
    assert!(d.path().join("made.txt").exists());
}

#[test]
fn stdin_is_closed() {
    let d = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let r = call(d.path(), "run", json!({"command": "cat"}));
    assert!(r.text.ends_with("[exit code 0]"), "{}", r.text);
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn runaway_command_is_killed_with_its_children() {
    let d = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let r = tools::call(
        d.path(),
        Duration::from_secs(1),
        "run",
        &json!({"command": "sleep 300 & echo $! > child.pid; sh -c 'sleep 300 & echo $! > grandchild.pid; wait' & sleep 300"}),
    );
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(r.text.contains("[killed after 1s"), "{}", r.text);
    assert!(!r.text.contains("may still be running"), "{}", r.text);
    assert_group_dead(d.path(), &["child.pid", "grandchild.pid"]);
}

/// Waits up to 5 s for each recorded pid to be gone. CI runs this on
/// ubuntu-24.04, whose /bin/sh is dash like the bookworm sandbox's.
fn assert_group_dead(dir: &std::path::Path, files: &[&str]) {
    for f in files {
        let pid = fs::read_to_string(dir.join(f)).unwrap();
        let alive = || {
            Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "{f}: a process in the group survived");
    }
}

#[test]
fn background_children_are_killed_when_the_command_exits() {
    let d = tempfile::tempdir().unwrap();
    let r = call(
        d.path(),
        "run",
        json!({"command": "sleep 300 > /dev/null 2>&1 & echo $! > child.pid; (sleep 300 > /dev/null 2>&1 & echo $! > grandchild.pid)"}),
    );
    assert!(r.text.ends_with("[exit code 0]"), "{}", r.text);
    assert_group_dead(d.path(), &["child.pid", "grandchild.pid"]);
}

#[test]
fn output_flood_is_capped_head_and_tail() {
    let d = tempfile::tempdir().unwrap();
    let r = call(
        d.path(),
        "run",
        json!({"command": "echo FIRST; yes x | head -c 5000000; echo; echo LAST"}),
    );
    assert!(r.text.len() < OUTPUT_CAP + 200, "{}", r.text.len());
    assert!(r.text.starts_with("FIRST"), "{}", &r.text[..50]);
    assert!(r.text.contains("bytes omitted"));
    assert!(r.text.contains("LAST\n[exit code 0]"));
}

#[test]
fn endless_output_is_killed_before_the_timeout() {
    let d = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let r = call(d.path(), "run", json!({"command": "yes"}));
    assert!(start.elapsed() < Duration::from_secs(20));
    assert!(
        r.text.contains("output limit was exceeded"),
        "{}",
        r.text.len()
    );
    assert!(r.text.len() < OUTPUT_CAP + 200, "{}", r.text.len());
}
