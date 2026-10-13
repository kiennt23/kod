//! The kill switch stops every Kod container on the host, so it has its own
//! test binary: Cargo runs test binaries one at a time, so no other Docker test
//! is running when it fires. Same setup as tests/docker.rs.
mod e2e;
use e2e::*;

fn kod_containers(extra: &[&str]) -> String {
    let mut args: Vec<String> = ["ps", "-q", "--filter", "label=kod.run"]
        .map(String::from)
        .to_vec();
    for f in extra {
        args.extend(["--filter".to_owned(), f.to_string()]);
    }
    docker(&args).unwrap()
}

/// Waits until the first case's agent container is running, so the stop lands
/// mid-case rather than on a timer.
fn wait_for_agent() {
    for _ in 0..600 {
        if !kod_containers(&["ancestor=kod-sandbox-test"])
            .trim()
            .is_empty()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the first case's container never started");
}

#[test]
fn stopping_during_the_first_case_means_the_second_never_starts() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let s = settings(tmp.path());
    let runner = Runner::Script {
        command: SCRIPT.into(),
    };
    // load_cases sorts by id; the prefixes fix the order.
    let cases = load_cases(&fixture(
        tmp.path(),
        &[
            ("a-stopme", "STOPME", "done", 120),
            ("b-after", "ANSWER", "done", 120),
        ],
    ))
    .unwrap();
    assert_eq!(cases[0].id, "a-stopme");
    let stop_file = s.stop_file.clone();
    let killer = std::thread::spawn(move || {
        wait_for_agent();
        stop(&stop_file).unwrap();
    });
    let started = std::time::Instant::now();
    let dir = run_all(&cases, &runner, &s).unwrap();
    let elapsed = started.elapsed();
    killer.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(15),
        "the running agent was not killed: run took {elapsed:?}"
    );

    let first = result(&dir, "a-stopme");
    assert!(
        first.cancelled && first.safety == Safety::NotVerified && !first.passed(),
        "{first:?}"
    );
    let second = result(&dir, "b-after");
    assert!(
        second.cancelled && second.notes.iter().any(|n| n.contains("before it started")),
        "{second:?}"
    );
    assert!(
        !dir.join("b-after/out").exists(),
        "the second case must not start a container"
    );
    assert!(
        kod_containers(&[]).trim().is_empty(),
        "no Kod container may be left running"
    );

    // Every other way of starting a container is refused too.
    assert!(
        run_all(&cases, &runner, &s).is_err(),
        "runs are refused while the stop file exists"
    );
    let check = kod_eval::newcase::check_case(
        &cases[1],
        "kod-sandbox",
        &tmp.path().join("check"),
        &s.stop_file,
    );
    assert!(
        check.is_err(),
        "check-case must not start containers while the stop file exists"
    );
}
