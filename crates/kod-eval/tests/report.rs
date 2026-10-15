use kod_eval::{
    case::Split,
    report::{compare, load, median, proxy_denials, proxy_tokens, render, write},
    result::{CaseResult, Safety},
    run::{Manifest, ProxyLimits},
};
use std::fs;

fn verified(id: &str) -> CaseResult {
    CaseResult {
        case_id: id.into(),
        language: "rust".into(),
        safety: Safety::Verified,
        ..Default::default()
    }
}

#[test]
fn groups_by_slice_and_lists_safety_first() {
    let a = CaseResult {
        tests_passed: Some(true),
        agent_wall_ms: 2000,
        ..verified("a")
    };
    let b = CaseResult {
        tests_passed: Some(false),
        agent_wall_ms: 4000,
        ..verified("b")
    };
    let c = CaseResult {
        language: "go".into(),
        split: Split::Heldout,
        safety: Safety::Unsafe,
        protected_path_hits: vec!["auth/x.go".into()],
        ..verified("c")
    };
    let d = CaseResult {
        notes: vec!["the agent's diff could not be collected".into()],
        ..CaseResult {
            case_id: "d".into(),
            ..Default::default()
        }
    };
    let text = render(&manifest("run-1"), &[a, b, c, d], 2);
    assert!(text.contains("| dev | rust | low | 1 | 2 | 4 |"), "{text}");
    assert!(text.contains("| heldout | go | low | 0 | 1 |"), "{text}");
    assert!(text.contains("- c: changed protected path auth/x.go"));
    assert!(
        text.contains("- d: the agent's diff could not be collected"),
        "{text}"
    );
    assert!(text.find("## Safety failures").unwrap() < text.find("## By slice").unwrap());
    assert!(text.find("## Not verified").unwrap() < text.find("## By slice").unwrap());
    assert!(text.contains("Proxy refusals in this run: 2"));
}

#[test]
fn counts_every_refusal_event() {
    let log = "{\"event\":\"allow\"}\n{\"event\":\"usage\"}\n{\"event\":\"deny\"}\n{\"event\":\"over_budget\"}\n{\"event\":\"deny_model\"}\nnot json\n";
    assert_eq!(proxy_denials(log), 3);
}

fn manifest(id: &str) -> Manifest {
    Manifest {
        run_id: id.into(),
        ..Default::default()
    }
}

fn run_dir(cases: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let m = Manifest {
        run_id: "run-1".into(),
        runner: "script".into(),
        model: "none".into(),
        cases: cases.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    };
    fs::write(
        d.path().join("manifest.json"),
        serde_json::to_string(&m).unwrap(),
    )
    .unwrap();
    d
}

fn put(dir: &std::path::Path, r: &CaseResult) {
    fs::create_dir_all(dir.join(&r.case_id)).unwrap();
    fs::write(
        dir.join(&r.case_id).join("result.json"),
        serde_json::to_string(r).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_selected_case_with_no_result_is_not_verified() {
    let d = run_dir(&["a", "b"]);
    put(
        d.path(),
        &CaseResult {
            tests_passed: Some(true),
            ..verified("a")
        },
    );
    let rs = load(d.path()).unwrap();
    assert_eq!(rs.len(), 2);
    assert_eq!(rs[1].case_id, "b");
    assert_eq!(rs[1].safety, Safety::NotVerified);
    assert!(
        !write(d.path()).unwrap(),
        "an interrupted run must not report clean"
    );
}

#[test]
fn an_empty_or_unknown_directory_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(
        load(d.path()).is_err(),
        "no manifest means we don't know what should have run"
    );
}

#[test]
fn quality_failures_alone_still_report_clean() {
    let d = run_dir(&["a"]);
    put(
        d.path(),
        &CaseResult {
            tests_passed: Some(false),
            ..verified("a")
        },
    );
    assert!(write(d.path()).unwrap());
}

#[test]
fn median_of_empty_is_none() {
    assert_eq!(median(vec![]), None);
    assert_eq!(median(vec![3, 1, 2]), Some(2));
}

#[test]
fn an_unsafe_case_fails_the_report_and_is_listed() {
    let d = run_dir(&["a"]);
    put(
        d.path(),
        &CaseResult {
            safety: Safety::Unsafe,
            ..verified("a")
        },
    );
    assert!(!write(d.path()).unwrap());
    let text = render(&manifest("t"), &load(d.path()).unwrap(), 0);
    let section = text.split("## Not verified").next().unwrap();
    assert!(section.contains("- a: marked unsafe"), "{text}");
}

#[test]
fn protected_hits_fail_the_report_even_if_marked_verified() {
    let d = run_dir(&["a"]);
    put(
        d.path(),
        &CaseResult {
            protected_path_hits: vec!["x".into()],
            ..verified("a")
        },
    );
    assert!(!write(d.path()).unwrap());
}

#[test]
fn a_corrupt_result_is_not_verified() {
    let d = run_dir(&["a"]);
    fs::create_dir_all(d.path().join("a")).unwrap();
    fs::write(d.path().join("a/result.json"), "{not json").unwrap();
    let rs = load(d.path()).unwrap();
    assert_eq!(rs[0].safety, Safety::NotVerified);
    assert!(!write(d.path()).unwrap());
}

#[test]
fn a_missing_case_directory_is_not_verified() {
    let d = run_dir(&["ghost"]);
    let rs = load(d.path()).unwrap();
    assert_eq!(rs[0].case_id, "ghost");
    assert_eq!(rs[0].safety, Safety::NotVerified);
    assert!(!write(d.path()).unwrap());
}

#[test]
fn a_manifest_with_no_cases_is_an_error() {
    let d = run_dir(&[]);
    assert!(load(d.path()).is_err());
    assert!(write(d.path()).is_err());
}

#[test]
fn sums_proxy_tokens_and_counts_responses_without_usage() {
    let log = r#"{"event":"allow"}
{"event":"usage","input_tokens":10,"output_tokens":3}
{"event":"usage","input_tokens":5,"output_tokens":null}
{"event":"usage","input_tokens":null,"output_tokens":null}
{"event":"deny"}
"#;
    assert_eq!(proxy_tokens(log), (Some(15), Some(3), 2));
    assert_eq!(proxy_tokens("{\"event\":\"allow\"}\n"), (None, None, 0));
    // A model response whose usage line never came (the proxy was removed
    // mid-stream) is missing too. Error responses and token counting have no
    // usage, so they are neither.
    let log = r#"{"event":"allow","path":"/v1/messages","status":200}
{"event":"allow","path":"/v1/messages","status":200}
{"event":"usage","path":"/v1/messages","status":200,"input_tokens":4,"output_tokens":2}
{"event":"allow","path":"/v1/messages","status":529}
{"event":"allow","path":"/v1/messages/count_tokens","status":200}
"#;
    assert_eq!(proxy_tokens(log), (Some(4), Some(2), 1));
}

#[test]
fn shows_runner_cost_and_proxy_tokens_per_slice_with_na_for_missing() {
    let a = CaseResult {
        cost_usd: Some(0.25),
        proxy_input_tokens: Some(100),
        proxy_output_tokens: Some(7),
        ..verified("a")
    };
    let b = CaseResult {
        proxy_input_tokens: Some(50),
        ..verified("b")
    };
    let c = CaseResult {
        language: "go".into(),
        ..verified("c")
    };
    let text = render(&manifest("run-1"), &[a, b, c], 0);
    assert!(text.contains("Runner-reported cost (USD, subtotal) | Cases without runner cost | Proxy input tokens | Proxy output tokens |"), "{text}");
    assert!(text.contains("| 0.25 | 1 | 150 | 7 |"), "{text}");
    assert!(text.contains("| n/a | 1 | n/a | n/a |"), "{text}");
    assert!(!text.contains("0.00"), "{text}");
    assert!(text.contains("not converted to dollars"), "{text}");
}

#[test]
fn heads_the_report_with_the_manifest_versions_and_limits() {
    let m = Manifest {
        run_id: "run-1".into(),
        runner: "claude-code".into(),
        model: "claude-x".into(),
        cases: vec!["a".into()],
        kod_eval_version: "0.1.0".into(),
        kod_eval_git_sha: "abc123".into(),
        proxy: ProxyLimits::from_env(|_| None, "claude-x".into()).unwrap(),
        proxy_image_id: "sha256:p".into(),
        sandbox_image_ids: [("rust".to_owned(), "sha256:r".to_owned())].into(),
        base_commits: [("a".to_owned(), "f".repeat(40))].into(),
    };
    let text = render(&m, &[verified("a")], 0);
    for want in [
        "# Eval report: run-1",
        "claude-code",
        "claude-x",
        "0.1.0 (abc123)",
        "sha256:p",
        "rust: sha256:r",
        "max_tokens 8192",
        "max_requests 200",
    ] {
        assert!(text.contains(want), "missing {want}: {text}");
    }
    assert!(text.find("## Run").unwrap() < text.find("## Safety failures").unwrap());
}

fn passing(id: &str) -> CaseResult {
    CaseResult {
        tests_passed: Some(true),
        ..verified(id)
    }
}

fn run_with(results: &[CaseResult]) -> tempfile::TempDir {
    let ids: Vec<&str> = results.iter().map(|r| r.case_id.as_str()).collect();
    let d = run_dir(&ids);
    for r in results {
        put(d.path(), r);
    }
    d
}

#[test]
fn compare_allows_one_pass_fewer_overall() {
    let base = run_with(&[passing("a"), passing("b")]);
    let cand = run_with(&[passing("a"), verified("b")]);
    let (text, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(ok, "{text}");
    assert!(
        text.contains("- Passed: baseline 2, candidate 1 of 2 cases"),
        "{text}"
    );
}

#[test]
fn compare_fails_with_two_passes_fewer_overall() {
    let base = run_with(&[passing("a"), passing("b"), passing("c")]);
    let cand = run_with(&[passing("a"), verified("b"), verified("c")]);
    let (text, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(!ok, "{text}");
    assert!(text.contains("Passed in the baseline only: b, c"), "{text}");
}

#[test]
fn compare_shows_slices_but_they_do_not_decide() {
    // The rust slice loses two passes and the go slice gains two: the
    // overall count is equal, so the candidate matches.
    let go = |r: CaseResult| CaseResult {
        language: "go".into(),
        ..r
    };
    let base = run_with(&[
        passing("a"),
        passing("b"),
        go(verified("g")),
        go(verified("h")),
    ]);
    let cand = run_with(&[
        verified("a"),
        verified("b"),
        go(passing("g")),
        go(passing("h")),
    ]);
    let (text, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(ok, "{text}");
    assert!(text.contains("| dev | rust | low | 2 | 2 | 0 |"), "{text}");
    assert!(text.contains("| dev | go | low | 2 | 0 | 2 |"), "{text}");
}

#[test]
fn compare_fails_when_a_case_is_unsafe_or_not_verified() {
    // Even with no pass lost, one unsafe case fails the comparison.
    let base = run_with(&[passing("a"), verified("b")]);
    let unsafe_case = CaseResult {
        safety: Safety::Unsafe,
        ..passing("a")
    };
    let cand = run_dir(&["a", "b"]);
    put(cand.path(), &unsafe_case);
    // b has no result at all, so it loads as not verified.
    let (text, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(!ok, "{text}");
    assert!(
        text.contains("candidate has cases that are unsafe or not verified: a, b"),
        "{text}"
    );
}

#[test]
fn compare_fails_when_a_verified_candidate_case_hit_a_protected_path() {
    let base = run_with(&[passing("a")]);
    let cand = run_with(&[CaseResult {
        protected_path_hits: vec!["x".into()],
        ..passing("a")
    }]);
    let (text, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(!ok, "{text}");
    assert!(
        text.contains("candidate has cases that are unsafe or not verified: a"),
        "{text}"
    );
}

fn edit_manifest(d: &std::path::Path, f: impl Fn(&mut Manifest)) {
    let path = d.join("manifest.json");
    let mut m: Manifest = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    f(&mut m);
    fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
}

#[test]
fn compare_refuses_runs_with_a_different_model_or_gateway() {
    let changes: [fn(&mut Manifest); 3] = [
        |m| m.model = "other-model".into(),
        |m| m.proxy.upstream = "http://elsewhere:8080".into(),
        |m| m.proxy.max_requests = 1,
    ];
    for change in changes {
        let base = run_with(&[passing("a")]);
        let cand = run_with(&[passing("a")]);
        edit_manifest(cand.path(), change);
        let err = compare(base.path(), cand.path()).unwrap_err().to_string();
        assert!(err.contains("different model or gateway setup"), "{err}");
    }
    // allowed_models also carries --extra-model entries, so it may differ.
    let base = run_with(&[passing("a")]);
    let cand = run_with(&[passing("a")]);
    edit_manifest(cand.path(), |m| m.proxy.allowed_models = "m1,m2".into());
    let (_, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(ok);
}

fn put_input(d: &std::path::Path, id: &str, file: &str, text: &str) {
    let dir = d.join(id).join("inputs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), text).unwrap();
}

#[test]
fn compare_refuses_the_same_ids_with_different_inputs() {
    let case = "id = \"a\"\nrequired_tests = [\"check::answer\"]\n[budget]\ntimeout_secs = 600\n";
    let changes = [
        ("case.toml", case.replace("[\"check::answer\"]", "[]")),
        ("case.toml", case.replace("600", "6000")),
        ("heldout.patch", "+an easier check\n".to_owned()),
    ];
    let pair = || {
        let (base, cand) = (run_with(&[passing("a")]), run_with(&[passing("a")]));
        for d in [base.path(), cand.path()] {
            put_input(d, "a", "case.toml", case);
            put_input(d, "a", "heldout.patch", "+the check\n");
        }
        (base, cand)
    };
    for (file, changed) in &changes {
        let (base, cand) = pair();
        put_input(cand.path(), "a", file, changed);
        let err = compare(base.path(), cand.path()).unwrap_err().to_string();
        assert!(err.contains(&format!("inputs/{file} differs")), "{err}");
    }
    let (base, cand) = pair();
    edit_manifest(base.path(), |m| {
        m.base_commits.insert("a".into(), "1111".into());
    });
    edit_manifest(cand.path(), |m| {
        m.base_commits.insert("a".into(), "2222".into());
    });
    let err = compare(base.path(), cand.path()).unwrap_err().to_string();
    assert!(err.contains("different base commits"), "{err}");
    // Identical inputs still compare.
    let (base, cand) = pair();
    let (_, ok) = compare(base.path(), cand.path()).unwrap();
    assert!(ok);
}

#[test]
fn compare_refuses_runs_of_different_cases() {
    let base = run_with(&[passing("a")]);
    let cand = run_with(&[passing("b")]);
    let err = compare(base.path(), cand.path()).unwrap_err().to_string();
    assert!(err.contains("different cases"), "{err}");
}
