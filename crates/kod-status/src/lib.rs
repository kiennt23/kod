pub mod records;

use anyhow::{Context, Result, bail, ensure};
use kod_audit::{AuditLog, RUN_AUDIT, redact::redact};
use kod_dispatcher::{RunRow, RunState};
use kod_gates::{CHECKS, Conclusion, check_pins, validate_repo, validate_sha};
use kod_tracker::{ReworkKind, State, Tracker};
use records::{PrState, Records, Snapshot, Source};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Requirements,
    Plan,
    Design,
    Code,
    Review,
    Build,
    Test,
    Release,
    Deploy,
    Operate,
}

impl Stage {
    pub const ALL: [Self; 10] = [
        Self::Requirements,
        Self::Plan,
        Self::Design,
        Self::Code,
        Self::Review,
        Self::Build,
        Self::Test,
        Self::Release,
        Self::Deploy,
        Self::Operate,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Requirements => "Requirements",
            Self::Plan => "Plan",
            Self::Design => "Design",
            Self::Code => "Code",
            Self::Review => "Review",
            Self::Build => "Build",
            Self::Test => "Test",
            Self::Release => "Release",
            Self::Deploy => "Deploy",
            Self::Operate => "Operate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Unknown,
    NotStarted,
    InProgress,
    InReview,
    Approved,
    Stale,
    Blocked,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotStarted => "not started",
            Self::InProgress => "in progress",
            Self::InReview => "in review",
            Self::Approved => "approved",
            Self::Stale => "stale",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StageView {
    pub stage: Stage,
    pub status: Status,
    pub details: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TicketView {
    pub key: String,
    pub title: String,
    pub status: Status,
    pub details: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct View {
    pub story: String,
    pub title: String,
    pub requester: String,
    pub stages: Vec<StageView>,
    pub tickets: Vec<TicketView>,
    pub rework: Vec<String>,
}

fn safe(text: &str) -> String {
    let chars: Vec<_> = redact(text).chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if c.is_alphanumeric()
                || (c == '-'
                    && i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric()))
            {
                c
            } else {
                ' '
            }
        })
        .collect()
}

impl View {
    pub fn render(&self) -> String {
        let mut lines = vec![
            format!(
                "{}  {}  requester: {}",
                safe(&self.story),
                safe(&self.title),
                safe(&self.requester)
            ),
            String::new(),
        ];
        for s in &self.stages {
            lines.push(format!(
                "{}: {}{}",
                s.stage.name(),
                s.status.name(),
                details(&s.details)
            ));
        }
        lines.push("\nTickets".into());
        for t in &self.tickets {
            lines.push(format!(
                "- {}  {}: {}",
                safe(&t.key),
                safe(&t.title),
                t.status.name()
            ));
            for detail in &t.details {
                lines.push(format!("  {detail}"));
            }
        }
        lines.push("\nOpen rework".into());
        if self.rework.is_empty() {
            lines.push("- none".into());
        }
        for r in &self.rework {
            lines.push(format!("- {r}"));
        }
        lines.join("\n") + "\n"
    }
}

fn details(items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!(" ({})", items.join("; "))
    }
}

fn workflow(state: State) -> Status {
    match state {
        State::Ready => Status::NotStarted,
        State::InProgress => Status::InProgress,
        State::InReview => Status::InReview,
        State::Blocked => Status::Blocked,
        State::Done => Status::Approved,
    }
}

fn run_status(state: RunState) -> Status {
    match state {
        RunState::Queued | RunState::Running | RunState::Publishing => Status::InProgress,
        RunState::Done => Status::InReview,
        RunState::Blocked | RunState::NeedsHuman | RunState::OverBudget | RunState::Canceled => {
            Status::Blocked
        }
    }
}

fn combined(states: impl IntoIterator<Item = Status>) -> Status {
    let states: Vec<_> = states.into_iter().collect();
    if states.is_empty() {
        return Status::Unknown;
    }
    for state in [
        Status::Blocked,
        Status::Stale,
        Status::Unknown,
        Status::InProgress,
        Status::InReview,
        Status::NotStarted,
    ] {
        if states.contains(&state) {
            if state == Status::NotStarted && states.contains(&Status::Approved) {
                return Status::InProgress;
            }
            return state;
        }
    }
    Status::Approved
}

fn validate(s: &Snapshot, runs: &[RunRow], story: &str) -> Result<()> {
    let keys = s.keys(story)?;
    let mut rework = BTreeSet::new();
    for r in &s.rework {
        ensure!(
            r.kind != ReworkKind::Replan || r.target == Stage::Plan,
            "a replan must target planning"
        );
        ensure!(
            !r.key.trim().is_empty() && !keys.contains(r.key.as_str()) && rework.insert(&r.key),
            "duplicate or invalid rework"
        );
        ensure!(
            s.story.links.contains(&r.key),
            "rework is not linked to this story"
        );
    }
    let mut artifacts = BTreeSet::new();
    for a in &s.artifacts {
        ensure!(
            !a.link.trim().is_empty() && artifacts.insert(&a.link),
            "duplicate or empty artifact"
        );
        ensure!(
            matches!(a.stage, Stage::Requirements | Stage::Plan | Stage::Design),
            "artifact stage is not an upstream document"
        );
        ensure!(
            a.approved.as_ref().is_none_or(|v| !v.trim().is_empty())
                && a.draft.as_ref().is_none_or(|v| !v.trim().is_empty()),
            "empty artifact revision"
        );
        ensure!(
            !a.in_review || a.draft.is_some(),
            "review has no draft revision"
        );
    }
    let mut pulls = BTreeSet::new();
    for p in &s.pulls {
        ensure!(
            keys.contains(p.ticket.as_str()) && p.evidence.story == story,
            "PR belongs to another story"
        );
        validate_repo(&p.evidence.repo)?;
        validate_sha(&p.head)?;
        validate_sha(&p.evidence.head)?;
        ensure!(
            p.evidence.pr > 0 && pulls.insert((&p.evidence.repo, p.evidence.pr)),
            "duplicate or invalid PR"
        );
        let mut checks = BTreeSet::new();
        for c in &p.checks {
            validate_sha(&c.sha)?;
            ensure!(
                checks.insert((&c.sha, &c.check.name)),
                "duplicate check record"
            );
        }
    }
    let mut ids = BTreeSet::new();
    for r in runs {
        ensure!(
            keys.contains(r.ticket.as_str()),
            "run belongs to another story"
        );
        let seq = kod_dispatcher::store::seq_of(&r.id).filter(|s| *s > 0);
        ensure!(
            seq.is_some() && ids.insert(&r.id),
            "duplicate or invalid run"
        );
    }
    Ok(())
}

pub fn load(source: &dyn Source, story: &str) -> Result<View> {
    let records = source.load(story)?;
    compute(&records, story)
}

pub fn compute(records: &Records, story: &str) -> Result<View> {
    let s = &records.snapshot;
    validate(s, &records.runs, story)?;
    let mut stages: BTreeMap<Stage, StageView> = Stage::ALL
        .into_iter()
        .map(|stage| {
            (
                stage,
                StageView {
                    stage,
                    status: Status::Unknown,
                    details: vec![],
                },
            )
        })
        .collect();
    for stage in [Stage::Requirements, Stage::Plan, Stage::Design] {
        let mut states = Vec::new();
        let view = stages
            .get_mut(&stage)
            .ok_or_else(|| anyhow::anyhow!("missing stage"))?;
        for a in s.artifacts.iter().filter(|a| a.stage == stage) {
            if let Some(v) = &a.approved {
                view.details
                    .push(format!("{} approved {}", safe(&a.link), safe(v)));
            }
            if let Some(v) = &a.draft {
                view.details
                    .push(format!("{} draft {}", safe(&a.link), safe(v)));
            }
            states.push(if a.in_review {
                Status::InReview
            } else if a.draft.is_some() {
                Status::InProgress
            } else if a.approved.is_some() {
                Status::Approved
            } else {
                Status::NotStarted
            });
        }
        view.status = combined(states);
    }
    let mut issues: Vec<_> = s.tickets.iter().collect();
    if issues.is_empty()
        || records.runs.iter().any(|r| r.ticket == story)
        || s.pulls.iter().any(|p| p.ticket == story)
    {
        issues.push(&s.story);
    }
    let mut tickets = Vec::new();
    let mut review_states = Vec::new();
    let mut build_states = Vec::new();
    let mut test_states = Vec::new();
    for i in issues {
        let mut status = workflow(i.state);
        let mut notes = Vec::new();
        let mut pull_states = Vec::new();
        let mut runs: Vec<_> = records.runs.iter().filter(|r| r.ticket == i.key).collect();
        runs.sort_by_key(|r| kod_dispatcher::store::seq_of(&r.id));
        for r in &runs {
            notes.push(format!(
                "{} {}{}",
                r.id,
                r.state.name(),
                if r.reason.is_empty() {
                    String::new()
                } else {
                    format!(": {}", safe(&r.reason))
                }
            ));
        }
        if let Some(r) = runs.last()
            && !matches!(i.state, State::Ready | State::Done | State::Blocked)
        {
            status = run_status(r.state);
        }
        let mut pulls: Vec<_> = s.pulls.iter().filter(|p| p.ticket == i.key).collect();
        pulls.sort_by_key(|p| (&p.evidence.repo, p.evidence.pr));
        for p in pulls {
            let label = format!("{} PR #{}", p.evidence.repo, p.evidence.pr);
            match p.state {
                PrState::Closed => {
                    notes.push(format!("{label} closed"));
                    continue;
                }
                PrState::Merged | PrState::Open => {}
            }
            let mut required = BTreeSet::new();
            let mut missing = false;
            for link in &s.story.approved_versions {
                match s
                    .artifacts
                    .iter()
                    .find(|a| &a.link == link)
                    .and_then(|a| a.approved.as_ref())
                {
                    Some(v) => {
                        required.insert(v.as_str());
                    }
                    None => {
                        missing = true;
                    }
                }
            }
            let merged = matches!(p.state, PrState::Merged);
            let stale = !merged
                && (p.head != p.evidence.head
                    || check_pins(&p.evidence.pins, &s.story.approved_versions, s).is_err());
            let current: BTreeMap<_, _> = p
                .checks
                .iter()
                .filter(|c| c.sha == p.head)
                .map(|c| (c.check.name.as_str(), &c.check))
                .collect();
            let gate =
                |names: &[&str]| {
                    combined(names.iter().map(
                        |name| match current.get(name).map(|c| c.conclusion) {
                            Some(Conclusion::Success) => Status::Approved,
                            Some(Conclusion::Failure) => Status::Blocked,
                            Some(Conclusion::Pending) => Status::InProgress,
                            None => Status::Unknown,
                        },
                    ))
                };
            notes.push(format!(
                "{label} {}{}",
                if merged {
                    "merged"
                } else if p.draft {
                    "draft"
                } else {
                    "in review"
                },
                if stale { "; stale inputs or head" } else { "" }
            ));
            if stale {
                notes.push(format!(
                    "pinned revisions: {}; current approved: {}{}",
                    p.evidence
                        .pins
                        .iter()
                        .map(|v| safe(v))
                        .collect::<Vec<_>>()
                        .join(", "),
                    required
                        .iter()
                        .map(|v| safe(v))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if missing {
                        "; required approval unavailable"
                    } else {
                        ""
                    }
                ));
            }
            for name in CHECKS {
                notes.push(format!(
                    "{name} {}",
                    current.get(name).map_or("unknown", |c| match c.conclusion {
                        Conclusion::Success => "success",
                        Conclusion::Failure => "failure",
                        Conclusion::Pending => "pending",
                    })
                ));
                if let Some(check) = current
                    .get(name)
                    .filter(|c| c.conclusion == Conclusion::Failure)
                {
                    notes.push(format!("{name} failure reason: {}", safe(&check.summary)));
                }
            }
            pull_states.push(if merged {
                Status::Approved
            } else if gate(&CHECKS) == Status::Blocked {
                Status::Blocked
            } else if stale {
                Status::Stale
            } else if p.draft {
                Status::InProgress
            } else {
                Status::InReview
            });
            review_states.push(if merged {
                Status::Approved
            } else if gate(&["inputs-current", "requester-check", "protected-paths"])
                == Status::Blocked
            {
                Status::Blocked
            } else if stale {
                Status::Stale
            } else {
                Status::InReview
            });
            build_states.push(if stale {
                Status::Stale
            } else {
                gate(&["tests", "sast", "dependency-scan", "secret-scan"])
            });
            test_states.push(if stale {
                Status::Stale
            } else {
                gate(&["tests"])
            });
        }
        let has_open = s
            .pulls
            .iter()
            .any(|p| p.ticket == i.key && matches!(p.state, PrState::Open));
        let latest_merge = s
            .pulls
            .iter()
            .filter(|p| p.ticket == i.key && matches!(p.state, PrState::Merged))
            .filter_map(|p| kod_dispatcher::store::seq_of(&p.evidence.run))
            .max();
        let newer_run = !has_open
            && latest_merge
                .zip(
                    runs.last()
                        .and_then(|r| kod_dispatcher::store::seq_of(&r.id)),
                )
                .is_some_and(|(merged, latest)| latest > merged);
        if !pull_states.is_empty() && !newer_run && (has_open || i.state != State::Ready) {
            if status == Status::Blocked && has_open {
                pull_states.push(status);
            }
            status = combined(pull_states);
        }
        tickets.push(TicketView {
            key: i.key.clone(),
            title: i.title.clone(),
            status,
            details: notes,
        });
    }
    stages
        .get_mut(&Stage::Code)
        .ok_or_else(|| anyhow::anyhow!("missing stage"))?
        .status = combined(tickets.iter().map(|t| t.status));
    for (stage, states) in [
        (Stage::Review, review_states),
        (Stage::Build, build_states),
        (Stage::Test, test_states),
    ] {
        stages
            .get_mut(&stage)
            .ok_or_else(|| anyhow::anyhow!("missing stage"))?
            .status = combined(states);
    }
    let mut rework = Vec::new();
    let mut rework_states: BTreeMap<Stage, Vec<Status>> = BTreeMap::new();
    for r in s.rework.iter().filter(|r| r.state != State::Done) {
        let target = r.target;
        rework.push(format!(
            "{} {:?} -> {} ({})",
            safe(&r.key),
            r.kind,
            target.name(),
            r.state.name()
        ));
        if r.kind != ReworkKind::Question {
            let view = stages
                .get_mut(&target)
                .ok_or_else(|| anyhow::anyhow!("missing stage"))?;
            rework_states
                .entry(target)
                .or_default()
                .push(workflow(r.state));
            view.details.push(format!("rework {}", safe(&r.key)));
        }
    }
    for (stage, states) in rework_states {
        let view = stages.get_mut(&stage).context("missing stage")?;
        if !matches!(view.status, Status::Stale | Status::Blocked) {
            view.status = combined(states);
        }
    }
    tickets.sort_by(|a, b| a.key.cmp(&b.key));
    for stage in stages.values_mut() {
        stage.details.sort();
    }
    rework.sort();
    Ok(View {
        story: story.into(),
        title: s.story.title.clone(),
        requester: s.story.reporter.clone(),
        stages: stages.into_values().collect(),
        tickets,
        rework,
    })
}

/// The host serializes refreshes and record replacement per story, from load through publish.
/// This is an operator hook, never a sandbox tool or a source of merge approval.
pub fn refresh(
    source: &dyn Source,
    tracker: &dyn Tracker,
    audit: &AuditLog,
    story: &str,
) -> Result<View> {
    refresh_with_writer(source, tracker, audit, story, |key, text, _intent| {
        tracker.status_comment(key, text)
    })
}

/// `refresh` with the write replaced by `writer(story, text, audit_intent)`, called only after the
/// audit intent is appended, so a host can persist its own write guard at exactly that point.
pub fn refresh_with_writer(
    source: &dyn Source,
    tracker: &dyn Tracker,
    audit: &AuditLog,
    story: &str,
    writer: impl FnOnce(&str, &str, u64) -> Result<()>,
) -> Result<View> {
    let view = load(source, story)?;
    let issue = tracker.get_issue(story)?;
    if issue.key != view.story {
        bail!("tracker returned another story");
    }
    let text = view.render();
    let intent = audit.append(RUN_AUDIT, &json!({"kind": "workflow_status_comment_intent", "tracker": tracker.name(), "story": story, "text": text}))?;
    match writer(story, &text, intent) {
        Ok(()) => {
            audit.append(RUN_AUDIT, &json!({"kind": "workflow_status_comment_posted", "tracker": tracker.name(), "story": story, "intent": intent})).context("comment posted but recording outcome failed; reconcile before retry")?;
        }
        Err(error) => {
            let reason = redact(&format!("{error:#}"));
            let failure = anyhow::anyhow!(reason.clone());
            if let Err(outcome) = audit.append(RUN_AUDIT, &json!({"kind": "workflow_status_comment_failed", "tracker": tracker.name(), "story": story, "intent": intent, "error": reason})) {
                return Err(failure.context(format!("recording failed comment outcome: {}", redact(&format!("{outcome:#}")))));
            }
            return Err(failure);
        }
    }
    Ok(view)
}
