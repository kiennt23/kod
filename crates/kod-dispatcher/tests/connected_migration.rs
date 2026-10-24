use kod_dispatcher::store;

#[test]
fn opening_an_already_migrated_store_does_not_wait_for_a_writer() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    c.execute_batch(
        "BEGIN IMMEDIATE; INSERT INTO runs(ticket,revision,state) VALUES('A','v1','running');",
    )
    .unwrap();
    let reader = store::open(&p).unwrap();
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    c.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn a_failed_migration_does_not_leave_half_the_connected_schema() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(include_str!("fixtures/pre-connected.sql"))
        .unwrap();
    c.execute_batch("CREATE TABLE one_uncertain_status(x); INSERT INTO runs(ticket,revision,state) VALUES('A','v1','running');").unwrap();
    drop(c);
    assert!(store::open(&p).is_err());
    let c = rusqlite::Connection::open(&p).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='run_bindings'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT state FROM runs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "running"
    );
}
#[test]
fn additive_migration_preserves_old_queue_control_and_cleanup() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(include_str!("fixtures/pre-connected.sql"))
        .unwrap();
    c.execute_batch("INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'A','v1','running');
        INSERT INTO deliveries(id,delivery,ticket,event,priority) VALUES('signed-id','delivery','A','{}',0);
        INSERT INTO followups(kind,target,arg,attempts,error) VALUES('stop','run-1','',7,'still stopping');
        INSERT INTO control(id,halted,by,epoch,kills) VALUES(1,1,'owner',5,3);").unwrap();
    drop(c);
    let c = store::open(&p).unwrap();
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT state FROM runs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "running"
    );
    assert_eq!(
        c.query_row("SELECT id FROM deliveries", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "signed-id"
    );
    assert_eq!(
        c.query_row("SELECT attempts FROM followups", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        c.query_row("SELECT epoch FROM control", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert!(c.execute("INSERT INTO model_requests(run_seq,request_id,reserved_tokens,reserved_cost) VALUES(999,'x','1','1')",[]).is_err());
}
#[test]
fn an_uncertain_status_create_stays_guarded_across_reopen() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    c.execute(
        "INSERT INTO tracker_write_intents(operation_id,story,kind) VALUES('first','A','status')",
        [],
    )
    .unwrap();
    drop(c);
    let c = store::open(&p).unwrap();
    assert!(c.execute("INSERT INTO tracker_write_intents(operation_id,story,kind) VALUES('second','A','status')",[]).is_err());
    c.execute("UPDATE tracker_write_intents SET outcome='posted',remote_id='123' WHERE operation_id='first'",[]).unwrap();
    c.execute(
        "INSERT INTO tracker_write_intents(operation_id,story,kind) VALUES('second','A','status')",
        [],
    )
    .unwrap();
}
