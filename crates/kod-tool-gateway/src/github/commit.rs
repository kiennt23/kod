use super::*;
use crate::forge::Commit;
use reqwest::Method;
use serde_json::json;
use sha2::{Digest, Sha256};
#[derive(Deserialize)]
struct Object {
    sha: String,
}
impl GitHub {
    pub(super) fn create_commit(&self, t: &Secret, c: &Commit) -> Result<String> {
        self.with_token(t,&c.repo,|mut record| {
        ensure!(c.base == record.permit.base, "GitHub commit base mismatch");
        let input = super::preflight::local(c)?;
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&input)?));
        ensure!(
            record.input.as_ref().is_none_or(|old| old == &digest),
            "run commit input changed"
        );
        let (tree, entries) = self.preflight(t, c)?;
        record.input = Some(digest);
        self.publish_state(&record)?;
        self.ensure_live(t,&record)?;
        let tree: Object = self
            .http
            .request(
                Method::POST,
                t.expose(),
                &self.repo_path(&["git", "trees"]),
                &[],
                Some(&json!({"base_tree":tree,"tree":entries})),
            )?
            .json()?;
        ensure!(sha(&tree.sha), "invalid GitHub tree SHA");
        let person = input["person"].clone();
        self.ensure_live(t,&record)?;
        let commit:Object=self.http.request(Method::POST,t.expose(),&self.repo_path(&["git","commits"]),&[],Some(&json!({"tree":tree.sha,"parents":[c.base],"message":c.message,"author":person,"committer":person})))?.json()?;
        ensure!(
            sha(&commit.sha)
                && record
                    .expected
                    .as_ref()
                    .is_none_or(|old| old == &commit.sha),
            "GitHub commit identity changed"
        );
        record.expected = Some(commit.sha.clone());
        self.publish_state(&record)?;
        Ok(commit.sha)
        })
    }
}
