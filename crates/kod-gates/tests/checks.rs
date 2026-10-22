use anyhow::{Result, bail};
use kod_audit::AuditLog;
use kod_gates::{
    Artifacts, CHECKS, Check, Conclusion, Config, Evidence, Forge, Gates, PullRequest, Review,
    ReviewState, Scanner,
};
use kod_policy::Policy;
use kod_tracker::fake::{self, FakeTracker};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PIN: &str = "https://docs.example/brief?rev=3";

struct Docs(Mutex<BTreeMap<String, String>>);
impl Artifacts for Docs {
    fn current(&self, link: &str) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(link)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown revision"))
    }
}

struct Git {
    pr: Mutex<PullRequest>,
    reports: Mutex<Vec<(String, Check)>>,
    reads: AtomicUsize,
    change_on_read: bool,
}
impl Forge for Git {
    fn pull(&self, _: u64) -> Result<PullRequest> {
        let mut pr = self.pr.lock().unwrap().clone();
        if self.reads.fetch_add(1, Ordering::SeqCst) > 0 && self.change_on_read {
            pr.head = BASE.into();
        }
        Ok(pr)
    }
    fn report(&self, sha: &str, check: &Check) -> Result<()> {
        self.reports
            .lock()
            .unwrap()
            .push((sha.into(), check.clone()));
        Ok(())
    }
}
#[derive(Default)]
struct Scans {
    fail: Option<String>,
    calls: Mutex<Vec<(String, String)>>,
}
impl Scanner for Scans {
    fn version(&self) -> Result<String> {
        Ok("fake-scanner-1".into())
    }
    fn scan(&self, name: &str, sha: &str) -> Result<()> {
        self.calls.lock().unwrap().push((name.into(), sha.into()));
        if self.fail.as_deref() == Some(name) {
            bail!("seeded scan failure");
        }
        Ok(())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    policy: Policy,
    audit: AuditLog,
    tracker: FakeTracker,
    docs: Docs,
    git: Git,
    scans: Scans,
    evidence: Evidence,
}
fn review(id: u64, who: &str) -> Review {
    Review {
        id,
        login: who.into(),
        human: true,
        can_approve: true,
        commit: HEAD.into(),
        state: ReviewState::Approved,
    }
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = Policy::load(&root.join("policy")).unwrap();
    let (tracker, _) = fake::sandbox();
    let mut story = tracker.issue("FAKE-1").unwrap();
    story.reporter = "requester".into();
    story.approved_versions = vec![PIN.into()];
    tracker.add_issue("FAKE-1", story);
    Fixture {
        config: Config {
            repo: "acme/app".into(),
            protect_all: false,
            protected: vec!["critical".into()],
            codeowners: vec!["kiennt23".into()],
        },
        evidence: Evidence {
            repo: "acme/app".into(),
            pr: 42,
            head: HEAD.into(),
            run: "r1".into(),
            story: "FAKE-1".into(),
            team: "core".into(),
            pins: vec![PIN.into()],
            policy_version: policy.version().into(),
            runner_config_version: "runner-sha256:123".into(),
        },
        policy,
        audit: AuditLog::open(&dir.path().join("audit.db")).unwrap(),
        tracker,
        docs: Docs(Mutex::new(BTreeMap::from([(PIN.into(), PIN.into())]))),
        git: Git {
            pr: Mutex::new(PullRequest {
                number: 42,
                head: HEAD.into(),
                base: BASE.into(),
                author: "author".into(),
                paths: vec!["src/main.rs".into()],
                reviews: vec![review(1, "reviewer")],
            }),
            reports: Mutex::default(),
            reads: AtomicUsize::new(0),
            change_on_read: false,
        },
        scans: Scans::default(),
        dir,
    }
}
impl Fixture {
    fn gates(&self) -> Gates<'_> {
        Gates {
            config: &self.config,
            policy: &self.policy,
            audit: &self.audit,
            tracker: &self.tracker,
            artifacts: &self.docs,
            forge: &self.git,
            scanner: &self.scans,
        }
    }
    fn run(&self) -> Vec<Check> {
        self.gates().run(&self.evidence, HEAD).unwrap()
    }
    fn fails(&self, name: &str) {
        let checks = self.run();
        assert_eq!(
            checks.iter().find(|c| c.name == name).unwrap().conclusion,
            Conclusion::Failure
        );
    }
}

#[test]
fn all_checks_pass_for_current_inputs_and_an_independent_approval() {
    let f = fixture();
    let checks = f.run();
    assert_eq!(checks.len(), 7);
    assert!(checks.iter().all(|c| c.conclusion == Conclusion::Success));
    let reports = f.git.reports.lock().unwrap();
    assert!(
        reports[..7]
            .iter()
            .all(|(_, c)| c.conclusion == Conclusion::Pending)
    );
    assert!(reports.iter().all(|(sha, _)| sha == HEAD));
}

#[test]
fn missing_superseded_unknown_and_duplicate_inputs_fail() {
    for variant in 0..4 {
        let mut f = fixture();
        match variant {
            0 => f.evidence.pins.clear(),
            1 => {
                f.docs
                    .0
                    .lock()
                    .unwrap()
                    .insert(PIN.into(), "brief-rev-4".into());
            }
            2 => f.docs.0.lock().unwrap().clear(),
            3 => f.evidence.pins.push(PIN.into()),
            _ => unreachable!(),
        }
        f.fails("inputs-current");
    }
}

#[test]
fn evidence_cannot_be_reused_for_another_head_repo_or_policy() {
    for field in ["head", "repo", "policy", "runner"] {
        let mut f = fixture();
        match field {
            "head" => f.evidence.head = BASE.into(),
            "repo" => f.evidence.repo = "other/repo".into(),
            "policy" => f.evidence.policy_version = "old-policy".into(),
            "runner" => f.evidence.runner_config_version.clear(),
            _ => unreachable!(),
        }
        f.fails("inputs-current");
    }
}

#[test]
fn requester_alone_bots_old_approvals_and_author_approval_do_not_count() {
    for variant in 0..4 {
        let f = fixture();
        let mut r = review(1, "requester");
        match variant {
            0 => {}
            1 => {
                r.login = "kod[bot]".into();
                r.human = false;
            }
            2 => {
                r.login = "reviewer".into();
                r.commit = BASE.into();
            }
            3 => r.login = "AUTHOR".into(),
            _ => unreachable!(),
        }
        f.git.pr.lock().unwrap().reviews = vec![r];
        f.fails("requester-check");
    }
}

#[test]
fn dismissal_and_changes_requested_replace_an_approval_but_comments_do_not() {
    for state in [
        ReviewState::Dismissed,
        ReviewState::ChangesRequested,
        ReviewState::Commented,
    ] {
        let f = fixture();
        let mut r = review(2, "REVIEWER");
        r.state = state.clone();
        f.git.pr.lock().unwrap().reviews.push(r);
        let check = f
            .run()
            .into_iter()
            .find(|c| c.name == "requester-check")
            .unwrap();
        assert_eq!(
            check.conclusion,
            if state == ReviewState::Commented {
                Conclusion::Success
            } else {
                Conclusion::Failure
            }
        );
    }
}

#[test]
fn protected_paths_need_an_extra_owner_approval() {
    for path in [
        ".github/workflows/ci.yml",
        "critical/api.rs",
        "tests/checks.rs",
        "CODEOWNERS",
        "auth/login.rs",
    ] {
        let f = fixture();
        f.git.pr.lock().unwrap().paths = vec![path.into()];
        f.fails("protected-paths");
    }
}

#[test]
fn protected_paths_pass_with_two_distinct_humans_including_the_owner() {
    let f = fixture();
    {
        let mut pr = f.git.pr.lock().unwrap();
        pr.paths = vec!["tests/checks.rs".into()];
        pr.reviews.push(review(2, "kiennt23"));
    }
    assert!(f.run().iter().all(|c| c.conclusion == Conclusion::Success));
}

#[test]
fn one_owner_cannot_supply_two_approvals_under_different_casing() {
    let f = fixture();
    {
        let mut pr = f.git.pr.lock().unwrap();
        pr.paths = vec!["tests/checks.rs".into()];
        pr.reviews = vec![review(1, "kiennt23"), review(2, "KienNT23")];
    }
    f.fails("protected-paths");
}

#[test]
fn every_scan_failure_blocks_its_required_check() {
    for name in &CHECKS[3..] {
        let mut f = fixture();
        f.scans.fail = Some((*name).into());
        f.fails(name);
    }
}

#[test]
fn a_public_read_only_reviewer_cannot_satisfy_requester_separation() {
    let f = fixture();
    let mut outsider = review(2, "outsider");
    outsider.can_approve = false;
    f.git.pr.lock().unwrap().reviews = vec![review(1, "requester"), outsider];
    f.fails("requester-check");
}

#[test]
fn an_artifact_approved_during_scanning_invalidates_all_results() {
    struct Approve<'a>(&'a Docs);
    impl Scanner for Approve<'_> {
        fn version(&self) -> Result<String> {
            Ok("scan-1".into())
        }
        fn scan(&self, _: &str, _: &str) -> Result<()> {
            self.0
                .0
                .lock()
                .unwrap()
                .insert(PIN.into(), "new-approved-revision".into());
            Ok(())
        }
    }
    let f = fixture();
    let scanner = Approve(&f.docs);
    let mut gates = f.gates();
    gates.scanner = &scanner;
    assert!(gates.run(&f.evidence, HEAD).is_err());
    assert!(
        f.git
            .reports
            .lock()
            .unwrap()
            .iter()
            .all(|(_, c)| c.conclusion != Conclusion::Success)
    );
}

#[test]
fn a_head_change_during_scans_leaves_only_failed_checks() {
    let mut f = fixture();
    f.git.change_on_read = true;
    assert!(f.gates().run(&f.evidence, HEAD).is_err());
    assert!(
        f.git
            .reports
            .lock()
            .unwrap()
            .iter()
            .all(|(_, c)| c.conclusion != Conclusion::Success)
    );
}

#[test]
fn audit_outage_prevents_any_github_update() {
    let f = fixture();
    std::fs::remove_file(f.dir.path().join("audit.db")).unwrap();
    assert!(f.gates().run(&f.evidence, HEAD).is_err());
    assert!(f.git.reports.lock().unwrap().is_empty());
}

#[test]
fn merge_candidates_scan_and_report_the_candidate_but_review_the_pr_head() {
    let f = fixture();
    let checks = f.gates().run(&f.evidence, BASE).unwrap();
    assert!(checks.iter().all(|c| c.conclusion == Conclusion::Success));
    assert!(
        f.git
            .reports
            .lock()
            .unwrap()
            .iter()
            .all(|(sha, _)| sha == BASE)
    );
    assert!(
        f.scans
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, sha)| sha == BASE)
    );
}

#[test]
fn bypass_events_are_durable_and_rules_pin_every_check_to_the_app() {
    let f = fixture();
    kod_gates::record_bypass(&f.audit, "acme/app", "delivery-1", "admin", HEAD, 123).unwrap();
    let conn = rusqlite::Connection::open(f.dir.path().join("audit.db")).unwrap();
    let text: String = conn
        .query_row("SELECT payload FROM audit", [], |r| r.get(0))
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["kind"], "admin_bypass");
    assert_eq!(value["delivery"], "delivery-1");
    let rules = kod_gates::ruleset(123).unwrap();
    assert_eq!(rules["bypass_actors"], serde_json::json!([]));
    let checks = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type"] == "required_status_checks")
        .unwrap();
    assert_eq!(
        checks["parameters"]["required_status_checks"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert!(
        checks["parameters"]["required_status_checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["integration_id"] == 123)
    );
    assert!(kod_gates::ruleset(0).is_err());
}
