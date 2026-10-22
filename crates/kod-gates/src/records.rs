use crate::Artifacts;
use anyhow::{Context, Result, bail};
use kod_tracker::{Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub story: String,
    pub requester: String,
    pub required: Vec<String>,
    pub current: BTreeMap<String, String>,
}

/// Read-only bridge for host-maintained tracker and approval snapshots.
/// The event ingester atomically replaces this file outside PR workspaces;
/// every lookup re-reads it. Connected tracker acceptance belongs to 6b/6c.
pub struct HostRecords(pub PathBuf);

impl HostRecords {
    fn read(&self) -> Result<Snapshot> {
        Ok(serde_json::from_slice(&std::fs::read(&self.0)?)?)
    }
}

impl Artifacts for HostRecords {
    fn current(&self, link: &str) -> Result<String> {
        self.read()?
            .current
            .get(link)
            .cloned()
            .context("unknown approved artifact")
    }
}

impl Tracker for HostRecords {
    fn name(&self) -> &str {
        "host-snapshot"
    }
    fn get_issue(&self, key: &str) -> Result<Issue> {
        let snapshot = self.read()?;
        if snapshot.story != key {
            bail!("snapshot belongs to another story");
        }
        Ok(Issue {
            key: key.into(),
            title: String::new(),
            body: String::new(),
            acceptance_criteria: vec![],
            requirement_id: None,
            risk_class: None,
            reporter: snapshot.requester,
            approved_versions: snapshot.required,
            links: vec![],
            state: State::InReview,
        })
    }
    fn comment(&self, _: &str, _: &str) -> Result<()> {
        bail!("read-only gate records")
    }
    fn transition(&self, _: &str, _: State) -> Result<()> {
        bail!("read-only gate records")
    }
    fn link_pr(&self, _: &str, _: &str) -> Result<()> {
        bail!("read-only gate records")
    }
    fn create_linked(&self, _: &str, _: ReworkKind, _: &Rework) -> Result<String> {
        bail!("read-only gate records")
    }
    fn is_team_member(&self, _: &str) -> Result<bool> {
        bail!("read-only gate records")
    }
    fn parse_webhook(&self, _: &Webhook) -> Result<Option<Delivery>> {
        bail!("read-only gate records")
    }
}
