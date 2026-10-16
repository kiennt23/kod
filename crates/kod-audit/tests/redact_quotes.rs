use kod_audit::redact::{REDACTED, redact};

#[test]
fn escaped_quotes_do_not_expose_the_rest_of_a_secret() {
    for (text, expected) in [
        (
            r#"password="prefix\"private-tail" after"#,
            format!("password={REDACTED} after"),
        ),
        (
            r#"password='prefix\'private-tail' after"#,
            format!("password={REDACTED} after"),
        ),
        (
            r#"{"password":"prefix\"private-tail","user":"kod"}"#,
            format!(r#"{{"password":{REDACTED},"user":"kod"}}"#),
        ),
        (
            r#"password="private\\" after"#,
            format!("password={REDACTED} after"),
        ),
        (
            r#"password="prefix\"private-tail"#,
            format!("password={REDACTED}"),
        ),
    ] {
        assert_eq!(redact(text), expected);
    }
}
