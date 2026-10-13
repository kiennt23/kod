#![allow(dead_code)]
use kod_eval::case::EvalCase;

pub const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

pub fn case_toml(id: &str) -> String {
    format!(
        r#"id = "{id}"
split = "dev"
language = "rust"
risk_class = "low"
repo = "/srv/kod/mirrors/demo"
base_commit = "{SHA}"
ticket = "Add a --verbose flag"
test_command = "cargo test"

[budget]
max_turns = 40
timeout_secs = 1800
memory = "4g"
cpus = 2.0
"#
    )
}

/// Parsed without validation, for tests that only need the fields.
pub fn case(id: &str) -> EvalCase {
    toml::from_str(&case_toml(id)).unwrap()
}
