//! The LLM gateway, grown from phase 0's model-only proxy. One long-running
//! process serves every run. A run is known by the token in its base URL,
//! `/r/<token>`, and gets its own request count, concurrency limit and spend
//! budget. The sandbox holds no key; the real one exists only here. Only
//! exact allowlisted paths are forwarded, so ".." and other paths can't
//! reach other endpoints.
//!
//! Each allowed request is checked in this order, and the first refusal
//! wins: request count, halted run, path, anthropic-beta header, body size,
//! body shape (an allowlist of the fields, content blocks and sources the
//! gateway can price and see), model and max_tokens, routing (kod-policy
//! `model_request`, recorded in the audit log), budget reservation, then the
//! redacted prompt capture in the audit log. A failed audit write refuses the
//! request. Every refusal is also a `run_audit` record. The response is
//! streamed back; when it ends, its cost settles the reservation and the
//! output capture is written to the audit log. A failed output write halts
//! the run, and a halt is a `run_audit` record too.
//!
//! Log lines are JSON on stdout and carry the run id. Each request within a
//! run's request budget gets one line when it is allowed or refused, and the
//! first request past it gets an "over_budget" line; later ones are refused
//! silently, so the log is bounded by the budget. Requests for an unknown
//! run are refused with no line. Logged paths are cut at LOG_PATH_MAX bytes.
//! An allowed /v1/messages request with a 2xx response gets a second line,
//! event "usage", when its response body ends, with the token counts the
//! response reported (null when it reported none or the stream ended early)
//! and the run's spend so far.
pub mod budget;
pub mod config;
mod lifecycle;
use lifecycle::until_revoked;
pub use lifecycle::{MODEL_CANCEL_DEADLINE, ReservationSink, RunRegistry};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use budget::Ledger;
pub use budget::Spend;
use config::{Price, Prices, RunSpec};
use futures_util::StreamExt;
use kod_audit::{AuditLog, RUN_AUDIT};
use kod_policy::Policy;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Never forwarded in either direction. Client credentials are replaced.
const DROP: [&str; 7] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "x-api-key",
    "authorization",
    // reqwest here has no gzip, so a compressed reply could not be metered.
    "accept-encoding",
];

/// Defaults for the KOD_* settings, shared with kod-eval, which records them.
pub const DEFAULT_ALLOWED_PATHS: &str = "/v1/messages,/v1/messages/count_tokens";
pub const DEFAULT_MAX_TOKENS: u64 = 8192;
pub const DEFAULT_MAX_BODY: u64 = 4 << 20;
pub const DEFAULT_MAX_REQUESTS: u64 = 200;
pub const DEFAULT_MAX_CONCURRENT: u64 = 2;

/// Top-level request fields the gateway can price and bound. Anything else
/// is refused: `mcp_servers` and `container` reach systems the gateway can't
/// see, and fields like `service_tier` change the price.
const BODY_FIELDS: [&str; 13] = [
    "model",
    "max_tokens",
    "messages",
    "system",
    "metadata",
    "stop_sequences",
    "stream",
    "temperature",
    "top_k",
    "top_p",
    "tools",
    "tool_choice",
    "thinking",
];

/// Content block types the gateway forwards. Server tool blocks, search
/// results and anything newer are refused.
const BLOCK_TYPES: [&str; 7] = [
    "text",
    "image",
    "document",
    "tool_use",
    "tool_result",
    "thinking",
    "redacted_thinking",
];

/// Keys of a tool definition. A tool with a `type` is a server tool (web
/// search, web fetch, code execution), billed outside `usage` or fetching
/// content the gateway never sees, so it is refused.
const TOOL_FIELDS: [&str; 4] = ["name", "description", "input_schema", "cache_control"];

/// anthropic-beta values the gateway forwards. A beta can unlock features it
/// can't price or see, so each one is added only after checking that.
const ALLOWED_BETAS: [&str; 0] = [];

/// Image blocks per request, the API's own limit. Each reserves
/// `budget::IMAGE_TOKENS`.
pub const MAX_IMAGES: u64 = 100;

/// Audit record types, keys of the compliance profile's retention_days.
pub const PROMPTS: &str = "prompts";
pub const MODEL_OUTPUTS: &str = "model_outputs";

/// Bytes of each request and response body kept in the audit log. The rest
/// is represented by its length and sha256.
// ponytail: kod-audit caps a payload at 1 MiB and escaping can grow text up
// to 6x, so captures are cut; full capture needs a blob store (open question).
pub const CAPTURE_MAX: usize = 128 << 10;

/// Bytes of a response (or of a stream's assembled text) held for capture.
/// Redaction runs on all of it before the cut to CAPTURE_MAX.
// ponytail: a secret straddling this cut could survive only if redaction
// shrank the first ASSEMBLE_MAX bytes below CAPTURE_MAX, i.e. an output that
// is almost all secrets.
pub const ASSEMBLE_MAX: usize = 8 * CAPTURE_MAX;

/// Logged paths are cut to this many bytes; the agent chooses them.
const LOG_PATH_MAX: usize = 256;

#[derive(Clone)]
pub struct Limits {
    /// Exact model ids. Anything else is refused.
    pub allowed_models: Vec<String>,
    /// Cap on `max_tokens` for /v1/messages, which must be present.
    pub max_tokens: u64,
    /// Request body limit in bytes.
    pub max_body: usize,
    /// Requests per run, counted before any other check.
    pub max_requests: u64,
    /// Requests per run in flight at once; further requests wait.
    pub max_concurrent: usize,
}

/// The one model endpoint. Routing asks kod-policy whether `name` is allowed
/// for the run's data classes.
#[derive(Clone)]
pub struct Endpoint {
    pub name: String,
    /// Without a trailing slash, e.g. "https://api.anthropic.com".
    pub upstream: String,
    pub api_key: HeaderValue,
}

#[derive(Clone)]
pub struct Config {
    pub endpoint: Endpoint,
    pub allowed_paths: Vec<String>,
    pub limits: Limits,
    /// Every allowed model has a price (checked by `config::parse_prices`).
    pub prices: Prices,
    /// Run token to run.
    pub runs: HashMap<String, RunSpec>,
    pub policy: Arc<Policy>,
    pub audit: Arc<AuditLog>,
}

struct Run {
    spec: RunSpec,
    requests: AtomicU64,
    in_flight: Arc<Semaphore>,
    ledger: Mutex<Ledger>,
    cancel: tokio::sync::watch::Sender<bool>,
    sink: Option<Arc<dyn ReservationSink>>,
}

struct AppState {
    cfg: Config,
    client: reqwest::Client,
    runs: RunRegistry,
}

pub fn app(cfg: Config) -> Router {
    app_with_registry(cfg).0
}

pub fn app_with_registry(cfg: Config) -> (Router, RunRegistry) {
    let runs = RunRegistry::new(cfg.limits.max_concurrent);
    for (token, spec) in &cfg.runs {
        // Config's file loader validates these; fail closed (visibly) for invalid direct callers.
        runs.register(token, spec.clone())
            .unwrap_or_else(|error| panic!("model registration refused: {error}"));
    }
    let registry = runs.clone();
    let state = Arc::new(AppState {
        cfg,
        // The client sends the key, so it never follows a redirect, which could
        // carry the key to another host. A connect timeout catches a dead
        // upstream; there is no total or read timeout, because responses stream
        // and a long generation can be silent for minutes. The run's timeout
        // and the kill switch bound a stalled request.
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("building the upstream client"),
        runs,
    });
    (Router::new().fallback(forward).with_state(state), registry)
}

/// A refusal: the log event, the status, and the reason kept in the audit log.
type Refusal = (&'static str, StatusCode, String);

fn shape(why: impl Into<String>) -> Refusal {
    ("deny_shape", StatusCode::BAD_REQUEST, why.into())
}

/// Logs a refusal and records it in the audit log. The request is refused
/// whether or not the record is written.
async fn refuse(
    st: &AppState,
    run: &str,
    path: &str,
    (event, status, reason): Refusal,
) -> Response {
    let extra = Map::from_iter([("reason".into(), json!(reason))]);
    log_with(event, run, path, status.as_u16(), extra);
    let record = json!({
        "kind": "gateway_refusal",
        "run": run,
        "event": event,
        "path": cut(path),
        "status": status.as_u16(),
        "reason": reason,
    });
    if audit(&st.cfg.audit, move |a| a.append(RUN_AUDIT, &record))
        .await
        .is_err()
    {
        eprintln!("audit: a refusal for run {run} was not recorded");
    }
    status.into_response()
}

/// Records that a run was halted. Called off the async threads.
fn record_halt(log: &AuditLog, run: &str, reason: &str) {
    let record = json!({"kind": "gateway_halt", "run": run, "reason": reason});
    if log.append(RUN_AUDIT, &record).is_err() {
        eprintln!("audit: the halt of run {run} was not recorded");
    }
}

/// What an allowed body asks for.
struct Checked {
    model: String,
    /// 0 for count_tokens.
    max_tokens: u64,
    images: u64,
    /// The checked body re-serialized: what is forwarded, so a duplicate key
    /// cannot show the upstream something other than what was checked.
    body: Vec<u8>,
}

/// Checks the request body against the model and token limits and the shape
/// allowlist: only fields, content blocks, sources and tools whose cost is
/// bounded by the reservation, and that send nothing where the gateway can't
/// see it, are forwarded.
fn check_body(lim: &Limits, path: &str, body: &[u8]) -> Result<Checked, Refusal> {
    let Ok(Value::Object(v)) = serde_json::from_slice::<Value>(body) else {
        let why = "the body is not a JSON object".into();
        return Err(("bad_request", StatusCode::BAD_REQUEST, why));
    };
    let model = v.get("model").and_then(Value::as_str).unwrap_or("");
    if !lim.allowed_models.iter().any(|m| m == model) {
        let why = format!("model {model:?} is not allowed");
        return Err(("deny_model", StatusCode::FORBIDDEN, why));
    }
    if let Some(k) = v.keys().find(|k| !BODY_FIELDS.contains(&k.as_str())) {
        return Err(shape(format!("field {k} is not allowed")));
    }
    let mut images = 0;
    let Some(messages) = v.get("messages").and_then(Value::as_array) else {
        return Err(shape("messages must be a list"));
    };
    for m in messages {
        check_content(&m["content"], &mut images)?;
    }
    if let Some(system) = v.get("system") {
        check_content(system, &mut images)?;
    }
    if let Some(tools) = v.get("tools") {
        let Some(tools) = tools.as_array() else {
            return Err(shape("tools must be a list"));
        };
        for t in tools {
            let Some(o) = t.as_object() else {
                return Err(shape("a tool must be an object"));
            };
            if let Some(k) = o.keys().find(|k| !TOOL_FIELDS.contains(&k.as_str())) {
                return Err(shape(format!("tool key {k} is not allowed")));
            }
            check_cache(t)?;
        }
    }
    if images > MAX_IMAGES {
        return Err(shape(format!("more than {MAX_IMAGES} images")));
    }
    let model = model.to_owned();
    if path != "/v1/messages" {
        return Ok(Checked {
            model,
            max_tokens: 0,
            images,
            body: serde_json::to_vec(&v).unwrap_or_default(),
        });
    }
    match v.get("max_tokens").and_then(Value::as_u64) {
        Some(max_tokens) if max_tokens <= lim.max_tokens => Ok(Checked {
            model,
            max_tokens,
            images,
            body: serde_json::to_vec(&v).unwrap_or_default(),
        }),
        _ => {
            let why = format!("max_tokens must be a whole number up to {}", lim.max_tokens);
            Err(("bad_request", StatusCode::BAD_REQUEST, why))
        }
    }
}

/// Message or system content: text, or a list of allowed blocks. Images and
/// documents must be inline (no URL or file sources, which the gateway can't
/// size), and documents plain text (a PDF page costs far more than its bytes).
fn check_content(v: &Value, images: &mut u64) -> Result<(), Refusal> {
    let blocks = match v {
        Value::String(_) => return Ok(()),
        Value::Array(b) => b,
        _ => return Err(shape("content must be text or a list of blocks")),
    };
    for b in blocks {
        let t = b["type"].as_str().unwrap_or_default();
        if !BLOCK_TYPES.contains(&t) {
            return Err(shape(format!("content block type {t:?} is not allowed")));
        }
        check_cache(b)?;
        let source = b["source"]["type"].as_str().unwrap_or_default();
        match t {
            "image" if source != "base64" => return Err(shape("image sources must be base64")),
            "image" => *images = images.saturating_add(1),
            "document" if source != "text" => {
                return Err(shape("document sources must be plain text"));
            }
            "tool_result" if b.get("content").is_some() => check_content(&b["content"], images)?,
            _ => {}
        }
    }
    Ok(())
}

/// Only the 5-minute prompt cache is priced (`cache_write`); a 1-hour write
/// costs more.
fn check_cache(b: &Value) -> Result<(), Refusal> {
    match b.get("cache_control").map(|c| &c["ttl"]) {
        None | Some(Value::Null) => Ok(()),
        Some(t) if t == "5m" => Ok(()),
        _ => Err(shape("only the 5-minute prompt cache is allowed")),
    }
}

/// The kod-policy input for a model request from this run.
pub fn route_input(spec: &RunSpec, endpoint: &str) -> Value {
    json!({
        "action": "model_request",
        "actor": {"kind": "agent"},
        "run": {
            "id": spec.id,
            "ticket": spec.ticket,
            "repo": spec.repo,
            "canceled": false,
            "data_classes": spec.data_classes,
            "auth_mode": "api_key",
        },
        "endpoint": endpoint,
    })
}

/// Runs an audit write off the async threads. Any failure is an error.
async fn audit<T: Send + 'static>(
    log: &Arc<AuditLog>,
    f: impl FnOnce(&AuditLog) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let log = log.clone();
    tokio::task::spawn_blocking(move || f(&log)).await?
}

/// A body as kept in the audit log. Secrets are redacted from the whole text
/// before it is cut to CAPTURE_MAX bytes, so a cut can't leave part of one
/// behind. `complete` is false when `text` is already only part of the body;
/// `bytes` and `sha256` describe the full body.
pub fn capture(text: &str, complete: bool, bytes: u64, sha256: String) -> Value {
    let red = kod_audit::redact::redact(text);
    let kept = &red[..red.floor_char_boundary(CAPTURE_MAX)];
    json!({
        "text": kept,
        "bytes": bytes,
        "sha256": sha256,
        "truncated": !complete || kept.len() < red.len(),
    })
}

fn hex(d: impl AsRef<[u8]>) -> String {
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

async fn forward(State(st): State<Arc<AppState>>, req: Request) -> Response {
    // "/r/<token>/v1/messages" becomes run <token> and path "/v1/messages".
    let Some((run, path)) = req
        .uri()
        .path()
        .strip_prefix("/r/")
        .and_then(|r| r.split_once('/'))
        .and_then(|(token, rest)| {
            let run = st.runs.lookup(token)?;
            Some((run.clone(), format!("/{rest}")))
        })
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    until_revoked(run.cancel.subscribe(), async {
        Ok(forward_run(st, req, run, path).await)
    })
    .await
    .unwrap_or_else(|_| StatusCode::UNAUTHORIZED.into_response())
}

async fn forward_run(st: Arc<AppState>, req: Request, run: Arc<Run>, path: String) -> Response {
    let id = run.spec.id.as_str();
    let lim = &st.cfg.limits;
    // Every request counts, refused ones too, so a script can neither probe
    // nor fill the log for free.
    let n = run.requests.fetch_add(1, Ordering::SeqCst);
    if n > lim.max_requests {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if n == lim.max_requests {
        let why = format!("more than {} requests", lim.max_requests);
        return refuse(
            &st,
            id,
            &path,
            ("over_budget", StatusCode::TOO_MANY_REQUESTS, why),
        )
        .await;
    }
    if run.ledger.lock().map_or(true, |l| l.halted()) {
        let why = "the run is halted".into();
        return refuse(&st, id, &path, ("halted", StatusCode::FORBIDDEN, why)).await;
    }
    if !st.cfg.allowed_paths.contains(&path) {
        let why = "the path is not allowed".into();
        return refuse(&st, id, &path, ("deny", StatusCode::FORBIDDEN, why)).await;
    }
    let betas = req.headers().get_all("anthropic-beta");
    let mut betas = betas
        .iter()
        .flat_map(|v| v.to_str().unwrap_or("?").split(','))
        .map(str::trim);
    if let Some(b) = betas.find(|b| !ALLOWED_BETAS.contains(b)) {
        let why = format!("anthropic-beta {b:?} is not allowed");
        return refuse(&st, id, &path, shape(why)).await;
    }
    let ep = &st.cfg.endpoint;
    let url = match req.uri().query() {
        Some(q) => format!("{}{path}?{q}", ep.upstream),
        None => format!("{}{path}", ep.upstream),
    };
    let method = req.method().clone();
    let mut headers = req.headers().clone();
    for h in DROP {
        headers.remove(h);
    }
    headers.insert("x-api-key", ep.api_key.clone());
    let Ok(body) = to_bytes(req.into_body(), lim.max_body).await else {
        let why = format!("the body is over {} bytes", lim.max_body);
        return refuse(
            &st,
            id,
            &path,
            ("too_large", StatusCode::PAYLOAD_TOO_LARGE, why),
        )
        .await;
    };
    let Checked {
        model,
        max_tokens,
        images,
        body: checked,
    } = match check_body(lim, &path, &body) {
        Ok(c) => c,
        Err(r) => return refuse(&st, id, &path, r).await,
    };
    // Routing: the policy decides whether this endpoint may serve the run's
    // data classes, and the decision is recorded before anything else.
    let input = route_input(&run.spec, &ep.name);
    let decision = st.cfg.policy.decide(&input);
    let (allow, reasons) = (decision.allow, decision.reasons.clone());
    if audit(&st.cfg.audit, move |a| a.record_decision(&input, &decision))
        .await
        .is_err()
    {
        let why = "the routing decision was not recorded".into();
        return refuse(
            &st,
            id,
            &path,
            ("audit_error", StatusCode::SERVICE_UNAVAILABLE, why),
        )
        .await;
    }
    if !allow {
        return refuse(
            &st,
            id,
            &path,
            ("deny_route", StatusCode::FORBIDDEN, reasons.join("; ")),
        )
        .await;
    }
    // count_tokens is free, so it reserves nothing.
    let Some(price) = st.cfg.prices.get(&model).copied() else {
        let why = format!("model {model:?} has no price");
        return refuse(&st, id, &path, ("deny_model", StatusCode::FORBIDDEN, why)).await;
    };
    let reserved = if path == "/v1/messages" {
        budget::reservation(&price, body.len().max(checked.len()), images, max_tokens)
    } else {
        Spend::default()
    };
    if !run.ledger.lock().is_ok_and(|mut l| l.reserve(reserved)) {
        let why = "the request could pass the run's budget".into();
        return refuse(&st, id, &path, ("over_spend", StatusCode::FORBIDDEN, why)).await;
    }
    // Releases the reservation on drop, so a client that disconnects while
    // the handler waits does not leak it. The Meter takes it over.
    let mut reservation = Reservation {
        run: run.clone(),
        spend: reserved,
        held: true,
        dispatched: false,
    };
    let prompt = json!({
        "run": id,
        "endpoint": ep.name,
        "model": model,
        "path": path,
        "request": capture(
            &String::from_utf8_lossy(&body),
            true,
            body.len() as u64,
            hex(Sha256::digest(&body)),
        ),
    });
    if audit(&st.cfg.audit, move |a| a.append(PROMPTS, &prompt))
        .await
        .is_err()
    {
        let why = "the prompt was not recorded".into();
        return refuse(
            &st,
            id,
            &path,
            ("audit_error", StatusCode::SERVICE_UNAVAILABLE, why),
        )
        .await;
    }
    // Held through settlement and the output audit write, so queued calls
    // see any halt before they can reach the upstream.
    let Ok(permit) = until_revoked(run.cancel.subscribe(), async {
        Ok(run.in_flight.clone().acquire_owned().await?)
    })
    .await
    else {
        let why = "the run's request limit was closed".into();
        return refuse(
            &st,
            id,
            &path,
            ("upstream_error", StatusCode::SERVICE_UNAVAILABLE, why),
        )
        .await;
    };
    if run.ledger.lock().map_or(true, |l| l.halted()) {
        let why = "the run is halted".into();
        return refuse(&st, id, &path, ("halted", StatusCode::FORBIDDEN, why)).await;
    }
    if *run.cancel.borrow() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let request_id = format!("{}:{n}", id);
    if let Some(sink) = &run.sink {
        let (sink, id, request_id) = (sink.clone(), id.to_owned(), request_id.clone());
        let result =
            tokio::task::spawn_blocking(move || sink.reserve(&id, &request_id, reserved)).await;
        if !matches!(result, Ok(Ok(()))) {
            run.cancel.send_replace(true);
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    // Once dispatch may begin, transport loss is an unknown bill, never a refund.
    reservation.dispatched = true;
    match until_revoked(run.cancel.subscribe(), async {
        Ok(st
            .client
            .request(method, url)
            .headers(headers)
            .body(checked)
            .send()
            .await?)
    })
    .await
    {
        Ok(up) => {
            log("allow", id, &path, up.status().as_u16());
            let mut res = Response::builder().status(up.status());
            for (k, v) in up.headers() {
                if !DROP.contains(&k.as_str()) {
                    res = res.header(k, v);
                }
            }
            let sse = up
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            let meter = Meter {
                sse,
                usage: Usage::new(sse),
                head: vec![],
                bytes: 0,
                sha: Sha256::new(),
                run: run.clone(),
                audit: st.cfg.audit.clone(),
                reservation,
                request_id,
                price,
                path: path.clone(),
                status: up.status().as_u16(),
                permit: Some(permit),
            };
            // Streamed, so server-sent events reach the runner as they arrive.
            let stream = futures_util::stream::unfold(
                (up.bytes_stream(), meter),
                |(mut upstream, mut meter)| async move {
                    let chunk = until_revoked(meter.run.cancel.subscribe(), async {
                        Ok(upstream.next().await)
                    })
                    .await;
                    match chunk {
                        Ok(Some(Ok(bytes))) => {
                            meter.feed(&bytes);
                            Some((Ok::<_, std::io::Error>(bytes), (upstream, meter)))
                        }
                        Ok(None) | Err(_) => None,
                        Ok(Some(Err(error))) => {
                            Some((Err(std::io::Error::other(error)), (upstream, meter)))
                        }
                    }
                },
            );
            res.body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => {
            // Without the URL, which carries a query string the agent chose.
            let why = e
                .downcast_ref::<reqwest::Error>()
                .map_or_else(|| e.to_string(), |_| "upstream transport failed".to_owned());
            eprintln!("upstream: {why}");
            refuse(
                &st,
                id,
                &path,
                ("upstream_error", StatusCode::BAD_GATEWAY, why),
            )
            .await
        }
    }
}

/// A path cut to LOG_PATH_MAX bytes; the agent chooses it.
fn cut(path: &str) -> &str {
    &path[..path.floor_char_boundary(LOG_PATH_MAX)]
}

fn log(event: &str, run: &str, path: &str, status: u16) {
    log_with(event, run, path, status, Map::new());
}

fn log_with(event: &str, run: &str, path: &str, status: u16, mut extra: Map<String, Value>) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for (k, v) in [
        ("ts", json!(ts)),
        ("event", json!(event)),
        ("run", json!(run)),
        ("path", json!(cut(path))),
        ("status", json!(status)),
    ] {
        extra.insert(k.into(), v);
    }
    println!("{}", Value::Object(extra));
}

/// A run's reserved spend, given back on drop unless a Meter settled it.
struct Reservation {
    run: Arc<Run>,
    spend: Spend,
    held: bool,
    dispatched: bool,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.held
            && let Ok(mut l) = self.run.ledger.lock()
        {
            if self.dispatched {
                l.settle(self.spend, None);
            } else {
                l.release(self.spend);
            }
        }
    }
}

/// Watches a response body as it passes through. When the body is dropped
/// (after its last byte, or when the client went away) it settles the run's
/// reservation, logs the usage line and writes the output capture.
struct Meter {
    sse: bool,
    usage: Usage,
    head: Vec<u8>,
    bytes: u64,
    sha: Sha256,
    run: Arc<Run>,
    audit: Arc<AuditLog>,
    reservation: Reservation,
    request_id: String,
    price: Price,
    path: String,
    status: u16,
    permit: Option<OwnedSemaphorePermit>,
}

impl Meter {
    fn feed(&mut self, c: &[u8]) {
        if self.path == "/v1/messages" {
            self.usage.feed(c);
        }
        let room = ASSEMBLE_MAX.saturating_sub(self.head.len());
        self.head.extend_from_slice(&c[..c.len().min(room)]);
        self.bytes += c.len() as u64;
        self.sha.update(c);
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        // A stream's capture is its assembled text, so a secret split across
        // deltas is whole when it is redacted.
        let (text, complete) = if self.sse {
            self.usage.take_content()
        } else {
            let whole = self.bytes <= self.head.len() as u64;
            (String::from_utf8_lossy(&self.head).into_owned(), whole)
        };
        let usage = std::mem::replace(&mut self.usage, Usage::new(false)).finish();
        let ok = (200..300).contains(&self.status);
        // Definite client rejection is unbilled; server failures and unknown
        // success usage retain the reservation.
        let actual = if ok && self.path == "/v1/messages" {
            budget::cost(&self.price, &usage)
        } else if (400..500).contains(&self.status) || self.path != "/v1/messages" {
            Some(Spend::default())
        } else {
            None
        };
        let sha = hex(std::mem::take(&mut self.sha).finalize());
        let output = json!({
            "run": self.run.spec.id,
            "path": self.path,
            "status": self.status,
            "usage": usage,
            "response": capture(&text, complete, self.bytes, sha),
        });
        let (run, log, path) = (self.run.clone(), self.audit.clone(), self.path.clone());
        let permit = self.permit.take();
        let request_id = self.request_id.clone();
        let reserved = self.reservation.spend;
        self.reservation.held = false;
        let status = self.status;
        tokio::task::spawn_blocking(move || {
            let _held = permit;
            let id = &run.spec.id;
            let actual = match (&run.sink, actual) {
                (Some(sink), Some(actual)) => match sink.settle(id, &request_id, actual) {
                    Ok(()) => Some(actual),
                    Err(error) => {
                        run.cancel.send_replace(true);
                        eprintln!("model settlement failed: {error}");
                        Some(Spend {
                            tokens: actual.tokens.max(reserved.tokens),
                            pico_usd: actual.pico_usd.max(reserved.pico_usd),
                        })
                    }
                },
                (_, actual) => actual,
            };
            let (spent, halted, newly_halted) = match run.ledger.lock() {
                Ok(mut ledger) => {
                    let was = ledger.halted();
                    ledger.settle(reserved, actual);
                    (ledger.spent(), ledger.halted(), !was && ledger.halted())
                }
                Err(_) => {
                    run.cancel.send_replace(true);
                    (Spend::default(), true, false)
                }
            };
            if ok && path == "/v1/messages" {
                let mut extra = usage.clone();
                extra.insert("run_tokens".into(), json!(spent.tokens));
                extra.insert("run_cost_pico_usd".into(), json!(spent.pico_usd));
                extra.insert("run_halted".into(), json!(halted));
                log_with("usage", id, &path, status, extra);
            }
            if newly_halted {
                record_halt(&log, id, "a response cost more than its reservation");
            }
            if log.append(MODEL_OUTPUTS, &output).is_err() {
                if let Ok(mut l) = run.ledger.lock() {
                    l.halt();
                }
                self::log("audit_error", id, &path, 503);
                record_halt(&log, id, "the output capture was not written");
            }
        });
    }
}

/// The usage fields logged for each /v1/messages response.
pub const USAGE_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
];
/// A JSON body or SSE line longer than this is not parsed; its usage is
/// recorded as missing.
const USAGE_BUF_CAP: usize = 4 << 20;

/// Token usage read from a Messages API response while its bytes pass
/// through unchanged. A JSON body is held (up to USAGE_BUF_CAP) and parsed at
/// the end; a server-sent event stream is parsed line by line, so only the
/// current line is held.
pub struct Usage {
    sse: bool,
    buf: Vec<u8>,
    overflow: bool,
    /// message_stop arrived, so the last output count is final.
    stopped: bool,
    seen: Map<String, Value>,
    /// The stream's content blocks as text, up to ASSEMBLE_MAX bytes.
    content: String,
    content_cut: bool,
}

impl Usage {
    pub fn new(sse: bool) -> Self {
        Usage {
            sse,
            buf: vec![],
            overflow: false,
            stopped: false,
            seen: Map::new(),
            content: String::new(),
            content_cut: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.overflow {
            return;
        }
        self.buf.extend_from_slice(chunk);
        if self.sse {
            while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                self.sse_line(&line);
            }
        }
        if self.buf.len() > USAGE_BUF_CAP {
            self.buf = vec![];
            self.overflow = !self.sse;
        }
    }

    /// A stream's assembled content: the text, tool input JSON and thinking
    /// of each content block, blocks separated by a newline; and whether it
    /// is all there (false when it passed ASSEMBLE_MAX). Empty for JSON.
    pub fn take_content(&mut self) -> (String, bool) {
        (std::mem::take(&mut self.content), !self.content_cut)
    }

    fn push_content(&mut self, text: &str) {
        let room = ASSEMBLE_MAX.saturating_sub(self.content.len());
        if text.len() > room {
            self.content_cut = true;
        }
        self.content
            .push_str(&text[..text.floor_char_boundary(room)]);
    }

    /// The usage fields, each a number or null when the response had none.
    pub fn finish(mut self) -> Map<String, Value> {
        let rest = std::mem::take(&mut self.buf);
        if self.sse {
            self.sse_line(&rest);
            if !self.stopped {
                self.seen.remove("output_tokens");
            }
        } else if !self.overflow
            && let Ok(v) = serde_json::from_slice::<Value>(&rest)
        {
            self.take(&v["usage"]);
        }
        USAGE_FIELDS
            .iter()
            .map(|&k| {
                (
                    k.to_owned(),
                    self.seen.get(k).cloned().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    /// message_start carries message.usage; message_delta carries usage with
    /// the output_tokens so far, final once message_stop arrives. Later values
    /// replace earlier ones.
    fn sse_line(&mut self, line: &[u8]) {
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        if let Ok(v) = serde_json::from_slice::<Value>(data.trim_ascii()) {
            match v["type"].as_str() {
                Some("message_start") => self.take(&v["message"]["usage"]),
                Some("message_delta") => self.take(&v["usage"]),
                Some("message_stop") => self.stopped = true,
                Some("content_block_start") => {
                    if !self.content.is_empty() {
                        self.push_content("\n");
                    }
                    if let Some(t) = v["content_block"]["text"].as_str() {
                        self.push_content(t);
                    }
                }
                Some("content_block_delta") => {
                    for k in ["text", "partial_json", "thinking"] {
                        if let Some(t) = v["delta"][k].as_str() {
                            self.push_content(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn take(&mut self, usage: &Value) {
        for k in USAGE_FIELDS {
            if let Some(n) = usage.get(k).filter(|n| n.is_u64()) {
                self.seen.insert(k.into(), n.clone());
            }
        }
    }
}
