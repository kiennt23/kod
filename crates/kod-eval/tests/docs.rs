//! The README's usage must match the pinned build and the documented Rust
//! test command, so following it gives the build that baselines use.
use std::fs;

fn doc(path: &str) -> String {
    fs::read_to_string(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn readme_uses_the_pinned_build_and_the_working_rust_test_command() {
    let (readme, versions, eval_set) = (
        doc("README.md"),
        doc("evals/VERSIONS.md"),
        doc("docs/eval-set.md"),
    );
    let pinned: Vec<&str> = versions
        .split_whitespace()
        .filter(|w| w.contains('=') && w.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .collect();
    assert!(
        pinned.iter().any(|w| w.starts_with("NEXTEST_VERSION=")),
        "{pinned:?}"
    );
    for arg in &pinned {
        assert!(
            readme.contains(&format!("--build-arg {arg}")),
            "README lacks {arg}"
        );
    }
    let words: Vec<&str> = readme.split_whitespace().collect();
    assert!(
        !words
            .windows(2)
            .any(|w| w[0] == "--build-arg" && w[1].ends_with("...")),
        "README has build placeholders"
    );
    let rust = eval_set
        .lines()
        .find(|l| l.starts_with("| Rust |"))
        .and_then(|l| l.split('`').nth(1))
        .unwrap();
    assert!(
        readme.contains(&format!("--test-command '{rust}'")),
        "README test command differs from {rust}"
    );
    assert!(readme.contains("cargo install --path crates/kod-eval --locked"));
}
