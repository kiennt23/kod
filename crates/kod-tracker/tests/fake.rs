use kod_tracker::{
    Delivery, State, TaskEvent, Tracker,
    fake::{FakeIssue, hmac_sha256, sandbox},
};
use serde_json::json;

/// A workflow state the mapping doesn't know is an error, not a guess.
#[test]
fn an_unmapped_workflow_state_is_an_error() {
    let (t, _) = sandbox();
    t.add_issue(
        "FAKE-2",
        FakeIssue {
            workflow_state: "Won't Do".into(),
            ..FakeIssue::default()
        },
    );
    let e = t.get_issue("FAKE-2").unwrap_err().to_string();
    assert!(
        e.contains("\"Won't Do\", which the mapping does not map"),
        "{e}"
    );
    // Transition still works, and makes it readable again.
    t.transition("FAKE-2", State::Blocked).unwrap();
    assert_eq!(t.issue("FAKE-2").unwrap().workflow_state, "Waiting");
    assert_eq!(t.get_issue("FAKE-2").unwrap().state, State::Blocked);
}

#[test]
fn missing_custom_fields_read_as_none() {
    let (t, _) = sandbox();
    t.add_issue(
        "FAKE-3",
        FakeIssue {
            workflow_state: "Open".into(),
            fields: [("other".into(), "x".into())].into(),
            ..FakeIssue::default()
        },
    );
    let i = t.get_issue("FAKE-3").unwrap();
    assert_eq!((i.requirement_id, i.risk_class), (None, None));
}

#[test]
fn a_signed_webhook_that_is_malformed_is_an_error() {
    let (t, _) = sandbox();
    for body in [
        json!({"delivery": "m1", "type": "comment", "issue": "FAKE-1", "user": "dev"}),
        json!({"delivery": "m2", "type": "label_added", "issue": "FAKE-1", "label": "kod"}),
        json!({"delivery": "m3", "type": "comment", "issue": 1, "user": "dev", "body": "x"}),
        json!({"delivery": 4, "type": "comment", "issue": "FAKE-1", "user": "dev", "body": "x"}),
        json!({"delivery": "m6", "type": 4}),
        json!({"delivery": "m7"}),
    ] {
        assert!(t.parse_webhook(&t.webhook(&body)).is_err(), "{body}");
    }
    // Header names are case-insensitive, as in HTTP.
    let mut w = t.webhook(
        &json!({"delivery": "m5", "type": "comment", "issue": "FAKE-1", "user": "u", "body": "b"}),
    );
    w.headers[0].0 = "X-Fake-Signature".into();
    assert_eq!(
        t.parse_webhook(&w).unwrap(),
        Some(Delivery {
            id: "m5".into(),
            event: TaskEvent::Commented {
                issue: "FAKE-1".into(),
                author: "u".into(),
                body: "b".into()
            }
        })
    );
}

/// RFC 4231, test case 2.
#[test]
fn the_signature_is_hmac_sha256() {
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // A key longer than a block is hashed first (RFC 4231, test case 6).
    let mac = hmac_sha256(
        &[0xaa; 131],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(mac[..4], [0x60, 0xe4, 0x31, 0x59]);
}

/// A request that fails the signature check doesn't use up its delivery ID,
/// so a forger can't block the real delivery; the ID is spent once the real
/// one is accepted, and a signed request with no ID is refused.
#[test]
fn delivery_ids_are_spent_only_by_signed_requests() {
    let (t, s) = sandbox();
    assert!(t.parse_webhook(&s.tampered).is_err());
    assert!(t.parse_webhook(&s.trigger).unwrap().is_some());
    let e = t.parse_webhook(&s.trigger).unwrap_err().to_string();
    assert!(e.contains("already accepted"), "{e}");
    let e = t.parse_webhook(&s.unidentified).unwrap_err().to_string();
    assert!(e.contains("no delivery ID"), "{e}");
}
