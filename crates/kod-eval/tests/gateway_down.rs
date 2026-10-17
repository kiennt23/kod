//! A gateway that dies mid-run. Its own test binary: run ids are per second
//! and process, so two runs in one test binary can collide, and Cargo runs
//! test binaries one at a time. Same setup as tests/docker.rs.
mod e2e;
use e2e::*;

/// The gateway holds every case's budget in memory and is not restarted: if
/// it dies, the cases after it are cancelled and the run fails, rather than
/// running against nothing.
#[test]
fn a_gateway_that_dies_cancels_the_remaining_cases_and_fails_the_run() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let s = settings(tmp.path());
    let runner = Runner::Script {
        command: SCRIPT.into(),
    };
    let cases = load_cases(&fixture(
        tmp.path(),
        &[
            ("a-gwdown", "SLEEP", "over_budget", 3),
            ("b-gwdown", "ANSWER", "done", 120),
        ],
    ))
    .unwrap();
    // Kills the gateway while the first case's agent runs.
    let killer = std::thread::spawn(|| {
        for _ in 0..600 {
            let ps = ["ps", "--filter", "name=-a-gwdown", "--format", "{{.Names}}"];
            let names = docker(&ps.map(String::from)).unwrap();
            if let Some(run_id) = names
                .lines()
                .find_map(|n| n.strip_prefix("kod-")?.strip_suffix("-a-gwdown"))
            {
                docker(&["kill".into(), format!("kod-llm-proxy-{run_id}")]).unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the first case's agent never started");
    });
    let err = run_all(&cases, &runner, &s).unwrap_err();
    killer.join().unwrap();
    assert!(err.to_string().contains("the gateway stopped"), "{err:#}");
    let dir = fs::read_dir(&s.out_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let second = result(&dir, "b-gwdown");
    assert!(
        second.cancelled
            && second
                .notes
                .iter()
                .any(|n| n.contains("the gateway stopped")),
        "{second:?}"
    );
    assert!(!dir.join("b-gwdown/out").exists());
}
