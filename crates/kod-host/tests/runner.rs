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

/// Drives `runner::run` against a fake Docker whose collector/exporter bodies
/// are shell snippets run in the run directory. Returns (run result, whether
/// the workspace holds a collected result).
fn drive(collect: &str, export: &str) -> (anyhow::Result<kod_dispatcher::RunState>, bool) {
    use kod_tool_gateway::forge::Workspace;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let root = tempfile::tempdir().unwrap();
    let mirror = root.path().join("mirror");
    fs::create_dir(&mirror).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&mirror)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    git(&["init", "-q"]);
    fs::write(mirror.join("a"), "a\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]).trim().to_owned();
    let dir = root.path().join("run");
    fs::create_dir(&dir).unwrap();
    let docker = root.path().join("docker");
    fs::write(
        &docker,
        format!(
            r#"#!/bin/sh
[ "$1" = run ] || exit 0
for a in "$@"; do case $a in kod-agent-*) k=agent;; kod-collect-*) k=collect;; kod-export-*) k=export;; esac; done
cd '{dir}'
BASE='{base}'
case $k in
agent) printf 0 > agent_out/exit_code; printf 0 > agent_out/agent_procs; echo '{{"end_state":"done"}}' > agent_out/agent.json;;
collect) cd collect_out; {collect};;
export) cd export_out; {export};;
esac
"#,
            dir = dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    let spec = kod_eval::connected::ConnectedSpec {
        run: "run-1".into(),
        base: base.clone(),
        mirror,
        dir,
        agent_image: "agent".into(),
        collector_image: "collector".into(),
        network: "internal".into(),
        model: "m".into(),
        max_tokens: 10,
        budget: kod_eval::case::Budget {
            max_turns: 2,
            timeout_secs: 30,
            memory: "1g".into(),
            disk: "128m".into(),
            cpus: 1.0,
        },
        prompt: "issue".into(),
        model_url: "http://model/r/x".into(),
        mcp_url: "http://tools/r/x".into(),
    };
    let workspace = kod_host::workspace::CollectedWorkspace::default();
    let result = kod_eval::connected::with_docker(&docker, || {
        kod_host::runner::run(&spec, &SpawnGate::default(), &|| Ok(false), &workspace)
    });
    (result, workspace.collected("run-1").is_ok())
}
const PATCH_OK: &str = "touch collect_ok; printf 'diff --git a/a b/a\\nindex aaa..bbb 100644\\n--- a/a\\n+++ b/a\\n@@ -1 +1 @@\\n-a\\n+b\\n' > diff.patch";
const EXPORT_OK: &str = r#"touch export_ok; printf '{"version":1,"base":"%s","files":[{"path":"a","mode":"100644","blob":0}]}' "$BASE" > manifest.json; printf 'b\n' > 0"#;
#[test]
fn run_collects_only_through_the_complete_trusted_sequence() {
    let (result, collected) = drive(PATCH_OK, EXPORT_OK);
    assert_eq!(result.unwrap(), kod_dispatcher::RunState::Done);
    assert!(collected);
}
#[test]
fn any_failed_stage_leaves_no_collected_result() {
    let rename = "touch collect_ok; printf 'diff --git a/a b/b\\nrename from a\\nrename to b\\n' > diff.patch";
    for (collect, export) in [
        // missing and symlinked collector markers
        ("printf x > diff.patch", EXPORT_OK),
        (
            "ln -s /etc/passwd collect_ok; printf '' > diff.patch",
            EXPORT_OK,
        ),
        ("touch collect_ok", EXPORT_OK),
        ("exit 3", EXPORT_OK),
        // unsupported patch intent
        (rename, EXPORT_OK),
        // missing and symlinked export markers
        (PATCH_OK, "true"),
        (
            PATCH_OK,
            "ln -s /etc/passwd export_ok; cp /dev/null manifest.json",
        ),
        // forged base and exporter failure
        (
            PATCH_OK,
            r#"touch export_ok; printf '{"version":1,"base":"wrong","files":[]}' > manifest.json"#,
        ),
        (PATCH_OK, "exit 3"),
    ] {
        let (result, collected) = drive(collect, export);
        assert!(result.is_err(), "{collect} / {export}");
        assert!(!collected, "{collect} / {export}");
    }
}

fn stop_with(script: &str) -> bool {
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    let state = dir.path().join("gone");
    fs::write(
        &docker,
        script.replace("GONE", &state.display().to_string()),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    kod_eval::connected::with_docker(&docker, || {
        kod_eval::connected::with_stop_clock(|| {
            kod_eval::connected::stop_owned(
                "run-1",
                &["kod-agent-run-1".into()],
                Duration::from_secs(5),
                Duration::from_secs(10),
            )
            .is_ok()
        })
    })
}
#[test]
fn container_that_exits_by_itself_during_stop_is_stopped() {
    // inspect and kill fail "No such object", and ps no longer lists it.
    let ps = "case $1 in ps) [ -f GONE ] || echo container;; inspect) touch GONE; exit 1;; kill) exit 1;; esac\n";
    assert!(stop_with(&format!("#!/bin/sh\n{ps}")));
    // inspect succeeds, TERM kill fails because it just exited.
    let kill = "case $1 in ps) [ -f GONE ] || echo container;; inspect) echo run-1;; kill) touch GONE; exit 1;; esac\n";
    assert!(stop_with(&format!("#!/bin/sh\n{kill}")));
}
#[test]
fn failing_inspect_or_kill_on_a_listed_container_is_an_error() {
    assert!(!stop_with(
        "#!/bin/sh\ncase $1 in ps) echo container;; inspect) exit 1;; esac\n"
    ));
    assert!(!stop_with(
        "#!/bin/sh\ncase $1 in ps) echo container;; inspect) echo run-1;; kill) exit 1;; esac\n"
    ));
}
