#![allow(dead_code)]
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use kod_loop::{Config, Outcome};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Serves `app` on its own thread and runtime; returns its base URL.
pub fn serve(app: Router) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let l = tokio::net::TcpListener::from_std(l).unwrap();
                axum::serve(l, app).await.unwrap();
            })
    });
    url
}

pub struct Fake {
    /// (status, body) in order; a null body is sent empty.
    pub replies: Vec<(u16, Value)>,
    pub seen: Vec<(HeaderMap, Value)>,
}

pub type Model = Arc<Mutex<Fake>>;

/// A model that plays back `replies` and records each request. Once out of
/// replies it answers 500.
pub fn model(replies: Vec<(u16, Value)>) -> (String, Model) {
    let m: Model = Arc::new(Mutex::new(Fake {
        replies,
        seen: vec![],
    }));
    let app = Router::new()
        .route("/v1/messages", post(answer))
        .with_state(m.clone());
    (serve(app), m)
}

async fn answer(
    State(m): State<Model>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, HeaderMap, String) {
    let mut m = m.lock().unwrap();
    m.seen.push((headers, body));
    let (status, reply) = if m.replies.is_empty() {
        (500, Value::Null)
    } else {
        m.replies.remove(0)
    };
    let body = if reply.is_null() {
        String::new()
    } else {
        reply.to_string()
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    // Test convention: a top-level "retry_after" in a reply is also sent as
    // the Retry-After header.
    if let Some(secs) = reply["retry_after"].as_u64() {
        h.insert(header::RETRY_AFTER, secs.into());
    }
    (StatusCode::from_u16(status).unwrap(), h, body)
}

pub fn bodies(m: &Model) -> Vec<Value> {
    m.lock().unwrap().seen.iter().map(|s| s.1.clone()).collect()
}

pub fn msg(content: Value, stop: &str) -> (u16, Value) {
    (
        200,
        json!({"type": "message", "role": "assistant", "content": content, "stop_reason": stop,
               "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3}}),
    )
}

pub fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

pub fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

pub fn error_body(kind: &str) -> Value {
    json!({"type": "error", "error": {"type": kind, "message": "x"}})
}

pub fn config(url: &str, ws: &Path) -> Config {
    Config {
        base_url: url.into(),
        model: "m".into(),
        max_turns: 10,
        max_tokens: 1000,
        workspace: ws.into(),
        system: "SYSTEM".into(),
        context_bytes: 1_000_000,
        command_timeout: Duration::from_secs(30),
        retry_backoff: vec![Duration::ZERO; 3],
    }
}

/// Runs the loop; returns the outcome and the parsed trace lines.
pub fn go(cfg: &Config, prompt: &str) -> (Outcome, Vec<Value>) {
    let mut trace = vec![];
    let o = kod_loop::run(cfg, prompt, &mut trace);
    let lines = String::from_utf8(trace)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (o, lines)
}
