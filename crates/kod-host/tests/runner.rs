use kod_eval::connected::SpawnGate;
use std::process::Command;
#[test]
fn closed_spawn_gate_starts_nothing() {
    let gate = SpawnGate::default();
    gate.close();
    assert!(
        gate.spawn(Command::new("sh").args(["-c", "exit 0"]), || Ok(false))
            .is_err()
    );
    let gate = SpawnGate::default();
    assert!(
        gate.spawn(Command::new("sh").args(["-c", "exit 0"]), || Ok(true))
            .is_err()
    );
}
#[test]
fn nonzero_container_exit_cannot_collect() {
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    fs::write(
        &docker,
        "#!/bin/sh\ncase $1 in run) exit 7;; ps) exit 0;; *) exit 9;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    kod_eval::connected::with_docker(&docker, || {
        let exit = kod_eval::connected::launch(
            &["run".into()],
            "kod-agent-run-1",
            Duration::from_secs(1),
            &SpawnGate::default(),
            &|| Ok(false),
        )
        .unwrap();
        assert!(!exit.success);
        assert!(kod_host::runner::ready_to_collect(exit.success, Some("0"), Some("0")).is_err());
    });
}
#[test]
fn owned_stop_escalates_and_daemon_errors_are_not_absence() {
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    let log = dir.path().join("argv");
    fs::write(&docker,format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase $1 in ps) echo container;; inspect) echo run-1;; kill) exit 0;; esac\n",log.display())).unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    kod_eval::connected::with_docker(&docker, || {
        kod_eval::connected::with_stop_clock(|| {
            assert!(
                kod_eval::connected::stop_owned(
                    "run-1",
                    &["kod-agent-run-1".into()],
                    Duration::from_secs(5),
                    Duration::from_secs(10)
                )
                .is_err()
            );
        })
    });
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.find("kill --signal TERM").unwrap() < calls.find("kill --signal KILL").unwrap());
    let before_kill = calls.split("kill --signal KILL").next().unwrap();
    assert_eq!(
        before_kill
            .lines()
            .filter(|line| line.starts_with("ps "))
            .count(),
        6
    );
    assert_eq!(
        calls.lines().filter(|line| line.starts_with("ps ")).count(),
        11
    );
    fs::write(&docker, "#!/bin/sh\nexit 1\n").unwrap();
    kod_eval::connected::with_docker(&docker, || {
        assert!(
            kod_eval::connected::stop_owned(
                "run-1",
                &["kod-agent-run-1".into()],
                Duration::ZERO,
                Duration::from_millis(100)
            )
            .is_err()
        );
    });
}
#[test]
fn cancel_waits_for_delayed_create_and_cli_exit() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    let present = dir.path().join("present");
    let created = dir.path().join("created");
    fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
case $1 in
run) sleep 0.1; touch '{present}' '{created}'; while [ -f '{present}' ]; do sleep 0.01; done;;
ps) if [ -f '{present}' ]; then echo container; fi;;
inspect) echo run-1;;
kill) rm -f '{present}';;
esac
"#,
            present = present.display(),
            created = created.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    let calls = AtomicUsize::new(0);
    kod_eval::connected::with_docker(&docker, || {
        let result = kod_eval::connected::launch(
            &["run".into()],
            "kod-agent-run-1",
            Duration::from_secs(1),
            &SpawnGate::default(),
            &|| Ok(calls.fetch_add(1, Ordering::SeqCst) > 0),
        )
        .unwrap();
        assert!(result.stopped);
        assert!(!result.success);
    });
    assert!(created.exists());
    assert!(!present.exists());
}
#[test]
fn stuck_docker_cli_is_bounded_and_foreign_label_is_never_signaled() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    fs::write(&docker, "#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    kod_eval::connected::with_docker(&docker, || {
        let start = Instant::now();
        assert!(
            kod_eval::connected::stop_owned(
                "run-1",
                &["kod-agent-run-1".into()],
                Duration::ZERO,
                Duration::from_millis(200)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    });
    let signaled = dir.path().join("signaled");
    fs::write(&docker,format!("#!/bin/sh\ncase $1 in ps) echo container;; inspect) echo run-2;; kill) touch '{}';; esac\n",signaled.display())).unwrap();
    kod_eval::connected::with_docker(&docker, || {
        assert!(
            kod_eval::connected::stop_owned(
                "run-1",
                &["kod-agent-run-1".into()],
                Duration::ZERO,
                Duration::from_secs(2)
            )
            .is_err()
        );
    });
    assert!(!signaled.exists());
}
#[test]
fn agent_argv_has_history_free_mounts_and_opaque_gateway_urls() {
    use kod_eval::{
        case::Budget,
        connected::{ConnectedSpec, agent_args},
    };
    let spec = ConnectedSpec {
        run: "run-1".into(),
        base: "a".repeat(40),
        mirror: "/mirror".into(),
        dir: "/runs/run-1".into(),
        agent_image: "agent@sha256:pin".into(),
        collector_image: "collector@sha256:pin".into(),
        network: "internal".into(),
        model: "model".into(),
        max_tokens: 100,
        budget: Budget {
            max_turns: 2,
            timeout_secs: 30,
            memory: "1g".into(),
            disk: "128m".into(),
            cpus: 1.0,
        },
        prompt: "issue".into(),
        model_url: "http://model/r/opaque".into(),
        mcp_url: "http://tools/r/opaque".into(),
    };
    let args = agent_args(&spec, (1, 2)).unwrap();
    for value in [
        "/runs/run-1/seed:/seed:ro",
        "/runs/run-1/in:/in:ro",
        "--read-only",
        "ANTHROPIC_BASE_URL=http://model/r/opaque",
        "KOD_MCP_URL=http://tools/r/opaque",
    ] {
        assert!(args.iter().any(|a| a == value));
    }
    assert!(
        !args
            .iter()
            .any(|a| a.contains("docker.sock") || a.contains("/mirror") || a.contains("API_KEY"))
    );
    let image = args.iter().position(|a| a == "agent@sha256:pin").unwrap();
    assert_eq!(&args[image + 1..image + 3], ["agent", "kod-loop"]);
}
#[test]
fn eval_stop_deadlines_equal_host_deadlines() {
    assert_eq!(
        kod_eval::connected::TERM_GRACE,
        kod_host::deadlines::TERM_GRACE
    );
    assert_eq!(kod_eval::connected::ABSENCE, kod_host::deadlines::ABSENCE);
}
