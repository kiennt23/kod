use kod_dispatcher::Config;

#[test]
fn a_valid_config_loads_with_logins_lower_cased() {
    let c =
        Config::parse(r#"{"repo": "acme/app", "owner": "KienNT23", "backup": "Bob-2"}"#).unwrap();
    assert_eq!(
        c,
        Config {
            repo: "acme/app".into(),
            owner: "kiennt23".into(),
            backup: Some("bob-2".into()),
        }
    );
    assert!(c.may_always_stop(" KIENNT23 "));
    assert!(c.may_always_stop("bob-2"));
    assert!(!c.may_always_stop("mallory"));
    assert!(!c.may_always_stop(""));
}

#[test]
fn a_bad_config_does_not_load() {
    for (text, why) in [
        ("[]", "must be an object"),
        ("{", "not JSON"),
        (
            r#"{"repo": "acme/app", "owner": "k"}"#,
            "missing key \"backup\"",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": null, "wip": 2}"#,
            "unknown key \"wip\"",
        ),
        (
            r#"{"repo": "Acme/App", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme/app/x", "owner": "k", "backup": null}"#,
            "repo must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "", "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": " k", "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": null, "backup": null}"#,
            "owner must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": ""}"#,
            "backup must be",
        ),
        (
            r#"{"repo": "acme/app", "owner": "k", "backup": "K"}"#,
            "someone other",
        ),
    ] {
        let e = format!("{:#}", Config::parse(text).unwrap_err());
        assert!(e.contains(why), "{text}: {e}");
    }
}

#[test]
fn load_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.json");
    std::fs::write(&path, "{}").unwrap();
    let e = format!("{:#}", Config::load(&path).unwrap_err());
    assert!(e.contains("dispatcher.json"), "{e}");
}
