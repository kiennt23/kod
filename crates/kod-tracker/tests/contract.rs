use anyhow::Result;
use kod_tracker::{
    Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook,
    contract::check,
    fake::{FakeTracker, SIGNATURE, sandbox},
};
use std::{
    collections::HashMap,
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
#[derive(Clone, Copy, Debug)]
enum Flaw {
    IgnoresSignatures,
    /// Accepts any request that has a signature header, whatever its value.
    ChecksPresenceOnly,
    /// Keeps no delivery IDs, so a recorded webhook can be sent again.
    AcceptsReplays,
    EveryoneIsAMember,
    TransitionDoesNothing,
    MissingIssueIsOk,
    DropsRequirementId,
    DropsLinks,
}

/// The fake with one flaw, to show the contract catches it. `seen` holds
/// what AcceptsReplays answered, by body.
struct Broken(FakeTracker, Flaw, Mutex<HashMap<Vec<u8>, Option<Delivery>>>);

impl Tracker for Broken {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn get_issue(&self, key: &str) -> Result<Issue> {
        let mut i = self.0.get_issue(key)?;
        match self.1 {
            Flaw::DropsRequirementId => i.requirement_id = None,
            Flaw::DropsLinks => i.links.clear(),
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
        match self.1 {
            Flaw::TransitionDoesNothing => Ok(()),
            _ => self.0.transition(key, state),
        }
    }
    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        self.0.link_pr(key, url)
    }
    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
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
            Flaw::AcceptsReplays => {
                let mut seen = self.2.lock().unwrap();
                if let Some(d) = seen.get(&request.body) {
                    return Ok(d.clone());
                }
                let d = self.0.parse_webhook(&request)?;
                seen.insert(request.body.clone(), d.clone());
                return Ok(d);
            }
            _ => {}
        }
        self.0.parse_webhook(&request)
    }
}

#[test]
fn the_contract_fails_an_adapter_with_any_one_flaw() {
    for flaw in [
        Flaw::IgnoresSignatures,
        Flaw::ChecksPresenceOnly,
        Flaw::AcceptsReplays,
        Flaw::EveryoneIsAMember,
        Flaw::TransitionDoesNothing,
        Flaw::MissingIssueIsOk,
        Flaw::DropsRequirementId,
        Flaw::DropsLinks,
    ] {
        let (t, s) = sandbox();
        let broken = Broken(t, flaw, Mutex::default());
        let r = catch_unwind(AssertUnwindSafe(|| check(&broken, &s)));
        assert!(
            r.is_err(),
            "the contract passed an adapter whose flaw is {flaw:?}"
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
