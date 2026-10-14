use kod_eval::{
    case::Split,
    report::{load, median, proxy_denials, proxy_tokens, render, write},
    result::{CaseResult, Safety},
    run::Manifest,
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
    let text = render("run-1", &[a, b, c, d], 2);
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

fn run_dir(cases: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let m = Manifest {
        run_id: "run-1".into(),
        runner: "script".into(),
        model: "none".into(),
        cases: cases.iter().map(|c| c.to_string()).collect(),
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
    let text = render("t", &load(d.path()).unwrap(), 0);
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
    let text = render("run-1", &[a, b, c], 0);
    assert!(text.contains("Runner-reported cost (USD, subtotal) | Cases without runner cost | Proxy input tokens | Proxy output tokens |"), "{text}");
    assert!(text.contains("| 0.25 | 1 | 150 | 7 |"), "{text}");
    assert!(text.contains("| n/a | 1 | n/a | n/a |"), "{text}");
    assert!(!text.contains("0.00"), "{text}");
    assert!(text.contains("not converted to dollars"), "{text}");
}
