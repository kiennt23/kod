mod common;
use kod_eval::runner::Runner;
use kod_eval::sandbox::{Mounts, image, run_args};
use std::path::Path;

#[test]
fn agent_container_is_locked_down() {
    let c = common::case("PAY-1");
    let m = Mounts {
        read_only: &[(Path::new("/r/work"), "/seed"), (Path::new("/r/in"), "/in")],
        out: Path::new("/r/out"),
        tmpfs: &["/work", "/tmp"],
    };
    let a = run_args(
        "kod-run-1-PAY-1",
        "run-1",
        &c,
        "kod-run-1",
        &m,
        (1000, 1000),
        &[],
    )
    .join(" ");
    for want in [
        "run --rm --name kod-run-1-PAY-1",
        "--label kod.run=run-1",
        "--network kod-run-1",
        "--workdir /work",
        "--memory 6g --memory-swap 6g",
        "--cpus 2",
        "--pids-limit 512",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "-e KOD_HOST_UID=1000",
        "-v /r/work:/seed:ro",
        "-v /r/out:/out",
        "-v /r/in:/in:ro",
        "--read-only",
        "--tmpfs /work:size=2g,exec",
        "--tmpfs /tmp:size=2g,exec",
        "--tmpfs /home/agent:size=256m,exec",
        "-e KOD_DISK_BYTES=2147483648",
        "-e KOD_LOG_CAP=16777216",
    ] {
        assert!(a.contains(want), "missing {want:?} in {a}");
    }
    assert!(!a.contains("--privileged"));
    // The agent's writable workspace is a capped tmpfs, never a host directory.
    assert!(!a.contains(":/work"), "{a}");
}

#[test]
fn score_container_sees_the_workspace_read_only() {
    let c = common::case("PAY-1");
    let m = Mounts {
        read_only: &[(Path::new("/r/w"), "/work")],
        out: Path::new("/r/o"),
        tmpfs: &["/tmp"],
    };
    let a = run_args("n", "run-1", &c, "none", &m, (1, 1), &[]).join(" ");
    for want in [
        "--network none",
        "--workdir /work",
        "-v /r/w:/work:ro",
        "--read-only",
        "--tmpfs /tmp:size=2g,exec",
    ] {
        assert!(a.contains(want), "missing {want:?} in {a}");
    }
    assert!(!a.contains("/in"));
    assert!(
        !a.contains(":/tmp"),
        "scratch is a tmpfs, not a host directory: {a}"
    );
}

/// Collect extracts agent-made archives, so it writes only to capped tmpfs.
#[test]
fn collect_container_writes_only_to_capped_tmpfs() {
    let c = common::case("PAY-1");
    let m = Mounts {
        read_only: &[(Path::new("/r/b"), "/in"), (Path::new("/r/a"), "/agent")],
        out: Path::new("/r/o"),
        tmpfs: &["/tmp", "/base", "/wt"],
    };
    let a = run_args("n", "run-1", &c, "none", &m, (1, 1), &[]).join(" ");
    for want in [
        "--network none",
        "-v /r/b:/in:ro",
        "-v /r/a:/agent:ro",
        "--read-only",
        "--tmpfs /tmp:size=2g,exec",
        "--tmpfs /base:size=2g,exec",
        "--tmpfs /wt:size=2g,exec",
        "-e KOD_DISK_BYTES=2147483648",
    ] {
        assert!(a.contains(want), "missing {want:?} in {a}");
    }
}

#[test]
fn image_is_per_language() {
    assert_eq!(
        image(&common::case("PAY-1"), "kod-sandbox"),
        "kod-sandbox-rust"
    );
}

/// The model key lives only in the proxy. Nothing in the agent, collect or
/// score arguments may name it, the way `-e KOD_API_KEY` would pass it through.
#[test]
fn no_sandbox_container_gets_the_model_key() {
    let c = common::case("PAY-1");
    let runner = Runner::ClaudeCode {
        model: "claude-test-1".into(),
    };
    let m = |read_only, tmpfs| Mounts {
        read_only,
        out: Path::new("/r/o"),
        tmpfs,
    };
    let agent_ro = [(Path::new("/r/w"), "/seed"), (Path::new("/r/in"), "/in")];
    let collect_ro = [(Path::new("/r/b"), "/in"), (Path::new("/r/o"), "/agent")];
    let score_ro = [(Path::new("/r/w"), "/work")];
    let agent = [
        run_args(
            "a",
            "r",
            &c,
            "net",
            &m(&agent_ro, &["/work", "/tmp"]),
            (1, 1),
            &runner.env(),
        ),
        runner.command(&c),
    ]
    .concat();
    let collect = run_args(
        "c",
        "r",
        &c,
        "none",
        &m(&collect_ro, &["/tmp", "/base", "/wt"]),
        (1, 1),
        &[],
    );
    let score_env = [("KOD_TEST_COMMAND".to_owned(), c.test_command.clone())];
    let score = run_args(
        "s",
        "r",
        &c,
        "none",
        &m(&score_ro, &["/tmp"]),
        (1, 1),
        &score_env,
    );
    for args in [agent, collect, score] {
        let a = args.join(" ");
        assert!(!a.contains("KOD_API_KEY"), "{a}");
    }
    assert!(
        runner
            .env()
            .contains(&("ANTHROPIC_API_KEY".into(), "sk-kod-dummy".into())),
        "the agent gets a dummy key"
    );
}
