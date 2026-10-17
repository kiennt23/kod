use kod_tracker::{State, mapping::Mapping};
use serde_json::{Value, json};

fn good() -> Value {
    json!({
        "tracker": "acme",
        "states": {
            "ready": ["To Do", "Backlog"],
            "in_progress": ["In Progress"],
            "in_review": ["Review"],
            "blocked": ["Blocked"],
            "done": ["Done", "Closed"]
        },
        "fields": {"requirement_id": "customfield_1", "risk_class": "customfield_2"},
        "trigger_label": "kod",
        "team_group": "kod-team"
    })
}

/// A change to the good mapping, and the error it must give.
type Case = (fn(&mut Value), &'static str);

fn err(v: &Value) -> String {
    Mapping::parse("acme.json", &v.to_string())
        .unwrap_err()
        .to_string()
}

#[test]
fn reads_a_mapping() {
    let m = Mapping::parse("acme.json", &good().to_string()).unwrap();
    assert_eq!(m.tracker, "acme");
    assert_eq!(m.standard("Backlog"), Some(State::Ready));
    assert_eq!(m.standard("Closed"), Some(State::Done));
    assert_eq!(m.standard("Review"), Some(State::InReview));
    // Exact names only: no case folding, no trimming.
    assert_eq!(m.standard("review"), None);
    assert_eq!(m.standard("Review "), None);
    assert_eq!(m.standard("Won't Do"), None);
    // The first listed is where a transition goes.
    assert_eq!(m.target(State::Ready), "To Do");
    assert_eq!(m.target(State::Done), "Done");
    assert_eq!(
        (m.requirement_id_field.as_str(), m.risk_class_field.as_str()),
        ("customfield_1", "customfield_2")
    );
    assert_eq!(
        (m.trigger_label.as_str(), m.team_group.as_str()),
        ("kod", "kod-team")
    );
}

#[test]
fn refuses_a_mapping_with_any_fault() {
    let cases: Vec<Case> = vec![
        (|v| *v = json!([]), "acme.json must be an object"),
        (|v| v["extra"] = json!(1), "unexpected key extra"),
        (
            |v| _ = v.as_object_mut().unwrap().remove("team_group"),
            "missing key team_group",
        ),
        (
            |v| _ = v["states"].as_object_mut().unwrap().remove("blocked"),
            "states: missing key blocked",
        ),
        (
            |v| v["states"]["cancelled"] = json!(["Cancelled"]),
            "states: unexpected key cancelled",
        ),
        (
            |v| v["states"]["done"] = json!([]),
            "states.done must be a non-empty list",
        ),
        (
            |v| v["states"]["done"] = json!("Done"),
            "states.done must be a non-empty list",
        ),
        (
            |v| v["states"]["done"] = json!(["Done", ""]),
            "states.done must be a non-empty name",
        ),
        (
            |v| v["states"]["done"] = json!([" Done"]),
            "no spaces at the ends",
        ),
        (
            |v| v["states"]["done"] = json!(["Review"]),
            "states.done: workflow state \"Review\" is mapped twice",
        ),
        (
            |v| v["states"]["ready"] = json!(["To Do", "To Do"]),
            "mapped twice",
        ),
        (
            |v| v["fields"]["risk_class"] = json!("customfield_1"),
            "are the same field",
        ),
        (
            |v| _ = v["fields"].as_object_mut().unwrap().remove("risk_class"),
            "fields: missing key risk_class",
        ),
        (
            |v| v["fields"]["requirement_id"] = json!(7),
            "fields.requirement_id must be a non-empty name",
        ),
        (
            |v| v["tracker"] = json!(""),
            "tracker must be a non-empty name",
        ),
        (
            |v| v["trigger_label"] = json!(null),
            "trigger_label must be a non-empty name",
        ),
    ];
    for (change, want) in cases {
        let mut v = good();
        change(&mut v);
        let e = err(&v);
        assert!(e.contains(want), "want {want:?}, got {e:?}");
    }
    let e = Mapping::parse("acme.json", "{").unwrap_err().to_string();
    assert!(e.contains("acme.json: not JSON"), "{e}");
}

#[test]
fn load_names_the_file_in_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acme.json");
    let e = Mapping::load(&path).unwrap_err().to_string();
    assert!(e.contains("reading") && e.contains("acme.json"), "{e}");
    let mut v = good();
    v["trigger_label"] = json!(" kod");
    std::fs::write(&path, v.to_string()).unwrap();
    let e = format!("{:#}", Mapping::load(&path).unwrap_err());
    assert!(e.contains("acme.json: trigger_label"), "{e}");
    std::fs::write(&path, good().to_string()).unwrap();
    assert_eq!(Mapping::load(&path).unwrap().tracker, "acme");
}
