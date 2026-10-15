#![allow(dead_code)] // each test file uses a different subset

use std::path::PathBuf;
use std::sync::OnceLock;

use kod_policy::{Decision, Policy};
use serde_json::Value;

pub fn policy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

/// The repo's policy, loaded once.
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| Policy::load(&policy_dir()).expect("the repo policy loads"))
}

/// A temp copy of `policy/`.
pub fn copy_policy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
    }
    dir
}

/// The repo's policy with one data file replaced, for values that are
/// placeholders in the repo.
pub fn policy_with(file: &str, contents: Value) -> Policy {
    let dir = copy_policy();
    std::fs::write(dir.path().join(file), contents.to_string()).unwrap();
    Policy::load(dir.path()).unwrap()
}

pub fn decide(input: Value) -> Decision {
    policy().decide(&input)
}

#[track_caller]
pub fn allowed_by(policy: &Policy, input: Value) {
    let d = policy.decide(&input);
    assert!(d.allow, "expected allow for {input}: {d:?}");
    assert!(d.reasons.is_empty(), "{d:?}");
}

#[track_caller]
pub fn denied_by(policy: &Policy, input: Value, reason: &str) {
    let d = policy.decide(&input);
    assert!(!d.allow, "expected deny for {input}: {d:?}");
    assert!(
        d.reasons.iter().any(|r| r.contains(reason)),
        "expected a reason containing {reason:?} for {input}: {d:?}"
    );
}

#[track_caller]
pub fn allowed(input: Value) {
    allowed_by(policy(), input)
}

#[track_caller]
pub fn denied(input: Value, reason: &str) {
    denied_by(policy(), input, reason)
}

/// `input` with the value at a JSON pointer replaced.
pub fn set(mut input: Value, pointer: &str, value: Value) -> Value {
    *input.pointer_mut(pointer).expect(pointer) = value;
    input
}

/// `input` with the key at a JSON pointer removed.
pub fn unset(mut input: Value, pointer: &str) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    input
        .pointer_mut(parent)
        .expect(parent)
        .as_object_mut()
        .unwrap()
        .remove(key);
    input
}

/// Loads a copy of `policy/` with `file` replaced by `contents`, or removed
/// for `None`, and returns the load error. Panics if it loads.
pub fn load_error(file: &str, contents: Option<Value>) -> String {
    let dir = copy_policy();
    let path = dir.path().join(file);
    match contents {
        Some(value) => std::fs::write(&path, value.to_string()).unwrap(),
        None => std::fs::remove_file(&path).unwrap(),
    }
    match Policy::load(dir.path()) {
        Ok(_) => panic!("{file} loaded"),
        Err(e) => format!("{e:#}"),
    }
}

/// A temp copy of `policy/`'s data files only, for a test's own Rego.
pub fn data_only() -> tempfile::TempDir {
    let dir = copy_policy();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rego") {
            std::fs::remove_file(path).unwrap();
        }
    }
    dir
}
