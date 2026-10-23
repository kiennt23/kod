use super::*;
use crate::broker::Broker;
use reqwest::Method;
use serde_json::json;
#[derive(Deserialize)]
struct Issued {
    token: String,
    expires_at: String,
    permissions: BTreeMap<String, String>,
    repositories: Option<Vec<protection::Repository>>,
}
impl GitHub {
    fn clear_issuance(&self, record: &store::Record) -> Result<()> {
        let mut s = self.lock()?;
        let mut current = s
            .records
            .get(&record.permit.run)
            .context("unregistered GitHub run")?
            .clone();
        ensure!(
            current.pending == record.pending,
            "GitHub issuance identity changed"
        );
        current.pending = None;
        self.store.save(&current)?;
        s.insert(current)
    }
    fn receive_token(&self, record: &store::Record, token: store::Token) -> Result<store::Record> {
        let mut s = self.lock()?;
        let mut current = s
            .records
            .get(&record.permit.run)
            .context("unregistered GitHub run")?
            .clone();
        ensure!(
            current.pending == record.pending,
            "GitHub issuance identity changed"
        );
        current.tokens.push(token);
        current.pending = None;
        self.store.save(&current)?;
        s.insert(current.clone())?;
        Ok(current)
    }
    pub(super) fn issue_record(&self, mut r: store::Record, now: i64) -> Result<Secret> {
        let generation = r.generation;
        let jwt = signing::jwt(&self.app, now)?;
        r.pending = Some(now.checked_add(3720).context("invalid lease time")?);
        r.updated_at = now;
        self.persist_lease(&r)?;
        let path = vec![
            "app".into(),
            "installations".into(),
            self.config.installation_id.to_string(),
            "access_tokens".into(),
        ];
        let reply=self.http.request(Method::POST,&jwt,&path,&[],Some(&json!({"repository_ids":[self.config.repository_id],"permissions":{"contents":"write","pull_requests":"write"}})))?;
        if reply.status.is_client_error() {
            self.clear_issuance(&r)?;
            anyhow::bail!("GitHub token issuance refused");
        }
        let issued: Issued = reply.json()?;
        ensure!(!issued.token.is_empty(), "empty GitHub credential");
        let expires = chrono::DateTime::parse_from_rfc3339(&issued.expires_at)
            .ok()
            .map(|d| d.timestamp());
        let valid_expiry =
            expires.is_some_and(|e| e > now.saturating_add(60) && e <= now.saturating_add(3660));
        let token = store::Token {
            secret: issued.token.clone(),
            expires: expires
                .filter(|_| valid_expiry)
                .unwrap_or(now.saturating_add(3720)),
            usable: false,
        };
        r = match self.receive_token(&r, token) {
            Ok(current) => current,
            Err(_) => {
                let revoked = self.revoke_token(&issued.token);
                if revoked && self.clear_issuance(&r).is_ok() {
                    anyhow::bail!("GitHub lease persistence failed; credential revoked");
                }
                anyhow::bail!("GitHub lease persistence failed; cleanup pending");
            }
        };
        if r.generation != generation {
            self.revoke(&r.permit.run)?;
            anyhow::bail!("GitHub issuance revoked");
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
        if verified.is_err() {
            self.revoke(&r.permit.run)?;
            anyhow::bail!("GitHub activation refused");
        }
        r.tokens
            .iter_mut()
            .find(|t| t.secret == issued.token)
            .context("missing issued lease")?
            .usable = true;
        if self.persist_lease(&r).is_err() {
            self.revoke(&r.permit.run)?;
            anyhow::bail!("GitHub issuance canceled or lease persistence failed");
        }
        Ok(Secret::new(issued.token))
    }
}
