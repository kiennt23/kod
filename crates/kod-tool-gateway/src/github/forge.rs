use super::*;
use crate::forge::{Commit, Forge};
impl Forge for GitHub {
    fn commit(&self, t: &Secret, c: &Commit) -> Result<String> {
        self.create_commit(t, c)
    }
    fn set_branch(&self, t: &Secret, r: &str, b: &str, s: &str) -> Result<()> {
        self.point_branch(t, r, b, s)
    }
    fn delete_branch(&self, t: &Secret, r: &str, b: &str) -> Result<()> {
        self.remove_branch(t, r, b)
    }
    fn find_pr(&self, t: &Secret, r: &str, b: &str) -> Result<Option<String>> {
        self.existing_pr(t, r, b)
    }
    fn open_draft_pr(
        &self,
        t: &Secret,
        r: &str,
        b: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        self.create_pr(t, r, b, title, body)
    }
}
