use super::*;
use crate::forge::Commit;
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Method;
use serde_json::{Value, json};
#[derive(Deserialize)]
struct Object {
    sha: String,
}
#[derive(Deserialize)]
struct Parent {
    sha: String,
    tree: Object,
}
#[derive(Deserialize)]
struct Tree {
    sha: String,
    truncated: bool,
    tree: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
#[derive(Deserialize)]
struct Blob {
    sha: String,
    encoding: String,
    content: String,
    size: usize,
}
fn path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && !p.chars().any(char::is_control)
        && p.split('/')
            .all(|s| !s.is_empty() && !matches!(s, "." | "..") && !s.eq_ignore_ascii_case(".git"))
}
pub(super) fn local(c: &Commit) -> Result<Value> {
    ensure!(
        sha(&c.base)
            && identity(&c.committer.name)
            && identity(&c.committer.email)
            && !c.committer.name.contains(['<', '>'])
            && !c.committer.email.contains(['<', '>'])
            && !c.message.is_empty()
            && !c.message.contains('\0'),
        "invalid Git commit identity"
    );
    let date = chrono::DateTime::from_timestamp(c.date, 0)
        .filter(|_| c.date > 0 && c.date <= 253_402_300_799)
        .context("invalid Git commit date")?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    ensure!(
        !c.changes.is_empty() && c.changes.len() <= crate::MAX_FILES,
        "invalid publish file count"
    );
    let mut files = BTreeMap::new();
    let mut bytes = 0usize;
    for change in &c.changes {
        ensure!(path(&change.path), "unsafe Git path");
        if let Some(text) = &change.content {
            ensure!(!text.contains('\0'), "binary change refused");
            bytes = bytes
                .checked_add(text.len())
                .context("publish size overflow")?;
        }
        ensure!(
            files
                .insert(change.path.clone(), change.content.clone())
                .is_none(),
            "duplicate Git path"
        );
    }
    ensure!(bytes <= crate::MAX_PUBLISH_BYTES, "publish too large");
    for p in files.keys() {
        for (i, _) in p.match_indices('/') {
            ensure!(
                !files.contains_key(&p[..i]),
                "file/directory change collision"
            );
        }
    }
    Ok(
        json!({"base":c.base,"message":c.message,"person":{"name":c.committer.name,"email":c.committer.email,"date":date},"changes":files}),
    )
}
impl GitHub {
    pub(super) fn preflight(&self, t: &Secret, c: &Commit) -> Result<(String, Vec<Value>)> {
        let parent: Parent = self
            .http
            .request(
                Method::GET,
                t.expose(),
                &self.repo_path(&["git", "commits", &c.base]),
                &[],
                None,
            )?
            .json()?;
        ensure!(
            parent.sha == c.base && sha(&parent.tree.sha),
            "invalid GitHub base commit"
        );
        let tree: Tree = self
            .http
            .request(
                Method::GET,
                t.expose(),
                &self.repo_path(&["git", "trees", &parent.tree.sha]),
                &[("recursive", "1".into())],
                None,
            )?
            .json()?;
        ensure!(
            !tree.truncated && tree.sha == parent.tree.sha,
            "incomplete GitHub tree"
        );
        let mut entries = BTreeMap::new();
        for entry in tree.tree {
            ensure!(
                path(&entry.path) && sha(&entry.sha),
                "malformed GitHub tree"
            );
            ensure!(
                entries.insert(entry.path.clone(), entry).is_none(),
                "duplicate GitHub tree entry"
            );
        }
        let mut output = Vec::new();
        let mut inspected = 0usize;
        let mut changes: Vec<_> = c.changes.iter().collect();
        changes.sort_by_key(|c| &c.path);
        for change in changes {
            for (i, _) in change.path.match_indices('/') {
                if let Some(parent) = entries.get(&change.path[..i]) {
                    ensure!(
                        parent.kind == "tree" && parent.mode == "040000",
                        "unsafe GitHub parent directory"
                    );
                }
            }
            let mode = match entries.get(&change.path) {
                Some(old) => {
                    ensure!(
                        old.kind == "blob" && matches!(old.mode.as_str(), "100644" | "100755"),
                        "unsupported GitHub entry"
                    );
                    let blob: Blob = self
                        .http
                        .request(
                            Method::GET,
                            t.expose(),
                            &self.repo_path(&["git", "blobs", &old.sha]),
                            &[],
                            None,
                        )?
                        .json()?;
                    ensure!(
                        blob.sha == old.sha && blob.encoding == "base64",
                        "invalid GitHub blob"
                    );
                    let bytes = STANDARD
                        .decode(
                            blob.content
                                .bytes()
                                .filter(|b| !b.is_ascii_whitespace())
                                .collect::<Vec<_>>(),
                        )
                        .map_err(|_| anyhow::anyhow!("invalid GitHub blob encoding"))?;
                    inspected = inspected
                        .checked_add(bytes.len())
                        .context("blob size overflow")?;
                    ensure!(
                        blob.size == bytes.len()
                            && inspected <= crate::MAX_PUBLISH_BYTES
                            && !bytes.contains(&0)
                            && std::str::from_utf8(&bytes).is_ok(),
                        "binary or oversized GitHub blob refused"
                    );
                    old.mode.as_str()
                }
                None => {
                    ensure!(change.content.is_some(), "cannot delete absent file");
                    "100644"
                }
            };
            output.push(match &change.content {
                Some(text) => json!({"path":change.path,"mode":mode,"type":"blob","content":text}),
                None => json!({"path":change.path,"mode":mode,"type":"blob","sha":null}),
            });
        }
        Ok((parent.tree.sha, output))
    }
}
