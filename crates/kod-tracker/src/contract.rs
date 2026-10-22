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
    /// The trigger with only its signature header removed.
    pub unsigned: Webhook,
    /// A signed webhook with no delivery ID.
    pub unidentified: Webhook,
    /// A recorded comment webhook, and its delivery.
    pub comment: Webhook,
    pub comment_event: Delivery,
    /// A recorded, signed webhook that is not a task event.
    pub ignored: Webhook,
    /// A recorded webhook removing the trigger label, and its delivery.
    pub untrigger: Webhook,
    pub untrigger_event: Delivery,
    /// A recorded webhook removing some other label: not a task event.
    pub other_unlabel: Webhook,
    /// A recorded webhook for an edit of the story's fields, and its delivery.
    pub edited: Webhook,
    pub edited_event: Delivery,
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
    for (field, value) in [
        ("stage", rework.stage.as_str()),
        ("artifact", rework.artifact.as_str()),
        ("version", rework.version.as_str()),
        ("evidence", rework.evidence[0].as_str()),
        ("kind", "question"),
    ] {
        assert!(
            r.body.to_lowercase().contains(&value.to_lowercase()),
            "the rework request lacks its {field}: {r:?}"
        );
    }
    assert!(
        r.links.iter().any(|linked| linked == key),
        "the rework request has no story back-link: {r:?}"
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
    // Forgeries are errors, never a quiet None.
    assert!(
        parse(&s.tampered).is_err(),
        "a changed body passed the signature check"
    );
    assert!(
        parse(&s.wrong_signature).is_err(),
        "a wrong signature value was accepted"
    );
    assert!(
        parse(&s.unsigned).is_err(),
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
    assert_eq!(
        parse(&s.untrigger).expect("untrigger webhook"),
        Some(s.untrigger_event.clone()),
        "the unlabel was not reported with who removed it"
    );
    assert_eq!(
        parse(&s.other_unlabel).expect("other unlabel webhook"),
        None,
        "removing a label other than the trigger is not a task event"
    );
    assert_eq!(
        parse(&s.edited).expect("edited webhook"),
        Some(s.edited_event.clone()),
        "an edit was not reported"
    );
    // The adapter keeps no state: a redelivery parses the same, and the
    // dispatcher's store, not the adapter, refuses it as a duplicate. An
    // adapter that spent IDs would lose an event whose first write failed.
    assert_eq!(
        parse(&s.trigger).expect("redelivered trigger"),
        Some(s.trigger_event.clone()),
        "a redelivery parsed differently"
    );
    assert_eq!(
        parse(&s.comment).expect("redelivered comment"),
        Some(s.comment_event.clone()),
        "a redelivery parsed differently"
    );
}
