use super::*;
use crate::broker::Broker;

#[test]
fn auth_administration_permission_is_revoked() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().extra_permission = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_app_bypass_is_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().app_bypass = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_received_token_is_revoked_on_persistence_failure() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let leases = f.root().join("leases");
    f.remote.lock().unwrap().after_mint = Some(Box::new(move || {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(leases, std::fs::Permissions::from_mode(0o755)).unwrap();
    }));
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
    assert_eq!(f.requests("DELETE", "/installation/token"), 1);
}
#[test]
fn auth_restart_revokes_every_known_lease() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    a.mint("run-1", "acme/app").unwrap();
    a.mint("run-1", "acme/app").unwrap();
    assert_eq!(f.live(), 2);
    drop(a);
    let restarted = f.adapter();
    restarted.recover().unwrap();
    assert_eq!(f.live(), 0);
    assert_eq!(f.requests("DELETE", "/installation/token"), 2);
    restarted.revoke("never-started").unwrap();
}
#[test]
fn auth_failed_revoke_retains_lease() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    a.mint("run-1", "acme/app").unwrap();
    f.remote.lock().unwrap().fail_revoke = true;
    assert!(a.revoke("run-1").is_err());
    drop(a);
    let a = f.adapter();
    assert!(a.recover().is_err());
    assert_eq!(f.live(), 1);
    f.remote.lock().unwrap().fail_revoke = false;
    a.recover().unwrap();
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_disabled_rules_refuses_token() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().disabled = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
    assert_eq!(f.mutations(), 0);
}
#[test]
fn auth_widened_scope_revoked() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().widened = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_second_owner_denied() {
    let f = Fixture::start();
    let _a = f.adapter();
    assert!(f.adapter_with(f.config()).is_err());
}
