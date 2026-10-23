use super::*;
use crate::{broker::Broker, forge::Forge};
use std::sync::atomic::Ordering;
#[test]
fn auth_lost_mint_reply_blocks_restart() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().lost_mint = true;
    assert!(a.mint("run-1", "acme/app").is_err());
    assert!(a.mint("run-1", "acme/app").is_err());
    drop(a);
    let a = f.adapter();
    assert!(a.recover().is_err());
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.requests("POST", "/app/installations/2/access_tokens"), 1);
    f.clock.fetch_add(3661, Ordering::SeqCst);
    a.recover().unwrap();
}
#[test]
fn auth_errors_redacted() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.remote.lock().unwrap().lost_mint = true;
    let error = a.mint("run-1", "acme/app").unwrap_err();
    assert!(!format!("{error:#}").contains("ghs_"));
}
#[test]
fn auth_old_tokens_cannot_be_reused_after_restart() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let t = a.mint("run-1", "acme/app").unwrap();
    drop(a);
    let a = f.adapter();
    assert!(a.commit(&t, &super::commits::text_commit(&f)).is_err());
    assert_eq!(f.mutations(), 0);
}
#[test]
fn auth_lease_write_failure() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        f.root().join("leases"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_clock_rollback_refused() {
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    f.clock.fetch_sub(1, Ordering::SeqCst);
    assert!(a.mint("run-1", "acme/app").is_err());
    assert_eq!(f.live(), 0);
}
#[test]
fn auth_private_files_and_corruption_refused() {
    let f = Fixture::start();
    std::fs::write(f.root().join("leases/bad.json"), b"{}").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        f.root().join("leases/bad.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(f.adapter_with(f.config()).is_err());
}

#[test]
fn auth_symlink_and_public_key_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::start();
    let key = f.root().join("key.pem");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(f.adapter_with(f.config()).is_err());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.root().join("alias.pem");
    symlink(&key, &link).unwrap();
    let mut app = f.app();
    app.private_key = link;
    assert!(
        GitHub::with_test_api(
            f.config(),
            app,
            f.root().join("leases"),
            &f.api,
            Arc::new(|| 1_800_000_000)
        )
        .is_err()
    );
}

#[test]
fn auth_revoke_accepts_authenticated_401_and_checks_disk_modes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::start();
    let a = f.adapter();
    a.register_run(f.permit()).unwrap();
    let token = a.mint("run-1", "acme/app").unwrap();
    for entry in std::fs::read_dir(f.root().join("leases")).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    f.remote.lock().unwrap().live.remove(token.expose());
    a.revoke("run-1").unwrap();
    drop(a);
    f.adapter().recover().unwrap();
}
