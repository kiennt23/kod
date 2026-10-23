use super::*;
use reqwest::Method;
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
    fn lookup_pr(&self, t: &Secret, r: &store::Record) -> Result<Option<Pr>> {
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
                found = Some(p);
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
        self.with_token(t, repo, |r| {
            ensure!(
                branch == r.permit.branch() && branch != self.config.base_branch,
                "GitHub branch outside run permit"
            );
            if let Some(number) = r.pr.as_ref().filter(|i| i.blocked).and_then(|i| i.created) {
                return Err(pr_errors::review_error(number));
            }
            Ok(self.lookup_pr(t, &r)?.map(|pr| pr.html_url))
        })
    }
    /// A created PR that failed identity validation is closed rather than left
    /// orphaned; the intent clears only once GitHub confirms the close.
    fn close_invalid(&self, t: &Secret, r: &mut store::Record) -> Result<()> {
        let Some(number) = r.pr.as_ref().filter(|i| i.blocked).and_then(|i| i.created) else {
            return Ok(());
        };
        self.ensure_live(t, r)?;
        let path = self.repo_path(&["pulls", &number.to_string()]);
        let closed = self
            .http
            .request(
                Method::PATCH,
                t.expose(),
                &path,
                &[],
                Some(&json!({"state":"closed"})),
            )
            .is_ok_and(|reply| reply.status.is_success());
        if !closed {
            return Err(pr_errors::review_error(number));
        }
        r.pr = None;
        self.publish_state(r)
    }
    fn remember_pr(&self, r: &mut store::Record, pr: Pr) -> Result<String> {
        if let Some(intent) = &mut r.pr {
            intent.url = Some(pr.html_url.clone());
            intent.created = Some(pr.number);
            intent.blocked = false;
            self.publish_state(r)?;
        }
        Ok(pr.html_url)
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
        self.with_token(t, repo, |mut r| {
            ensure!(branch == r.permit.branch() && branch != self.config.base_branch, "GitHub branch outside run permit");
            self.close_invalid(t, &mut r)?;
            if let Some(pr) = self.lookup_pr(t, &r)? {
                return self.remember_pr(&mut r, pr);
            }
            ensure!(r.pr.is_none(), "GitHub PR creation outcome unknown; reconcile before retry");
            r.pr = Some(store::Intent {
                sha: r.expected.clone().context("missing GitHub commit identity")?,
                branch: branch.into(),
                base: self.config.base_branch.clone(),
                url: None,
                created: None,
                blocked: false,
            });
            self.publish_state(&r)?;
            self.ensure_live(t, &r)?;
            let reply = self.http.request(Method::POST, t.expose(), &self.repo_path(&["pulls"]), &[],
                Some(&json!({"head":branch,"base":self.config.base_branch,"title":title,"body":body,"draft":true,"maintainer_can_modify":false})));
            let mut refusal = None;
            if let Ok(reply) = &reply {
                if reply.status.is_success() {
                    if let Some(number) = pr_errors::created_number(&reply.body) {
                        let intent = r.pr.as_mut().context("missing PR intent")?;
                        intent.created = Some(number);
                        intent.blocked = true;
                        intent.url = Some(format!("https://github.com/{}/pull/{number}", self.config.repo));
                        self.publish_state(&r)?;
                        if let Some(pr) = reply.json::<Pr>().ok().filter(|pr| self.validate_pr(&r, pr).is_ok()) {
                            return self.remember_pr(&mut r, pr);
                        }
                        self.close_invalid(t, &mut r)?;
                        anyhow::bail!("GitHub PR #{number} failed identity validation and was closed");
                    }
                } else if reply.status.is_client_error() {
                    // A received 4xx is a definite refusal; 5xx stays uncertain because
                    // GitHub can create the PR before its edge reports an error.
                    refusal = Some(pr_errors::definite_rejection(&reply.body).unwrap_or(
                        "request refused; check GitHub permissions and request parameters",
                    ));
                }
            }
            if let Some(pr) = self.lookup_pr(t, &r)? {
                return self.remember_pr(&mut r, pr);
            }
            if let Some(cause) = refusal {
                r.pr = None;
                self.publish_state(&r)?;
                anyhow::bail!("GitHub PR creation rejected: {cause}");
            }
            anyhow::bail!("GitHub PR creation outcome unknown; reconcile before retry")
        })
    }
}
