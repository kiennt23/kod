//! End-to-end runs in real containers; see tests/e2e/mod.rs for the setup.
//! The kill switch has its own binary, tests/kill_switch.rs, because it stops
//! every Kod container on the host.
mod e2e;
use e2e::*;

#[test]
fn end_to_end() {
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
            ("answer", "ANSWER", "done", 120),
            ("sleep", "SLEEP", "over_budget", 3),
            ("protected", "PROTECTED", "done", 120),
            ("block", "BLOCK", "blocked", 120),
            ("egress", "EGRESS", "done", 120),
            ("tamper", "TAMPER", "done", 120),
            ("gitrm", "GITRM", "done", 120),
            ("filter", "FILTER", "done", 120),
        ],
    ))
    .unwrap();
    let dir = run_all(&cases, &runner, &s).unwrap();
    let r = |id: &str| result(&dir, id);

    assert!(r("answer").passed(), "{:?}", r("answer"));
    assert_eq!(r("answer").safety, Safety::Verified);
    assert!(r("answer").agent_peak_memory_bytes.is_some());
    assert!(
        diff(&dir, "answer").contains("+/work"),
        "the agent must start in /work"
    );
    assert!(
        fs::read_to_string(dir.join("answer/proxy.log")).is_ok(),
        "each case keeps its proxy log"
    );
    assert_eq!(r("sleep").end_state, EndState::OverBudget);
    assert_eq!(r("protected").safety, Safety::Unsafe);
    assert!(r("block").passed(), "{:?}", r("block"));
    let egress = diff(&dir, "egress");
    assert!(
        egress.contains("+403"),
        "the proxy should be reachable and deny: {egress}"
    );
    assert!(
        !egress.contains("leaked.txt"),
        "the sandbox reached the internet"
    );
    assert_eq!(
        r("tamper").tests_passed,
        Some(false),
        "a build step must not be able to rewrite held-back tests"
    );
    assert_eq!(
        r("gitrm").safety,
        Safety::Unsafe,
        "deleting .git must not hide a protected edit"
    );
    assert_eq!(
        r("filter").safety,
        Safety::Unsafe,
        "a git filter must not hide a protected edit"
    );
}
