#![allow(dead_code)]
//! A gateway on a local port with the repo's policy, an audit log in a
//! temporary directory and one run.
use axum::Router;
use kod_audit::AuditLog;
use kod_llm_proxy::{
    Config, Endpoint, Limits, app,
    config::{Budget, Price, RunSpec},
};
use kod_policy::Policy;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::TcpListener;

pub const MODEL: &str = "claude-test-1";
pub const TOKEN: &str = "0123456789abcdef0123456789abcdef";
/// The only class and endpoint the placeholder routing allows.
pub const CLASS: &str = "placeholder";
pub const ENDPOINT: &str = "placeholder";
/// 1 micro-USD per million tokens for every field: a token costs 1 picodollar.
pub const PRICE: Price = Price {
    input: 1,
    output: 1,
    cache_write: 1,
    cache_read: 1,
};
pub const PRICES_JSON: &str =
    r#"{"claude-test-1":{"input":1,"output":1,"cache_write":1,"cache_read":1}}"#;
pub const RUNS_JSON: &str = r#"{"0123456789abcdef0123456789abcdef":{"id":"r1","ticket":"KOD-1","repo":"acme/app","data_classes":["placeholder"],"max_tokens":1000000000,"max_cost_micro_usd":1000000}}"#;

pub async fn serve(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

pub fn policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

pub fn limits() -> Limits {
    Limits {
        allowed_models: vec![MODEL.into()],
        max_tokens: 1000,
        max_body: 4096,
        max_requests: 100,
        max_concurrent: 2,
    }
}

/// A large budget, so only the budget tests meet it.
pub fn budget() -> Budget {
    Budget {
        max_tokens: 1_000_000_000,
        max_cost_micro_usd: 1_000_000,
    }
}

pub fn run(classes: &[&str], budget: Budget) -> RunSpec {
    RunSpec {
        id: "r1".into(),
        ticket: "KOD-1".into(),
        repo: "acme/app".into(),
        data_classes: classes.iter().map(|c| c.to_string()).collect(),
        budget,
    }
}

pub struct Gateway {
    /// The run's base URL, ending in /r/<token>.
    pub url: String,
    pub db: PathBuf,
    _dir: tempfile::TempDir,
}

pub struct Setup {
    pub limits: Limits,
    pub run: RunSpec,
    pub endpoint: &'static str,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            limits: limits(),
            run: run(&[CLASS], budget()),
            endpoint: ENDPOINT,
        }
    }
}

pub async fn gateway(upstream: &str, s: Setup) -> Gateway {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    let cfg = Config {
        endpoint: Endpoint {
            name: s.endpoint.into(),
            upstream: upstream.into(),
            api_key: "real-key".parse().unwrap(),
        },
        allowed_paths: vec!["/v1/messages".into(), "/v1/messages/count_tokens".into()],
        limits: s.limits,
        prices: HashMap::from([(MODEL.to_owned(), PRICE)]),
        runs: HashMap::from([(TOKEN.to_owned(), s.run)]),
        policy: Arc::new(Policy::load(&policy_dir()).unwrap()),
        audit: Arc::new(AuditLog::open(&db).unwrap()),
    };
    let root = serve(app(cfg)).await;
    Gateway {
        url: format!("{root}/r/{TOKEN}"),
        db,
        _dir: dir,
    }
}

/// Every audit entry as (record_type, payload), oldest first.
pub fn entries(db: &Path) -> Vec<(String, serde_json::Value)> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut q = c
        .prepare("SELECT record_type, payload FROM audit ORDER BY seq")
        .unwrap();
    q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

pub fn msg(model: &str, max_tokens: u64) -> String {
    format!(r#"{{"model":"{model}","max_tokens":{max_tokens},"messages":[]}}"#)
}

pub async fn post_msg(p: &str, body: String) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{p}/v1/messages"))
        .body(body)
        .send()
        .await
        .unwrap()
}
