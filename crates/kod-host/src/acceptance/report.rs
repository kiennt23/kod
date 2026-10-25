use super::Spend;
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Docker,
    Live,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Blocked,
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCode {
    #[default]
    Observed,
    Denied,
    Audited,
    Published,
    Reconciled,
    Stopped,
    Revoked,
    CleanupConfirmed,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Observation {
    pub code: ObservationCode,
    pub count: Option<u64>,
    pub duration_ms: Option<u64>,
    pub head_sha: Option<String>,
    pub tree_sha: Option<String>,
    pub audit_sequence: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Scenario {
    pub name: String,
    pub outcome: Outcome,
    pub observations: Vec<Observation>,
}
#[derive(Default)]
pub struct Execution {
    pub scenarios: Vec<Scenario>,
    pub external_writes: u64,
    pub spend: Spend,
}
pub const DOCKER_SCENARIOS: &[&str] = &[
    "mcp-tool-call",
    "planted-secret-denied-and-audited",
    "draft-pr-story-agent-trailers",
    "connected-ingress-safety",
    "connected-gateway-safety",
    "collector-integrity",
    "approval-refresh",
    "current-source-fail-closed",
    "status-comment-reconciliation",
    "gate-container-isolation",
    "cancel-revokes-queued-and-streaming-requests",
    "label-removal-revokes-run",
    "kill-confirms-container-absence",
    "restart-never-resumes-and-reconciles-publish",
    "kill-during-dependency-outages",
];
pub const LIVE_SCENARIOS: &[&str] = &[
    "mcp-tool-call",
    "planted-secret-denied-and-audited",
    "draft-pr-story-agent-trailers",
    "head-bound-gates-and-current-status",
    "approval-refresh",
    "cancel-and-container-stop",
    "crash-restart-ended-access",
    "owner-kill-and-explicit-resume",
];
#[derive(Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub mode: Mode,
    pub scenarios: Vec<Scenario>,
    pub blocked: Vec<String>,
    pub external_writes: u64,
    pub model_requests: u64,
    pub configured_spend: Spend,
    pub actual_spend: Spend,
    pub deployed_commit: Option<String>,
    pub image_digests: Vec<String>,
    pub repository: Option<String>,
    pub app_id: Option<u64>,
    pub bot_id: Option<u64>,
    pub authorization_sha256: Option<String>,
    #[serde(skip)]
    actual_live: bool,
}
impl Report {
    pub fn new(mode: Mode) -> Self {
        Self {
            schema_version: 1,
            mode,
            scenarios: Self::required(mode)
                .iter()
                .map(|name| Scenario {
                    name: (*name).into(),
                    outcome: Outcome::Blocked,
                    observations: vec![],
                })
                .collect(),
            blocked: vec![],
            external_writes: 0,
            model_requests: 0,
            configured_spend: Spend::default(),
            actual_spend: Spend::default(),
            deployed_commit: None,
            image_digests: vec![],
            repository: None,
            app_id: None,
            bot_id: None,
            authorization_sha256: None,
            actual_live: false,
        }
    }
    pub const fn required(mode: Mode) -> &'static [&'static str] {
        match mode {
            Mode::Docker => DOCKER_SCENARIOS,
            Mode::Live => LIVE_SCENARIOS,
        }
    }
    pub fn exit_code(&self) -> i32 {
        if self.scenarios.iter().any(|s| s.outcome == Outcome::Fail) {
            return 1;
        }
        let required = Self::required(self.mode);
        if !self.blocked.is_empty()
            || self.scenarios.len() != required.len()
            || required
                .iter()
                .any(|name| self.scenarios.iter().filter(|s| s.name == *name).count() != 1)
            || self
                .scenarios
                .iter()
                .any(|s| s.outcome != Outcome::Pass || s.observations.is_empty())
        {
            return 2;
        }
        0
    }
    pub fn outcome(&self) -> Outcome {
        match self.exit_code() {
            0 => Outcome::Pass,
            1 => Outcome::Fail,
            _ => Outcome::Blocked,
        }
    }
    pub fn passed(&self, name: &str) -> bool {
        self.scenarios
            .iter()
            .any(|s| s.name == name && s.outcome == Outcome::Pass && !s.observations.is_empty())
    }
    pub fn curated_live(&self) -> Result<Vec<u8>> {
        ensure!(
            self.mode == Mode::Live && self.actual_live && self.exit_code() == 0,
            "actual complete live evidence required"
        );
        // Live provenance is minted only by a future deployed execution driver, never imported JSON.
        self.json()
    }
    pub fn json(&self) -> Result<Vec<u8>> {
        ensure!(
            self.scenarios
                .iter()
                .all(|s| Self::required(self.mode).contains(&s.name.as_str())),
            "unknown scenario code"
        );
        ensure!(
            self.blocked.iter().all(|code| [
                "authorization",
                "deployment_prerequisites",
                "live_driver_and_gate_prerequisites",
                "connected_test_support_disabled",
                "scenario_execution_failed",
                "suite_budget_exhausted",
                "docker_prerequisites"
            ]
            .contains(&code.as_str())),
            "unknown prerequisite code"
        );
        for observation in self.scenarios.iter().flat_map(|s| &s.observations) {
            for sha in [&observation.head_sha, &observation.tree_sha]
                .into_iter()
                .flatten()
            {
                ensure!(
                    [40, 64].contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid observed digest"
                );
            }
        }
        let mut value = serde_json::to_value(self)?;
        value["outcome"] = serde_json::to_value(self.outcome())?;
        value["executed"] = serde_json::json!(
            self.scenarios
                .iter()
                .filter(|s| s.outcome != Outcome::Blocked)
                .count()
        );
        Ok(serde_json::to_vec_pretty(&value)?)
    }
}
