//! Trusted gates. Adapters supply authoritative records; no decision reads
//! PR-authored evidence, configuration, scanner output files or CODEOWNERS.
pub mod github;
pub mod records;
pub mod scanner;

use anyhow::{Result, bail};
use kod_audit::{AuditLog, RUN_AUDIT};
use kod_policy::Policy;
use kod_tracker::{Issue, Tracker};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const CHECKS: [&str; 7] = [
    "inputs-current",
    "requester-check",
    "protected-paths",
    "tests",
    "sast",
    "dependency-scan",
    "secret-scan",
];

/// An immutable record produced by the host's publish operation. Never
/// reconstruct this from PR text, a branch file or sandbox test evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub repo: String,
    pub pr: u64,
    pub head: String,
    pub run: String,
    pub story: String,
    pub team: String,
    pub pins: Vec<String>,
    pub policy_version: String,
    pub runner_config_version: String,
}

/// Current artifact approvals, queried afresh from the trusted registry.
/// Each issue link resolves to its latest approved immutable revision.
/// Unknown, unapproved or deleted links MUST return an error.
pub trait Artifacts {
    fn current(&self, pinned_link: &str) -> Result<String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Dismissed,
    Commented,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub id: u64,
    pub login: String,
    pub human: bool,
    pub can_approve: bool,
    pub commit: String,
    pub state: ReviewState,
}

/// Complete GitHub state for a PR, including rename source paths. The
/// adapter refuses incomplete or truncated lists. Approvals bind to `head`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub number: u64,
    pub head: String,
    pub base: String,
    pub author: String,
    pub paths: Vec<String>,
    pub reviews: Vec<Review>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Pending,
    Success,
    Failure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub conclusion: Conclusion,
    pub summary: String,
}

/// Implementations must update the SAME check run when moving it from
/// pending to completed. A retry may create a new generation, never revive
/// an older successful generation. One evaluator owns each repo's checks.
pub trait Forge {
    fn pull(&self, number: u64) -> Result<PullRequest>;
    fn report(&self, sha: &str, check: &Check) -> Result<()>;
}

/// Scans exactly `sha`, never a branch name. Nonzero exits, missing tools,
/// timeout and infrastructure failures are errors. No sandbox evidence.
pub trait Scanner {
    fn version(&self) -> Result<String>;
    fn scan(&self, name: &str, sha: &str) -> Result<()>;
}

/// Configuration loaded outside every PR workspace. Prefixes are literal
/// repository-relative directories or files, not globs. Owners are resolved
/// human GitHub logins, reviewed alongside the real CODEOWNERS file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: String,
    pub protect_all: bool,
    pub protected: Vec<String>,
    pub codeowners: Vec<String>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let parts: Vec<_> = self.repo.split('/').collect();
        if parts.len() != 2 || parts.iter().any(|s| !login(s) || matches!(*s, "." | "..")) {
            bail!("repo must be owner/name");
        }
        if self.codeowners.is_empty() || self.codeowners.iter().any(|s| !login(s)) {
            bail!("codeowners must contain resolved human logins");
        }
        for path in &self.protected {
            validate_path(path.trim_end_matches('/'))?;
        }
        Ok(())
    }

    pub fn version(&self) -> Result<String> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(self)?)
        ))
    }

    fn protects(&self, path: &str) -> bool {
        const DEFAULTS: &[&str] = &[
            ".github",
            ".cargo",
            "Cargo.toml",
            "build.rs",
            "CODEOWNERS",
            "policy",
            "harness",
            "auth",
            "payments",
            "crypto",
            "migrations",
            "infra",
            "infrastructure",
            "tests",
            "test",
        ];
        self.protect_all
            || self
                .protected
                .iter()
                .any(|p| under(path, p.trim_end_matches('/')))
            || path.split('/').any(|s| DEFAULTS.contains(&s))
            || path.ends_with("_test.go")
            || path.ends_with("_test.rs")
            || path.rsplit('/').next().is_some_and(|p| {
                p.starts_with("test_") || p.ends_with(".test.ts") || p.ends_with(".spec.ts")
            })
    }
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|s| s.starts_with('/'))
}

fn login(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

pub fn validate_sha(sha: &str) -> Result<()> {
    if sha.len() != 40
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("expected a full lowercase Git SHA");
    }
    Ok(())
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', '\0', '\n', '\r'])
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        bail!("invalid repository-relative path");
    }
    Ok(())
}

fn approvers(pr: &PullRequest) -> Result<Vec<String>> {
    let mut latest = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut reviews: Vec<_> = pr.reviews.iter().collect();
    reviews.sort_by_key(|r| r.id);
    for r in reviews {
        if !ids.insert(r.id) {
            bail!("duplicate review identity");
        }
        if !r.human || !r.can_approve {
            continue;
        }
        if !login(&r.login) {
            bail!("invalid review identity");
        }
        match r.state {
            ReviewState::Commented | ReviewState::Pending => {}
            ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Dismissed => {
                latest.insert(r.login.to_ascii_lowercase(), r);
            }
        }
    }
    if latest
        .values()
        .any(|r| r.human && r.state == ReviewState::ChangesRequested)
    {
        bail!("an effective human review requests changes");
    }
    Ok(latest
        .into_iter()
        .filter_map(|(name, r)| {
            (r.human
                && r.state == ReviewState::Approved
                && r.commit == pr.head
                && !name.eq_ignore_ascii_case(&pr.author))
            .then_some(name)
        })
        .collect())
}

pub struct Gates<'a> {
    pub config: &'a Config,
    pub policy: &'a Policy,
    pub audit: &'a AuditLog,
    pub tracker: &'a dyn Tracker,
    pub artifacts: &'a dyn Artifacts,
    pub forge: &'a dyn Forge,
    pub scanner: &'a dyn Scanner,
}

impl Gates<'_> {
    pub fn invalidate(&self, target: &str) -> Result<()> {
        self.config.validate()?;
        validate_sha(target)?;
        for name in CHECKS {
            self.report(
                target,
                &Check {
                    name: name.into(),
                    conclusion: Conclusion::Pending,
                    summary: "Evaluating trusted records and scans".into(),
                },
            )?;
        }
        Ok(())
    }
    fn report(&self, sha: &str, check: &Check) -> Result<()> {
        self.audit.before(
            RUN_AUDIT,
            &json!({"kind": "gate_report", "repo": self.config.repo,
            "sha": sha, "check": check, "config_version": self.config.version()?}),
            || self.forge.report(sha, check),
        )??;
        self.audit.append(
            RUN_AUDIT,
            &json!({"kind": "gate_reported", "sha": sha, "check": check}),
        )?;
        Ok(())
    }

    fn inputs(&self, e: &Evidence, pr: &PullRequest, issue: &Issue) -> Result<()> {
        if e.repo != self.config.repo
            || e.pr != pr.number
            || e.head != pr.head
            || e.run.trim().is_empty()
            || e.story.trim().is_empty()
            || e.team.trim().is_empty()
            || e.runner_config_version.trim().is_empty()
            || e.policy_version != self.policy.version()
        {
            bail!("run evidence is missing or does not bind this head and policy");
        }
        if issue.approved_versions.is_empty() {
            bail!("the story has no required approved inputs");
        }
        let pins: BTreeSet<_> = e.pins.iter().cloned().collect();
        if pins.len() != e.pins.len() {
            bail!("duplicate run pins");
        }
        let mut required = BTreeSet::new();
        for link in &issue.approved_versions {
            let current = self.artifacts.current(link)?;
            if current.trim().is_empty() {
                bail!("artifact registry returned an empty revision");
            }
            required.insert(current);
        }
        if pins != required {
            bail!("required inputs are missing or superseded");
        }
        Ok(())
    }

    fn approved(
        &self,
        action: &str,
        e: &Evidence,
        issue: &Issue,
        approvals: &[String],
    ) -> Result<()> {
        let input = json!({"action": action, "team": e.team, "requester": issue.reporter,
            "approvals": approvals.iter().map(|login| json!({"login": login, "agent": false})).collect::<Vec<_>>()});
        let decision = self.policy.decide(&input);
        self.audit.record_decision(&input, &decision)?;
        if !decision.allow {
            bail!("{}", decision.reasons.join("; "));
        }
        Ok(())
    }

    /// Run all seven checks. `target` is the PR head, or the merge candidate
    /// SHA from a VERIFIED merge-group delivery whose constituent PR head
    /// equals `e.head`. The caller serializes deliveries and registry updates
    /// for the repository. Every constituent must pass before a group passes.
    /// No control-plane adapter is allowed to accept PR-supplied evidence.
    pub fn run(&self, e: &Evidence, target: &str) -> Result<Vec<Check>> {
        self.config.validate()?;
        validate_sha(&e.head)?;
        validate_sha(target)?;
        self.invalidate(target)?;
        let result = self.evaluate(e, target);
        match result {
            Ok(checks) => {
                for check in &checks {
                    self.report(target, check)?;
                }
                Ok(checks)
            }
            Err(error) => {
                for name in CHECKS {
                    self.report(target, &Check { name: name.into(), conclusion: Conclusion::Failure, summary: "Gate infrastructure or authoritative records unavailable; retry required".into() })?;
                }
                Err(error)
            }
        }
    }

    fn evaluate(&self, e: &Evidence, target: &str) -> Result<Vec<Check>> {
        let pr = self.forge.pull(e.pr)?;
        validate_sha(&pr.head)?;
        validate_sha(&pr.base)?;
        for path in &pr.paths {
            validate_path(path)?;
        }
        let issue = self.tracker.get_issue(&e.story)?;
        let approvals = approvers(&pr);
        let inputs = self.inputs(e, &pr, &issue);
        let requester = approvals
            .as_ref()
            .map_err(|_| anyhow::anyhow!("review state is invalid"))
            .and_then(|a| self.approved("merge", e, &issue, a));
        let protected = if pr.paths.iter().any(|p| self.config.protects(p)) {
            approvals
                .as_ref()
                .map_err(|_| anyhow::anyhow!("review state is invalid"))
                .and_then(|a| {
                    self.approved("harness_change", e, &issue, a)?;
                    if a.len() < 2
                        || !a.iter().any(|login| {
                            self.config
                                .codeowners
                                .iter()
                                .any(|o| o.eq_ignore_ascii_case(login))
                        })
                    {
                        bail!(
                            "protected paths need an extra CODEOWNER approval (two distinct humans)"
                        );
                    }
                    Ok(())
                })
        } else {
            Ok(())
        };
        let mut checks = vec![
            outcome("inputs-current", inputs),
            outcome("requester-check", requester),
            outcome("protected-paths", protected),
        ];
        for name in &CHECKS[3..] {
            self.audit.append(RUN_AUDIT, &json!({"kind": "gate_scan", "repo": e.repo, "sha": target, "name": name,
                "run": e.run, "policy_version": self.policy.version(), "runner_config_version": e.runner_config_version,
                "scan_config_version": self.scanner.version()?}))?;
            checks.push(outcome(name, self.scanner.scan(name, target)));
        }
        if self.forge.pull(e.pr)? != pr || self.tracker.get_issue(&e.story)? != issue {
            bail!("PR or story changed during evaluation");
        }
        if checks[0].conclusion == Conclusion::Success {
            self.inputs(e, &pr, &issue)?;
        }
        self.audit.append(RUN_AUDIT, &json!({"kind": "gate_evaluated", "evidence": e, "target": target,
            "base": pr.base, "approvers": approvals.ok(), "checks": checks, "config_version": self.config.version()?}))?;
        Ok(checks)
    }
}

fn outcome(name: &str, result: Result<()>) -> Check {
    Check {
        name: name.into(),
        conclusion: if result.is_ok() {
            Conclusion::Success
        } else {
            Conclusion::Failure
        },
        summary: match result {
            Ok(()) => "Passed".into(),
            Err(e) => kod_audit::redact::redact(&format!("{e:#}")),
        },
    }
}

/// Ingest a verified GitHub ruleset bypass event. Replays are harmless;
/// advance the upstream cursor only after this durable append succeeds.
pub fn record_bypass(
    audit: &AuditLog,
    repo: &str,
    delivery: &str,
    actor: &str,
    sha: &str,
    ruleset: u64,
) -> Result<()> {
    validate_sha(sha)?;
    if delivery.trim().is_empty() || !login(actor) {
        bail!("missing bypass delivery or actor");
    }
    audit.append(
        RUN_AUDIT,
        &json!({"kind": "admin_bypass", "repo": repo, "delivery": delivery,
        "actor": actor, "sha": sha, "ruleset": ruleset}),
    )?;
    Ok(())
}

/// A deployment artifact, not an automatic write to GitHub. App binding
/// prevents another workflow or user from spoofing a required check.
pub fn ruleset(app_id: u64) -> Result<serde_json::Value> {
    if app_id == 0 {
        bail!("a dedicated GitHub App ID is required");
    }
    Ok(
        json!({"name": "kod-gates", "target": "branch", "enforcement": "active", "bypass_actors": [],
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": [
            {"type": "deletion"}, {"type": "non_fast_forward"},
            {"type": "pull_request", "parameters": {"required_approving_review_count": 1,
                "dismiss_stale_reviews_on_push": true, "require_code_owner_review": true,
                "require_last_push_approval": true, "required_review_thread_resolution": true}},
            {"type": "required_status_checks", "parameters": {"strict_required_status_checks_policy": true,
                "required_status_checks": CHECKS.iter().map(|context| json!({"context": context, "integration_id": app_id})).collect::<Vec<_>>()}},
            {"type": "merge_queue", "parameters": {"check_response_timeout_minutes": 60,
                "grouping_strategy": "ALLGREEN", "max_entries_to_build": 1, "max_entries_to_merge": 1,
                "merge_method": "SQUASH", "min_entries_to_merge": 1, "min_entries_to_merge_wait_minutes": 0}}
        ]}),
    )
}
