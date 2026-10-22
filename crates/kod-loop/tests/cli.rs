use kod_loop::{EndState, Outcome};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

fn kod_loop(env: &[(&str, &str)], stdin: &str, args: &[&str]) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kod-loop"))
        .args(args)
        .env_remove("ANTHROPIC_BASE_URL")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // It may exit before reading stdin.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

#[test]
fn setup_errors_still_print_one_outcome_line() {
    let sys = tempfile::NamedTempFile::new().unwrap();
    fs::write(sys.path(), "SYSTEM").unwrap();
    let s = sys.path().to_str().unwrap();
    let url = ("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
    let args = ["--model", "m", "--max-turns", "3", "--system-prompt", s];
    let missing = [
        "--model",
        "m",
        "--max-turns",
        "3",
        "--system-prompt",
        "/nonexistent/system.md",
    ];
    for (env, stdin, args, why) in [
        (vec![], "ticket", &args, "ANTHROPIC_BASE_URL"),
        (vec![url], "  \n", &args, "prompt"),
        (vec![url], "ticket", &missing, "system.md"),
    ] {
        let (code, out) = kod_loop(&env, stdin, args);
        assert_eq!(code, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        let o: Outcome = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(o.end_state, EndState::NeedsHuman);
        let e = o.error.unwrap();
        assert!(e.contains(why), "{e}");
    }
}

#[test]
fn version_names_the_binary() {
    let out = Command::new(env!("CARGO_BIN_EXE_kod-loop"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("kod-loop ")
    );
}

#[cfg(unix)]
#[test]
fn a_non_unicode_mcp_url_is_a_failed_run() {
    use std::os::unix::ffi::OsStrExt;
    let sys = tempfile::NamedTempFile::new().unwrap();
    fs::write(sys.path(), "SYSTEM").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kod-loop"))
        .args(["--model", "m", "--max-turns", "3", "--system-prompt"])
        .arg(sys.path())
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1")
        .env("KOD_MCP_URL", std::ffi::OsStr::from_bytes(b"http://\xff"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(b"ticket");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let o: Outcome = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(o.end_state, EndState::NeedsHuman);
    assert!(o.error.unwrap().contains("KOD_MCP_URL"));
}
