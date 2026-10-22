use kod_gates::{
    Artifacts,
    records::{HostRecords, Snapshot},
};
use kod_tracker::Tracker;
use std::collections::BTreeMap;

#[test]
fn host_records_are_reread_and_unknown_or_unavailable_inputs_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.json");
    let mut snapshot = Snapshot {
        story: "STORY-1".into(),
        requester: "alice".into(),
        required: vec!["design".into()],
        current: BTreeMap::from([("design".into(), "v1".into())]),
    };
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let records = HostRecords(path.clone());
    assert_eq!(records.get_issue("STORY-1").unwrap().reporter, "alice");
    assert_eq!(records.current("design").unwrap(), "v1");
    snapshot.current.insert("design".into(), "v2".into());
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert_eq!(records.current("design").unwrap(), "v2");
    assert!(records.current("unknown").is_err());
    assert!(records.get_issue("STORY-2").is_err());
    std::fs::remove_file(path).unwrap();
    assert!(records.current("design").is_err());
}
