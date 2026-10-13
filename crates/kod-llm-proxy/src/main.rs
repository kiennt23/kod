use anyhow::{Context, Result, bail};
use kod_llm_proxy::{Config, Limits, app};

#[tokio::main]
async fn main() -> Result<()> {
    let env = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_owned());
    let num = |k: &str, default: &str| -> Result<u64> {
        env(k, default)
            .parse()
            .with_context(|| format!("{k} must be a number"))
    };
    let list = |s: String| -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let allowed_models =
        list(std::env::var("KOD_ALLOWED_MODELS").context("KOD_ALLOWED_MODELS is required")?);
    if allowed_models.is_empty() {
        bail!("KOD_ALLOWED_MODELS is empty");
    }
    let cfg = Config {
        upstream: env("KOD_UPSTREAM", "https://api.anthropic.com")
            .trim_end_matches('/')
            .to_owned(),
        api_key: std::env::var("KOD_API_KEY")
            .context("KOD_API_KEY is required")?
            .parse()
            .context("KOD_API_KEY is not a valid header value")?,
        allowed_paths: list(env(
            "KOD_ALLOWED_PATHS",
            "/v1/messages,/v1/messages/count_tokens",
        )),
        limits: Limits {
            allowed_models,
            max_tokens: num("KOD_MAX_TOKENS", "8192")?,
            max_body: num("KOD_MAX_BODY", "4194304")? as usize,
            max_requests: num("KOD_MAX_REQUESTS", "200")?,
            max_concurrent: num("KOD_MAX_CONCURRENT", "2")? as usize,
        },
    };
    let listen = env("KOD_LISTEN", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    axum::serve(listener, app(cfg)).await?;
    Ok(())
}
