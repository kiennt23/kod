//! Kod's policy engine: the Rego rules and data files in one directory,
//! evaluated in-process with regorus. Callers act only on `Decision::allow`
//! and record the whole decision, `policy_version` included.

mod config;

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use regorus::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The one rule every decision reads.
const DECISION_RULE: &str = "data.kod.decision";

/// The pinned regorus version, part of `policy_version` so an engine bump
/// with unchanged files is still a new version. A test keeps it equal to
/// the version in Cargo.lock.
pub const REGORUS_VERSION: &str = "0.13.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub allow: bool,
    pub reasons: Vec<String>,
    pub policy_version: String,
}

#[derive(Deserialize)]
struct Raw {
    allow: bool,
    reasons: Vec<String>,
}

pub struct Policy {
    // ponytail: one engine behind a Mutex serializes decisions; clone the
    // engine per thread if lock contention ever shows up.
    engine: Mutex<Engine>,
    version: String,
}

impl Policy {
    /// Loads every `*.rego` and `*.json` file directly in `dir`. A data file
    /// `<name>.json` becomes `data.config.<name>`. Fails if there is no Rego, a
    /// file doesn't parse, a data file the rules read is missing or malformed
    /// (see `config.rs`), or the rules don't yield a decision.
    pub fn load(dir: &Path) -> Result<Policy> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if path.is_file() && matches!(ext, Some("rego" | "json")) {
                files.push(path);
            }
        }
        files.sort();
        if !files
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "rego"))
        {
            bail!("no .rego files in {}", dir.display());
        }

        let mut engine = Engine::new();
        let mut data = Map::new();
        let mut hash = Sha256::new();
        for path in &files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .with_context(|| format!("non-UTF-8 file name: {}", path.display()))?;
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            hash.update(name.as_bytes());
            hash.update([0]);
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
            if let Some(stem) = name.strip_suffix(".json") {
                let value: Value =
                    serde_json::from_str(&text).with_context(|| format!("{name}: invalid JSON"))?;
                data.insert(stem.to_string(), value);
            } else {
                engine
                    .add_policy(name.to_string(), text)
                    .map_err(|e| anyhow!("{name}: {e}"))?;
            }
        }
        config::check(&data)?;
        let data = Value::Object(Map::from_iter([(
            "config".to_string(),
            Value::Object(data),
        )]));
        engine
            .add_data_json(&data.to_string())
            .map_err(|e| anyhow!("data: {e}"))?;

        let policy = Policy {
            engine: Mutex::new(engine),
            version: format!("sha256:{:x};regorus={REGORUS_VERSION}", hash.finalize()),
        };
        // A broken rule set fails here, at startup, not as a deny on every request.
        policy
            .eval(&Value::Object(Map::new()))
            .context("the rules don't yield a decision")?;
        Ok(policy)
    }

    /// `sha256:<hex>;regorus=<version>`: the hash covers the loaded files'
    /// names and contents.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Decides one request. Never fails: an input that isn't a JSON object,
    /// or any evaluation error, is a deny whose reason says why.
    pub fn decide(&self, input: &Value) -> Decision {
        let (allow, reasons) = match self.eval(input) {
            Ok(raw) => (raw.allow, raw.reasons),
            Err(e) => (false, vec![format!("policy error: {e:#}")]),
        };
        Decision {
            allow,
            reasons,
            policy_version: self.version.clone(),
        }
    }

    fn eval(&self, input: &Value) -> Result<Raw> {
        if !input.is_object() {
            bail!("input must be a JSON object");
        }
        let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        engine
            .set_input_json(&input.to_string())
            .map_err(|e| anyhow!("input: {e}"))?;
        let value = engine
            .eval_rule(DECISION_RULE.to_string())
            .map_err(|e| anyhow!("{e}"))?;
        let json = value.to_json_str().map_err(|e| anyhow!("{e}"))?;
        serde_json::from_str(&json).with_context(|| format!("not a decision: {json}"))
    }
}
