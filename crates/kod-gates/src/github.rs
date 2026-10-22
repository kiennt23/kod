use crate::{Check, Conclusion, Forge, PullRequest, Review, ReviewState, validate_sha};
use anyhow::{Context, Result, bail};
use reqwest::{
    Method,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::Mutex,
    time::Duration,
};

pub struct GitHub {
    client: Client,
    repo: String,
    api: String,
    checks: Mutex<BTreeMap<(String, String), u64>>,
}

#[derive(Deserialize)]
struct User {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Commit {
    sha: String,
    repo: Option<Repo>,
}
#[derive(Deserialize)]
struct Repo {
    full_name: String,
}
#[derive(Deserialize)]
struct Pull {
    number: u64,
    head: Commit,
    base: Commit,
    user: User,
    changed_files: usize,
    state: String,
}
#[derive(Deserialize)]
struct File {
    filename: String,
    previous_filename: Option<String>,
}
#[derive(Deserialize)]
struct RawReview {
    id: u64,
    user: User,
    commit_id: String,
    state: ReviewState,
}

impl GitHub {
    pub fn new(repo: &str, token: &str) -> Result<Self> {
        Self::with_api(repo, token, "https://api.github.com")
    }

    fn with_api(repo: &str, token: &str, api: &str) -> Result<Self> {
        crate::Config {
            repo: repo.into(),
            protect_all: false,
            protected: vec![],
            codeowners: vec!["owner".into()],
        }
        .validate()?;
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid GitHub credential")?;
        auth.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("kod-gates")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            repo: repo.into(),
            api: api.into(),
            checks: Mutex::default(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/repos/{}/{}", self.api, self.repo, path)
    }

    fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut request = self.client.request(method, self.url(path));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|e| anyhow::anyhow!("GitHub request: {}", e.without_url()))?;
        if !response.status().is_success() {
            bail!("GitHub returned HTTP {}", response.status());
        }
        let mut bytes = Vec::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 * 1024 {
            bail!("GitHub response exceeds limit");
        }
        serde_json::from_slice(&bytes).context("invalid GitHub response")
    }

    fn pages<T: for<'de> Deserialize<'de>>(&self, path: &str, max: usize) -> Result<Vec<T>> {
        let mut all = Vec::new();
        for page in 1..=max / 100 + 1 {
            let value = self.request(
                Method::GET,
                &format!("{path}?per_page=100&page={page}"),
                None,
            )?;
            let batch: Vec<T> = serde_json::from_value(value)?;
            let n = batch.len();
            all.extend(batch);
            if all.len() >= max {
                bail!("GitHub list may be truncated");
            }
            if n < 100 {
                return Ok(all);
            }
        }
        bail!("GitHub pagination limit reached")
    }

    pub fn archive(&self, sha: &str, out: &mut dyn Write) -> Result<()> {
        validate_sha(sha)?;
        let response = self
            .client
            .get(self.url(&format!("tarball/{sha}")))
            .send()
            .map_err(|e| anyhow::anyhow!("GitHub archive: {}", e.without_url()))?;
        if response.status() != reqwest::StatusCode::FOUND {
            bail!("GitHub archive did not redirect to codeload");
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .context("missing archive location")?
            .to_str()?;
        let url = reqwest::Url::parse(location)?;
        if url.scheme() != "https"
            || url.host_str() != Some("codeload.github.com")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("unexpected archive host");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(url)
            .send()
            .map_err(|e| anyhow::anyhow!("archive download: {}", e.without_url()))?;
        if !response.status().is_success() {
            bail!("archive download failed: HTTP {}", response.status());
        }
        let n = std::io::copy(&mut response.take(512 * 1024 * 1024 + 1), out)?;
        if n > 512 * 1024 * 1024 {
            bail!("archive exceeds 512 MiB");
        }
        Ok(())
    }
}

impl Forge for GitHub {
    fn pull(&self, number: u64) -> Result<PullRequest> {
        let path = format!("pulls/{number}");
        let raw: Pull = serde_json::from_value(self.request(Method::GET, &path, None)?)?;
        if raw.number != number
            || raw.state != "open"
            || raw.base.repo.as_ref().map(|r| r.full_name.as_str()) != Some(&self.repo)
        {
            bail!("PR is not open in the configured repository");
        }
        let files: Vec<File> = self.pages(&format!("{path}/files"), 3000)?;
        if files.len() != raw.changed_files {
            bail!("incomplete changed paths");
        }
        let mut paths = Vec::new();
        for file in files {
            paths.push(file.filename);
            if let Some(old) = file.previous_filename {
                paths.push(old);
            }
        }
        paths.sort();
        paths.dedup();
        let reviews: Vec<RawReview> = self.pages(&format!("{path}/reviews"), 10000)?;
        let mut permissions = BTreeMap::new();
        let reviews = reviews
            .into_iter()
            .map(|r| -> Result<Review> {
                let human = r.user.kind == "User";
                let can_approve = if human {
                    if !crate::login(&r.user.login) {
                        bail!("invalid GitHub review login");
                    }
                    let key = r.user.login.to_ascii_lowercase();
                    if let Some(allowed) = permissions.get(&key) {
                        *allowed
                    } else {
                        let value = self.request(
                            Method::GET,
                            &format!("collaborators/{key}/permission"),
                            None,
                        )?;
                        let permission = value["permission"]
                            .as_str()
                            .context("missing reviewer permission")?;
                        let allowed = matches!(permission, "write" | "admin" | "maintain");
                        permissions.insert(key, allowed);
                        allowed
                    }
                } else {
                    false
                };
                Ok(Review {
                    id: r.id,
                    login: r.user.login,
                    human,
                    can_approve,
                    commit: r.commit_id,
                    state: r.state,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PullRequest {
            number,
            head: raw.head.sha,
            base: raw.base.sha,
            author: raw.user.login,
            paths,
            reviews,
        })
    }

    fn report(&self, sha: &str, check: &Check) -> Result<()> {
        validate_sha(sha)?;
        if !crate::CHECKS.contains(&check.name.as_str()) {
            bail!("unknown check");
        }
        let key = (sha.to_owned(), check.name.clone());
        let mut ids = self
            .checks
            .lock()
            .map_err(|_| anyhow::anyhow!("check state lock poisoned"))?;
        let body = match check.conclusion {
            Conclusion::Pending => {
                json!({"name": check.name, "head_sha": sha, "status": "in_progress",
                "output": {"title": check.name, "summary": check.summary}})
            }
            Conclusion::Success | Conclusion::Failure => {
                json!({"name": check.name, "status": "completed",
                "conclusion": if check.conclusion == Conclusion::Success {"success"} else {"failure"},
                "output": {"title": check.name, "summary": check.summary}})
            }
        };
        match check.conclusion {
            Conclusion::Pending => {
                let value = self.request(Method::POST, "check-runs", Some(&body))?;
                let id = value["id"]
                    .as_u64()
                    .context("GitHub returned no check ID")?;
                ids.insert(key, id);
            }
            Conclusion::Success | Conclusion::Failure => {
                let id = ids
                    .get(&key)
                    .context("check must be started before completion")?;
                self.request(Method::PATCH, &format!("check-runs/{id}"), Some(&body))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
