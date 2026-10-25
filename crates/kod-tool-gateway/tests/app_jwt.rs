use kod_tool_gateway::github::AppCredentials;
use std::os::unix::fs::PermissionsExt;

#[test]
fn the_app_signer_is_exposed_as_an_opaque_secret_and_refuses_a_public_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = dir.path().canonicalize().unwrap().join("key.pem");
    assert!(
        std::process::Command::new("openssl")
            .args(["genrsa", "-traditional", "-out"])
            .arg(&key)
            .arg("2048")
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let app = AppCredentials {
        app_id: 7,
        private_key: key.clone(),
    };
    let jwt = app.jwt(1_800_000_000).unwrap();
    assert_eq!(jwt.expose().split('.').count(), 3);
    assert!(!format!("{jwt:?}").contains(jwt.expose()));
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(app.jwt(1_800_000_000).is_err());
}
