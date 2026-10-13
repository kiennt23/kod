mod common;
use kod_eval::runner::Runner;
use kod_eval::sandbox::{Mounts, image, run_args};
use std::path::Path;

#[test]
fn agent_container_is_locked_down() {
    let c = common::case("PAY-1");
    let m = Mounts {
        work: Path::new("/r/work"),
        work_read_only: false,
        out: Path::new("/r/out"),
        input: Some(Path::new("/r/in")),
        scratch: None,
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
        "--memory 4g --memory-swap 4g",
        "--cpus 2",
        "--pids-limit 512",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "-e KOD_HOST_UID=1000",
        "-v /r/work:/work ",
        "-v /r/out:/out",
        "-v /r/in:/in:ro",
    ] {
        assert!(a.contains(want), "missing {want:?} in {a}");
    }
    assert!(!a.contains("--privileged"));
}

#[test]
fn score_container_sees_the_workspace_read_only() {
    let c = common::case("PAY-1");
    let m = Mounts {
        work: Path::new("/r/w"),
        work_read_only: true,
        out: Path::new("/r/o"),
        input: None,
        scratch: Some(Path::new("/r/s")),
    };
    let a = run_args("n", "run-1", &c, "none", &m, (1, 1), &[]).join(" ");
    for want in [
        "--network none",
        "--workdir /work",
        "-v /r/w:/work:ro",
        "-v /r/s:/tmp",
    ] {
        assert!(a.contains(want), "missing {want:?} in {a}");
    }
    assert!(!a.contains("/in"));
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
    let m = |work_read_only, input, scratch| Mounts {
        work: Path::new("/r/w"),
        work_read_only,
        out: Path::new("/r/o"),
        input,
        scratch,
    };
    let agent = [
        run_args(
            "a",
            "r",
            &c,
            "net",
            &m(false, Some(Path::new("/r/in")), None),
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
        &m(true, Some(Path::new("/r/b")), None),
        (1, 1),
        &[],
    );
    let score_env = [("KOD_TEST_COMMAND".to_owned(), c.test_command.clone())];
    let score = run_args(
        "s",
        "r",
        &c,
        "none",
        &m(true, None, Some(Path::new("/r/s"))),
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
