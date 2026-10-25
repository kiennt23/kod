//! Loopback upstreams; repository objects are produced by git from requests the real adapter sends.
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

pub fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "kod-agent")
        .env("GIT_AUTHOR_EMAIL", "agent@kod.invalid")
        .env("GIT_COMMITTER_NAME", "kod-agent")
        .env("GIT_COMMITTER_EMAIL", "agent@kod.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(bytes) = input {
        child.stdin.take().context("git stdin")?.write_all(bytes)?;
    }
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "fixture git command failed");
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .into())
}
#[derive(Default)]
pub struct Remote {
    pub requests: Vec<(String, String, Value)>,
    pub models: Vec<Value>,
    pub repo: PathBuf,
    pub refs: BTreeMap<String, String>,
    pub pulls: Vec<Value>,
    pub issue: Value,
    pub live: Vec<String>,
    pub fail: bool,
    pub model_blocked: Arc<std::sync::atomic::AtomicBool>,
    pub model_entered: Arc<tokio::sync::Notify>,
}
pub struct Provider {
    pub api: String,
    pub state: Arc<Mutex<Remote>>,
    task: tokio::task::JoinHandle<()>,
}
impl Provider {
    pub async fn start(repo: PathBuf) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let api = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(Remote {
            repo,
            issue: issue(&api),
            ..Default::default()
        }));
        let app = Router::new().fallback(handler).with_state(state.clone());
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("fixture listener: {e}");
            }
        });
        Ok(Self { api, state, task })
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn issue(api: &str) -> Value {
    json!({"id":7,"number":7,"repository_url":format!("{api}/repos/acme/app"),"title":"Connected acceptance","body":"Read approved design and refuse the planted secret.\n<!-- kod:issue\n{\"requirement_id\":\"R-1\",\"risk_class\":\"low\",\"approved_versions\":[\"design\"],\"acceptance_criteria\":[\"Write answer.txt with 42\"]}\n-->","state":"open","user":{"id":9,"login":"kiennt23","type":"User"},"labels":[{"name":"kod"}]})
}
fn model(body: &Value) -> Result<Value> {
    let step = body["messages"]
        .as_array()
        .context("model messages")?
        .iter()
        .filter(|v| v["role"] == "assistant")
        .count();
    let (name, input) = match step {
        0 => ("read_doc", json!({"id":"design-v1"})),
        1 => ("read_doc", json!({"id":"planted-secret"})),
        2 => ("write_file", json!({"path":"answer.txt","content":"42\n"})),
        _ => (
            "finish",
            json!({"state":"done","summary":"Connected acceptance finished"}),
        ),
    };
    Ok(
        json!({"type":"message","role":"assistant","content":[{"type":"tool_use","id":format!("call-{step}"),"name":name,"input":input}],"stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":5}}),
    )
}
async fn handler(State(state): State<Arc<Mutex<Remote>>>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().replace("%23", "#");
    let bytes = match to_bytes(request.into_body(), 16 << 20).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    if path == "/v1/messages" {
        let hold = match state.lock() {
            Ok(mut s) => {
                if s.model_blocked.load(std::sync::atomic::Ordering::SeqCst) {
                    s.models.push(body.clone());
                    s.model_entered.notify_one();
                    true
                } else {
                    false
                }
            }
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if hold {
            std::future::pending::<()>().await;
        }
    }
    let result = tokio::task::spawn_blocking(move || {
        let mut s = state.lock().map_err(|_| anyhow::anyhow!("provider lock"))?;
        serve(&mut s, &method, &path, body)
    })
    .await;
    match result {
        Ok(Ok((code, value))) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(value),
        )
            .into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
fn serve(s: &mut Remote, m: &str, p: &str, b: Value) -> Result<(u16, Value)> {
    s.requests.push((m.into(), p.into(), b.clone()));
    if s.fail {
        return Ok((503, json!({})));
    }
    if p == "/v1/messages" {
        s.models.push(b.clone());
        return Ok((200, model(&b)?));
    }
    if p == "/app/installations/2/access_tokens" {
        ensure!(
            b == json!({"repository_ids":[1],"permissions":{"contents":"write","pull_requests":"write"}}),
            "unexpected token scope"
        );
        let token = format!("fixture-token-{}", s.live.len());
        s.live.push(token.clone());
        return Ok((
            201,
            json!({"token":token,"expires_at":(chrono::Utc::now()+chrono::Duration::minutes(59)).to_rfc3339(),"permissions":{"contents":"write","pull_requests":"write","metadata":"read"},"repositories":[{"id":1,"full_name":"acme/app"}]}),
        ));
    }
    if p == "/installation/token" {
        s.live.clear();
        return Ok((204, Value::Null));
    }
    if p == "/installation/repositories" {
        return Ok((
            200,
            json!({"total_count":1,"repositories":[{"id":1,"full_name":"acme/app"}]}),
        ));
    }
    if p == "/repos/acme/app" {
        return Ok((
            200,
            json!({"id":1,"full_name":"acme/app","default_branch":"main"}),
        ));
    }
    if p == "/repos/acme/app/rulesets/3" {
        return Ok((
            200,
            json!({"id":3,"target":"branch","enforcement":"active","bypass_actors":[],"conditions":{"ref_name":{"include":["~ALL"],"exclude":["refs/heads/agent/*","refs/heads/agent/**/*"]}},"rules":[{"type":"creation"},{"type":"update"},{"type":"deletion"}]}),
        ));
    }
    if p.ends_with("/collaborators/kiennt23/permission") {
        return Ok((
            200,
            json!({"permission":"write","user":{"id":9,"login":"kiennt23","type":"User"}}),
        ));
    }
    if p == "/repos/acme/app/issues/7" {
        if m == "PATCH" {
            if let Some(v) = b.get("state") {
                s.issue["state"] = v.clone();
            }
            if let Some(v) = b.get("labels") {
                s.issue["labels"] = json!(
                    v.as_array()
                        .context("labels")?
                        .iter()
                        .map(|x| json!({"name":x}))
                        .collect::<Vec<_>>()
                );
            }
        }
        return Ok((200, s.issue.clone()));
    }
    if p.ends_with("/sub_issues") || p.ends_with("/comments") {
        return Ok((if m == "GET" { 200 } else { 201 }, json!([])));
    }
    if let Some(branch) = p.strip_prefix("/repos/acme/app/git/ref/heads/") {
        return Ok(match s.refs.get(branch) {
            Some(sha) => (
                200,
                json!({"ref":format!("refs/heads/{branch}"),"object":{"type":"commit","sha":sha}}),
            ),
            None => (404, json!({})),
        });
    }
    if p == "/repos/acme/app/git/refs" && m == "POST" {
        s.refs.insert(
            b["ref"]
                .as_str()
                .context("ref")?
                .trim_start_matches("refs/heads/")
                .into(),
            b["sha"].as_str().context("sha")?.into(),
        );
        return Ok((201, b));
    }
    if p == "/repos/acme/app/pulls" {
        if m == "GET" {
            return Ok((200, json!(s.pulls)));
        }
        let branch = b["head"].as_str().context("head")?;
        let sha = s.refs.get(branch).context("unpublished head")?;
        let pr = json!({"number":1,"html_url":"https://github.com/acme/app/pull/1","state":"open","draft":b["draft"],"user":{"id":4},"head":{"repo":{"id":1},"ref":branch,"sha":sha},"base":{"repo":{"id":1},"ref":b["base"]}});
        s.pulls.push(pr.clone());
        return Ok((201, pr));
    }
    if let Some(part) = p.strip_prefix("/repos/acme/app/git/") {
        return objects(s, m, part, &b);
    }
    Ok((404, json!({})))
}
fn objects(s: &Remote, m: &str, p: &str, b: &Value) -> Result<(u16, Value)> {
    let repo = &s.repo;
    if m == "GET" {
        if let Some(sha) = p.strip_prefix("commits/") {
            return Ok((
                200,
                json!({"sha":sha,"tree":{"sha":git(repo,&["rev-parse",&format!("{sha}^{{tree}}")],None)?}}),
            ));
        }
        if let Some(sha) = p.strip_prefix("trees/") {
            let lines = git(repo, &["ls-tree", "-r", "-t", sha], None)?;
            let entries:Result<Vec<_>>=lines.lines().map(|line|{let (head,path)=line.split_once('\t').context("tree entry")?;let mut fields=head.split_whitespace();Ok(json!({"mode":fields.next(),"type":fields.next(),"sha":fields.next(),"path":path}))}).collect();
            return Ok((200, json!({"sha":sha,"truncated":false,"tree":entries?})));
        }
        if let Some(sha) = p.strip_prefix("blobs/") {
            let output = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["cat-file", "blob", sha])
                .output()?;
            ensure!(output.status.success(), "blob read failed");
            return Ok((
                200,
                json!({"sha":sha,"encoding":"base64","content":STANDARD.encode(&output.stdout),"size":output.stdout.len()}),
            ));
        }
    }
    if m == "POST" && p == "trees" {
        git(
            repo,
            &["read-tree", b["base_tree"].as_str().context("base tree")?],
            None,
        )?;
        for entry in b["tree"].as_array().context("tree")? {
            let path = entry["path"].as_str().context("path")?;
            if let Some(text) = entry.get("content") {
                let sha = git(
                    repo,
                    &["hash-object", "-w", "--stdin"],
                    Some(text.as_str().context("content")?.as_bytes()),
                )?;
                git(
                    repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        entry["mode"].as_str().context("mode")?,
                        &sha,
                        path,
                    ],
                    None,
                )?;
            } else {
                git(repo, &["update-index", "--force-remove", "--", path], None)?;
            }
        }
        return Ok((201, json!({"sha":git(repo,&["write-tree"],None)?})));
    }
    if m == "POST" && p == "commits" {
        return Ok((
            201,
            json!({"sha":git(repo,&["commit-tree",b["tree"].as_str().context("tree")?,"-p",b["parents"][0].as_str().context("parent")?],Some(b["message"].as_str().context("message")?.as_bytes()))?}),
        ));
    }
    Ok((404, json!({})))
}
