use crate::{
    Delivery, Issue, Rework, ReworkKind, State, TaskEvent, Tracker, Webhook, hmac_sha256,
    mapping::Mapping,
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    Method, StatusCode,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Read, sync::Mutex, time::Duration};
use subtle::ConstantTimeEq;

const META_START: &str = "<!-- kod:issue\n";
const META_END: &str = "\n-->";
const STATUS: &str = "<!-- kod:status -->";
const RESPONSE_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: String,
    pub comment_author_id: u64,
}

pub struct GitHub {
    config: Config,
    mapping: Mapping,
    client: Client,
    api: String,
    secret: String,
    writes: Mutex<BTreeSet<u64>>,
}

#[derive(Deserialize)]
struct User {
    id: u64,
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct RawIssue {
    id: u64,
    number: u64,
    repository_url: String,
    title: String,
    body: Option<String>,
    state: String,
    user: User,
    labels: Vec<Label>,
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct Comment {
    id: u64,
    issue_url: String,
    body: String,
    user: User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReworkMetadata {
    pub story: String,
    pub kind: ReworkKind,
    pub fields: Rework,
}

fn name(s: &str) -> bool {
    !s.is_empty()
        && !matches!(s, "." | "..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// Whether a failed GitHub write may have changed the remote. Carried as an error context; an
/// untagged failure is `Uncertain`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    NotApplied,
    Uncertain,
}

impl std::fmt::Display for WriteOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotApplied => "GitHub write was not applied",
            Self::Uncertain => "GitHub write outcome unknown; reconcile before retry",
        })
    }
}

impl std::error::Error for WriteOutcome {}

pub fn write_outcome(error: &anyhow::Error) -> WriteOutcome {
    error
        .downcast_ref::<WriteOutcome>()
        .copied()
        .unwrap_or(WriteOutcome::Uncertain)
}

impl GitHub {
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_api(
        config: Config,
        mapping: Mapping,
        token: &str,
        secret: &str,
        api: &str,
    ) -> Result<Self> {
        let u = reqwest::Url::parse(api)?;
        let ip = u
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok());
        ensure!(
            u.scheme() == "http"
                && ip.is_some_and(|ip| ip.is_loopback())
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.path() == "/"
                && u.query().is_none()
                && u.fragment().is_none(),
            "test API must be literal loopback"
        );
        Self::with_api(config, mapping, token, secret, api.trim_end_matches('/'))
    }

    pub fn new(config: Config, mapping: Mapping, token: &str, secret: &str) -> Result<Self> {
        Self::with_api(config, mapping, token, secret, "https://api.github.com")
    }

    fn with_api(
        config: Config,
        mapping: Mapping,
        token: &str,
        secret: &str,
        api: &str,
    ) -> Result<Self> {
        let parts: Vec<_> = config.repo.split('/').collect();
        ensure!(
            parts.len() == 2 && parts.iter().all(|s| name(s)),
            "invalid GitHub repository"
        );
        ensure!(
            config.comment_author_id > 0 && !token.is_empty() && !secret.is_empty(),
            "missing GitHub identity or credentials"
        );
        ensure!(
            mapping.tracker == "github" && mapping.team_group == "repository-writers",
            "unsupported GitHub tracker or team group"
        );
        ensure!(
            mapping.target(State::Ready) == "open" && mapping.target(State::Done) == "closed",
            "GitHub ready/done must target open/closed"
        );
        ensure!(
            mapping.standard("open") == Some(State::Ready)
                && mapping.standard("closed") == Some(State::Done),
            "invalid native GitHub states"
        );
        let reserved = [
            "acceptance_criteria",
            "approved_versions",
            "links",
            "rework",
        ];
        ensure!(
            !reserved.contains(&mapping.requirement_id_field.as_str())
                && !reserved.contains(&mapping.risk_class_field.as_str()),
            "mapped fields collide with metadata keys"
        );
        ensure!(
            mapping.standard(&mapping.trigger_label).is_none(),
            "trigger label is a workflow state"
        );
        ensure!(
            !mapping.trigger_label.starts_with("kod:")
                && mapping.workflows().all(|w| matches!(w, "open" | "closed")
                    || (w.starts_with("kod:") && w.len() > 4 && !w.chars().any(char::is_control))),
            "GitHub workflow labels must use the reserved kod: namespace"
        );
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
            HeaderValue::from_static("2026-03-10"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("kod-tracker")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            config,
            mapping,
            client,
            api: api.trim_end_matches('/').into(),
            secret: secret.into(),
            writes: Mutex::default(),
        })
    }

    fn key(&self, number: u64) -> String {
        format!("{}#{number}", self.config.repo)
    }

    fn number(&self, key: &str) -> Result<u64> {
        let (repo, digits) = key.split_once('#').context("invalid issue key")?;
        ensure!(
            repo.eq_ignore_ascii_case(&self.config.repo),
            "issue key is not in this repository"
        );
        let number = digits
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .context("issue key is not in this repository")?;
        ensure!(
            number.to_string() == digits,
            "non-canonical GitHub issue key"
        );
        Ok(number)
    }

    fn repo_url(&self) -> String {
        format!("{}/repos/{}", self.api, self.config.repo)
    }

    fn issue_url(&self, number: u64) -> String {
        format!("{}/issues/{number}", self.repo_url())
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let write = method != Method::GET;
        let mut request = self.client.request(
            method,
            format!("{}/repos/{}/{}", self.api, self.config.repo, path),
        );
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().map_err(|error| {
            let error = anyhow::Error::new(error.without_url()).context("GitHub request failed");
            if write {
                error.context(WriteOutcome::Uncertain)
            } else {
                error
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Ok((status, Value::Null));
        }
        let decoded = (|| -> Result<Value> {
            let mut bytes = Vec::new();
            response
                .by_ref()
                .take(RESPONSE_LIMIT + 1)
                .read_to_end(&mut bytes)
                .context("reading GitHub response")?;
            ensure!(
                u64::try_from(bytes.len())? <= RESPONSE_LIMIT,
                "GitHub response too large"
            );
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).context("invalid GitHub response")?
            };
            Ok(value)
        })()
        .map_err(|error| {
            if write {
                error.context(WriteOutcome::Uncertain)
            } else {
                error
            }
        })?;
        Ok((status, decoded))
    }

    fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let write = method != Method::GET;
        let (status, value) = self.request(method, path, body)?;
        if write && status.is_server_error() {
            return Err(
                anyhow::anyhow!("GitHub returned HTTP {status}").context(WriteOutcome::Uncertain)
            );
        }
        if !status.is_success() {
            let error = anyhow::anyhow!("GitHub returned HTTP {status}");
            return Err(if write && status.is_client_error() {
                error.context(WriteOutcome::NotApplied)
            } else {
                error
            });
        }
        Ok(value)
    }

    fn list<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<Vec<T>> {
        let mut all = Vec::new();
        for page in 1..=100 {
            let value = self.call(
                Method::GET,
                &format!("{path}?per_page=100&page={page}"),
                None,
            )?;
            let rows: Vec<T> =
                serde_json::from_value(value).context("invalid GitHub list response")?;
            let done = rows.len() < 100;
            all.extend(rows);
            if done {
                return Ok(all);
            }
        }
        bail!("GitHub list exceeded page limit; refusing incomplete records")
    }

    fn raw(&self, number: u64) -> Result<RawIssue> {
        let issue: RawIssue =
            serde_json::from_value(self.call(Method::GET, &format!("issues/{number}"), None)?)?;
        self.bind(&issue, number)?;
        Ok(issue)
    }

    fn bind(&self, issue: &RawIssue, number: u64) -> Result<()> {
        ensure!(
            number > 0
                && issue.number == number
                && issue.id > 0
                && issue.repository_url.eq_ignore_ascii_case(&self.repo_url()),
            "GitHub returned another issue or repository"
        );
        ensure!(
            issue.pull_request.is_none(),
            "pull requests are not tracker issues"
        );
        Ok(())
    }

    fn metadata(&self, body: &str) -> Result<serde_json::Map<String, Value>> {
        let normalized = body.replace("\r\n", "\n");
        let body = normalized.as_str();
        let Some((_, tail)) = body.split_once(META_START) else {
            ensure!(
                !body.contains("<!-- kod:issue"),
                "malformed issue metadata marker"
            );
            return Ok(Default::default());
        };
        ensure!(
            body.matches("<!-- kod:issue").count() == 1,
            "multiple issue metadata markers"
        );
        let (text, rest) = tail
            .split_once(META_END)
            .context("unterminated issue metadata")?;
        ensure!(
            !rest.contains("<!-- kod:issue"),
            "multiple issue metadata blocks"
        );
        let value = crate::strict_json::parse(text).context("invalid issue metadata")?;
        let object = value
            .as_object()
            .context("issue metadata is not an object")?
            .clone();
        let allowed = [
            self.mapping.requirement_id_field.as_str(),
            self.mapping.risk_class_field.as_str(),
            "acceptance_criteria",
            "approved_versions",
            "links",
            "rework",
        ];
        ensure!(
            object.keys().all(|k| allowed.contains(&k.as_str())),
            "unknown issue metadata key"
        );
        Ok(object)
    }

    fn state(&self, issue: &RawIssue) -> Result<State> {
        ensure!(
            issue
                .labels
                .iter()
                .all(|l| !l.name.starts_with("kod:") || self.mapping.standard(&l.name).is_some()),
            "unmapped GitHub workflow label"
        );
        ensure!(
            matches!(issue.state.as_str(), "open" | "closed"),
            "unmapped GitHub state"
        );
        let states: Vec<_> = issue
            .labels
            .iter()
            .filter(|l| l.name.starts_with("kod:"))
            .filter_map(|l| self.mapping.standard(&l.name))
            .collect();
        ensure!(states.len() <= 1, "ambiguous workflow labels");
        if issue.state == "closed" {
            return Ok(State::Done);
        }
        if let Some(state) = states.first() {
            ensure!(
                *state != State::Done,
                "closed workflow label on an open issue"
            );
            return Ok(*state);
        }
        Ok(State::Ready)
    }

    fn status_comments(&self, number: u64) -> Result<Vec<Comment>> {
        let comments: Vec<Comment> = self.list(&format!("issues/{number}/comments"))?;
        let mut owned = Vec::new();
        for comment in comments {
            ensure!(
                comment.id > 0
                    && comment
                        .issue_url
                        .eq_ignore_ascii_case(&self.issue_url(number)),
                "comment belongs to another issue"
            );
            if comment.user.id == self.config.comment_author_id
                && comment.user.kind == "Bot"
                && comment.body.starts_with(STATUS)
            {
                owned.push(comment);
            }
        }
        ensure!(
            owned.len() <= 1,
            "multiple owned status comments; reconcile before retry"
        );
        Ok(owned)
    }
}

fn optional_text(object: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        _ => bail!("invalid issue field {key}"),
    }
}

fn texts(object: &serde_json::Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(value) = object.get(key) else {
        return Ok(vec![]);
    };
    let values: Vec<String> = serde_json::from_value(value.clone())
        .with_context(|| format!("invalid issue field {key}"))?;
    ensure!(
        values.iter().all(|s| !s.trim().is_empty()),
        "empty issue field {key}"
    );
    Ok(values)
}

impl Tracker for GitHub {
    fn name(&self) -> &str {
        &self.mapping.tracker
    }

    fn get_issue(&self, key: &str) -> Result<Issue> {
        let number = self.number(key)?;
        let raw = self.raw(number)?;
        let state = self.state(&raw)?;
        let body = raw.body.unwrap_or_default();
        let object = self.metadata(&body)?;
        let mut links: BTreeSet<_> = texts(&object, "links")?.into_iter().collect();
        for key in &links {
            self.number(key)?;
        }
        let children: Vec<RawIssue> = self.list(&format!("issues/{number}/sub_issues"))?;
        for child in children {
            if !child.repository_url.eq_ignore_ascii_case(&self.repo_url()) {
                continue;
            }
            self.bind(&child, child.number)?;
            links.insert(self.key(child.number));
        }
        if let Some(rework) = object.get("rework") {
            let rework: ReworkMetadata =
                serde_json::from_value(rework.clone()).context("invalid rework metadata")?;
            self.number(&rework.story)?;
            ensure!(
                links.contains(&rework.story),
                "rework has no story back-link"
            );
        }
        Ok(Issue {
            key: self.key(number),
            title: raw.title,
            body,
            acceptance_criteria: texts(&object, "acceptance_criteria")?,
            requirement_id: optional_text(&object, &self.mapping.requirement_id_field)?,
            risk_class: optional_text(&object, &self.mapping.risk_class_field)?,
            reporter: raw.user.login,
            approved_versions: texts(&object, "approved_versions")?,
            links: links.into_iter().collect(),
            state,
        })
    }

    fn comment(&self, key: &str, text: &str) -> Result<()> {
        ensure!(
            !text.contains(STATUS),
            "status marker is reserved for the status updater"
        );
        let number = self.number(key)?;
        self.raw(number)?;
        self.call(
            Method::POST,
            &format!("issues/{number}/comments"),
            Some(&json!({"body": text})),
        )?;
        Ok(())
    }

    fn transition(&self, key: &str, state: State) -> Result<()> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub write lock poisoned"))?;
        let number = self.number(key)?;
        let issue = self.raw(number)?;
        let target = self.mapping.target(state);
        if !matches!(target, "open" | "closed") {
            self.call(
                Method::POST,
                &format!("issues/{number}/labels"),
                Some(&json!({"labels": [target]})),
            )?;
        }
        for label in issue.labels.iter().filter(|l| {
            l.name != target
                && l.name.starts_with("kod:")
                && self.mapping.standard(&l.name).is_some()
        }) {
            let mut url =
                reqwest::Url::parse(&format!("{}/issues/{number}/labels", self.repo_url()))?;
            url.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("invalid GitHub API base"))?
                .push(&label.name);
            let base = reqwest::Url::parse(&format!("{}/", self.repo_url()))?;
            let path = url
                .path()
                .strip_prefix(base.path())
                .context("invalid GitHub label path")?;
            let (status, _) = self.request(Method::DELETE, path, None)?;
            ensure!(
                status.is_success() || status == StatusCode::NOT_FOUND,
                "GitHub label removal failed: {status}"
            );
        }
        self.call(
            Method::PATCH,
            &format!("issues/{number}"),
            Some(&json!({"state": if state == State::Done { "closed" } else { "open" }})),
        )?;
        ensure!(
            self.state(&self.raw(number)?)? == state,
            "GitHub transition did not reach requested state"
        );
        Ok(())
    }

    fn link_pr(&self, key: &str, url: &str) -> Result<()> {
        let path = url
            .strip_prefix("https://github.com/")
            .context("PR link must use GitHub.com HTTPS")?;
        let parts: Vec<_> = path.split('/').collect();
        ensure!(
            parts.len() == 4 && name(parts[0]) && name(parts[1]) && parts[2] == "pull",
            "invalid GitHub PR link"
        );
        let n = parts[3]
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .context("invalid PR number")?;
        ensure!(n.to_string() == parts[3], "non-canonical PR link");
        self.comment(key, &format!("Pull request: {url}"))
    }

    fn create_linked(&self, story: &str, kind: ReworkKind, fields: &Rework) -> Result<String> {
        let _guard = self
            .writes
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub write lock poisoned"))?;
        let number = self.number(story)?;
        self.raw(number)?;
        ensure!(
            !fields.stage.trim().is_empty()
                && !fields.artifact.trim().is_empty()
                && !fields.version.trim().is_empty()
                && !fields.text.trim().is_empty(),
            "incomplete rework fields"
        );
        ensure!(
            !fields.text.contains("<!-- kod:issue"),
            "rework text contains a metadata marker"
        );
        let metadata = json!({"links": [story], "rework": ReworkMetadata { story: story.into(), kind, fields: fields.clone() }});
        let body = format!(
            "{}\n\n{META_START}{}{META_END}",
            fields.text,
            serde_json::to_string_pretty(&metadata)?
        );
        let value = self.call(
            Method::POST,
            "issues",
            Some(
                &json!({"title": format!("Rework for {story}: {}", fields.artifact), "body": body}),
            ),
        )?;
        let raw: RawIssue = serde_json::from_value(value).context(WriteOutcome::Uncertain)?;
        self.bind(&raw, raw.number)
            .context(WriteOutcome::Uncertain)?;
        let key = self.key(raw.number);
        self.call(
            Method::POST,
            &format!("issues/{number}/sub_issues"),
            Some(&json!({"sub_issue_id": raw.id})),
        )
        .with_context(|| format!("created {key} but linking failed; reconcile before retry"))?;
        let children: Vec<RawIssue> = self
            .list(&format!("issues/{number}/sub_issues"))
            .with_context(|| {
                format!("created {key} but link verification failed; reconcile before retry")
            })?;
        ensure!(
            children.iter().any(|child| child.id == raw.id
                && child.number == raw.number
                && child.repository_url.eq_ignore_ascii_case(&self.repo_url())
                && child.pull_request.is_none()),
            "created {key} but rework missing from story; reconcile before retry"
        );
        Ok(key)
    }

    fn is_team_member(&self, user: &str) -> Result<bool> {
        if user.ends_with("[bot]") {
            return Ok(false);
        }
        ensure!(name(user), "invalid GitHub login");
        let (status, value) = self.request(
            Method::GET,
            &format!("collaborators/{user}/permission"),
            None,
        )?;
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        ensure!(
            status.is_success(),
            "GitHub membership lookup failed: {status}"
        );
        let identity: User = serde_json::from_value(value["user"].clone())?;
        ensure!(
            identity.id > 0 && identity.login.eq_ignore_ascii_case(user),
            "GitHub returned another member"
        );
        let permission = value["permission"]
            .as_str()
            .context("missing GitHub permission")?;
        ensure!(
            ["admin", "write", "read", "none"].contains(&permission),
            "unknown GitHub permission"
        );
        Ok(identity.kind == "User" && matches!(permission, "admin" | "write"))
    }

    fn status_comment(&self, key: &str, text: &str) -> Result<()> {
        ensure!(text.len() <= 60_000, "status comment too large");
        let number = self.number(key)?;
        self.raw(number)?;
        let mut uncertain = self
            .writes
            .lock()
            .map_err(|_| anyhow::anyhow!("GitHub write lock poisoned"))?;
        let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(3.max(longest + 1));
        let body = format!("{STATUS}\n\n{fence}text\n{text}\n{fence}");
        if body.len() > 60_000 {
            return Err(
                anyhow::anyhow!("status comment too large").context(WriteOutcome::NotApplied)
            );
        }
        let comments = self.status_comments(number)?;
        if let Some(existing) = comments.first() {
            self.call(
                Method::PATCH,
                &format!("issues/comments/{}", existing.id),
                Some(&json!({"body": body})),
            )?;
            uncertain.remove(&number);
            return Ok(());
        }
        ensure!(
            !uncertain.contains(&number),
            "status create outcome unknown; reconcile before retry"
        );
        uncertain.insert(number);
        let result = self.call(
            Method::POST,
            &format!("issues/{number}/comments"),
            Some(&json!({"body": body})),
        );
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if write_outcome(&error) == WriteOutcome::NotApplied {
                    uncertain.remove(&number);
                }
                return Err(error);
            }
        };
        let comment: Comment = serde_json::from_value(value).context(WriteOutcome::Uncertain)?;
        ensure!(
            comment.user.id == self.config.comment_author_id
                && comment.user.kind == "Bot"
                && comment
                    .issue_url
                    .eq_ignore_ascii_case(&self.issue_url(number)),
            "created status comment has unexpected owner or issue; reconcile before retry"
        );
        uncertain.remove(&number);
        Ok(())
    }

    fn parse_webhook(&self, request: &Webhook) -> Result<Option<Delivery>> {
        ensure!(
            u64::try_from(request.body.len())? <= RESPONSE_LIMIT,
            "webhook body too large"
        );
        let signatures: Vec<_> = request
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("x-hub-signature-256"))
            .collect();
        ensure!(
            signatures.len() == 1,
            "missing or duplicate webhook signature"
        );
        let expected = crate::signature::hex(&hmac_sha256(self.secret.as_bytes(), &request.body));
        let signature = signatures[0]
            .1
            .strip_prefix("sha256=")
            .context("invalid webhook signature algorithm")?;
        ensure!(
            bool::from(signature.as_bytes().ct_eq(expected.as_bytes())),
            "webhook signature does not match"
        );
        let text = std::str::from_utf8(&request.body).context("webhook is not UTF-8")?;
        let value = crate::strict_json::parse(text).context("invalid webhook JSON")?;
        ensure!(value.is_object(), "webhook is not an object");
        if value.get("issue").is_none() {
            return Ok(None);
        }
        ensure!(
            value["repository"]["full_name"]
                .as_str()
                .is_some_and(|repo| repo.eq_ignore_ascii_case(&self.config.repo)),
            "webhook belongs to another repository"
        );
        let issue: RawIssue =
            serde_json::from_value(value["issue"].clone()).context("invalid webhook issue")?;
        if issue.pull_request.is_some() {
            return Ok(None);
        }
        self.bind(&issue, issue.number)?;
        let action = value["action"].as_str().context("missing webhook action")?;
        let event = if let Some(comment) = value.get("comment") {
            if action != "created" {
                return Ok(None);
            }
            let comment: Comment =
                serde_json::from_value(comment.clone()).context("invalid webhook comment")?;
            if comment.user.kind != "User" {
                return Ok(None);
            }
            ensure!(
                comment.id > 0
                    && comment.user.id > 0
                    && name(&comment.user.login)
                    && comment
                        .issue_url
                        .eq_ignore_ascii_case(&self.issue_url(issue.number)),
                "invalid comment identity"
            );
            TaskEvent::Commented {
                issue: self.key(issue.number),
                author: comment.user.login,
                body: comment.body,
            }
        } else {
            match action {
                "labeled" | "unlabeled" => {
                    let label = value["label"]["name"]
                        .as_str()
                        .context("missing webhook label")?;
                    if label != self.mapping.trigger_label && !label.starts_with("kod:") {
                        return Ok(None);
                    }
                    let sender: User = serde_json::from_value(value["sender"].clone())
                        .context("missing webhook sender")?;
                    if sender.kind != "User" {
                        return Ok(None);
                    }
                    ensure!(
                        sender.id > 0 && name(&sender.login),
                        "invalid webhook sender"
                    );
                    if label.starts_with("kod:") {
                        TaskEvent::Edited {
                            issue: self.key(issue.number),
                        }
                    } else if action == "labeled" {
                        TaskEvent::Labeled {
                            issue: self.key(issue.number),
                            by: sender.login,
                        }
                    } else {
                        TaskEvent::Unlabeled {
                            issue: self.key(issue.number),
                            by: sender.login,
                        }
                    }
                }
                "edited" | "closed" | "reopened" => TaskEvent::Edited {
                    issue: self.key(issue.number),
                },
                _ => return Ok(None),
            }
        };
        Ok(Some(Delivery {
            id: format!("sha256:{:x}", Sha256::digest(&request.body)),
            event,
        }))
    }
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
