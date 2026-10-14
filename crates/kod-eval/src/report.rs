use crate::{
    result::{CaseResult, Safety},
    run::Manifest,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};

/// One result per case in the manifest. A case with no readable result is
/// reported as not verified rather than left out.
pub fn load(run_dir: &Path) -> Result<Vec<CaseResult>> {
    let mp = run_dir.join("manifest.json");
    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(&mp).with_context(|| format!("reading {}", mp.display()))?,
    )
    .with_context(|| format!("parsing {}", mp.display()))?;
    anyhow::ensure!(
        !manifest.cases.is_empty(),
        "{} selects no cases",
        mp.display()
    );
    let mut out: Vec<CaseResult> = manifest
        .cases
        .iter()
        .map(|id| {
            let p = run_dir.join(id).join("result.json");
            fs::read_to_string(&p)
                .map_err(anyhow::Error::from)
                .and_then(|t| Ok(serde_json::from_str::<CaseResult>(&t)?))
                .unwrap_or_else(|e| CaseResult {
                    case_id: id.clone(),
                    notes: vec![format!("no readable result: {e}")],
                    ..Default::default()
                })
        })
        .collect();
    out.sort_by(|a, b| a.case_id.cmp(&b.case_id));
    Ok(out)
}

fn proxy_events(log: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    log.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
}

/// Every proxy event that refused a request.
pub fn proxy_denials(log: &str) -> usize {
    proxy_events(log)
        .filter(|v| {
            v["event"]
                .as_str()
                .is_some_and(|e| e != "allow" && e != "usage")
        })
        .count()
}

/// Input and output tokens summed over the proxy's usage lines (None when no
/// line had a value), and how many usage lines lacked either count.
pub fn proxy_tokens(log: &str) -> (Option<u64>, Option<u64>, usize) {
    let (mut input, mut output, mut missing) = (None, None, 0);
    for v in proxy_events(log).filter(|v| v["event"] == "usage") {
        let (i, o) = (v["input_tokens"].as_u64(), v["output_tokens"].as_u64());
        input = sum(input, i);
        output = sum(output, o);
        missing += usize::from(i.is_none() || o.is_none());
    }
    (input, output, missing)
}

/// Adds what is known; None only when nothing is.
fn sum<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

pub fn median(mut v: Vec<u64>) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// The serde name, e.g. "heldout" or "over_budget".
fn name<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn or_dash(v: Option<u64>, f: impl Fn(u64) -> String) -> String {
    v.map_or("-".into(), f)
}

pub fn render(title: &str, results: &[CaseResult], denials: usize) -> String {
    let mut s = format!("# Eval report: {title}\n\n## Safety failures\n\n");
    let safety: Vec<String> = results
        .iter()
        .flat_map(|r| {
            let mut f = r.safety_failures();
            if f.is_empty() && r.safety == Safety::Unsafe {
                f.push(format!("{}: marked unsafe", r.case_id));
            }
            f
        })
        .collect();
    if safety.is_empty() {
        s.push_str("None.\n");
    }
    for f in &safety {
        s.push_str(&format!("- {f}\n"));
    }

    s.push_str("\n## Not verified\n\nThese cases have no safety evidence, so the run can't count as clean.\n\n");
    let unverified: Vec<&CaseResult> = results
        .iter()
        .filter(|r| r.safety == Safety::NotVerified)
        .collect();
    if unverified.is_empty() {
        s.push_str("None.\n");
    }
    for r in unverified {
        s.push_str(&format!("- {}: {}\n", r.case_id, r.notes.join("; ")));
    }

    s.push_str("\n## By slice\n\n| Split | Language | Risk | Passed | Cases | Median wall (s) | Median peak memory (MiB) | Median CPU (s) | Runner-reported cost (USD, subtotal) | Cases without runner cost | Proxy input tokens | Proxy output tokens |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut slices: BTreeMap<_, Vec<&CaseResult>> = BTreeMap::new();
    for r in results {
        slices
            .entry((r.split, r.language.clone(), r.risk_class))
            .or_default()
            .push(r);
    }
    for ((split, language, risk), rs) in &slices {
        let passed = rs.iter().filter(|r| r.passed()).count();
        let wall = or_dash(median(rs.iter().map(|r| r.agent_wall_ms).collect()), |v| {
            format!("{:.0}", v as f64 / 1e3)
        });
        let mem = or_dash(
            median(
                rs.iter()
                    .filter_map(|r| r.agent_peak_memory_bytes)
                    .collect(),
            ),
            |v| (v >> 20).to_string(),
        );
        let cpu = or_dash(
            median(rs.iter().filter_map(|r| r.agent_cpu_usec).collect()),
            |v| format!("{:.0}", v as f64 / 1e6),
        );
        let na = |v: Option<String>| v.unwrap_or_else(|| "n/a".into());
        let cost = na(rs
            .iter()
            .map(|r| r.cost_usd)
            .fold(None, sum)
            .map(|c| format!("{c:.2}")));
        let no_cost = rs.iter().filter(|r| r.cost_usd.is_none()).count();
        let tokens = |f: fn(&CaseResult) -> Option<u64>| {
            na(rs
                .iter()
                .map(|r| f(r))
                .fold(None, sum)
                .map(|t| t.to_string()))
        };
        let (input, output) = (
            tokens(|r| r.proxy_input_tokens),
            tokens(|r| r.proxy_output_tokens),
        );
        s.push_str(&format!(
            "| {} | {language} | {} | {passed} | {} | {wall} | {mem} | {cpu} | {cost} | {no_cost} | {input} | {output} |\n",
            name(split),
            name(risk),
            rs.len()
        ));
    }

    s.push_str("\nCost is the subtotal of what the runner reported, over the cases that reported one. Proxy tokens are summed from each case's proxy.log and are not converted to dollars in phase 0.\n");
    s.push_str(&format!(
        "\n## Cases\n\nProxy refusals in this run: {denials}\n\n| Case | Expected | Got | Safety | Tests | Passed | Notes |\n|---|---|---|---|---|---|---|\n"
    ));
    for r in results {
        let tests = match r.tests_passed {
            Some(true) => "pass",
            Some(false) => "fail",
            None => "-",
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} | {tests} | {} | {} |\n",
            r.case_id,
            name(&r.expected_end_state),
            name(&r.end_state),
            name(&r.safety),
            if r.passed() { "yes" } else { "no" },
            r.notes.join("; ").replace('|', "\\|")
        ));
    }
    s
}

/// Writes report.md into the run directory and prints it. Returns false when
/// any case is unsafe or not verified.
pub fn write(run_dir: &Path) -> Result<bool> {
    let results = load(run_dir)?;
    // A missing proxy log already made that case not verified.
    let denials = results
        .iter()
        .map(|r| {
            proxy_denials(
                &fs::read_to_string(run_dir.join(&r.case_id).join("proxy.log")).unwrap_or_default(),
            )
        })
        .sum();
    let title = run_dir
        .file_name()
        .map_or("run".into(), |n| n.to_string_lossy().into_owned());
    let text = render(&title, &results, denials);
    fs::write(run_dir.join("report.md"), &text)?;
    print!("{text}");
    Ok(results
        .iter()
        .all(|r| r.safety == Safety::Verified && r.protected_path_hits.is_empty()))
}
