use super::*;
use reqwest::{Method, StatusCode};
use serde_json::json;
#[derive(Deserialize)]
struct Ref {
    #[serde(rename = "ref")]
    name: String,
    object: Target,
}
#[derive(Deserialize)]
struct Target {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
impl GitHub {
    pub(super) fn branch_record<'a>(
        &self,
        s: &'a mut State,
        t: &Secret,
        repo: &str,
        branch: &str,
    ) -> Result<&'a mut store::Record> {
        let r = self.authorized(s, t, repo)?;
        ensure!(
            branch == r.permit.branch() && branch != self.config.base_branch,
            "GitHub branch scope mismatch"
        );
        Ok(r)
    }
    fn remote_ref(&self, t: &Secret, branch: &str) -> Result<Option<String>> {
        let reply = self.http.request(
            Method::GET,
            t.expose(),
            &self.repo_path(&["git", "ref", "heads", branch]),
            &[],
            None,
        )?;
        if reply.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let r: Ref = reply.json()?;
        ensure!(
            r.name == format!("refs/heads/{branch}")
                && r.object.kind == "commit"
                && sha(&r.object.sha),
            "invalid GitHub ref"
        );
        Ok(Some(r.object.sha))
    }
    pub(super) fn point_branch(
        &self,
        t: &Secret,
        repo: &str,
        branch: &str,
        sha: &str,
    ) -> Result<()> {
        let mut s = self.lock()?;
        let r = self.branch_record(&mut s, t, repo, branch)?;
        ensure!(
            r.expected.as_deref() == Some(sha),
            "GitHub branch SHA mismatch"
        );
        if let Some(old) = self.remote_ref(t, branch)? {
            ensure!(old == sha, "divergent GitHub branch");
            return Ok(());
        }
        let result = self.http.request(
            Method::POST,
            t.expose(),
            &self.repo_path(&["git", "refs"]),
            &[],
            Some(&json!({"ref":format!("refs/heads/{branch}"),"sha":sha})),
        );
        if let Ok(reply) = &result {
            ensure!(
                reply.status.is_success()
                    || reply.status == StatusCode::UNPROCESSABLE_ENTITY
                    || reply.status.is_server_error(),
                "GitHub ref creation refused"
            );
        }
        ensure!(
            self.remote_ref(t, branch)?.as_deref() == Some(sha),
            "GitHub ref creation unconfirmed"
        );
        Ok(())
    }
    pub(super) fn remove_branch(&self, t: &Secret, repo: &str, branch: &str) -> Result<()> {
        let mut s = self.lock()?;
        let r = self.branch_record(&mut s, t, repo, branch)?;
        let Some(old) = self.remote_ref(t, branch)? else {
            return Ok(());
        };
        ensure!(
            r.expected.as_deref() == Some(&old),
            "divergent GitHub branch"
        );
        let result = self.http.request(
            Method::DELETE,
            t.expose(),
            &self.repo_path(&["git", "refs", "heads", branch]),
            &[],
            None,
        );
        if let Ok(reply) = &result {
            ensure!(
                reply.status == StatusCode::NO_CONTENT
                    || reply.status == StatusCode::NOT_FOUND
                    || reply.status.is_server_error(),
                "GitHub branch deletion refused"
            );
        }
        ensure!(
            self.remote_ref(t, branch)?.is_none(),
            "GitHub branch deletion unconfirmed"
        );
        Ok(())
    }
}
