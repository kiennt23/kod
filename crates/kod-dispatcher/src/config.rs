//! The dispatcher's own config, checked whole at load. The owner and backup
//! are here, not only in the policy's `people.json`, so their kill switch
//! works when the policy doesn't load (policy plan D14).
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The repo this tracker's tickets delegate to, lower-case `owner/name`.
    pub repo: String,
    /// GitHub logins, lower-cased.
    pub owner: String,
    pub backup: Option<String>,
}

const KEYS: [&str; 3] = ["repo", "owner", "backup"];

fn login(v: &Value, key: &str) -> Result<String> {
    match v.as_str() {
        Some(s)
            if !s.is_empty()
                && !s.starts_with('-')
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') =>
        {
            Ok(s.to_ascii_lowercase())
        }
        _ => bail!("{key} must be a GitHub login, got {v}"),
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let v: Value = serde_json::from_str(text).context("the dispatcher config is not JSON")?;
        let obj = v
            .as_object()
            .context("the dispatcher config must be an object")?;
        if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            bail!("unknown key {k:?}");
        }
        if let Some(k) = KEYS.iter().find(|k| !obj.contains_key(**k)) {
            bail!("missing key {k:?}");
        }
        let repo = obj["repo"].as_str().unwrap_or_default();
        let valid = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c))
        };
        if !repo
            .split_once('/')
            .is_some_and(|(o, n)| valid(o) && valid(n))
        {
            bail!("repo must be lower-case owner/name, got {}", obj["repo"]);
        }
        let owner = login(&obj["owner"], "owner")?;
        let backup = match &obj["backup"] {
            Value::Null => None,
            b => Some(login(b, "backup")?),
        };
        if backup.as_ref() == Some(&owner) {
            bail!("backup must be someone other than the owner");
        }
        Ok(Config {
            repo: repo.to_owned(),
            owner,
            backup,
        })
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Config::parse(&text).with_context(|| path.display().to_string())
    }

    /// The owner or backup, whose kill switch never waits on the policy.
    pub fn may_always_stop(&self, login: &str) -> bool {
        let login = login.trim().to_ascii_lowercase();
        login == self.owner || self.backup.as_ref() == Some(&login)
    }
}
