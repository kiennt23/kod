//! The gateway's data files, checked at load the way kod-policy's config.rs
//! checks policy data: every value is validated before it is used, an
//! unexpected or missing key is an error, and nothing has a default. A file
//! that fails any check stops the gateway from starting.
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// Rates in micro-USD per million tokens, one per usage field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

/// Model id to price.
pub type Prices = HashMap<String, Price>;

/// What one run may spend. Tokens count all four usage fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub max_tokens: u64,
    pub max_cost_micro_usd: u64,
}

/// One run the gateway serves, keyed by its token.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSpec {
    pub id: String,
    pub ticket: String,
    pub repo: String,
    pub data_classes: Vec<String>,
    pub budget: Budget,
}

/// Run tokens are at least this many lower-case hex characters.
pub const MIN_TOKEN_LEN: usize = 32;

/// A budget above this many micro-USD would overflow when held in picodollars.
pub const MAX_COST_MICRO_USD: u64 = u64::MAX / 1_000_000;

/// A non-empty string with no spaces at the ends.
fn name(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s == s.trim())
}

/// An object with exactly the keys in `keys`.
fn exact_keys<'a>(at: &str, v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let o = v
        .as_object()
        .with_context(|| format!("{at} must be an object"))?;
    if let Some(key) = o.keys().find(|k| !keys.contains(&k.as_str())) {
        bail!("{at}: unexpected key {key}");
    }
    if let Some(key) = keys.iter().find(|k| !o.contains_key(**k)) {
        bail!("{at}: missing key {key}");
    }
    Ok(o)
}

fn int(at: &str, o: &Map<String, Value>, key: &str) -> Result<u64> {
    o[key]
        .as_u64()
        .with_context(|| format!("{at}.{key} must be a whole number"))
}

fn field(at: &str, o: &Map<String, Value>, key: &str) -> Result<String> {
    if !name(&o[key]) {
        bail!("{at}.{key} must be a non-empty name with no spaces at the ends");
    }
    Ok(o[key].as_str().unwrap_or_default().to_owned())
}

fn parse(file: &str, text: &str) -> Result<Map<String, Value>> {
    let v: Value = serde_json::from_str(text).with_context(|| format!("{file}: not JSON"))?;
    match v {
        Value::Object(o) if !o.is_empty() => Ok(o),
        _ => bail!("{file} must be a non-empty object"),
    }
}

/// prices.json: `{"<model>": {"input", "output", "cache_write", "cache_read"}}`.
/// Every allowed model needs a price, so no request is ever unpriced.
pub fn parse_prices(text: &str, allowed_models: &[String]) -> Result<Prices> {
    let mut prices = Prices::new();
    for (model, v) in parse("prices.json", text)? {
        let at = format!("prices.json: {model}");
        if !name(&Value::String(model.clone())) {
            bail!("{at}: the model id must have no spaces at the ends");
        }
        let o = exact_keys(&at, &v, &["input", "output", "cache_write", "cache_read"])?;
        let price = Price {
            input: int(&at, o, "input")?,
            output: int(&at, o, "output")?,
            cache_write: int(&at, o, "cache_write")?,
            cache_read: int(&at, o, "cache_read")?,
        };
        prices.insert(model, price);
    }
    if let Some(m) = allowed_models.iter().find(|m| !prices.contains_key(*m)) {
        bail!("prices.json: no price for allowed model {m}");
    }
    Ok(prices)
}

/// runs.json: `{"<token>": {"id", "ticket", "repo", "data_classes",
/// "max_tokens", "max_cost_micro_usd"}}`. Tokens and run ids are unique.
pub fn parse_runs(text: &str) -> Result<HashMap<String, RunSpec>> {
    let mut runs = HashMap::new();
    let mut ids = HashSet::new();
    for (token, v) in parse("runs.json", text)? {
        if token.len() < MIN_TOKEN_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!(
                "runs.json: a run token must be at least {MIN_TOKEN_LEN} lower-case hex characters"
            );
        }
        let o = exact_keys(
            "runs.json: a run",
            &v,
            &[
                "id",
                "ticket",
                "repo",
                "data_classes",
                "max_tokens",
                "max_cost_micro_usd",
            ],
        )?;
        let id = field("runs.json: a run", o, "id")?;
        let at = format!("runs.json: run {id}");
        if !ids.insert(id.clone()) {
            bail!("{at}: the id is used twice");
        }
        let classes = o["data_classes"]
            .as_array()
            .filter(|l| !l.is_empty() && l.iter().all(name))
            .with_context(|| format!("{at}.data_classes must be a non-empty list of names"))?;
        let budget = Budget {
            max_tokens: int(&at, o, "max_tokens")?,
            max_cost_micro_usd: int(&at, o, "max_cost_micro_usd")?,
        };
        if budget.max_tokens == 0 || budget.max_cost_micro_usd == 0 {
            bail!("{at}: max_tokens and max_cost_micro_usd must be above zero");
        }
        if budget.max_cost_micro_usd > MAX_COST_MICRO_USD {
            bail!("{at}.max_cost_micro_usd must be at most {MAX_COST_MICRO_USD}");
        }
        let spec = RunSpec {
            ticket: field(&at, o, "ticket")?,
            repo: field(&at, o, "repo")?,
            data_classes: classes
                .iter()
                .map(|c| c.as_str().unwrap_or_default().to_owned())
                .collect(),
            budget,
            id,
        };
        runs.insert(token, spec);
    }
    Ok(runs)
}

/// The only paths the gateway can forward. Both are priced or free:
/// /v1/messages reserves its worst case and count_tokens costs nothing. Any
/// other path (batches, files) could spend outside the budget.
pub const PATHS: [&str; 2] = ["/v1/messages", "/v1/messages/count_tokens"];

/// KOD_ALLOWED_PATHS: a comma-separated, non-empty subset of PATHS.
pub fn parse_paths(text: &str) -> Result<Vec<String>> {
    let paths: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        bail!("KOD_ALLOWED_PATHS is empty");
    }
    if let Some(p) = paths.iter().find(|p| !PATHS.contains(&p.as_str())) {
        bail!("KOD_ALLOWED_PATHS: {p} is not one of {}", PATHS.join(", "));
    }
    Ok(paths)
}

/// A KOD_MAX_* limit: a whole number above zero. Zero would refuse every
/// request, or with KOD_MAX_CONCURRENT make every request wait forever.
pub fn parse_limit(key: &str, text: &str) -> Result<u64> {
    match text.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!("{key} must be a whole number above zero"),
    }
}
