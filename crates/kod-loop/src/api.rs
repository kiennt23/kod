//! The model client: the Messages API through the Kod gateway. It sends no
//! API key (the gateway adds it) and follows no redirects.
use serde_json::Value;
use std::time::Duration;

pub enum ApiError {
    /// The gateway refused because the run's budget is spent: a 429 without
    /// an Anthropic error body.
    Budget,
    /// Worth retrying (5xx, a provider 429 with an error body, a transport
    /// error), with the capped Retry-After if the response had one.
    Transient(String, Option<Duration>),
    /// Anything else that produced no message, with what came back.
    Failed(String),
}

pub struct Client {
    http: reqwest::blocking::Client,
    url: String,
}

impl Client {
    pub fn new(base_url: &str) -> Client {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("the HTTP client builds");
        Client {
            http,
            url: format!("{base_url}/v1/messages"),
        }
    }

    /// One request, one attempt. The loop decides on retries, so that every
    /// request counts as a turn.
    pub fn messages(&self, body: &Value) -> Result<Value, ApiError> {
        let r = self
            .http
            .post(&self.url)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            // The URL holds the run's gateway token, a credential.
            .map_err(|e| {
                ApiError::Transient(format!("request failed: {}", e.without_url()), None)
            })?;
        let status = r.status();
        let retry_after = r
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
            .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER));
        let text = r.text().map_err(|e| {
            ApiError::Transient(format!("{status}: reading the body: {e}"), retry_after)
        })?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let why = format!("{status}: {}", head(&text, 500));
        if status.is_success() {
            if v["type"] == "message" {
                return Ok(v);
            }
            return Err(ApiError::Failed(format!(
                "{status}: not a message: {}",
                head(&text, 500)
            )));
        }
        let api_error = v["type"] == "error";
        if status == 429 && !api_error {
            return Err(ApiError::Budget);
        }
        if status == 429 || status.is_server_error() {
            return Err(ApiError::Transient(why, retry_after));
        }
        Err(ApiError::Failed(why))
    }
}

/// The longest Retry-After honoured, so a bad value can't stall the run.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// The first `n` characters of `s`.
pub(crate) fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
