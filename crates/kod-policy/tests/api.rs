mod common;

use std::fs;

use common::{copy_policy, data_only, decide, denied, denied_by, policy};
use kod_policy::{Policy, REGORUS_VERSION};
use serde_json::json;

#[test]
fn the_repo_policy_loads_with_a_content_hash_version() {
    let v = policy().version();
    let (hash, engine) = v.split_once(';').unwrap();
    assert!(hash.starts_with("sha256:") && hash.len() == 7 + 64, "{v}");
    assert_eq!(engine, format!("regorus={REGORUS_VERSION}"));
    assert_eq!(decide(json!({})).policy_version, v);
}

#[test]
fn the_version_follows_the_content() {
    let dir = copy_policy();
    let first = Policy::load(dir.path()).unwrap().version().to_string();
    assert_eq!(Policy::load(dir.path()).unwrap().version(), first);

    fs::write(dir.path().join("README.md"), "not loaded").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/extra.rego"), "package kod\n").unwrap();
    assert_eq!(
        Policy::load(dir.path()).unwrap().version(),
        first,
        "files that aren't loaded don't change the version"
    );

    fs::write(dir.path().join("extra.json"), "{}").unwrap();
    let with_extra = Policy::load(dir.path()).unwrap().version().to_string();
    assert_ne!(with_extra, first, "a new data file changes it");
    fs::rename(
        dir.path().join("extra.json"),
        dir.path().join("extra2.json"),
    )
    .unwrap();
    assert_ne!(
        Policy::load(dir.path()).unwrap().version(),
        with_extra,
        "a rename changes it"
    );
    fs::remove_file(dir.path().join("extra2.json")).unwrap();

    let rego = dir.path().join("kod.rego");
    let text = fs::read_to_string(&rego).unwrap();
    fs::write(&rego, format!("{text}\n# changed\n")).unwrap();
    assert_ne!(Policy::load(dir.path()).unwrap().version(), first);
}

#[test]
fn a_broken_policy_fails_to_load() {
    let empty = tempfile::tempdir().unwrap();
    assert!(Policy::load(empty.path()).is_err(), "no rego files");
    assert!(
        Policy::load(&empty.path().join("missing")).is_err(),
        "no directory"
    );

    for text in [
        "package kod\nthis is not rego\n",
        "package kod\nother := 1\n",
        "package kod\ndecision := {\"allow\": \"yes\"}\n",
    ] {
        let dir = data_only();
        fs::write(dir.path().join("kod.rego"), text).unwrap();
        assert!(Policy::load(dir.path()).is_err(), "{text}");
    }

    let dir = copy_policy();
    fs::write(dir.path().join("broken.json"), "{not json").unwrap();
    assert!(Policy::load(dir.path()).is_err(), "invalid JSON");
}

#[test]
fn input_that_is_not_an_object_is_denied() {
    for input in [
        json!(null),
        json!("publish"),
        json!([{"action": "publish"}]),
        json!(1),
    ] {
        let d = policy().decide(&input);
        assert!(!d.allow, "{d:?}");
        assert!(
            d.reasons
                .iter()
                .any(|r| r.contains("input must be a JSON object")),
            "{d:?}"
        );
    }
}

#[test]
fn a_missing_or_unknown_action_is_denied() {
    denied(json!({}), "unknown action");
    denied(json!({"action": "delete_repo"}), "unknown action");
    denied(json!({"action": 5}), "unknown action");
    denied(json!({"action": ["publish"]}), "unknown action");
}

#[test]
fn external_rows_are_never_granted() {
    denied(
        json!({"action": "local_command", "actor": {"kind": "agent"}}),
        "local_command is enforced by the sandbox",
    );
    denied(
        json!({"action": "sign_release", "actor": {"kind": "trusted_ci"}}),
        "sign_release is enforced by trusted CI runners",
    );
}

#[test]
fn evaluation_errors_deny() {
    let dir = data_only();
    fs::write(
        dir.path().join("kod.rego"),
        r#"package kod

decision := {"allow": true, "reasons": []} if not input.x

decision := {"allow": count(input.x) > 0, "reasons": []} if input.x
"#,
    )
    .unwrap();
    let probe = Policy::load(dir.path()).unwrap();
    assert!(
        probe.decide(&json!({})).allow,
        "the probe allows when it evaluates"
    );

    // count(5) is a builtin error at request time.
    let d = probe.decide(&json!({"x": 5}));
    assert!(!d.allow, "{d:?}");
    assert!(d.reasons[0].starts_with("policy error"), "{d:?}");
    assert_eq!(d.policy_version, probe.version());
}

#[test]
fn policy_is_send_and_sync() {
    fn shareable<T: Send + Sync>() {}
    shareable::<Policy>();
}

#[test]
fn data_files_cannot_override_rules() {
    let dir = copy_policy();
    fs::write(dir.path().join("allow.json"), "true").unwrap();
    fs::write(
        dir.path().join("decision.json"),
        r#"{"allow": true, "reasons": []}"#,
    )
    .unwrap();
    fs::write(dir.path().join("grant.json"), "true").unwrap();
    let p = Policy::load(dir.path()).unwrap();
    denied_by(&p, json!({"action": "delete_repo"}), "unknown action");
}

#[test]
fn the_regorus_version_matches_cargo_lock() {
    let lock =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let pinned = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"regorus\"\n"))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = ")))
        .unwrap();
    assert_eq!(pinned, format!("\"{REGORUS_VERSION}\""));
}
