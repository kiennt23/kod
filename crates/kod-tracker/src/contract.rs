//! The contract every adapter must pass, against a recorded webhook and a
//! sandbox project. An adapter counts as supported once `check` passes.
//! It panics on the first broken rule, like a test.
use crate::{Delivery, Issue, Rework, ReworkKind, State, Tracker, Webhook};

/// What the sandbox project holds. For a real tracker: a story set up by
/// hand, and webhooks recorded from it.
pub struct Sandbox {
    /// A story as `get_issue` must read it, with no links yet. Its state is
    /// not compared: the check moves the story along `transitions`.
    pub issue: Issue,
    /// A walk through the tracker's workflow from the story's state, as its
    /// rules allow; it must reach all 5 standard states.
    pub transitions: Vec<State>,
    /// A key with no issue.
    pub missing: String,
    /// A user in the team group, and one outside it.
    pub member: String,
    pub outsider: String,
    /// A recorded webhook adding the trigger label, and its delivery.
    pub trigger: Webhook,
    pub trigger_event: Delivery,
    /// The trigger with a value in its body changed (still valid JSON, same
    /// delivery ID) and its signature kept.
    pub tampered: Webhook,
    /// The trigger with one character of its signature value changed.
    pub wrong_signature: Webhook,
    /// A signed webhook with no delivery ID.
    pub unidentified: Webhook,
    /// A recorded comment webhook, and its delivery.
    pub comment: Webhook,
    pub comment_event: Delivery,
    /// A recorded, signed webhook that is not a task event.
    pub ignored: Webhook,
}

pub fn check(t: &dyn Tracker, s: &Sandbox) {
    let key = s.issue.key.as_str();
    let missing = s.missing.as_str();
    assert!(!t.name().is_empty(), "the tracker has no name");

    let got = t.get_issue(key).expect("get_issue on the sandbox story");
    assert_eq!(
        got,
        Issue {
            state: got.state,
            ..s.issue.clone()
        },
        "get_issue"
    );
    assert!(
        t.get_issue(missing).is_err(),
        "get_issue on a missing issue"
    );

    for state in State::ALL {
        assert!(
            s.transitions.contains(&state),
            "the sandbox walk never reaches {}",
            state.name()
        );
    }
    for &state in &s.transitions {
        t.transition(key, state).expect("transition");
        let now = t.get_issue(key).expect("get_issue after transition").state;
        assert_eq!(now, state, "transition to {}", state.name());
    }
    assert!(
        t.transition(missing, State::Ready).is_err(),
        "transition on a missing issue"
    );

    t.comment(key, "kod contract test").expect("comment");
    assert!(
        t.comment(missing, "x").is_err(),
        "comment on a missing issue"
    );
    t.link_pr(key, "https://github.com/acme/app/pull/1")
        .expect("link_pr");
    assert!(
        t.link_pr(missing, "https://github.com/acme/app/pull/1")
            .is_err(),
        "link_pr on a missing issue"
    );

    let rework = Rework {
        stage: "build".into(),
        artifact: "design".into(),
        version: "rev-3".into(),
        text: "kod contract test: is the limit 100 or 1000?".into(),
        evidence: vec!["run r1".into()],
    };
    let created = t
        .create_linked(key, ReworkKind::Question, &rework)
        .expect("create_linked");
    assert_ne!(created, key, "create_linked returned the story's key");
    let r = t
        .get_issue(&created)
        .expect("get_issue on the rework request");
    assert_eq!(r.state, State::Ready, "a new rework request is ready");
    assert!(
        r.body.contains(&rework.text),
        "the rework request lacks its text: {r:?}"
    );
    let story = t.get_issue(key).expect("get_issue after create_linked");
    assert!(
        story.links.contains(&created),
        "the story does not link its rework request: {story:?}"
    );
    assert!(
        t.create_linked(missing, ReworkKind::Question, &rework)
            .is_err(),
        "create_linked on a missing story"
    );

    assert!(
        t.is_team_member(&s.member).expect("is_team_member"),
        "member"
    );
    assert!(
        !t.is_team_member(&s.outsider).expect("is_team_member"),
        "outsider"
    );

    let parse = |w: &Webhook| t.parse_webhook(w);
    // Forgeries first, while the trigger's delivery ID is unspent, so only a
    // signature check can refuse them; they are errors, never a quiet None.
    assert!(
        parse(&s.tampered).is_err(),
        "a changed body passed the signature check"
    );
    assert!(
        parse(&s.wrong_signature).is_err(),
        "a wrong signature value was accepted"
    );
    let unsigned = Webhook {
        headers: vec![],
        body: s.trigger.body.clone(),
    };
    assert!(
        parse(&unsigned).is_err(),
        "an unsigned webhook was accepted"
    );
    assert!(
        parse(&s.unidentified).is_err(),
        "a webhook with no delivery ID was accepted"
    );
    assert_eq!(
        parse(&s.trigger).expect("trigger webhook"),
        Some(s.trigger_event.clone())
    );
    assert_eq!(
        parse(&s.comment).expect("comment webhook"),
        Some(s.comment_event.clone())
    );
    assert_eq!(parse(&s.ignored).expect("ignored webhook"), None);
    assert!(
        parse(&s.trigger).is_err(),
        "a replayed trigger was accepted"
    );
    assert!(
        parse(&s.comment).is_err(),
        "a replayed comment was accepted"
    );
}
