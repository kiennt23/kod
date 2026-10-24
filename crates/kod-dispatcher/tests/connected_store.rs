mod common;
use common::*;
use kod_dispatcher::store;
use serde_json::json;

#[test]
fn restart_stops_old_execution_before_looking_up_a_publishing_pr() {
    let h = harness();
    h.queued("FAKE-1");
    h.sql("INSERT INTO runs(ticket,revision,state) VALUES('FAKE-2','v1','publishing')");
    h.reopen();
    let calls = h.gw.calls();
    let stopped = calls.iter().position(|c| c == "stop run-1").unwrap();
    let lookup = calls
        .iter()
        .position(|c| c == "find_published run-2 FAKE-2")
        .unwrap();
    assert!(stopped < lookup, "{calls:?}");
}

#[test]
fn a_binding_cannot_be_changed_or_deleted_through_sql() {
    let dir = tempfile::tempdir().unwrap();
    let c = store::open(&dir.path().join("db")).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'A','v1','running')",
        [],
    )
    .unwrap();
    store::bind_once(&c, "run-1", &json!({"version":1})).unwrap();
    assert!(
        c.execute("UPDATE run_bindings SET binding='{}' WHERE run_seq=1", [])
            .is_err()
    );
    assert!(
        c.execute("DELETE FROM run_bindings WHERE run_seq=1", [])
            .is_err()
    );
}

#[test]
fn a_run_binding_is_immutable_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dispatcher.db");
    let c = store::open(&path).unwrap();
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#1','v1','running')",
        [],
    )
    .unwrap();
    let binding = json!({"version":1,"base":"a".repeat(40)});
    store::bind_once(&c, "run-1", &binding).unwrap();
    store::bind_once(&c, "run-1", &binding).unwrap();
    assert!(store::bind_once(&c, "run-1", &json!({"version":1,"base":"b".repeat(40)})).is_err());
    drop(c);
    let c = store::open(&path).unwrap();
    assert_eq!(store::binding(&c, "run-1").unwrap(), binding);
}
#[test]
fn bindings_refuse_unknown_runs_and_invalid_versions() {
    let dir = tempfile::tempdir().unwrap();
    let c = store::open(&dir.path().join("db")).unwrap();
    for id in [
        "run-0",
        "run--1",
        "run-01",
        "run-+1",
        "run-1 ",
        "run-9999999999999999999999999",
    ] {
        assert!(store::seq_of(id).is_none(), "accepted {id}");
    }
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'A','v1','running')",
        [],
    )
    .unwrap();
    for value in [
        json!(null),
        json!([]),
        json!({}),
        json!({"version":2}),
        json!({"version":"1"}),
    ] {
        assert!(store::bind_once(&c, "run-1", &value).is_err());
    }
    assert!(store::bind_once(&c, "run-2", &json!({"version":1})).is_err());
    assert!(store::binding(&c, "run-1").is_err());
}
#[test]
fn retiring_active_runs_is_atomic_idempotent_and_leaves_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = store::open(&dir.path().join("db")).unwrap();
    for (seq, state) in [
        (1, "queued"),
        (2, "running"),
        (3, "publishing"),
        (4, "done"),
    ] {
        c.execute(
            "INSERT INTO runs(seq,ticket,revision,state) VALUES(?1,?2,'v1',?3)",
            rusqlite::params![seq, format!("A-{seq}"), state],
        )
        .unwrap();
    }
    store::retire_active(&mut c, "host shutting down").unwrap();
    store::retire_active(&mut c, "repeat").unwrap();
    let states: Vec<String> = c
        .prepare("SELECT state FROM runs ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(states, ["needs_human", "needs_human", "publishing", "done"]);
    for kind in ["revoke", "stop"] {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM followups WHERE kind=?1 AND done=0",
                [kind],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }
}
#[test]
fn failed_retirement_does_not_commit_a_partial_state_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = store::open(&dir.path().join("db")).unwrap();
    c.execute_batch("INSERT INTO runs(ticket,revision,state) VALUES('A','v1','running');
        CREATE TRIGGER fail_cleanup BEFORE INSERT ON followups WHEN NEW.kind='stop' BEGIN SELECT RAISE(ABORT,'injected cleanup write failure'); END;").unwrap();
    assert!(store::retire_active(&mut c, "shutdown").is_err());
    assert_eq!(
        c.query_row("SELECT state FROM runs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "running"
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM followups", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn restarting_never_resumes_queued_or_running_execution() {
    let h = harness();
    h.queued("FAKE-1");
    h.sql("INSERT INTO runs(ticket,revision,state) VALUES('FAKE-2','v1','running')");
    let reopened = h.reopen();
    assert_eq!(
        h.states(),
        [
            ("run-1".into(), "needs_human"),
            ("run-2".into(), "needs_human")
        ]
    );
    assert!(
        h.gw.calls()
            .iter()
            .all(|c| !c.starts_with("start ") && !c.starts_with("publish "))
    );
    assert_eq!(h.gw.revoked(), ["run-1", "run-2"]);
    assert!(h.gw.calls().contains(&"stop run-1".into()));
    assert!(h.gw.calls().contains(&"stop run-2".into()));
    assert!(reopened.stopped("run-1").unwrap());
    assert!(reopened.stopped("run-2").unwrap());
}
