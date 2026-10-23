use super::*;
use crate::broker::Broker;
use reqwest::{Method, StatusCode};
use serde_json::json;
use store::{Record, Token};
#[derive(Deserialize)]
struct Issued {
    token: String,
    expires_at: String,
    permissions: BTreeMap<String, String>,
    repositories: Option<Vec<super::protection::Repository>>,
}
impl GitHub {
    pub fn recover(&self) -> Result<()> {
        self.revoke_all()
    }
    pub fn verify_repository(&self) -> Result<()> {
        self.register_run(RunPermit {
            run: "activation-probe".into(),
            ticket: "activation".into(),
            repo: self.config.repo.clone(),
            base: "0".repeat(40),
        })?;
        self.mint("activation-probe", &self.config.repo)?;
        self.revoke("activation-probe")
    }
    fn retire(&self, r: &mut Record, now: i64) -> Result<()> {
        let mut pending = Vec::new();
        for mut t in std::mem::take(&mut r.tokens) {
            t.usable = false;
            let removed = t.expires <= now
                || self
                    .http
                    .request(
                        Method::DELETE,
                        &t.secret,
                        &["installation".into(), "token".into()],
                        &[],
                        None,
                    )
                    .is_ok_and(|reply| {
                        matches!(
                            reply.status,
                            StatusCode::NO_CONTENT | StatusCode::UNAUTHORIZED
                        )
                    });
            if !removed {
                pending.push(t);
            }
        }
        r.tokens = pending;
        if r.pending.is_some_and(|until| until <= now) {
            r.pending = None;
        }
        r.updated_at = now;
        self.store.save(r)?;
        ensure!(
            r.tokens.is_empty() && r.pending.is_none(),
            "GitHub credential cleanup pending"
        );
        Ok(())
    }
    fn issue(&self, r: &mut Record, now: i64) -> Result<Secret> {
        ensure!(
            r.pending.is_none(),
            "GitHub issuance outcome unknown; recover before retry"
        );
        let jwt = signing::jwt(&self.app, now)?;
        r.pending = Some(now.checked_add(3660).context("invalid lease time")?);
        r.updated_at = now;
        self.store.save(r)?;
        let path = vec![
            "app".into(),
            "installations".into(),
            self.config.installation_id.to_string(),
            "access_tokens".into(),
        ];
        let reply=self.http.request(Method::POST,&jwt,&path,&[],Some(&json!({"repository_ids":[self.config.repository_id],"permissions":{"contents":"write","pull_requests":"write"}})))?;
        if reply.status.is_client_error() {
            r.pending = None;
            self.store.save(r)?;
            anyhow::bail!("GitHub token issuance refused");
        }
        let issued: Issued = reply.json()?;
        ensure!(!issued.token.is_empty(), "empty GitHub credential");
        let expires = chrono::DateTime::parse_from_rfc3339(&issued.expires_at)
            .ok()
            .map(|d| d.timestamp());
        let valid_expiry = expires.is_some_and(|e| e > now && e <= now.saturating_add(3600));
        r.tokens.push(Token {
            secret: issued.token.clone(),
            expires: expires
                .filter(|_| valid_expiry)
                .unwrap_or(now.saturating_add(3660)),
            usable: false,
        });
        if self.store.save(r).is_err() {
            let cleanup = self.retire(r, now);
            return Err(anyhow::anyhow!(if cleanup.is_ok() {
                "GitHub lease persistence failed; credential revoked"
            } else {
                "GitHub lease persistence failed; cleanup pending"
            }));
        }
        let verified = (|| -> Result<()> {
            ensure!(valid_expiry, "invalid GitHub credential expiry");
            ensure!(
                issued
                    .permissions
                    .get("contents")
                    .is_some_and(|v| v == "write")
                    && issued
                        .permissions
                        .get("pull_requests")
                        .is_some_and(|v| v == "write")
                    && issued.permissions.iter().all(|(k, v)| matches!(
                        (k.as_str(), v.as_str()),
                        ("contents", "write") | ("pull_requests", "write") | ("metadata", "read")
                    )),
                "overbroad GitHub permissions"
            );
            self.verify_scope(&issued.token, issued.repositories)?;
            self.protected(&issued.token)
        })();
        r.pending = None;
        if verified.is_err() {
            self.retire(r, now)?;
            anyhow::bail!("GitHub activation refused");
        }
        r.tokens.last_mut().context("missing issued lease")?.usable = true;
        if self.store.save(r).is_err() {
            self.retire(r, now)?;
            anyhow::bail!("GitHub lease persistence failed; credential revoked");
        }
        Ok(Secret::new(issued.token))
    }
}
impl Broker for GitHub {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        ensure!(repo == self.config.repo, "GitHub repository mismatch");
        let mut s = self.lock()?;
        let now = self.now(&mut s)?;
        ensure!(
            !s.records
                .values()
                .any(|r| r.pending.is_some() || r.tokens.iter().any(|t| !t.usable)),
            "GitHub issuance cleanup pending"
        );
        let r = s.records.get_mut(run).context("unregistered GitHub run")?;
        self.issue(r, now)
    }
    fn revoke(&self, run: &str) -> Result<()> {
        let mut s = self.lock()?;
        let now = self.now(&mut s)?;
        if let Some(r) = s.records.get_mut(run) {
            self.retire(r, now)?;
        }
        Ok(())
    }
    fn revoke_all(&self) -> Result<()> {
        let mut s = self.lock()?;
        let now = self.now(&mut s)?;
        let mut failed = false;
        for r in s.records.values_mut() {
            if self.retire(r, now).is_err() {
                failed = true;
            }
        }
        ensure!(!failed, "GitHub credential cleanup pending");
        Ok(())
    }
}
