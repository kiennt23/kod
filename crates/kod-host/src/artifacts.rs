use anyhow::{Context, Result, ensure};
use kod_tool_gateway::docs::{Doc, Docs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub link: String,
    pub revision: String,
    pub sha256: String,
    pub class: String,
    pub path: PathBuf,
    pub approved_by: String,
    pub approved_at: String,
    pub approval_ref: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub artifacts: Vec<Artifact>,
}

pub struct Artifacts {
    manifest: PathBuf,
    approvers: BTreeSet<String>,
    classes: BTreeSet<String>,
    history: Mutex<BTreeMap<String, (String, Doc)>>,
}

impl Artifacts {
    pub fn new(manifest: PathBuf, policy: &Path) -> Result<Self> {
        let people: serde_json::Value =
            crate::config::strict_json(&crate::config::read_local(&policy.join("people.json"))?)?;
        let routing: serde_json::Value =
            crate::config::strict_json(&crate::config::read_local(&policy.join("routing.json"))?)?;
        let approvers = [people["owner"].as_str(), people["backup"].as_str()]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect();
        let classes = routing["classes"]
            .as_object()
            .context("missing policy classes")?
            .keys()
            .cloned()
            .collect();
        let this = Self {
            manifest,
            approvers,
            classes,
            history: Mutex::default(),
        };
        this.current_manifest()?;
        Ok(this)
    }

    pub fn validate(&self, path: &Path) -> Result<Manifest> {
        let manifest: Manifest = crate::config::strict_json(&crate::config::read_private(path)?)?;
        ensure!(
            manifest.version == 1 && !manifest.artifacts.is_empty(),
            "invalid approval manifest"
        );
        let root = path.parent().context("manifest has no directory")?;
        let mut links = BTreeSet::new();
        let mut revisions = BTreeSet::new();
        for artifact in &manifest.artifacts {
            ensure!(
                !artifact.link.trim().is_empty() && links.insert(&artifact.link),
                "duplicate or empty artifact link"
            );
            ensure!(
                !artifact.revision.trim().is_empty() && revisions.insert(&artifact.revision),
                "duplicate or empty artifact revision"
            );
            ensure!(
                self.approvers.contains(&artifact.approved_by),
                "unknown artifact approver"
            );
            ensure!(
                self.classes.contains(&artifact.class),
                "unknown artifact class"
            );
            ensure!(
                !artifact.approval_ref.trim().is_empty(),
                "missing recorded approval reference"
            );
            ensure!(
                artifact.approved_at.ends_with('Z') && chrono_timestamp(&artifact.approved_at),
                "invalid approval timestamp"
            );
            ensure!(
                !artifact.path.as_os_str().is_empty()
                    && artifact
                        .path
                        .components()
                        .all(|p| matches!(p, std::path::Component::Normal(_))),
                "artifact path escapes snapshot root"
            );
            let bytes = crate::config::read_private(&root.join(&artifact.path))?;
            ensure!(
                artifact.sha256 == format!("{:x}", Sha256::digest(&bytes)),
                "artifact content hash mismatch"
            );
            let text = String::from_utf8(bytes).context("artifact is not UTF-8")?;
            let mut history = self
                .history
                .lock()
                .map_err(|_| anyhow::anyhow!("artifact history poisoned"))?;
            if let Some((digest, doc)) = history.get(&artifact.revision) {
                ensure!(
                    digest == &artifact.sha256 && doc.class == artifact.class,
                    "immutable artifact revision changed"
                );
            } else {
                history.insert(
                    artifact.revision.clone(),
                    (
                        artifact.sha256.clone(),
                        Doc {
                            text,
                            class: artifact.class.clone(),
                        },
                    ),
                );
            }
        }
        Ok(manifest)
    }

    pub fn current_manifest(&self) -> Result<Manifest> {
        self.validate(&self.manifest)
    }

    pub fn pins(&self, links: &[String]) -> Result<(BTreeMap<String, String>, Vec<String>)> {
        let manifest = self.current_manifest()?;
        let mut pins = BTreeMap::new();
        let mut classes = BTreeSet::new();
        ensure!(!links.is_empty(), "no approved inputs");
        for link in links {
            let artifact = manifest
                .artifacts
                .iter()
                .find(|a| &a.link == link)
                .context("unknown approved artifact")?;
            pins.insert(link.clone(), artifact.revision.clone());
            classes.insert(artifact.class.clone());
        }
        Ok((pins, classes.into_iter().collect()))
    }
}

fn chrono_timestamp(text: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

impl Docs for Artifacts {
    fn read(&self, revision: &str) -> Result<Doc> {
        self.history
            .lock()
            .map_err(|_| anyhow::anyhow!("artifact history poisoned"))?
            .get(revision)
            .map(|(_, doc)| doc.clone())
            .context("unknown admitted artifact revision")
    }
}

impl kod_gates::Artifacts for Artifacts {
    fn current(&self, link: &str) -> Result<String> {
        self.current_manifest()?
            .artifacts
            .into_iter()
            .find(|a| a.link == link)
            .map(|a| a.revision)
            .context("unknown approved artifact")
    }
}
