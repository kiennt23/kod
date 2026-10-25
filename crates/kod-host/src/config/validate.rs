use super::files::{canonical, database, private_dir};
use super::*;
use kod_llm_proxy::config::{parse_limit, parse_paths, parse_prices, parse_runs};
use std::collections::HashSet;

fn text(path: &Path) -> Result<String> {
    Ok(String::from_utf8(read_local(path)?)?)
}
fn digest(value: &str) -> bool {
    value.rsplit_once("@sha256:").is_some_and(|(name, hash)| {
        !name.is_empty()
            && !name.contains('@')
            && hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
impl FsConfig {
    /// Only the directories and databases: enough to trust durable identities for safety cleanup.
    pub fn validate_paths(&self) -> Result<()> {
        let p = &self.paths;
        let run = private_dir(&p.run_root)?;
        let credentials = private_dir(&p.credential_dir)?;
        let mirror = canonical(&p.mirror)?;
        let policy = canonical(&p.policy_dir)?;
        ensure!(
            mirror.is_dir() && policy.is_dir(),
            "mirror and policy_dir must be directories"
        );
        ensure!(
            !run.starts_with(&credentials) && !credentials.starts_with(&run),
            "run and credential roots overlap"
        );
        let dispatcher_db = database(&p.dispatcher_db)?;
        let audit_db = database(&p.audit_db)?;
        ensure!(
            dispatcher_db != audit_db,
            "dispatcher_db and audit_db must differ"
        );
        for db in [&dispatcher_db, &audit_db] {
            ensure!(
                !db.starts_with(&run) && !db.starts_with(&mirror),
                "database is below agent-visible directories"
            );
        }
        Ok(())
    }
    pub fn validate_local(&self) -> Result<()> {
        self.validate_paths()?;
        if let Some(acceptance) = &self.acceptance {
            acceptance.validate(self)?;
        }
        let p = &self.paths;
        let policy = canonical(&p.policy_dir)?;
        let g = &self.github;
        g.product.validate()?;
        ensure!(
            g.app.app_id > 0
                && g.tracker_repository_id > 0
                && g.tracker_installation_id > 0
                && g.tracker.comment_author_id > 0,
            "missing GitHub App or tracker identity"
        );
        let parts: Vec<_> = g.tracker.repo.split('/').collect();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|s| !s.is_empty()
                    && !matches!(*s, "." | "..")
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
            "invalid tracker repository"
        );
        ensure!(
            !g.agent_name.trim().is_empty()
                && !g.agent_name.chars().any(char::is_control)
                && g.agent_email.contains('@')
                && !g.agent_email.chars().any(char::is_whitespace),
            "invalid agent identity"
        );
        for path in [
            &g.app.private_key,
            &g.webhook_secret_file,
            &g.owner_key_file,
            &self.model.api_key_file,
        ]
        .into_iter()
        .chain(g.backup_key_file.iter())
        {
            ensure!(
                !read_private(path)?.is_empty(),
                "secret file must not be empty"
            );
        }
        let dispatcher = text(&self.dispatcher_config)?;
        strict_json::<serde_json::Value>(dispatcher.as_bytes())?;
        let dispatcher = kod_dispatcher::config::Config::parse(&dispatcher)?;
        ensure!(
            dispatcher.repo == g.product.repo,
            "product and dispatcher repository mismatch"
        );
        ensure!(
            dispatcher.backup.is_some() == g.backup_key_file.is_some(),
            "backup identity and backup_key_file must agree"
        );
        kod_tracker::mapping::Mapping::parse("github.mapping", &text(&g.mapping)?)?;
        crate::artifacts::Artifacts::new(p.approval_manifest.clone(), &policy)?;
        for entry in std::fs::read_dir(&policy)? {
            let path = entry?.path();
            match path.extension().and_then(|s| s.to_str()) {
                Some("json") => {
                    strict_json::<serde_json::Value>(&read_local(&path)?)?;
                }
                Some("rego") => {
                    read_local(&path)?;
                }
                _ => {}
            }
        }
        kod_policy::Policy::load(&policy)?;
        self.validate_model()?;
        let s = &self.sandbox;
        ensure!(
            digest(&s.agent_image) && digest(&s.collector_image),
            "sandbox image must have exact sha256 digest"
        );
        ensure!(
            s.cpus.is_finite() && s.cpus > 0.0 && !s.language.trim().is_empty(),
            "invalid sandbox CPU or language"
        );
        let disk = kod_eval::case::parse_size(&s.disk)
            .filter(|n| *n > 0)
            .context("invalid sandbox disk")?;
        let memory = kod_eval::case::parse_size(&s.memory).context("invalid sandbox memory")?;
        let home = kod_eval::case::parse_size(kod_eval::sandbox::HOME_SIZE)
            .context("invalid home size")?;
        ensure!(
            disk.checked_mul(2)
                .and_then(|n| n.checked_add(home))
                .is_some_and(|minimum| memory > minimum),
            "sandbox memory must exceed twice disk plus home"
        );
        parse_limit("wall_seconds", &s.wall_seconds.to_string())?;
        parse_limit("max_turns", &s.max_turns.to_string())?;
        let n = &self.networking;
        ensure!(
            n.ingress_listen != n.gateway_listen
                && !n.accepted_hosts.is_empty()
                && n.accepted_hosts
                    .iter()
                    .all(|h| !h.is_empty() && !h.chars().any(char::is_whitespace)),
            "invalid networking listeners or accepted_hosts"
        );
        ensure!(
            !n.sandbox_network.is_empty()
                && n.sandbox_network
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "invalid sandbox_network"
        );
        let url =
            reqwest::Url::parse(&n.sandbox_gateway_url).context("invalid sandbox_gateway_url")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid sandbox_gateway_url"
        );
        Ok(())
    }
    fn validate_model(&self) -> Result<()> {
        let m = &self.model;
        let unique: HashSet<_> = m.models.iter().collect();
        ensure!(
            !m.endpoint_name.trim().is_empty()
                && !m.models.is_empty()
                && unique.len() == m.models.len()
                && m.models.iter().all(|s| !s.is_empty() && s.trim() == s)
                && m.models.contains(&m.selected_model),
            "invalid model selection"
        );
        let upstream = reqwest::Url::parse(&m.upstream).context("invalid model upstream")?;
        ensure!(
            upstream.scheme() == "https"
                && upstream.host_str().is_some()
                && upstream.username().is_empty()
                && upstream.password().is_none()
                && upstream.query().is_none()
                && upstream.fragment().is_none(),
            "invalid model upstream"
        );
        for (name, value) in [
            ("max_tokens", m.max_tokens),
            ("max_body", u64::try_from(m.max_body)?),
            ("max_requests", m.max_requests),
            ("max_concurrent", u64::try_from(m.max_concurrent)?),
            ("run_max_tokens", m.run_max_tokens),
            ("run_max_cost_micro_usd", m.run_max_cost_micro_usd),
        ] {
            parse_limit(name, &value.to_string())?;
        }
        parse_paths(&m.allowed_paths)?;
        let prices = text(&m.prices)?;
        strict_json::<serde_json::Value>(prices.as_bytes())?;
        parse_prices(&prices, &m.models)?;
        let run = serde_json::json!({"00000000000000000000000000000000":{"id":"validation", "ticket":"validation", "repo":self.github.product.repo,"data_classes":["validation"],"max_tokens":m.run_max_tokens,"max_cost_micro_usd":m.run_max_cost_micro_usd}});
        parse_runs(&run.to_string())?;
        Ok(())
    }
}
