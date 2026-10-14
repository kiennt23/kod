//! Model-only reverse proxy, the seed of the LLM gateway. The sandbox holds a
//! dummy key; the real one exists only here. Only exact allowlisted paths are
//! forwarded, so ".." and other paths can't reach other endpoints. Every
//! request is checked against the case's limits before it is forwarded,
//! because code in the sandbox can call the proxy without the runner.
//!
//! Log lines are JSON on stdout. Each request gets one line when it is
//! allowed or refused; an allowed /v1/messages request gets a second line,
//! event "usage", when its response body ends, with the token counts the
//! response reported (null when it reported none).
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::Semaphore;

/// Never forwarded in either direction. Client credentials are replaced.
const DROP: [&str; 6] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "x-api-key",
    "authorization",
];

#[derive(Clone)]
pub struct Limits {
    /// Exact model ids. Anything else is refused.
    pub allowed_models: Vec<String>,
    /// Cap on `max_tokens` for /v1/messages, which must be present.
    pub max_tokens: u64,
    /// Request body limit in bytes.
    pub max_body: usize,
    /// Requests per proxy (one proxy per case), counted before any other check.
    pub max_requests: u64,
    /// Requests in flight at once; further requests wait.
    pub max_concurrent: usize,
}

#[derive(Clone)]
pub struct Config {
    /// Without a trailing slash, e.g. "https://api.anthropic.com".
    pub upstream: String,
    pub api_key: HeaderValue,
    pub allowed_paths: Vec<String>,
    pub limits: Limits,
}

struct AppState {
    cfg: Config,
    client: reqwest::Client,
    requests: AtomicU64,
    in_flight: Arc<Semaphore>,
}

pub fn app(cfg: Config) -> Router {
    let in_flight = Arc::new(Semaphore::new(cfg.limits.max_concurrent));
    let state = Arc::new(AppState {
        cfg,
        // The client sends the key, so it never follows a redirect, which could
        // carry the key to another host. A connect timeout catches a dead
        // upstream; there is no total or read timeout, because responses stream
        // and a long generation can be silent for minutes. The case timeout and
        // the kill switch bound a stalled request.
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("building the upstream client"),
        requests: AtomicU64::new(0),
        in_flight,
    });
    Router::new().fallback(forward).with_state(state)
}

fn refuse(event: &str, path: &str, status: StatusCode) -> Response {
    log(event, path, status.as_u16());
    status.into_response()
}

/// Checks the request body against the model and token limits.
fn check_body(lim: &Limits, path: &str, body: &[u8]) -> Result<(), (&'static str, StatusCode)> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Err(("bad_request", StatusCode::BAD_REQUEST));
    };
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("");
    if !lim.allowed_models.iter().any(|m| m == model) {
        return Err(("deny_model", StatusCode::FORBIDDEN));
    }
    if path == "/v1/messages" {
        match v.get("max_tokens").and_then(|t| t.as_u64()) {
            Some(t) if t <= lim.max_tokens => {}
            _ => return Err(("bad_request", StatusCode::BAD_REQUEST)),
        }
    }
    Ok(())
}

async fn forward(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    if !st.cfg.allowed_paths.contains(&path) {
        return refuse("deny", &path, StatusCode::FORBIDDEN);
    }
    let lim = &st.cfg.limits;
    if st.requests.fetch_add(1, Ordering::SeqCst) >= lim.max_requests {
        return refuse("over_budget", &path, StatusCode::TOO_MANY_REQUESTS);
    }
    let url = format!(
        "{}{}",
        st.cfg.upstream,
        req.uri().path_and_query().map_or("", |p| p.as_str())
    );
    let method = req.method().clone();
    let mut headers = req.headers().clone();
    for h in DROP {
        headers.remove(h);
    }
    headers.insert("x-api-key", st.cfg.api_key.clone());
    let Ok(body) = to_bytes(req.into_body(), lim.max_body).await else {
        return refuse("too_large", &path, StatusCode::PAYLOAD_TOO_LARGE);
    };
    if let Err((event, status)) = check_body(lim, &path, &body) {
        return refuse(event, &path, status);
    }
    // Held until the response body has been fully streamed.
    let Ok(permit) = st.in_flight.clone().acquire_owned().await else {
        return refuse("upstream_error", &path, StatusCode::SERVICE_UNAVAILABLE);
    };
    match st
        .client
        .request(method, url)
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(up) => {
            log("allow", &path, up.status().as_u16());
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
            let mut meter = (path == "/v1/messages").then(|| Meter {
                usage: Usage::new(sse),
                path: path.clone(),
                status: up.status().as_u16(),
            });
            // Streamed, so server-sent events reach the runner as they arrive.
            let stream = up.bytes_stream().map(move |chunk| {
                let _held = &permit;
                if let (Some(m), Ok(c)) = (&mut meter, &chunk) {
                    m.usage.feed(c);
                }
                chunk
            });
            res.body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => {
            eprintln!("upstream: {e}");
            refuse("upstream_error", &path, StatusCode::BAD_GATEWAY)
        }
    }
}

fn log(event: &str, path: &str, status: u16) {
    log_with(event, path, status, Map::new());
}

fn log_with(event: &str, path: &str, status: u16, mut extra: Map<String, Value>) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for (k, v) in [
        ("ts", json!(ts)),
        ("event", json!(event)),
        ("path", json!(path)),
        ("status", json!(status)),
    ] {
        extra.insert(k.into(), v);
    }
    println!("{}", Value::Object(extra));
}

/// Logs the usage line when the response body is dropped: after its last
/// byte, or when the client went away.
struct Meter {
    usage: Usage,
    path: String,
    status: u16,
}

impl Drop for Meter {
    fn drop(&mut self) {
        let usage = std::mem::replace(&mut self.usage, Usage::new(false));
        log_with("usage", &self.path, self.status, usage.finish());
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
    seen: Map<String, Value>,
}

impl Usage {
    pub fn new(sse: bool) -> Self {
        Usage {
            sse,
            buf: vec![],
            overflow: false,
            seen: Map::new(),
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

    /// The usage fields, each a number or null when the response had none.
    pub fn finish(mut self) -> Map<String, Value> {
        let rest = std::mem::take(&mut self.buf);
        if self.sse {
            self.sse_line(&rest);
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
    /// the final output_tokens. Later values replace earlier ones.
    fn sse_line(&mut self, line: &[u8]) {
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        if let Ok(v) = serde_json::from_slice::<Value>(data.trim_ascii()) {
            match v["type"].as_str() {
                Some("message_start") => self.take(&v["message"]["usage"]),
                Some("message_delta") => self.take(&v["usage"]),
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
