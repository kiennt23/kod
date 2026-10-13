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

/// Every proxy event that refused a request.
pub fn proxy_denials(log: &str) -> usize {
    log.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event"].as_str().is_some_and(|e| e != "allow"))
        .count()
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

    s.push_str("\n## By slice\n\n| Split | Language | Risk | Passed | Cases | Median wall (s) | Median peak memory (MiB) | Median CPU (s) | Cost (USD) |\n|---|---|---|---|---|---|---|---|---|\n");
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
        let cost: f64 = rs.iter().filter_map(|r| r.cost_usd).sum();
        s.push_str(&format!(
            "| {} | {language} | {} | {passed} | {} | {wall} | {mem} | {cpu} | {cost:.2} |\n",
            name(split),
            name(risk),
            rs.len()
        ));
    }

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
