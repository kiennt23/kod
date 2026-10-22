use anyhow::Result;
use kod_tracker::{
    Delivery, Issue, Rework, ReworkKind, State, TaskEvent, Tracker, Webhook,
    contract::check,
    fake::{FakeTracker, SIGNATURE, sandbox},
};
use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

#[test]
fn the_fake_passes_the_contract() {
    let (t, s) = sandbox();
    check(&t, &s);
    // What the check wrote is where a reader of the fake can see it.
    assert_eq!(t.comments("FAKE-1"), ["kod contract test"]);
    assert_eq!(t.prs("FAKE-1"), ["https://github.com/acme/app/pull/1"]);
    let story = t.issue("FAKE-1").unwrap();
    assert_eq!(story.workflow_state, "Closed");
    assert_eq!(story.links, ["REWORK-1"]);
    assert_eq!(t.issue("REWORK-1").unwrap().links, ["FAKE-1"]);
}

/// One way an adapter can be wrong.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Flaw {
    IgnoresSignatures,
    /// Accepts any request that has a signature header, whatever its value.
    ChecksPresenceOnly,
    /// Accepts a request with no signature header, but rejects a bad one.
    AcceptsUnsigned,
    /// Refuses a delivery ID it has seen, before the dispatcher stored it.
    SpendsIds,
    /// Reports the removal of any label, not only the trigger.
    UnlabelsAnyLabel,
    /// Reads no edit events.
    IgnoresEdits,
    /// Loses who removed the label.
    DropsUnlabeledBy,
    EveryoneIsAMember,
    TransitionDoesNothing,
    MissingIssueIsOk,
    MissingReadIsOk,
    MissingTransitionIsOk,
    MissingLinkIsOk,
    MissingReworkIsOk,
    DropsReworkFields,
    DropsReworkBacklink,
    DropsRequirementId,
    DropsLinks,
}

/// The fake with one flaw, to show the contract catches it. `seen` holds
/// the bodies SpendsIds has parsed.
struct Broken(FakeTracker, Flaw, Mutex<HashSet<Vec<u8>>>);

impl Tracker for Broken {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn get_issue(&self, key: &str) -> Result<Issue> {
        if self.1 == Flaw::MissingReadIsOk && key == "FAKE-404" {
            return self.0.get_issue("FAKE-1");
        }
        let mut i = self.0.get_issue(key)?;
        if key.starts_with("REWORK-") {
            if self.1 == Flaw::DropsReworkFields {
                i.body = "kod contract test: is the limit 100 or 1000?".into();
            }
            if self.1 == Flaw::DropsReworkBacklink {
                i.links.clear();
            }
        }
        match self.1 {
            Flaw::DropsRequirementId => i.requirement_id = None,
            Flaw::DropsLinks if key == "FAKE-1" => i.links.clear(),
            _ => {}
        }
        Ok(i)
    }
    fn comment(&self, key: &str, text: &str) -> Result<()> {
        match self.1 {
            Flaw::MissingIssueIsOk => Ok(()),
            _ => self.0.comment(key, text),
        }
    }
    fn transition(&self, key: &str, state: State) -> Result<()> {
        if self.1 == Flaw::MissingTransitionIsOk && key == "FAKE-404" {
            return Ok(());
        }
        match self.1 {
            Flaw::TransitionDoesNothing => Ok(()),
            _ => self.0.transition(key, state),
        }
    }
    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        if self.1 == Flaw::MissingLinkIsOk && key == "FAKE-404" {
            return Ok(());
        }
        self.0.link_pr(key, url)
    }
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
        if self.1 == Flaw::MissingReworkIsOk && story == "FAKE-404" {
            return Ok("nonexistent".into());
        }
        self.0.create_linked(story, kind, fields)
    }
    fn is_team_member(&self, user: &str) -> Result<bool> {
        match self.1 {
            Flaw::EveryoneIsAMember => Ok(true),
            _ => self.0.is_team_member(user),
        }
    }
    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>> {
        let mut request = request.clone();
        match self.1 {
            // As if the check were skipped: every request is signed afresh.
            Flaw::IgnoresSignatures => {
                request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
            }
            Flaw::ChecksPresenceOnly if request.header(SIGNATURE).is_some() => {
                request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
            }
            Flaw::AcceptsUnsigned if request.header(SIGNATURE).is_none() => {
                request.headers = vec![(SIGNATURE.into(), self.0.sign(&request.body))];
            }
            Flaw::UnlabelsAnyLabel => {
                let mut v: serde_json::Value = serde_json::from_slice(&request.body)?;
                if v["type"] == "label_removed" {
                    v["label"] = "kod".into();
                    request = self.0.webhook(&v);
                }
            }
            _ => {}
        }
        let d = self.0.parse_webhook(&request)?;
        // Only a body that parsed can be spent, as a forged one never is.
        if self.1 == Flaw::SpendsIds && d.is_some() && !self.2.lock().unwrap().insert(request.body)
        {
            anyhow::bail!("a replay");
        }
        Ok(match (self.1, d) {
            (
                Flaw::IgnoresEdits,
                Some(Delivery {
                    event: TaskEvent::Edited { .. },
                    ..
                }),
            ) => None,
            (Flaw::DropsUnlabeledBy, Some(mut d)) => {
                if let TaskEvent::Unlabeled { by, .. } = &mut d.event {
                    by.clear();
                }
                Some(d)
            }
            (_, d) => d,
        })
    }
}

#[test]
fn the_contract_fails_an_adapter_with_any_one_flaw() {
    for (flaw, reason) in [
        (Flaw::IgnoresSignatures, "a changed body passed"),
        (Flaw::ChecksPresenceOnly, "a changed body passed"),
        (Flaw::AcceptsUnsigned, "an unsigned webhook was accepted"),
        (Flaw::SpendsIds, "redelivered trigger"),
        (Flaw::UnlabelsAnyLabel, "other than the trigger"),
        (Flaw::IgnoresEdits, "an edit was not reported"),
        (Flaw::DropsUnlabeledBy, "who removed it"),
        (Flaw::EveryoneIsAMember, "outsider"),
        (Flaw::TransitionDoesNothing, "transition to"),
        (Flaw::MissingIssueIsOk, "comment on a missing issue"),
        (Flaw::MissingReadIsOk, "get_issue on a missing issue"),
        (Flaw::MissingTransitionIsOk, "transition on a missing issue"),
        (Flaw::MissingLinkIsOk, "link_pr on a missing issue"),
        (Flaw::MissingReworkIsOk, "create_linked on a missing story"),
        (Flaw::DropsReworkFields, "lacks its stage"),
        (Flaw::DropsReworkBacklink, "has no story back-link"),
        (Flaw::DropsRequirementId, "get_issue"),
        (Flaw::DropsLinks, "does not link"),
    ] {
        let (t, s) = sandbox();
        let broken = Broken(t, flaw, Mutex::default());
        let err = catch_unwind(AssertUnwindSafe(|| check(&broken, &s))).expect_err(&format!(
            "the contract passed an adapter whose flaw is {flaw:?}"
        ));
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains(reason),
            "flaw {flaw:?} failed for the wrong reason (wanted {reason:?}): {msg}"
        );
    }
}

#[test]
fn the_contract_refuses_a_walk_that_misses_a_state() {
    let (t, mut s) = sandbox();
    s.transitions.retain(|st| *st != State::Blocked);
    let r = catch_unwind(AssertUnwindSafe(|| check(&t, &s)));
    assert!(r.is_err(), "a walk without blocked passed");
}
