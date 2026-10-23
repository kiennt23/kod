use super::*;
use crate::broker::Broker;
use std::io::Read;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    config: Config,
    app: AppCredentials,
    lease_dir: std::path::PathBuf,
    base: String,
}
#[test]
#[ignore = "requires owner-authorized disposable GitHub App installation"]
fn live_broker_contract() -> Result<()> {
    ensure!(
        std::env::var("KOD_LIVE_BROKER").as_deref() == Ok("1"),
        "live broker authorization missing"
    );
    let path = std::path::PathBuf::from(
        std::env::var("KOD_GITHUB_BROKER_CONFIG").context("owner broker config missing")?,
    );
    super::super::store::ancestors(&path)?;
    super::super::store::checked(&path, false)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take((1 << 20) + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1 << 20, "owner config too large");
    let owner: Owner = serde_json::from_slice(&bytes).context("invalid owner config")?;
    let a = GitHub::new(owner.config, owner.app, owner.lease_dir)?;
    a.recover()?;
    a.verify_repository()?;
    a.register_run(RunPermit {
        run: "live-broker-contract".into(),
        ticket: "contract".into(),
        repo: a.config.repo.clone(),
        base: owner.base,
    })?;
    let token = a.mint("live-broker-contract", &a.config.repo)?;
    let read = a.http.request(
        reqwest::Method::GET,
        token.expose(),
        &a.repo_path(&[]),
        &[],
        None,
    );
    let cleanup = a.revoke("live-broker-contract");
    ensure!(
        read?.status == reqwest::StatusCode::OK,
        "live broker read failed"
    );
    cleanup?;
    ensure!(
        a.http
            .request(
                reqwest::Method::GET,
                token.expose(),
                &a.repo_path(&[]),
                &[],
                None
            )?
            .status
            == reqwest::StatusCode::UNAUTHORIZED,
        "revoked live credential remains usable"
    );
    Ok(())
}
