//! The load-time gate for the data files, run before Rego sees them. Every
//! file the rules read must be present, and every value the rules read is
//! checked here, so no `data.config` value reaches Rego without passing it.
//! "Membership only" is not safe on its own: `"" == ""` matches, and `in` on
//! an object matches its values.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// Every data file the rules read, by stem.
const REQUIRED: &[&str] = &[
    "compliance",
    "delegation",
    "people",
    "publish",
    "routing",
    "subscription",
    "teams",
];

pub(crate) fn check(data: &Map<String, Value>) -> Result<()> {
    for stem in REQUIRED {
        if !data.contains_key(*stem) {
            bail!("missing {stem}.json");
        }
    }
    delegation(&data["delegation"])?;
    people(&data["people"])?;
    teams(&data["teams"])?;
    compliance(&data["compliance"])?;
    routing(&data["routing"])?;
    subscription(&data["subscription"])?;
    names(
        "publish.json: cleared_classes",
        data["publish"].get("cleared_classes"),
    )
}

/// A non-empty string with no spaces at the ends.
fn name(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s == s.trim())
}

/// A list (not an object: `in` would match its values) of names.
fn names(at: &str, v: Option<&Value>) -> Result<()> {
    if !v
        .and_then(Value::as_array)
        .is_some_and(|l| l.iter().all(name))
    {
        bail!("{at} must be a list of non-empty names with no spaces at the ends");
    }
    Ok(())
}

/// An object whose keys are all in `allowed`.
fn only_keys<'a>(at: &str, v: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>> {
    let o = v
        .as_object()
        .with_context(|| format!("{at} must be an object"))?;
    if let Some(key) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
        bail!("{at}: unexpected key {key}");
    }
    Ok(o)
}

fn routing(r: &Value) -> Result<()> {
    let classes = r
        .get("classes")
        .and_then(Value::as_object)
        .filter(|c| !c.is_empty())
        .context("routing.json: classes must be a non-empty object")?;
    for (class, route) in classes {
        if !canonical_key(class) || class.is_empty() {
            bail!("routing.json: class key {class:?} must be lower-case with no spaces");
        }
        let at = format!("routing.json: classes.{class}");
        let route = only_keys(&at, route, &["endpoints"])?;
        names(&format!("{at}.endpoints"), route.get("endpoints"))?;
    }
    Ok(())
}

fn subscription(s: &Value) -> Result<()> {
    names("subscription.json: contexts", s.get("contexts"))?;
    names("subscription.json: classes", s.get("classes"))?;
    let runners = s
        .get("runners")
        .and_then(Value::as_object)
        .context("subscription.json: runners must be an object")?;
    for (runner, seat) in runners {
        let at = format!("subscription.json: runners.{runner}");
        let seat = only_keys(&at, seat, &["plans", "workspace"])?;
        names(&format!("{at}.plans"), seat.get("plans"))?;
        // Absent or null: no workspace recorded yet, so the rules refuse the runner.
        if !seat.get("workspace").is_none_or(|w| w.is_null() || name(w)) {
            bail!("{at}.workspace must be null or a name");
        }
    }
    Ok(())
}

/// The same normalisation as `path_segments` in policy/delegation.rego
/// (trimmed, lower-cased, `\` read as `/`, empty and `.` segments dropped),
/// keeping a trailing `/`.
fn normalize_path(p: &str) -> String {
    let p = p.trim().to_lowercase().replace('\\', "/");
    let mut out = p
        .split('/')
        .filter(|s| !matches!(*s, "" | "."))
        .collect::<Vec<_>>()
        .join("/");
    if p.ends_with('/') && !out.is_empty() {
        out.push('/');
    }
    out
}

/// A key the rules look up after lower-casing and trimming the caller's value.
fn canonical_key(k: &str) -> bool {
    k == k.trim().to_lowercase()
}

/// `owner/name`: lower-case, one `/`, both parts non-empty, no whitespace.
fn repo_key(k: &str) -> bool {
    canonical_key(k)
        && !k.contains(char::is_whitespace)
        && k.split_once('/')
            .is_some_and(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
}

fn login(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && !s.contains(char::is_whitespace))
}

fn delegation(d: &Value) -> Result<()> {
    let classes = d
        .get("risk_classes")
        .and_then(Value::as_object)
        .filter(|c| !c.is_empty())
        .context("delegation.json: risk_classes must be a non-empty object")?;
    let mut ranks = BTreeSet::new();
    for (class, rank) in classes {
        if !rank.as_u64().is_some_and(|r| ranks.insert(r)) {
            bail!(
                "delegation.json: risk_classes.{class} must be a rank no other class has (0, 1, 2, ...)"
            );
        }
    }
    let allowed = d
        .get("allowed_classes")
        .and_then(Value::as_array)
        .context("delegation.json: allowed_classes must be a list")?;
    for c in allowed {
        if !c.as_str().is_some_and(|c| classes.contains_key(c)) {
            bail!("delegation.json: allowed_classes entry {c} is not in risk_classes");
        }
    }
    for key in ["wip_limit", "bounce_limit"] {
        if d.get(key).and_then(Value::as_u64).is_none() {
            bail!("delegation.json: {key} must be a non-negative integer");
        }
    }

    let check = |at: &str, rules: Option<&Value>| -> Result<usize> {
        let rules = rules
            .and_then(Value::as_array)
            .with_context(|| format!("delegation.json: {at} must be a list of rules"))?;
        for (i, rule) in rules.iter().enumerate() {
            let at = format!("delegation.json: {at}[{i}]");
            let rule = rule
                .as_object()
                .with_context(|| format!("{at}: a rule must be an object"))?;
            if let Some(key) = rule
                .keys()
                .find(|k| !matches!(k.as_str(), "prefix" | "class"))
            {
                bail!("{at}: unexpected key {key}");
            }
            let prefix = rule
                .get("prefix")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .with_context(|| format!("{at}: prefix must be a non-empty string"))?;
            // Rego matches normalised paths, so any other prefix would never match.
            if prefix != normalize_path(prefix) || prefix.split('/').any(|s| s == "..") {
                bail!(
                    "{at}: prefix {prefix:?} is not a normalised path: lower-case, relative, no \\, no doubled /, no . or .. segment, no spaces at the ends"
                );
            }
            let class = rule.get("class").and_then(Value::as_str);
            if !class.is_some_and(|c| classes.contains_key(c)) {
                bail!("{at}: class must be one of risk_classes");
            }
        }
        Ok(rules.len())
    };
    if check("default_path_rules", d.get("default_path_rules"))? == 0 {
        bail!("delegation.json: default_path_rules must not be empty");
    }
    let repos = d
        .get("repo_path_rules")
        .and_then(Value::as_object)
        .context("delegation.json: repo_path_rules must be an object")?;
    for (repo, rules) in repos {
        if !repo_key(repo) {
            bail!(
                "delegation.json: repo_path_rules key {repo:?} must be lower-case owner/name with no spaces"
            );
        }
        check(&format!("repo_path_rules.{repo}"), Some(rules))?;
    }
    Ok(())
}

fn people(p: &Value) -> Result<()> {
    if !p.get("owner").is_some_and(login) {
        bail!("people.json: owner must be a login");
    }
    if !p.get("backup").is_some_and(|b| b.is_null() || login(b)) {
        bail!("people.json: backup must be null or a login");
    }
    let leads = p.get("tech_leads").and_then(Value::as_array);
    if !leads.is_some_and(|l| l.iter().all(login)) {
        bail!("people.json: tech_leads must be a list of logins");
    }
    Ok(())
}

fn teams(t: &Value) -> Result<()> {
    let teams = t
        .as_object()
        .context("teams.json must be an object of teams")?;
    for (team, setting) in teams {
        if !canonical_key(team) {
            bail!("teams.json: team key {team:?} must be lower-case with no spaces");
        }
        let known = setting.as_object().is_some_and(|s| {
            s.len() == 1
                && s.get("requester_approval")
                    .and_then(Value::as_str)
                    .is_some_and(|r| matches!(r, "allowed" | "not_sole" | "forbidden"))
        });
        if !known {
            bail!(
                "teams.json: {team} must be {{\"requester_approval\": \"allowed\" | \"not_sole\" | \"forbidden\"}}"
            );
        }
    }
    Ok(())
}

fn compliance(c: &Value) -> Result<()> {
    let profiles = c
        .get("profiles")
        .and_then(Value::as_object)
        .context("compliance.json: profiles must be an object")?;
    for (name, profile) in profiles {
        let profile = profile
            .as_object()
            .with_context(|| format!("compliance.json: profile {name} must be an object"))?;
        // A profile may still be a placeholder; a missing period denies in Rego.
        let Some(days) = profile.get("retention_days") else {
            continue;
        };
        let days = days
            .as_object()
            .with_context(|| format!("compliance.json: {name}.retention_days must be an object"))?;
        for (record, d) in days {
            if !d.as_f64().is_some_and(|d| d >= 0.0) {
                bail!(
                    "compliance.json: {name}.retention_days.{record} must be a non-negative number"
                );
            }
        }
    }
    Ok(())
}
