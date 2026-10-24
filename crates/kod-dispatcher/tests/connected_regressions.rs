use kod_dispatcher::store::{self, ModelCharge, ModelRequest};
use serde_json::json;

fn run(c: &rusqlite::Connection, seq: i64, state: &str) {
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(?1,?2,'v1',?3)",
        rusqlite::params![seq, format!("A-{seq}"), state],
    )
    .unwrap();
}

#[test]
fn unsupported_schema_versions_are_rejected() {
    for version in [-1, 2] {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("db");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.pragma_update(None, "user_version", version).unwrap();
        drop(c);
        assert!(store::open(&p).is_err(), "accepted version {version}");
    }
}

#[test]
fn only_active_runs_accept_new_bindings_but_ended_retries_are_idempotent() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    let binding = json!({"version":1});
    for (seq, state) in [
        (1, "queued"),
        (2, "running"),
        (3, "publishing"),
        (4, "done"),
        (5, "blocked"),
        (6, "needs_human"),
        (7, "over_budget"),
        (8, "canceled"),
    ] {
        run(&c, seq, state);
        assert_eq!(
            store::bind_once(&c, &format!("run-{seq}"), &binding).is_ok(),
            seq <= 2
        );
    }
    c.execute("UPDATE runs SET state='needs_human' WHERE seq=1", [])
        .unwrap();
    store::bind_once(&c, "run-1", &binding).unwrap();
    assert!(store::bind_once(&c, "run-1", &json!({"version":1,"changed":true})).is_err());
    let error = store::bind_once(&c, "run-99", &binding).unwrap_err();
    assert!(error.to_string().contains("unknown run"), "{error:#}");
}

#[test]
fn ended_reservation_retries_preserve_settlement_and_reject_changes() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    run(&c, 1, "running");
    store::bind_once(&c, "run-1", &json!({"version":1})).unwrap();
    let request = ModelRequest {
        run: "run-1",
        id: "r",
        reservation: ModelCharge {
            tokens: 20,
            pico_usd: 30,
        },
    };
    store::reserve_model(&c, &request).unwrap();
    c.execute("UPDATE runs SET state='canceled' WHERE seq=1", [])
        .unwrap();
    store::reserve_model(&c, &request).unwrap();
    store::settle_model(&c, &request, ModelCharge::default()).unwrap();
    store::reserve_model(&c, &request).unwrap();
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        ModelCharge::default()
    );
    assert!(
        store::reserve_model(
            &c,
            &ModelRequest {
                reservation: ModelCharge::default(),
                ..request
            }
        )
        .is_err()
    );
    assert!(
        store::reserve_model(
            &c,
            &ModelRequest {
                id: "new",
                ..request
            }
        )
        .is_err()
    );
}

#[test]
fn connected_writes_join_the_callers_transaction() {
    let d = tempfile::tempdir().unwrap();
    let mut c = store::open(&d.path().join("db")).unwrap();
    for commit in [false, true] {
        let tx = c.transaction().unwrap();
        run(&tx, 1, "running");
        store::bind_once(&tx, "run-1", &json!({"version":1})).unwrap();
        let request = ModelRequest {
            run: "run-1",
            id: "r",
            reservation: ModelCharge {
                tokens: 20,
                pico_usd: 30,
            },
        };
        store::reserve_model(&tx, &request).unwrap();
        store::settle_model(
            &tx,
            &request,
            ModelCharge {
                tokens: 5,
                pico_usd: 6,
            },
        )
        .unwrap();
        assert!(store::bind_once(&tx, "run-1", &json!({"version":1,"changed":true})).is_err());
        assert!(
            store::reserve_model(
                &tx,
                &ModelRequest {
                    reservation: ModelCharge::default(),
                    ..request
                }
            )
            .is_err()
        );
        assert!(store::settle_model(&tx, &request, ModelCharge::default()).is_err());
        if commit {
            tx.commit().unwrap();
        } else {
            tx.rollback().unwrap();
        }
        let count: i64 = c
            .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, i64::from(commit));
    }
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        ModelCharge {
            tokens: 5,
            pico_usd: 6
        }
    );
}

#[test]
fn actual_overruns_remain_durable_and_aggregate_charges_saturate() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    run(&c, 1, "running");
    store::bind_once(&c, "run-1", &json!({"version":1})).unwrap();
    for id in ["first", "second"] {
        let request = ModelRequest {
            run: "run-1",
            id,
            reservation: ModelCharge {
                tokens: 1,
                pico_usd: 1,
            },
        };
        store::reserve_model(&c, &request).unwrap();
        store::settle_model(
            &c,
            &request,
            ModelCharge {
                tokens: u64::MAX,
                pico_usd: u64::MAX,
            },
        )
        .unwrap();
    }
    drop(c);
    let c = store::open(&p).unwrap();
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        ModelCharge {
            tokens: u64::MAX,
            pico_usd: u64::MAX
        }
    );
    let usage: String = c
        .query_row(
            "SELECT settled_usage FROM model_requests WHERE request_id='second'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&usage).unwrap(),
        json!({"tokens":u64::MAX,"pico_usd":u64::MAX})
    );
}

#[test]
fn unbound_history_has_zero_charge_but_unknown_and_corrupt_runs_fail() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    run(&c, 1, "done");
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        ModelCharge::default()
    );
    assert!(store::model_charge(&c, "run-2").is_err());
    assert!(store::model_charge(&c, "run-01").is_err());
    c.execute("INSERT INTO model_requests(run_seq,request_id,reserved_tokens,reserved_cost) VALUES(1,'corrupt','1','1')", []).unwrap();
    assert!(store::model_charge(&c, "run-1").is_err());
}
