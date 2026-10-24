use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub run: String,
    pub ticket: String,
    pub revision: String,
    pub repo: String,
    pub base: String,
    pub started_at: i64,
    pub data_classes: Vec<String>,
    pub pins: BTreeMap<String, String>,
    pub linked_tickets: Vec<String>,
    pub max_tokens: u64,
    pub max_cost_micro_usd: u64,
    pub agent_image: String,
    pub collector_image: String,
    pub agent_out: PathBuf,
    pub collect_out: PathBuf,
    pub model_token_hash: String,
    pub mcp_token_hash: String,
}

impl Binding {
    pub fn permit(&self) -> kod_tool_gateway::github::RunPermit {
        kod_tool_gateway::github::RunPermit {
            run: self.run.clone(),
            ticket: self.ticket.clone(),
            repo: self.repo.clone(),
            base: self.base.clone(),
        }
    }

    pub fn validate(&self, root: &Path) -> Result<()> {
        ensure!(
            self.version == 1 && kod_dispatcher::store::seq_of(&self.run).is_some(),
            "invalid binding identity"
        );
        kod_gates::validate_repo(&self.repo)?;
        kod_gates::validate_sha(&self.base)?;
        ensure!(
            !self.ticket.is_empty() && !self.revision.is_empty() && self.started_at > 0,
            "invalid binding metadata"
        );
        ensure!(
            !self.data_classes.is_empty()
                && !self.pins.is_empty()
                && self.max_tokens > 0
                && self.max_cost_micro_usd > 0,
            "invalid binding limits or pins"
        );
        ensure!(
            self.max_cost_micro_usd <= kod_llm_proxy::config::MAX_COST_MICRO_USD,
            "binding cost exceeds supported range"
        );
        let dir = root.join(&self.run);
        ensure!(
            self.agent_out == dir.join("agent_out") && self.collect_out == dir.join("collect_out"),
            "binding output paths differ from owned run"
        );
        for hash in [&self.model_token_hash, &self.mcp_token_hash] {
            ensure!(
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid token hash"
            );
        }
        Ok(())
    }

    pub fn load(db: &Path, run: &str) -> Result<Self> {
        let c =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let value = kod_dispatcher::store::binding(&c, run)?;
        let binding: Self =
            serde_json::from_value(value).context("invalid stored execution binding")?;
        ensure!(binding.run == run, "stored binding belongs to another run");
        Ok(binding)
    }
}

pub fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
