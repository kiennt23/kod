//! The git host behind the publish operation, the host-side collector that
//! supplies the change, and fakes of both for tests.
use crate::broker::{Broker, Secret};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

/// One file in a publish: its new text, or None to delete it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    pub content: Option<String>,
}

/// What the host collected from a finished run, outside the sandbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Collected {
    /// The workspace diff against the run's base.
    pub changes: Vec<Change>,
    /// The host's own check results, for the PR body.
    pub test_evidence: String,
}

/// A finished run's collected output. In 6b the runner's collector writes
/// it next to the sandbox, never inside it (kod-eval's
/// `collect_out/diff.patch` and check results); the real `Workspace` reads
/// those files. Nothing the sandbox sends ever becomes a change.
pub trait Workspace: Send + Sync {
    fn collected(&self, run: &str) -> Result<Collected>;
}

/// The agent's committer identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// A commit to create on top of `base`.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub repo: String,
    pub base: String,
    pub changes: Vec<Change>,
    pub message: String,
    pub committer: Identity,
    /// Unix seconds, used as both the author and the committer date, so
    /// the same commit always has the same SHA.
    pub date: i64,
}

/// The git host calls the publish operation makes. Every call carries the
/// credential the broker minted for this publish. `check` is the contract
/// every adapter must pass.
pub trait Forge: Send + Sync {
    /// Creates the commit and returns its SHA. The same commit twice gives
    /// the same SHA.
    fn commit(&self, token: &Secret, commit: &Commit) -> Result<String>;
    /// Points the branch at `sha`, creating it if needed.
    fn set_branch(&self, token: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()>;
    /// Deletes the branch. Deleting a missing branch is not an error.
    fn delete_branch(&self, token: &Secret, repo: &str, branch: &str) -> Result<()>;
    /// The URL of the open PR from `branch`, if there is one.
    fn find_pr(&self, token: &Secret, repo: &str, branch: &str) -> Result<Option<String>>;
    /// Opens a draft PR from `branch` and returns its URL.
    fn open_draft_pr(
        &self,
        token: &Secret,
        repo: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String>;
}

/// The forge and broker contract, against a repo whose default branch is at
/// `base`. It replays a publish that died after the push and again after
/// the PR was opened but before its URL came back: the retry must make the
/// same commit and find the PR, not open a second one. It ends by checking
/// that a revoked credential is refused.
pub fn check(forge: &dyn Forge, broker: &dyn Broker, repo: &str, base: &str) -> Result<()> {
    let (run, branch) = ("contract-run", "agent/CONTRACT-1-contract-run");
    let commit = Commit {
        repo: repo.into(),
        base: base.into(),
        changes: vec![Change {
            path: "contract.txt".into(),
            content: Some("contract\n".into()),
        }],
        message: "contract\n".into(),
        committer: Identity {
            name: "kod-agent".into(),
            email: "agent@kod.invalid".into(),
        },
        date: 1_700_000_000,
    };
    let token = broker.mint(run, repo).context("mint")?;
    let first = forge.commit(&token, &commit).context("commit")?;
    forge
        .set_branch(&token, repo, branch, &first)
        .context("set_branch")?;
    ensure!(
        forge.find_pr(&token, repo, branch)?.is_none(),
        "find_pr found a PR nobody opened"
    );
    // The process died here; the retry starts over with a fresh credential.
    let token = broker.mint(run, repo).context("mint on retry")?;
    let again = forge.commit(&token, &commit).context("commit on retry")?;
    ensure!(
        again == first,
        "the same commit got a new SHA ({first} then {again}), so a retry would move the branch"
    );
    forge
        .set_branch(&token, repo, branch, &again)
        .context("set_branch on retry")?;
    let url = forge.open_draft_pr(&token, repo, branch, "contract", "contract")?;
    // The PR's URL was lost on the way back; the retry must find it.
    ensure!(
        forge.find_pr(&token, repo, branch)? == Some(url),
        "find_pr does not find the PR just opened"
    );
    forge
        .delete_branch(&token, repo, branch)
        .context("delete_branch")?;
    forge
        .delete_branch(&token, repo, branch)
        .context("delete_branch twice")?;
    broker.revoke(run).context("revoke")?;
    ensure!(
        forge.set_branch(&token, repo, branch, &first).is_err(),
        "a revoked credential still works"
    );
    Ok(())
}

/// A PR as the fake stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct FakePr {
    pub repo: String,
    pub branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub url: String,
}

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Inner {
    /// Live token to the run and repo it was minted for.
    tokens: HashMap<String, (String, String)>,
    minted: u64,
    /// Repo to the commits that exist in it.
    repos: HashMap<String, Vec<String>>,
    commits: HashMap<String, Commit>,
    branches: BTreeMap<(String, String), String>,
    prs: Vec<FakePr>,
    lose_next_pr: bool,
    fail_next_pr: Option<String>,
    fail_mint: Option<String>,
    on_commit: Option<Hook>,
    on_set_branch: Option<Hook>,
}

/// An in-memory GitHub: it mints run- and repo-scoped tokens and refuses
/// any call without a live token for that repo.
#[derive(Default)]
pub struct FakeGitHub(Mutex<Inner>);

impl FakeGitHub {
    /// Adds a repo whose default branch is at `base`.
    pub fn add_repo(&self, repo: &str, base: &str) {
        self.lock()
            .repos
            .insert(repo.to_owned(), vec![base.to_owned()]);
    }

    /// Every token minted so far, live or revoked.
    pub fn minted(&self) -> Vec<String> {
        (1..=self.lock().minted).map(token).collect()
    }

    /// The tokens not yet revoked.
    pub fn live_tokens(&self) -> Vec<String> {
        let mut live: Vec<String> = self.lock().tokens.keys().cloned().collect();
        live.sort();
        live
    }

    pub fn branch(&self, repo: &str, branch: &str) -> Option<String> {
        let key = (repo.to_owned(), branch.to_owned());
        self.lock().branches.get(&key).cloned()
    }

    pub fn commit_of(&self, sha: &str) -> Option<Commit> {
        self.lock().commits.get(sha).cloned()
    }

    pub fn prs(&self) -> Vec<FakePr> {
        self.lock().prs.clone()
    }

    /// The next `open_draft_pr` opens the PR and then fails, as if the
    /// process died before the reply came back.
    pub fn lose_next_pr(&self) {
        self.lock().lose_next_pr = true;
    }

    /// The next `open_draft_pr` fails with this message and opens nothing.
    pub fn fail_next_pr(&self, message: &str) {
        self.lock().fail_next_pr = Some(message.to_owned());
    }

    /// The next `mint` fails with this message.
    pub fn fail_mint(&self, message: &str) {
        self.lock().fail_mint = Some(message.to_owned());
    }

    /// Runs `f` once, at the start of the next `commit`.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.lock().on_commit = Some(Box::new(f));
    }

    /// Runs `f` once, right after the next `set_branch` moves the branch.
    pub fn on_set_branch(&self, f: impl FnOnce() + Send + 'static) {
        self.lock().on_set_branch = Some(Box::new(f));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Locks after checking the token is live and minted for `repo`.
    fn authorized(&self, t: &Secret, repo: &str) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let inner = self.lock();
        if inner.tokens.get(t.expose()).map(|(_, r)| r.as_str()) != Some(repo) {
            bail!("fake github: bad credential for {repo}");
        }
        Ok(inner)
    }
}

fn token(n: u64) -> String {
    format!("ghs_fake{n:032}")
}

impl Broker for FakeGitHub {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret> {
        let mut inner = self.lock();
        if let Some(m) = inner.fail_mint.take() {
            bail!("fake github: {m}");
        }
        if !inner.repos.contains_key(repo) {
            bail!("fake github: no repo {repo}");
        }
        inner.minted += 1;
        let t = token(inner.minted);
        let scope = (run.to_owned(), repo.to_owned());
        inner.tokens.insert(t.clone(), scope);
        Ok(Secret::new(t))
    }

    fn revoke(&self, run: &str) -> Result<()> {
        self.lock().tokens.retain(|_, (r, _)| r != run);
        Ok(())
    }

    fn revoke_all(&self) -> Result<()> {
        self.lock().tokens.clear();
        Ok(())
    }
}

impl Forge for FakeGitHub {
    fn commit(&self, t: &Secret, c: &Commit) -> Result<String> {
        let hook = self.lock().on_commit.take();
        if let Some(f) = hook {
            f();
        }
        let mut inner = self.authorized(t, &c.repo)?;
        if !inner.repos[&c.repo].contains(&c.base) {
            bail!("fake github: no commit {} in {}", c.base, c.repo);
        }
        // Like git: the SHA covers the content, the identity and the date.
        let digest = Sha256::digest(format!("{c:?}"));
        let sha: String = digest[..20].iter().map(|b| format!("{b:02x}")).collect();
        inner.repos.get_mut(&c.repo).unwrap().push(sha.clone());
        inner.commits.insert(sha.clone(), c.clone());
        Ok(sha)
    }

    fn set_branch(&self, t: &Secret, repo: &str, branch: &str, sha: &str) -> Result<()> {
        let mut inner = self.authorized(t, repo)?;
        if !inner.repos[repo].iter().any(|s| s == sha) {
            bail!("fake github: no commit {sha} in {repo}");
        }
        let key = (repo.to_owned(), branch.to_owned());
        inner.branches.insert(key, sha.to_owned());
        let hook = inner.on_set_branch.take();
        drop(inner);
        if let Some(f) = hook {
            f();
        }
        Ok(())
    }

    fn delete_branch(&self, t: &Secret, repo: &str, branch: &str) -> Result<()> {
        let mut inner = self.authorized(t, repo)?;
        inner.branches.remove(&(repo.to_owned(), branch.to_owned()));
        Ok(())
    }

    fn find_pr(&self, t: &Secret, repo: &str, branch: &str) -> Result<Option<String>> {
        let inner = self.authorized(t, repo)?;
        let pr = inner
            .prs
            .iter()
            .find(|p| p.repo == repo && p.branch == branch);
        Ok(pr.map(|p| p.url.clone()))
    }

    fn open_draft_pr(
        &self,
        t: &Secret,
        repo: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let mut inner = self.authorized(t, repo)?;
        if let Some(m) = inner.fail_next_pr.take() {
            bail!("fake github: {m}");
        }
        let key = (repo.to_owned(), branch.to_owned());
        if !inner.branches.contains_key(&key) {
            bail!("fake github: no branch {branch} in {repo}");
        }
        let url = format!("https://github.example/{repo}/pull/{}", inner.prs.len() + 1);
        inner.prs.push(FakePr {
            repo: repo.to_owned(),
            branch: branch.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            draft: true,
            url: url.clone(),
        });
        if std::mem::take(&mut inner.lose_next_pr) {
            bail!("fake github: connection reset");
        }
        Ok(url)
    }
}

/// The collector's output for tests: set a run's by hand.
#[derive(Default)]
pub struct FakeWorkspace(Mutex<HashMap<String, Collected>>);

impl FakeWorkspace {
    pub fn set(&self, run: &str, collected: Collected) {
        let mut runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        runs.insert(run.to_owned(), collected);
    }
}

impl Workspace for FakeWorkspace {
    fn collected(&self, run: &str) -> Result<Collected> {
        let runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        runs.get(run)
            .cloned()
            .with_context(|| format!("no collected change for {run}"))
    }
}
