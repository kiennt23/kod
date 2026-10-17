use kod_dispatcher::store;

fn set(c: &rusqlite::Connection, state: &str) -> Result<usize, String> {
    c.execute("UPDATE runs SET state = ?1 WHERE seq = 1", [state])
        .map_err(|e| e.to_string())
}

#[test]
fn run_states_only_move_forward() {
    let dir = tempfile::tempdir().unwrap();
    let c = store::open(&dir.path().join("d.db")).unwrap();
    c.execute(
        "INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'r1', 'queued')",
        [],
    )
    .unwrap();
    assert_eq!(set(&c, "running"), Ok(1));
    let e = set(&c, "queued").unwrap_err();
    assert!(e.contains("run states only move forward"), "{e}");
    assert!(set(&c, "running").is_err());
    assert_eq!(set(&c, "publishing"), Ok(1));
    assert_eq!(set(&c, "canceled"), Ok(1));
    for later in ["done", "running", "canceled", "blocked"] {
        assert!(set(&c, later).is_err(), "canceled -> {later}");
    }
    assert!(set(&c, "finished").is_err(), "an unknown state");
    let e = c
        .execute("UPDATE runs SET revision = 'r2' WHERE seq = 1", [])
        .unwrap_err()
        .to_string();
    assert!(e.contains("keeps its ticket and revision"), "{e}");
}

#[test]
fn a_ticket_has_at_most_one_live_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.db");
    let c = store::open(&path).unwrap();
    let add = |c: &rusqlite::Connection| {
        c.execute(
            "INSERT INTO runs (ticket, revision, state) VALUES ('FAKE-1', 'r1', 'queued')",
            [],
        )
    };
    add(&c).unwrap();
    assert!(add(&c).is_err());
    set(&c, "done").unwrap();
    add(&c).unwrap();
    // Reopening keeps the schema and the rows.
    drop(c);
    let c = store::open(&path).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}
