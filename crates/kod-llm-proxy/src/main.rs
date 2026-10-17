use anyhow::{Context, Result, bail};
use kod_audit::AuditLog;
use kod_llm_proxy::{
    Config, DEFAULT_ALLOWED_PATHS, DEFAULT_MAX_BODY, DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_REQUESTS,
    DEFAULT_MAX_TOKENS, Endpoint, Limits, app, config,
};
use kod_policy::Policy;
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let env = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_owned());
    let required = |k: &str| std::env::var(k).with_context(|| format!("{k} is required"));
    let num = |k: &str, default: u64| -> Result<u64> {
        std::env::var(k).map_or(Ok(default), |v| config::parse_limit(k, &v))
    };
    let list = |s: String| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let read = |k: &str| -> Result<String> {
        let path = PathBuf::from(required(k)?);
        std::fs::read_to_string(&path).with_context(|| format!("{k}: reading {}", path.display()))
    };
    let allowed_models = list(required("KOD_ALLOWED_MODELS")?);
    if allowed_models.is_empty() {
        bail!("KOD_ALLOWED_MODELS is empty");
    }
    let prices = config::parse_prices(&read("KOD_PRICES")?, &allowed_models)?;
    let runs = config::parse_runs(&read("KOD_RUNS")?)?;
    let policy =
        Policy::load(&PathBuf::from(required("KOD_POLICY_DIR")?)).context("loading the policy")?;
    let audit = AuditLog::open(&PathBuf::from(required("KOD_AUDIT_DB")?))
        .context("opening the audit log")?;
    // No defaults: the endpoint name the policy allows and the URL it stands
    // for are set together, by whoever starts the gateway.
    let name = required("KOD_ENDPOINT")?;
    if name.is_empty() || name != name.trim() {
        bail!("KOD_ENDPOINT must be a non-empty name with no spaces at the ends");
    }
    let cfg = Config {
        endpoint: Endpoint {
            name,
            upstream: required("KOD_UPSTREAM")?.trim_end_matches('/').to_owned(),
            api_key: required("KOD_API_KEY")?
                .parse()
                .context("KOD_API_KEY is not a valid header value")?,
        },
        allowed_paths: config::parse_paths(&env("KOD_ALLOWED_PATHS", DEFAULT_ALLOWED_PATHS))?,
        limits: Limits {
            allowed_models,
            max_tokens: num("KOD_MAX_TOKENS", DEFAULT_MAX_TOKENS)?,
            max_body: num("KOD_MAX_BODY", DEFAULT_MAX_BODY)? as usize,
            max_requests: num("KOD_MAX_REQUESTS", DEFAULT_MAX_REQUESTS)?,
            max_concurrent: num("KOD_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT)? as usize,
        },
        prices,
        runs,
        policy: Arc::new(policy),
        audit: Arc::new(audit),
    };
    let listen = env("KOD_LISTEN", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    // kod-eval waits for this line: the config loaded and the port is open.
    println!(
        "{}",
        serde_json::json!({"event": "ready", "listen": listen})
    );
    axum::serve(listener, app(cfg)).await?;
    Ok(())
}
