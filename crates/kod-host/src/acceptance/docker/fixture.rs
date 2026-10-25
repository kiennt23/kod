use super::provider::Provider;
use crate::{
    config::FsConfig,
    service::{Service, production_parts},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

pub struct Fixture {
    pub config: Arc<FsConfig>,
    pub service: Option<Service>,
    pub provider: Provider,
    /// Acceptance: every model request reserves against this ledger before forwarding.
    pub suite: Option<Arc<crate::acceptance::SuiteBudget>>,
}
impl Fixture {
    pub async fn new(config: &FsConfig) -> Result<Self> {
        ensure!(
            config.github.product.repo == "acme/app" && config.github.tracker.repo == "acme/app",
            "fixture repository required"
        );
        ensure!(
            config.model.upstream == "https://connected-fixture.invalid",
            "fixture upstream required"
        );
        ensure!(
            crate::config::read_private(&config.model.api_key_file)? == b"connected-fixture-key",
            "fixture key required"
        );
        ensure!(
            crate::config::read_private(&config.github.webhook_secret_file)?
                == b"connected-fixture-webhook",
            "fixture webhook required"
        );
        config.validate_local()?;
        let provider = Provider::start(config.paths.mirror.clone()).await?;
        let mut cfg = config.clone();
        cfg.model.upstream = provider.api.clone();
        Ok(Self {
            config: Arc::new(cfg),
            service: None,
            provider,
            suite: None,
        })
    }
    pub async fn start(&mut self) -> Result<()> {
        ensure!(self.service.is_none(), "service already started");
        let cfg = self.config.clone();
        let api = self.provider.api.clone();
        let parts = tokio::task::spawn_blocking(move || {
            let mapping = kod_tracker::mapping::Mapping::load(&cfg.github.mapping)?;
            let tracker = Arc::new(kod_tracker::github::GitHub::with_test_api(
                cfg.github.tracker.clone(),
                mapping,
                "connected-fixture-token",
                "connected-fixture-webhook",
                &api,
            )?);
            production_parts(&cfg, tracker)
        })
        .await??;
        self.service = Some(
            Service::start_connected_fixture(
                self.config.clone(),
                parts,
                &self.provider.api,
                self.suite.clone(),
            )
            .await?,
        );
        Ok(())
    }
    pub async fn shutdown(&mut self) -> Result<()> {
        if let Some(service) = self.service.take() {
            service.shutdown().await?;
        }
        Ok(())
    }
    pub async fn delivery(
        &self,
        id: &str,
        action: &str,
        actor: &str,
    ) -> Result<reqwest::StatusCode> {
        let body = self.payload(action, actor)?;
        self.webhook(id, &body, true).await
    }
    pub fn payload(&self, action: &str, actor: &str) -> Result<Vec<u8>> {
        let issue = self
            .provider
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("provider lock"))?
            .issue
            .clone();
        Ok(serde_json::to_vec(
            &json!({"action":action,"repository":{"full_name":"acme/app"},"sender":{"id":9,"login":actor,"type":"User"},"issue":issue,"label":{"name":"kod"}}),
        )?)
    }
    pub async fn webhook(&self, id: &str, body: &[u8], valid: bool) -> Result<reqwest::StatusCode> {
        self.webhook_event("issues", id, body, valid).await
    }
    pub async fn webhook_event(
        &self,
        event: &str,
        id: &str,
        body: &[u8],
        valid: bool,
    ) -> Result<reqwest::StatusCode> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"connected-fixture-webhook")?;
        mac.update(body);
        let signature = if valid {
            format!("sha256={:x}", mac.finalize().into_bytes())
        } else {
            "sha256=invalid".into()
        };
        let ingress = self.service.as_ref().context("service missing")?.ingress;
        Ok(reqwest::Client::new()
            .post(format!("http://{ingress}/webhook"))
            .header("x-github-event", event)
            .header("x-github-delivery", id)
            .header("x-hub-signature-256", signature)
            .body(body.to_vec())
            .send()
            .await?
            .status())
    }
    pub async fn command(&self, command: Value) -> Result<reqwest::StatusCode> {
        let body = serde_json::to_vec(&command)?;
        let key = crate::config::read_private(&self.config.github.owner_key_file)?;
        let signature = kod_dispatcher::command::sign(&key, &body);
        self.command_signed(body, &signature).await
    }
    pub async fn command_signed(
        &self,
        body: Vec<u8>,
        signature: &str,
    ) -> Result<reqwest::StatusCode> {
        let ingress = self.service.as_ref().context("service missing")?.ingress;
        let reply = reqwest::Client::new()
            .post(format!("http://{ingress}/command"))
            .header("x-kod-signature", signature)
            .body(body)
            .send()
            .await?;
        let status = reply.status();
        if !status.is_success() {
            eprintln!(
                "command refused: {status} {}",
                reply.text().await.unwrap_or_default()
            );
        }
        Ok(status)
    }
    pub async fn terminal(&self) -> Result<(String, String)> {
        let end = tokio::time::Instant::now()
            + Duration::from_secs(self.config.sandbox.wall_seconds + 30);
        loop {
            let conn = kod_dispatcher::store::open(&self.config.paths.dispatcher_db)?;
            let mut st = conn.prepare("SELECT seq,state FROM runs ORDER BY seq DESC LIMIT 1")?;
            let found = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .next()
                .transpose()?;
            if let Some((seq, state)) = found.clone()
                && !matches!(state.as_str(), "queued" | "running" | "publishing")
            {
                return Ok((kod_dispatcher::run_id(seq), state));
            }
            ensure!(tokio::time::Instant::now() < end, "scenario deadline");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub fn audit(&self) -> Result<Vec<(u64, String)>> {
        let c = rusqlite::Connection::open(&self.config.paths.audit_db)?;
        let mut s = c.prepare("SELECT seq,payload FROM audit ORDER BY seq")?;
        let rows = s
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(n, p)| Ok((u64::try_from(n)?, p)))
            .collect()
    }
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
