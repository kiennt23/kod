use super::*;
use crate::{broker::Broker, forge::Forge};
use std::{sync::atomic::Ordering, time::Duration};
#[test]
fn clock_skew_within_one_minute_does_not_reject_tokens() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().expiry_offset = 2;
    a.mint("run-1", "acme/app").unwrap();
    a.revoke("run-1").unwrap();
    assert_eq!(f.live(), 0);
}
#[test]
fn excessive_token_lifetime_is_still_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().expiry_offset = 61;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn lost_issuance_does_not_block_another_run() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let mut other = f.permit();
    other.run = "run-2".into();
    a.register_run(other).unwrap();
    f.remote.lock().unwrap().lost_mint = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    f.remote.lock().unwrap().lost_mint = false;
    a.mint("run-2", "acme/app").unwrap();
    assert!(a.mint("run-1", "acme/app").is_err());
}
#[test]
fn clock_rollback_never_prevents_revoke_or_recovery() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    a.mint("run-1", "acme/app").unwrap();
    f.clock.fetch_sub(2, Ordering::SeqCst);
    a.revoke("run-1").unwrap();
    assert_eq!(f.live(), 0);
    drop(a);
    f.adapter().recover().unwrap();
}
#[test]
fn revocation_does_not_wait_for_a_blocked_publish() {
    let f = Fixture::start();
    let a = Arc::new(f.adapter());
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    let c = super::commits::text_commit(&f);
    let (entered, wait) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    f.remote.lock().unwrap().before_blob = Some(Box::new(move || {
        entered.send(()).unwrap();
        resume.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    let worker_a = a.clone();
    let worker = std::thread::spawn(move || worker_a.commit(&t, &c));
    let reached = wait.recv_timeout(Duration::from_secs(5));
    let (done, result) = std::sync::mpsc::channel();
    let revoker = a.clone();
    let revoke = std::thread::spawn(move || {
        done.send(revoker.revoke("run-1")).unwrap();
    });
    let revoked = result.recv_timeout(Duration::from_secs(2));
    let _ = release.send(());
    let published = worker.join().unwrap();
    revoke.join().unwrap();
    reached.unwrap();
    revoked
        .expect("revoke waited for the blocked publish")
        .unwrap();
    assert!(published.is_err());
    assert_eq!(f.live(), 0);
    assert_eq!(f.mutations(), 0);
}

#[test]
fn settled_runs_are_pruned_at_startup_but_live_ones_are_kept() {
    // Given one settled run and one run still holding a token, both a week old.
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    super::publication::publish(&a, &f).unwrap();
    let mut live = f.permit();
    live.run = "run-2".into();
    a.register_run(live).unwrap();
    a.mint("run-2", "acme/app").unwrap();
    drop(a);
    let leases = || std::fs::read_dir(f.root().join("leases")).unwrap().count();
    assert_eq!(leases(), 3);
    f.clock.fetch_add(7 * 24 * 3600 + 1, Ordering::SeqCst);
    // When the gateway restarts.
    let a = f.adapter();
    // Then only the settled record is deleted; the live token is still revocable.
    assert_eq!(leases(), 2);
    assert!(a.mint("run-1", "acme/app").is_err());
    a.revoke_all().unwrap();
}
