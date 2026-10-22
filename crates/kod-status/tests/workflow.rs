mod common;

use common::snapshot;
use kod_audit::AuditLog;
use kod_dispatcher::{RunRow, RunState};
use kod_gates::Conclusion;
use kod_status::{
    Stage, Status, compute, load,
    records::{Records, Snapshot, Source},
    refresh,
};
use kod_tracker::{ReworkKind, State, Tracker, fake::sandbox};
use std::sync::Mutex;

struct Memory {
    snapshot: Mutex<Snapshot>,
}

impl Source for Memory {
    fn load(&self, _: &str) -> anyhow::Result<Records> {
        Ok(Records {
            snapshot: self.snapshot.lock().unwrap().clone(),
            runs: vec![run()],
        })
    }
}

fn run() -> RunRow {
    RunRow {
        id: "run-2".into(),
        ticket: "TASK-1".into(),
        revision: "revision".into(),
        state: RunState::Done,
        reason: String::new(),
    }
}

fn stage(view: &kod_status::View, stage: Stage) -> Status {
    view.stages
        .iter()
        .find(|s| s.stage == stage)
        .unwrap()
        .status
}

#[test]
fn stale_pr_and_open_rework_are_visible_in_the_same_story_view() {
    let records = Records {
        snapshot: snapshot(),
        runs: vec![run()],
    };
    let view = compute(&records, "FAKE-1").unwrap();
    assert_eq!(stage(&view, Stage::Design), Status::NotStarted);
    assert_eq!(stage(&view, Stage::Code), Status::Stale);
    assert_eq!(stage(&view, Stage::Test), Status::Stale);
    assert_eq!(stage(&view, Stage::Release), Status::Unknown);
    let text = view.render();
    for want in [
        "approved design-v3",
        "PR #91",
        "stale inputs or head",
        "REWORK-1 Amendment -> Design",
        "run-2 done",
        "tests success",
        "secret-scan success",
        "pinned revisions: design-v2; current approved: design-v3",
    ] {
        assert!(text.contains(want), "missing {want}: {text}");
    }
}

#[test]
fn refreshing_rereads_records_and_updates_only_one_owned_comment() {
    let (tracker, _) = sandbox();
    let source = Memory {
        snapshot: Mutex::new(snapshot()),
    };
    let dir = tempfile::tempdir().unwrap();
    let audit = AuditLog::open(&dir.path().join("audit.db")).unwrap();
    tracker.comment("FAKE-1", "human comment").unwrap();
    let first = refresh(&source, &tracker, &audit, "FAKE-1").unwrap();
    assert_eq!(
        tracker.comments("FAKE-1"),
        vec!["human comment".to_owned(), first.render()]
    );
    {
        let mut s = source.snapshot.lock().unwrap();
        s.pulls[0].evidence.pins = vec!["design-v3".into()];
        s.rework[0].state = State::Done;
    }
    let second = refresh(&source, &tracker, &audit, "FAKE-1").unwrap();
    assert_eq!(stage(&second, Stage::Design), Status::Approved);
    assert_eq!(stage(&second, Stage::Code), Status::InProgress);
    assert_eq!(stage(&second, Stage::Test), Status::Approved);
    assert_eq!(
        tracker.comments("FAKE-1"),
        vec!["human comment".to_owned(), second.render()]
    );
    assert_eq!(load(&source, "FAKE-1").unwrap().render(), second.render());
    assert!(tracker.status_comment("missing", "x").is_err());
}

#[test]
fn audit_failure_prevents_a_status_comment_write() {
    let (tracker, _) = sandbox();
    let source = Memory {
        snapshot: Mutex::new(snapshot()),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let audit = AuditLog::open(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(refresh(&source, &tracker, &audit, "FAKE-1").is_err());
    assert!(tracker.comments("FAKE-1").is_empty());
}

#[test]
fn a_draft_revision_does_not_supersede_an_approved_revision() {
    let mut s = snapshot();
    s.rework.clear();
    s.artifacts[0].draft = Some("design-v4".into());
    s.artifacts[0].in_review = true;
    s.pulls[0].evidence.pins = vec!["design-v3".into()];
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Code), Status::InProgress);
    assert_eq!(stage(&v, Stage::Design), Status::InReview);
    s.artifacts[0].approved = Some("design-v4".into());
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Code), Status::Stale);
}

#[test]
fn old_head_checks_never_appear_as_current_success() {
    let mut s = snapshot();
    s.pulls[0].head = "b".repeat(40);
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Test), Status::Stale);
    assert!(v.render().contains("tests unknown"));
    assert!(!v.render().contains("tests success"));
}

#[test]
fn unknown_or_failed_current_checks_are_not_approved() {
    let mut s = snapshot();
    s.rework.clear();
    s.pulls[0].evidence.pins = vec!["design-v3".into()];
    s.pulls[0].checks.retain(|c| c.check.name != "tests");
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Test), Status::Unknown);
    s.pulls[0].checks[0].check.conclusion = Conclusion::Failure;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Code), Status::Blocked);
}

#[test]
fn questions_do_not_rewind_stages_and_replans_return_to_planning() {
    let mut s = snapshot();
    s.rework[0].kind = ReworkKind::Question;
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Design), Status::Approved);
    s.rework[0].kind = ReworkKind::Replan;
    s.rework[0].target = Stage::Plan;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Plan), Status::NotStarted);
}

#[test]
fn latest_run_uses_numeric_sequence_and_done_tickets_keep_their_state() {
    let mut s = snapshot();
    s.pulls.clear();
    let mut old = run();
    old.id = "run-9".into();
    old.state = RunState::Blocked;
    let current = RunRow {
        id: "run-10".into(),
        state: RunState::Running,
        ..run()
    };
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![current.clone(), old.clone()],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(v.tickets[0].status, Status::InProgress);
    assert!(v.render().contains("run-10 running"));
    s.tickets[0].state = State::Done;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![current, old],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(v.tickets[0].status, Status::Approved);
}

#[test]
fn malformed_or_cross_story_records_are_rejected() {
    let original = snapshot();
    let mut cases = Vec::new();
    let mut s = original.clone();
    s.pulls[0].evidence.story = "other".into();
    cases.push(s);
    let mut s = original.clone();
    s.story.links.clear();
    cases.push(s);
    let mut s = original.clone();
    s.artifacts.push(s.artifacts[0].clone());
    cases.push(s);
    let mut s = original.clone();
    let duplicate = s.pulls[0].checks[0].clone();
    s.pulls[0].checks.push(duplicate);
    cases.push(s);
    for snapshot in cases {
        assert!(
            compute(
                &Records {
                    snapshot,
                    runs: vec![]
                },
                "FAKE-1"
            )
            .is_err()
        );
    }
    assert!(
        compute(
            &Records {
                snapshot: original,
                runs: vec![]
            },
            "other"
        )
        .is_err()
    );
}

#[test]
fn comments_cannot_inject_mentions_terminal_controls_or_credentials() {
    let mut s = snapshot();
    s.story.title = "@all [click](https://evil.example)\n\u{1b}[2J api_key=super-secret".into();
    let text = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap()
    .render();
    for forbidden in ["@all", "[click]", "\u{1b}", "super-secret", "https://"] {
        assert!(!text.contains(forbidden), "{text}");
    }
}

#[test]
fn closed_and_merged_prs_remain_history_after_approvals_change() {
    let mut s = snapshot();
    s.rework.clear();
    s.pulls[0].state = kod_status::records::PrState::Merged;
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![run()],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Code), Status::Approved);
    assert_eq!(stage(&v, Stage::Review), Status::Approved);
    assert_eq!(stage(&v, Stage::Build), Status::Approved);
    assert_eq!(stage(&v, Stage::Test), Status::Approved);
    assert!(v.render().contains("PR #91 merged"));
    assert!(!v.render().contains("stale inputs"));
    s.pulls[0].state = kod_status::records::PrState::Closed;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert!(v.render().contains("PR #91 closed"));
    assert_eq!(stage(&v, Stage::Review), Status::Unknown);
}

#[test]
fn status_pins_use_exactly_the_same_required_set_as_the_gate() {
    let mut s = snapshot();
    s.rework.clear();
    s.tickets[0].approved_versions.push("ticket-only".into());
    for pins in [
        vec!["design-v3".into()],
        vec!["design-v3".into(), "ticket-v1".into()],
    ] {
        s.pulls[0].evidence.pins = pins.clone();
        let records = Records {
            snapshot: s.clone(),
            runs: vec![],
        };
        let view = compute(&records, "FAKE-1").unwrap();
        assert_eq!(stage(&view, Stage::Code) == Status::Stale, pins.len() != 1);
    }
}

#[test]
fn ready_tickets_are_not_blocked_by_a_previous_terminal_run() {
    let mut s = snapshot();
    s.pulls.clear();
    s.tickets[0].state = State::Ready;
    for state in [RunState::Canceled, RunState::OverBudget, RunState::Blocked] {
        let r = RunRow { state, ..run() };
        let v = compute(
            &Records {
                snapshot: s.clone(),
                runs: vec![r],
            },
            "FAKE-1",
        )
        .unwrap();
        assert_eq!(v.tickets[0].status, Status::NotStarted);
    }
}

#[test]
fn rework_uses_its_workflow_state_and_does_not_hide_stale_work() {
    let mut s = snapshot();
    for (state, expected) in [
        (State::Ready, Status::NotStarted),
        (State::InProgress, Status::InProgress),
        (State::InReview, Status::InReview),
        (State::Blocked, Status::Blocked),
    ] {
        s.rework[0].state = state;
        let v = compute(
            &Records {
                snapshot: s.clone(),
                runs: vec![],
            },
            "FAKE-1",
        )
        .unwrap();
        assert_eq!(stage(&v, Stage::Design), expected);
    }
    s.rework[0].target = Stage::Code;
    s.rework[0].state = State::InReview;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Code), Status::Stale);
}

#[test]
fn a_replan_for_anything_other_than_planning_is_rejected() {
    let mut s = snapshot();
    s.rework[0].kind = ReworkKind::Replan;
    assert!(
        compute(
            &Records {
                snapshot: s,
                runs: vec![]
            },
            "FAKE-1"
        )
        .is_err()
    );
}

#[test]
fn an_older_merged_pr_does_not_hide_a_later_run_or_a_ready_transition() {
    let mut s = snapshot();
    s.pulls[0].state = kod_status::records::PrState::Merged;
    let r = RunRow {
        id: "run-3".into(),
        state: RunState::Running,
        ..run()
    };
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![r],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(v.tickets[0].status, Status::InProgress);
    s.tickets[0].state = State::Ready;
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![run()],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(v.tickets[0].status, Status::NotStarted);
}

#[test]
fn tracker_markup_and_autolinks_are_neutralized_in_untrusted_fields() {
    let mut s = snapshot();
    s.story.title =
        "+underlined+ ^super^ ??cite?? https://evil.example www.evil.example [label|target]".into();
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap()
    .render();
    for markup in [
        "+underlined+",
        "^super^",
        "??cite??",
        "https://",
        "www.",
        "[label|target]",
    ] {
        assert!(!v.contains(markup), "{v}");
    }
    assert!(v.contains("underlined"));
}

#[test]
fn incomplete_document_sets_and_blocked_rework_do_not_look_complete() {
    let mut s = snapshot();
    s.rework.clear();
    let mut a = s.artifacts[0].clone();
    a.link = "threat-model".into();
    a.approved = None;
    s.artifacts.push(a);
    let v = compute(
        &Records {
            snapshot: s.clone(),
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Design), Status::InProgress);
    let mut blocked = snapshot().rework[0].clone();
    blocked.key = "REWORK-2".into();
    blocked.state = State::Blocked;
    s.story.links.push(blocked.key.clone());
    s.rework = vec![blocked, snapshot().rework[0].clone()];
    let v = compute(
        &Records {
            snapshot: s,
            runs: vec![],
        },
        "FAKE-1",
    )
    .unwrap();
    assert_eq!(stage(&v, Stage::Design), Status::Blocked);
}
