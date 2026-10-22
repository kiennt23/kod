use kod_gates::{CHECKS, Check, Conclusion, Evidence};
use kod_status::{
    Stage,
    records::{Artifact, CheckRecord, PrState, Pull, ReworkRecord, Snapshot},
};
use kod_tracker::{Issue, ReworkKind, State, Tracker, fake::sandbox};

pub fn snapshot() -> Snapshot {
    let (tracker, _) = sandbox();
    let mut story = tracker.get_issue("FAKE-1").unwrap();
    story.approved_versions = vec!["design".into()];
    story.links = vec!["TASK-1".into(), "REWORK-1".into()];
    let ticket = Issue {
        key: "TASK-1".into(),
        title: "CSV writer".into(),
        state: State::InReview,
        links: vec![],
        ..story.clone()
    };
    Snapshot {
        story,
        tickets: vec![ticket],
        artifacts: vec![Artifact {
            link: "design".into(),
            stage: Stage::Design,
            approved: Some("design-v3".into()),
            draft: None,
            in_review: false,
        }],
        pulls: vec![Pull {
            ticket: "TASK-1".into(),
            evidence: Evidence {
                repo: "acme/app".into(),
                pr: 91,
                head: "a".repeat(40),
                run: "run-2".into(),
                story: "FAKE-1".into(),
                team: "team".into(),
                pins: vec!["design-v2".into()],
                policy_version: "policy".into(),
                runner_config_version: "runner".into(),
            },
            head: "a".repeat(40),
            state: PrState::Open,
            draft: true,
            checks: CHECKS
                .into_iter()
                .map(|name| CheckRecord {
                    sha: "a".repeat(40),
                    check: Check {
                        name: name.into(),
                        conclusion: Conclusion::Success,
                        summary: "ok".into(),
                    },
                })
                .collect(),
        }],
        rework: vec![ReworkRecord {
            key: "REWORK-1".into(),
            kind: ReworkKind::Amendment,
            target: Stage::Design,
            state: State::Ready,
        }],
    }
}
