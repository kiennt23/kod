use crate::Stage;
use anyhow::{Context, Result};
use kod_dispatcher::{RunRow, RunState, run_id};
use kod_gates::{Check, Evidence};
use kod_tracker::{Issue, ReworkKind, State};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub link: String,
    pub stage: Stage,
    pub approved: Option<String>,
    pub draft: Option<String>,
    pub in_review: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRecord {
    pub sha: String,
    pub check: Check,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pull {
    pub ticket: String,
    pub evidence: Evidence,
    pub head: String,
    pub state: PrState,
    pub draft: bool,
    pub checks: Vec<CheckRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReworkRecord {
    pub key: String,
    pub kind: ReworkKind,
    pub target: Stage,
    pub state: State,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub story: Issue,
    pub tickets: Vec<Issue>,
    pub artifacts: Vec<Artifact>,
    pub pulls: Vec<Pull>,
    pub rework: Vec<ReworkRecord>,
}

pub struct Records {
    pub snapshot: Snapshot,
    pub runs: Vec<RunRow>,
}

pub trait Source {
    fn load(&self, story: &str) -> Result<Records>;
}

pub struct HostRecords {
    pub snapshot: PathBuf,
    pub dispatcher: PathBuf,
}

impl Source for HostRecords {
    fn load(&self, story: &str) -> Result<Records> {
        let snapshot: Snapshot = serde_json::from_slice(
            &std::fs::read(&self.snapshot).context("reading workflow source records")?,
        )
        .context("invalid workflow source records")?;
        anyhow::ensure!(
            snapshot.story.key == story,
            "records belong to another story"
        );
        let c = Connection::open_with_flags(&self.dispatcher, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("opening dispatcher records read-only")?;
        c.busy_timeout(kod_dispatcher::store::BUSY_TIMEOUT)?;
        let rows = c
            .prepare("SELECT seq, ticket, revision, state, reason FROM runs ORDER BY seq")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let tickets: std::collections::BTreeSet<_> = snapshot
            .tickets
            .iter()
            .map(|i| i.key.as_str())
            .chain([story])
            .collect();
        let runs = rows
            .into_iter()
            .filter(|(_, ticket, _, _, _)| tickets.contains(ticket.as_str()))
            .map(|(seq, ticket, revision, state, reason)| {
                anyhow::ensure!(seq > 0, "invalid dispatcher run sequence");
                Ok(RunRow {
                    id: run_id(seq),
                    ticket,
                    revision,
                    state: RunState::parse(&state)?,
                    reason,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Records { snapshot, runs })
    }
}
