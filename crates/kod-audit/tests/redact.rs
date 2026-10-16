use kod_audit::redact::{REDACTED, redact, redact_value};
use serde_json::json;

// The fake credentials are split with concat! so secret scanners, GitHub push
// protection among them, don't flag this file.
#[test]
fn known_credential_formats_are_redacted() {
    for secret in [
        concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("sk", "-proj-abcdefghijklmnopqrstuvwx"),
        concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("gi", "thub_pat_11ABCDEFG0123456789_abcdefghijklmnop"),
        concat!("gl", "pat-abcdefghijklmnopqrst"),
        concat!("xo", "xb-1234567890-abcdefghijklmnop"),
        concat!("AK", "IAIOSFODNN7EXAMPLE"),
        concat!("AI", "zaSyA-abcdefghijklmnopqrstuvwxyz012345"),
        concat!(
            "ey",
            "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abcdefghijklmnop"
        ),
        "Bearer abcdefghijklmnopqrstuvwxyz",
    ] {
        let text = format!("token: {secret} end");
        assert_eq!(redact(&text), format!("token: {REDACTED} end"), "{secret}");
    }
}

#[test]
fn private_key_blocks_are_redacted_even_without_an_end() {
    let pem =
        "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\nb";
    assert_eq!(redact(pem), format!("a\n{REDACTED}\nb"));
    let cut = "a\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA";
    assert_eq!(redact(cut), format!("a\n{REDACTED}"));
    let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    assert_eq!(redact(cert), cert);
}

#[test]
fn ordinary_text_is_left_alone() {
    for text in [
        "task-list and sk-learn are fine",
        "ask-me-anything-about-the-weather-today",
        "the AKIA prefix alone",
        "Bearer short",
        "unicode: héllo wörld ✓ sk-",
        "max_tokens=4096 and key=value",
        "https://example.com:8443/a@b and user@example.com",
        "PWD=/home/kod OLDPWD=/tmp author: Jane timeout = 30 port: 5432",
        "at 12:30 see https://example.com",
        r#"{"max_tokens": 4096}"#,
        r#"[{"max_tokens":4096}]"#,
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn tokens_after_punctuation_are_redacted() {
    for (text, want) in [
        (
            concat!("GITHUB_TOKEN=gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
            format!("GITHUB_TOKEN={REDACTED}"),
        ),
        (
            concat!("export AWS_ACCESS_KEY_ID=AK", "IAIOSFODNN7EXAMPLE"),
            format!("export AWS_ACCESS_KEY_ID={REDACTED}"),
        ),
        (
            concat!("--token=sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz"),
            format!("--token={REDACTED}"),
        ),
        (
            concat!("path/gh", "p_abcdefghijklmnopqrstuvwxyz0123456789/x"),
            format!("path/{REDACTED}"),
        ),
        (
            "AUTHORIZATION: BEARER abcdefghijklmnopqrstuvwxyz",
            format!("AUTHORIZATION: {REDACTED}"),
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn sensitive_assignments_and_env_dumps_are_redacted() {
    let env = "HOME=/home/kod\nDB_PASSWORD=hunter2hunter2\nAPI_KEY='abc def'\nmy_secret=\"x\"\nPATH=/usr/bin";
    assert_eq!(
        redact(env),
        format!(
            "HOME=/home/kod\nDB_PASSWORD={REDACTED}\nAPI_KEY={REDACTED}\nmy_secret={REDACTED}\nPATH=/usr/bin"
        )
    );
    assert_eq!(
        redact("kod run --db-password=s3cret --verbose"),
        format!("kod run --db-password={REDACTED} --verbose")
    );
}

#[test]
fn urls_are_redacted() {
    assert_eq!(
        redact("clone https://kod:s3cretpass@git.example.com/repo.git now"),
        format!("clone https://kod:{REDACTED}@git.example.com/repo.git now")
    );
    assert_eq!(
        redact("GET https://api.example.com/v1?access_token=abc123xyz&page=2"),
        format!("GET https://api.example.com/v1?access_token={REDACTED}&page=2")
    );
}

#[test]
fn every_string_and_key_in_a_payload_is_redacted() {
    let v = json!({
        "prompt": concat!("use gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
        "nested": {"list": [1, concat!("xo", "xp-1234567890-abcdefghijklmnop"), null]},
        "password": "hunter2",
        (concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz")): 1,
        (concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789")): 2
    });
    assert_eq!(
        redact_value(&v),
        json!({
            "prompt": format!("use {REDACTED}"),
            "nested": {"list": [1, REDACTED, null]},
            "password": REDACTED,
            (REDACTED): 2,
            (format!("{REDACTED}#2")): 1
        })
    );
}

#[test]
fn names_match_without_separators_or_case() {
    assert_eq!(
        redact("DB_PASS=x1 MYSQL_PWD=x2 db-auth=x3"),
        format!("DB_PASS={REDACTED} MYSQL_PWD={REDACTED} db-auth={REDACTED}")
    );
    assert_eq!(
        redact_value(&json!({"apiKey": "k", "auth": "a", "pass": "p", "max_tokens": 5})),
        json!({"apiKey": REDACTED, "auth": REDACTED, "pass": REDACTED, "max_tokens": 5})
    );
}

#[test]
fn spaced_and_colon_assignments_are_redacted() {
    for (text, want) in [
        (
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG",
            format!("aws_secret_access_key = {REDACTED}"),
        ),
        (
            r#"{"password": "hunter2", "user": "kod"}"#,
            format!(r#"{{"password": {REDACTED}, "user": "kod"}}"#),
        ),
        (
            "db:\n  password: hunter2\n  port: 5432",
            format!("db:\n  password: {REDACTED}\n  port: 5432"),
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn credential_headers_are_redacted() {
    for (text, want) in [
        (
            "Authorization: Basic dXNlcjpwYXNz",
            format!("Authorization: {REDACTED}"),
        ),
        (
            "authorization: basic dXNlcjpwYXNzd29yZA== x",
            format!("authorization: {REDACTED} x"),
        ),
        ("X-Api-Key: abc123def", format!("X-Api-Key: {REDACTED}")),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn credentials_in_command_flags_are_redacted() {
    for (text, want) in [
        (
            "curl -u kod:s3cret https://x",
            format!("curl -u {REDACTED} https://x"),
        ),
        (
            "mysql --password hunter2 db",
            format!("mysql --password {REDACTED} db"),
        ),
        ("ls -u -l", "ls -u -l".to_string()),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn any_private_key_label_is_redacted() {
    let pgp =
        "a\n-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOYBF\n-----END PGP PRIVATE KEY BLOCK-----\nb";
    assert_eq!(redact(pgp), format!("a\n{REDACTED}\nb"));
}

#[test]
fn stripe_keys_are_redacted() {
    for secret in [
        concat!("sk", "_live_abcdefghijklmnopqrstuvwx"),
        concat!("rk", "_live_abcdefghijklmnopqrstuvwx"),
    ] {
        assert_eq!(redact(&format!("key {secret}")), format!("key {REDACTED}"));
    }
}

#[test]
fn long_non_matching_tokens_are_left_alone() {
    let text = "_".repeat(128 * 1024);
    assert_eq!(redact(&text), text);
    let text = "a:".repeat(64 * 1024);
    assert_eq!(redact(&text), text);
    let text = format!("http://{}", ":".repeat(64 * 1024));
    assert_eq!(redact(&text), text);
}

#[test]
fn values_stop_at_closing_brackets() {
    assert_eq!(
        redact("(token=abcdef123456)"),
        format!("(token={REDACTED})")
    );
}

#[test]
fn brackets_inside_a_secret_do_not_end_it() {
    for (text, want) in [
        ("password=abc)def", format!("password={REDACTED}")),
        ("password=abc]def", format!("password={REDACTED}")),
        ("password=abc}def", format!("password={REDACTED}")),
        ("password=abc))def", format!("password={REDACTED}")),
        ("password=abc)]def", format!("password={REDACTED}")),
        (
            r#"{"a":{"api_key":abcdef123456}}"#,
            format!(r#"{{"a":{{"api_key":{REDACTED}}}}}"#),
        ),
        ("--password=ab)cd", format!("--password={REDACTED}")),
        (
            "Authorization: Basic abc)def",
            format!("Authorization: {REDACTED}"),
        ),
        (
            r#"{"api_key":abcdef123456}"#,
            format!(r#"{{"api_key":{REDACTED}}}"#),
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}
