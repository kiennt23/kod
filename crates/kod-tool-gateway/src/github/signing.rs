use super::AppCredentials;
use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{rand::SystemRandom, rsa::KeyPair, signature::RSA_PKCS1_SHA256};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
/// Reads the owner-only App key through one descriptor, so the checked file is
/// the parsed file. Accepts PKCS#1 (GitHub's download) and PKCS#8 PEM.
fn key(path: &Path) -> Result<KeyPair> {
    super::store::ancestors(path)?;
    super::store::checked(path.parent().context("missing key parent")?, true)?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("App key unavailable")?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == super::store::owner() && m.mode() & 0o077 == 0,
        "private path must be owner-only and not a symlink"
    );
    let mut pem = String::new();
    (&mut f).take(64 << 10).read_to_string(&mut pem)?;
    let der = |label: &str| -> Option<Vec<u8>> {
        let body = pem
            .trim()
            .strip_prefix(&format!("-----BEGIN {label}-----"))?
            .strip_suffix(&format!("-----END {label}-----"))?;
        STANDARD
            .decode(body.split_whitespace().collect::<String>())
            .ok()
    };
    let pair = if let Some(d) = der("RSA PRIVATE KEY") {
        KeyPair::from_der(&d)
    } else if let Some(d) = der("PRIVATE KEY") {
        KeyPair::from_pkcs8(&d)
    } else {
        anyhow::bail!("invalid App key")
    };
    pair.map_err(|_| anyhow::anyhow!("invalid App key"))
}
pub(super) fn validate(path: &Path) -> Result<()> {
    key(path).map(drop)
}
pub(super) fn jwt(app: &AppCredentials, now: i64) -> Result<String> {
    let pair = key(&app.private_key)?;
    let claims = serde_json::json!({"iss":app.app_id,"iat":now.checked_sub(60).context("invalid JWT time")?,"exp":now.checked_add(540).context("invalid JWT time")?});
    let content = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
    );
    let mut sig = vec![0; pair.public().modulus_len()];
    pair.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        content.as_bytes(),
        &mut sig,
    )
    .map_err(|_| anyhow::anyhow!("App signing failed"))?;
    Ok(format!("{content}.{}", URL_SAFE_NO_PAD.encode(sig)))
}
