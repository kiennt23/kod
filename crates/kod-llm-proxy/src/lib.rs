//! Model-only reverse proxy, the seed of the LLM gateway. The sandbox holds a
//! dummy key; the real one exists only here. Only exact allowlisted paths are
//! forwarded, so ".." and other paths can't reach other endpoints. Every
//! request is checked against the case's limits before it is forwarded,
//! because code in the sandbox can call the proxy without the runner.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
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
        client: reqwest::Client::new(),
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
            // Streamed, so server-sent events reach the runner as they arrive.
            let stream = up.bytes_stream().map(move |chunk| {
                let _held = &permit;
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
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    println!(
        "{}",
        serde_json::json!({ "ts": ts, "event": event, "path": path, "status": status })
    );
}
