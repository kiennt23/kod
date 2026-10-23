use super::*;
use reqwest::{Method, StatusCode};
use serde_json::json;
#[derive(Deserialize)]
struct Repo {
    id: u64,
}
#[derive(Deserialize)]
struct Side {
    repo: Repo,
    #[serde(rename = "ref")]
    branch: String,
}
#[derive(Deserialize)]
struct Head {
    repo: Repo,
    #[serde(rename = "ref")]
    branch: String,
    sha: String,
}
#[derive(Deserialize)]
struct User {
    id: u64,
}
#[derive(Deserialize)]
struct Pr {
    number: u64,
    html_url: String,
    state: String,
    draft: bool,
    user: User,
    head: Head,
    base: Side,
}
impl GitHub {
    fn validate_pr(&self, r: &store::Record, p: &Pr) -> Result<()> {
        ensure!(
            p.number > 0
                && p.state == "open"
                && p.draft
                && p.user.id == self.config.bot_user_id
                && p.head.repo.id == self.config.repository_id
                && p.base.repo.id == self.config.repository_id
                && p.head.branch == r.permit.branch()
                && p.base.branch == self.config.base_branch
                && r.expected.as_deref() == Some(&p.head.sha),
            "GitHub PR identity mismatch"
        );
        ensure!(
            p.html_url == format!("https://github.com/{}/pull/{}", self.config.repo, p.number),
            "invalid GitHub PR URL"
        );
        Ok(())
    }
    fn lookup_pr(&self, t: &Secret, r: &store::Record) -> Result<Option<String>> {
        let path = self.repo_path(&["pulls"]);
        let mut found = None;
        let owner = self
            .config
            .repo
            .split('/')
            .next()
            .context("missing repository owner")?;
        for page in 1..=100 {
            let reply = self.http.request(
                Method::GET,
                t.expose(),
                &path,
                &[
                    ("state", "open".into()),
                    ("head", format!("{owner}:{}", r.permit.branch())),
                    ("base", self.config.base_branch.clone()),
                    ("per_page", "100".into()),
                    ("page", page.to_string()),
                ],
                None,
            )?;
            let prs: Vec<Pr> = reply.json()?;
            for p in prs {
                self.validate_pr(r, &p)?;
                ensure!(found.is_none(), "ambiguous GitHub PRs");
                found = Some(p.html_url);
            }
            if !self.http.next_page(reply.link.as_deref(), &path, page)? {
                return Ok(found);
            }
        }
        anyhow::bail!("GitHub pagination limit")
    }
    pub(super) fn existing_pr(
        &self,
        t: &Secret,
        repo: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        let mut s = self.lock()?;
        let r = self.branch_record(&mut s, t, repo, branch)?;
        self.lookup_pr(t, r)
    }
    pub(super) fn create_pr(
        &self,
        t: &Secret,
        repo: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        ensure!(
            identity(title) && !body.contains('\0') && title.len() <= 256 && body.len() <= 65536,
            "invalid GitHub PR text"
        );
        let mut s = self.lock()?;
        let r = self.branch_record(&mut s, t, repo, branch)?;
        if let Some(url) = self.lookup_pr(t, r)? {
            return Ok(url);
        }
        ensure!(
            r.pr.is_none(),
            "GitHub PR creation outcome unknown; reconcile before retry"
        );
        let expected = r
            .expected
            .clone()
            .context("missing GitHub commit identity")?;
        r.pr = Some(store::Intent {
            sha: expected,
            branch: branch.into(),
            base: self.config.base_branch.clone(),
            url: None,
        });
        self.store.save(r)?;
        let reply=self.http.request(Method::POST,t.expose(),&self.repo_path(&["pulls"]),&[],Some(&json!({"head":branch,"base":self.config.base_branch,"title":title,"body":body,"draft":true,"maintainer_can_modify":false})));
        if let Ok(reply) = &reply {
            if reply.status.is_success() {
                let pr: Pr = reply.json()?;
                self.validate_pr(r, &pr)?;
                r.pr.as_mut().context("missing PR intent")?.url = Some(pr.html_url.clone());
                self.store.save(r)?;
                return Ok(pr.html_url);
            }
            if reply.status.is_client_error() && reply.status != StatusCode::UNPROCESSABLE_ENTITY {
                r.pr = None;
                self.store.save(r)?;
                anyhow::bail!("GitHub PR creation refused");
            }
        }
        if let Some(url) = self.lookup_pr(t, r)? {
            r.pr.as_mut().context("missing PR intent")?.url = Some(url.clone());
            self.store.save(r)?;
            return Ok(url);
        }
        anyhow::bail!("GitHub PR creation outcome unknown; reconcile before retry")
    }
}
