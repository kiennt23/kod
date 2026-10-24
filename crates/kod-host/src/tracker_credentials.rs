//! The tracker's own GitHub installation token: only the tracker repository, `issues:write`
//! (plus implicit `metadata:read`), never the publish token and never another repository.
use crate::config::FsConfig;
use anyhow::{Context, Result, bail, ensure};
use kod_tool_gateway::github::AppCredentials;
use reqwest::{Method, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// A token is renewed once it is this close to expiry.
const MARGIN: i64 = 300;

#[derive(Clone, Serialize, Deserialize)]
struct Lease {
    token: String,
    expires: i64,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct TrackerCredentials {
    app: AppCredentials,
    installation: u64,
    repository_id: u64,
    repo: String,
    bot_id: u64,
    dir: PathBuf,
    api: String,
    now: Clock,
    client: Client,
    lease: Mutex<Option<Lease>>,
}

impl TrackerCredentials {
    pub fn new(config: &FsConfig) -> Result<Arc<Self>> {
        Self::build(
            config,
            "https://api.github.com",
            Arc::new(crate::service::system_now),
        )
    }

    /// Loopback API and an injected clock, for tests.
    #[cfg(feature = "test-support")]
    pub fn with_api(config: &FsConfig, api: &str, now: Clock) -> Result<Arc<Self>> {
        let u = reqwest::Url::parse(api)?;
        ensure!(
            u.scheme() == "http"
                && u.host_str()
                    .is_some_and(|h| h == "127.0.0.1" || h == "[::1]")
                && u.port().is_some(),
            "test API must be literal loopback"
        );
        Self::build(config, api, now)
    }

    fn build(config: &FsConfig, api: &str, now: Clock) -> Result<Arc<Self>> {
        let dir = config.paths.credential_dir.join("tracker");
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let g = &config.github;
        Ok(Arc::new(Self {
            app: g.app.clone(),
            installation: g.tracker_installation_id,
            repository_id: g.tracker_repository_id,
            repo: g.tracker.repo.clone(),
            bot_id: g.tracker.comment_author_id,
            dir,
            api: api.trim_end_matches('/').into(),
            now,
            client: Client::builder()
                .user_agent("kod-host")
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
            lease: Mutex::new(None),
        }))
    }

    fn lease_path(&self) -> PathBuf {
        self.dir.join("lease.json")
    }

    fn call(
        &self,
        method: Method,
        bearer: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|_| anyhow::anyhow!("invalid GitHub credential"))?;
        auth.set_sensitive(true);
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.api))
            .header(reqwest::header::AUTHORIZATION, auth)
            .header("X-GitHub-Api-Version", "2026-03-10")
            .header("Accept", "application/vnd.github+json");
        if let Some(body) = body {
            req = req.json(body);
        }
        let reply = req
            .send()
            .map_err(|_| anyhow::anyhow!("GitHub transport failed"))?;
        let status = reply.status().as_u16();
        let bytes = reply
            .bytes()
            .map_err(|_| anyhow::anyhow!("GitHub response read failed"))?;
        ensure!(bytes.len() <= 1 << 20, "GitHub response too large");
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    fn app_get(&self, jwt: &str, path: &str) -> Result<Value> {
        let (status, v) = self.call(Method::GET, jwt, path, None)?;
        ensure!(status == 200, "GitHub provider status {status}");
        Ok(v)
    }

    /// Installation, App and bot identity, with the App JWT.
    fn verify_identity(&self, jwt: &str) -> Result<()> {
        let app = self.app_get(jwt, "/app")?;
        ensure!(app["id"] == self.app.app_id, "GitHub App identity mismatch");
        let slug = app["slug"].as_str().context("GitHub App has no slug")?;
        ensure!(
            slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid GitHub App slug"
        );
        let inst = self.app_get(jwt, &format!("/app/installations/{}", self.installation))?;
        ensure!(
            inst["id"] == self.installation && inst["app_id"] == self.app.app_id,
            "GitHub installation identity mismatch"
        );
        let bot = self.app_get(jwt, &format!("/users/{slug}%5Bbot%5D"))?;
        ensure!(
            bot["id"] == self.bot_id && bot["type"] == "Bot",
            "GitHub bot identity mismatch"
        );
        Ok(())
    }

    fn revoke_token(&self, token: &str) -> bool {
        self.call(Method::DELETE, token, "/installation/token", None)
            .is_ok_and(|(s, _)| matches!(s, 204 | 401))
    }

    fn save(&self, lease: &Lease) -> Result<()> {
        let tmp = self.dir.join("lease.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        std::io::Write::write_all(&mut f, &serde_json::to_vec(lease)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, self.lease_path())?;
        Ok(())
    }

    /// Revokes whatever a previous process left, then issues and verifies a fresh token.
    pub fn start(&self) -> Result<()> {
        let mut slot = self
            .lease
            .lock()
            .map_err(|_| anyhow::anyhow!("lease lock poisoned"))?;
        if let Ok(bytes) = std::fs::read(self.lease_path()) {
            let stale: Lease = serde_json::from_slice(&bytes).context("invalid tracker lease")?;
            ensure!(
                self.revoke_token(&stale.token),
                "stale tracker credential cleanup pending"
            );
            std::fs::remove_file(self.lease_path())?;
        }
        *slot = Some(self.mint()?);
        Ok(())
    }

    fn mint(&self) -> Result<Lease> {
        let now = (self.now)();
        let jwt = self.app.jwt(now)?;
        let jwt = jwt.expose();
        self.verify_identity(jwt)?;
        let (status, v) = self.call(
            Method::POST,
            jwt,
            &format!("/app/installations/{}/access_tokens", self.installation),
            Some(&json!({"repository_ids": [self.repository_id], "permissions": {"issues": "write"}})),
        )?;
        ensure!(
            status == 201 || status == 200,
            "GitHub token issuance refused"
        );
        let token = v["token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .context("empty GitHub credential")?;
        let expires = v["expires_at"]
            .as_str()
            .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
            .map(|d| d.timestamp())
            .filter(|e| *e > now + 60 && *e <= now + 3660);
        let lease = Lease {
            token: token.into(),
            expires: expires.unwrap_or(now),
        };
        // Persist before use, so a crash leaves a lease the next startup revokes.
        self.save(&lease).inspect_err(|_| {
            self.revoke_token(token);
        })?;
        let verified = (|| -> Result<()> {
            ensure!(expires.is_some(), "invalid GitHub credential expiry");
            let perms = v["permissions"]
                .as_object()
                .context("missing permissions")?;
            ensure!(
                perms.get("issues") == Some(&json!("write"))
                    && perms.iter().all(|(k, p)| matches!(
                        (k.as_str(), p.as_str()),
                        ("issues", Some("write")) | ("metadata", Some("read"))
                    )),
                "overbroad GitHub permissions"
            );
            let only = |repos: &Value| {
                repos.as_array().is_some_and(|r| {
                    r.len() == 1
                        && r[0]["id"] == self.repository_id
                        && r[0]["full_name"] == self.repo
                })
            };
            ensure!(only(&v["repositories"]), "GitHub repository scope mismatch");
            let (s, r) = self.call(Method::GET, token, "/installation/repositories", None)?;
            ensure!(
                s == 200 && r["total_count"] == 1 && only(&r["repositories"]),
                "GitHub repository scope mismatch"
            );
            Ok(())
        })();
        if let Err(e) = verified {
            if self.revoke_token(token) {
                std::fs::remove_file(self.lease_path())?;
            }
            return Err(e);
        }
        Ok(lease)
    }

    /// The current token, renewed first when it is near expiry. The same instance serves every
    /// request, so the tracker `GitHub` is never rebuilt (and its create guard never reset).
    pub fn token(&self) -> Result<String> {
        let mut slot = self
            .lease
            .lock()
            .map_err(|_| anyhow::anyhow!("lease lock poisoned"))?;
        let current = slot.clone().context("tracker credentials not started")?;
        if current.expires - (self.now)() > MARGIN {
            return Ok(current.token);
        }
        let next = self.mint()?;
        // ponytail: a failed revoke of the superseded token leaves it to expire within the hour.
        self.revoke_token(&current.token);
        let token = next.token.clone();
        *slot = Some(next);
        Ok(token)
    }

    pub fn token_source(self: &Arc<Self>) -> kod_tracker::github::TokenSource {
        let me = self.clone();
        Arc::new(move || me.token())
    }

    /// Revokes the lease; a failure keeps it on disk for the next startup.
    pub fn shutdown(&self) -> Result<()> {
        let mut slot = self
            .lease
            .lock()
            .map_err(|_| anyhow::anyhow!("lease lock poisoned"))?;
        let Some(lease) = slot.clone() else {
            return Ok(());
        };
        if !self.revoke_token(&lease.token) {
            bail!("tracker credential cleanup pending");
        }
        std::fs::remove_file(self.lease_path())?;
        *slot = None;
        Ok(())
    }
}
