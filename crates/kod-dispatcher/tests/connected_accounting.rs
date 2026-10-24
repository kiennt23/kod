use kod_dispatcher::store::{self, ModelCharge, ModelRequest};

#[test]
fn a_failed_settlement_keeps_the_reservation_charged() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    bound_run(&c);
    let request = ModelRequest {
        run: "run-1",
        id: "request-1",
        reservation: ModelCharge {
            tokens: 20,
            pico_usd: 30,
        },
    };
    store::reserve_model(&c, &request).unwrap();
    c.execute_batch("CREATE TRIGGER fail_settlement BEFORE UPDATE ON model_requests BEGIN SELECT RAISE(ABORT,'disk failure'); END;").unwrap();
    assert!(store::settle_model(&c, &request, ModelCharge::default()).is_err());
    drop(c);
    assert_eq!(
        store::model_charge(&store::open(&p).unwrap(), "run-1").unwrap(),
        request.reservation
    );
}

fn bound_run(c: &rusqlite::Connection) {
    c.execute(
        "INSERT INTO runs(seq,ticket,revision,state) VALUES(1,'acme/app#1','v1','running')",
        [],
    )
    .unwrap();
    store::bind_once(c, "run-1", &serde_json::json!({"version":1})).unwrap();
}
#[test]
fn unsettled_reservations_survive_restart_without_unsigned_truncation() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    bound_run(&c);
    let request = ModelRequest {
        run: "run-1",
        id: "request-1",
        reservation: ModelCharge {
            tokens: u64::MAX,
            pico_usd: u64::MAX,
        },
    };
    store::reserve_model(&c, &request).unwrap();
    store::reserve_model(&c, &request).unwrap();
    drop(c);
    let c = store::open(&p).unwrap();
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        request.reservation
    );
    let smaller = ModelCharge {
        tokens: 20,
        pico_usd: 123,
    };
    store::settle_model(&c, &request, smaller).unwrap();
    store::reserve_model(&c, &request).unwrap();
    assert_eq!(store::model_charge(&c, "run-1").unwrap(), smaller);
    store::settle_model(&c, &request, smaller).unwrap();
    assert!(
        store::settle_model(
            &c,
            &request,
            ModelCharge {
                tokens: 0,
                pico_usd: 0
            }
        )
        .is_err()
    );
}
#[test]
fn a_changed_request_cannot_replace_its_reservation() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    bound_run(&c);
    let request = ModelRequest {
        run: "run-1",
        id: "request-1",
        reservation: ModelCharge {
            tokens: 20,
            pico_usd: 30,
        },
    };
    store::reserve_model(&c, &request).unwrap();
    let changed = ModelRequest {
        reservation: ModelCharge {
            tokens: 1,
            pico_usd: 1,
        },
        ..request
    };
    assert!(store::reserve_model(&c, &changed).is_err());
    assert_eq!(
        store::model_charge(&c, "run-1").unwrap(),
        request.reservation
    );
}
#[test]
fn failed_reservation_write_leaves_no_partially_charged_request() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let c = store::open(&p).unwrap();
    bound_run(&c);
    c.execute_batch("CREATE TRIGGER fail_reservation BEFORE INSERT ON model_requests BEGIN SELECT RAISE(ABORT,'disk failure'); END;").unwrap();
    let request = ModelRequest {
        run: "run-1",
        id: "request-1",
        reservation: ModelCharge {
            tokens: 20,
            pico_usd: 30,
        },
    };
    assert!(store::reserve_model(&c, &request).is_err());
    drop(c);
    assert_eq!(
        store::model_charge(&store::open(&p).unwrap(), "run-1").unwrap(),
        ModelCharge {
            tokens: 0,
            pico_usd: 0
        }
    );
}
#[test]
fn ended_runs_cannot_reserve_but_can_settle_inflight_usage() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    bound_run(&c);
    let request = ModelRequest {
        run: "run-1",
        id: "request-1",
        reservation: ModelCharge {
            tokens: 20,
            pico_usd: 30,
        },
    };
    store::reserve_model(&c, &request).unwrap();
    c.execute("UPDATE runs SET state='canceled' WHERE seq=1", [])
        .unwrap();
    assert!(
        store::reserve_model(
            &c,
            &ModelRequest {
                id: "request-2",
                ..request
            }
        )
        .is_err()
    );
    store::settle_model(
        &c,
        &request,
        ModelCharge {
            tokens: 10,
            pico_usd: 15,
        },
    )
    .unwrap();
}
#[test]
fn unbound_runs_and_invalid_numbers_cannot_authorize_spend() {
    let d = tempfile::tempdir().unwrap();
    let c = store::open(&d.path().join("db")).unwrap();
    bound_run(&c);
    let request = ModelRequest {
        run: "run-2",
        id: "request-1",
        reservation: ModelCharge {
            tokens: 1,
            pico_usd: 1,
        },
    };
    assert!(store::reserve_model(&c, &request).is_err());
    c.execute("INSERT INTO model_requests(run_seq,request_id,reserved_tokens,reserved_cost) VALUES(1,'corrupt','-1','2')",[]).unwrap();
    assert!(store::model_charge(&c, "run-1").is_err());
}
