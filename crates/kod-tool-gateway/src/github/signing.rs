use super::AppCredentials;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
};
pub(super) fn validate(key: &Path) -> Result<()> {
    super::store::ancestors(key)?;
    super::store::checked(key, false)?;
    super::store::checked(key.parent().context("missing key parent")?, true)?;
    ensure!(
        Command::new("openssl")
            .arg("version")
            .output()
            .context("openssl unavailable")?
            .status
            .success(),
        "openssl unavailable"
    );
    Ok(())
}
pub(super) fn jwt(app: &AppCredentials, now: i64) -> Result<String> {
    validate(&app.private_key)?;
    let before = super::store::checked(&app.private_key, false)?;
    let claims = serde_json::json!({"iss":app.app_id,"iat":now.checked_sub(60).context("invalid JWT time")?,"exp":now.checked_add(540).context("invalid JWT time")?});
    let content = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
    );
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(&app.private_key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("App signing unavailable")?;
    let written = child
        .stdin
        .take()
        .context("App signing input unavailable")?
        .write_all(content.as_bytes());
    let output = child.wait_with_output().context("App signing failed")?;
    written.context("App signing failed")?;
    ensure!(output.status.success(), "App signing failed");
    let after = super::store::checked(&app.private_key, false)?;
    ensure!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec()
        ) == (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec()
        ),
        "App key changed during signing"
    );
    Ok(format!(
        "{content}.{}",
        URL_SAFE_NO_PAD.encode(output.stdout)
    ))
}
