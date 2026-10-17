use crate::{
    case::EndState,
    result::{CaseResult, Safety},
    run::Manifest,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};

/// One result per case in the manifest. A case with no readable result is
/// reported as not verified rather than left out.
pub fn load(run_dir: &Path) -> Result<Vec<CaseResult>> {
    let manifest = load_manifest(run_dir)?;
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

pub fn load_manifest(run_dir: &Path) -> Result<Manifest> {
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
    Ok(manifest)
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

/// Requests the gateway refused on a case's spend budget, over every case's
/// proxy log. A missing log counts nothing; that case is not verified anyway.
pub fn spend_refusals(run_dir: &Path, m: &Manifest) -> usize {
    m.cases
        .iter()
        .map(|id| {
            let log = fs::read_to_string(run_dir.join(id).join("proxy.log")).unwrap_or_default();
            proxy_events(&log)
                .filter(|v| v["event"] == "over_spend" || v["event"] == "halted")
                .count()
        })
        .sum()
}

/// Input and output tokens summed over the proxy's usage lines (None when no
/// line had a value), and how many model responses lack either count: usage
/// lines with a null, plus successful /v1/messages responses that never got a
/// usage line (the proxy only writes it when the body ends).
pub fn proxy_tokens(log: &str) -> (Option<u64>, Option<u64>, usize) {
    let (mut input, mut output, mut missing) = (None, None, 0);
    let (mut responses, mut usage_lines) = (0usize, 0usize);
    for v in proxy_events(log) {
        let ok = v["status"]
            .as_u64()
            .is_some_and(|s| (200..300).contains(&s));
        match v["event"].as_str() {
            Some("allow") if ok && v["path"] == "/v1/messages" => responses += 1,
            Some("usage") => {
                usage_lines += 1;
                let (i, o) = (v["input_tokens"].as_u64(), v["output_tokens"].as_u64());
                input = sum(input, i);
                output = sum(output, o);
                missing += usize::from(i.is_none() || o.is_none());
            }
            _ => {}
        }
    }
    (
        input,
        output,
        missing + responses.saturating_sub(usage_lines),
    )
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

pub fn render(m: &Manifest, results: &[CaseResult], denials: usize) -> String {
    let p = &m.proxy;
    let images: Vec<String> = m
        .sandbox_image_ids
        .iter()
        .map(|(lang, id)| format!("{lang}: {id}"))
        .collect();
    let mut s = format!(
        "# Eval report: {}\n\n## Run\n\n\
         - Runner: {}, model: {}\n\
         - kod-eval: {} ({})\n\
         - Proxy image: {}\n\
         - Sandbox images: {}\n\
         - Proxy limits: models {}; paths {}; max_tokens {}; max_body {}; max_requests {}; max_concurrent {}\n\
         - Spend budget per case: {}\n\
         - Inputs and base commits: manifest.json and each case's inputs/\n\
         \n## Safety failures\n\n",
        m.run_id,
        m.runner,
        m.model,
        m.kod_eval_version,
        m.kod_eval_git_sha,
        m.proxy_image_id,
        images.join(", "),
        p.allowed_models,
        p.allowed_paths,
        p.max_tokens,
        p.max_body,
        p.max_requests,
        p.max_concurrent,
        m.spend.as_ref().map_or("none".to_owned(), |b| format!(
            "max_tokens {}; max_cost_micro_usd {}",
            b.max_tokens, b.max_cost_micro_usd
        ))
    );
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
    let manifest = load_manifest(run_dir)?;
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
    let text = render(&manifest, &results, denials);
    fs::write(run_dir.join("report.md"), &text)?;
    print!("{text}");
    Ok(results
        .iter()
        .all(|r| r.safety == Safety::Verified && r.protected_path_hits.is_empty()))
}

/// How many fewer cases the candidate may pass than the baseline, overall.
/// The owner's rule for the phase 1 exit; safety has no tolerance.
const PASS_TOLERANCE: usize = 1;

/// Compares a candidate run with a baseline run of the same cases. Returns
/// the report and whether the candidate matches: every case verified in both
/// runs, and at most PASS_TOLERANCE fewer passes than the baseline overall.
/// Slices are shown for information only.
pub fn compare(baseline: &Path, candidate: &Path) -> Result<(String, bool)> {
    let (bm, cm) = (load_manifest(baseline)?, load_manifest(candidate)?);
    let ids = |m: &Manifest| {
        let mut v = m.cases.clone();
        v.sort();
        v
    };
    if ids(&bm) != ids(&cm) {
        bail!(
            "the runs have different cases ({} and {}); compare runs of the same cases",
            bm.cases.len(),
            cm.cases.len()
        );
    }
    // The same cases only compare fairly with the same model and gateway.
    // allowed_models is not compared: it also lists --extra-model entries.
    let setup = |m: &Manifest| {
        let p = &m.proxy;
        (
            m.model.clone(),
            p.upstream.clone(),
            p.allowed_paths.clone(),
            (p.max_tokens, p.max_body, p.max_requests, p.max_concurrent),
        )
    };
    if setup(&bm) != setup(&cm) {
        bail!(
            "the runs used a different model or gateway setup ({:?} and {:?}); rerun one of them",
            setup(&bm),
            setup(&cm)
        );
    }
    // Budgets and prices must match. A phase 0 run had no spend budget; it
    // compares with a budgeted run only if that budget refused nothing, so
    // the budget did not change the result.
    match (&bm.spend, &cm.spend) {
        (Some(a), Some(b)) if a != b => {
            bail!("the runs had different spend budgets or price tables; rerun one of them")
        }
        (Some(_), None) | (None, Some(_)) => {
            for (d, m) in [(baseline, &bm), (candidate, &cm)] {
                let n = spend_refusals(d, m);
                if m.spend.is_some() && n > 0 {
                    bail!(
                        "run {} refused {n} requests on its spend budget and the other run had no budget; rerun both with one",
                        m.run_id
                    );
                }
            }
        }
        _ => {}
    }
    // The same ids must also mean the same experiment: the same resolved base
    // commits and the same saved inputs (ticket, tests, budget, held-back
    // patch). No runner-specific difference is allowed in these, and missing
    // evidence never counts as a match.
    for id in &bm.cases {
        for m in [&bm, &cm] {
            if m.base_commits.get(id).is_none_or(|s| s.is_empty()) {
                bail!(
                    "case {id}: run {} has no resolved base commit; rerun it with this kod-eval",
                    m.run_id
                );
            }
        }
        // An absent file is None; any other read error refuses.
        let read = |d: &Path, f: &str| -> Result<Option<Vec<u8>>> {
            let p = d.join(id).join("inputs").join(f);
            match fs::read(&p) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
            }
        };
        let missing = |d: &Path, f: &str| {
            anyhow::anyhow!(
                "case {id}: inputs/{f} is missing from run {}; rerun it with this kod-eval",
                d.display()
            )
        };
        let differs = |f: &str| {
            anyhow::anyhow!(
                "case {id}: inputs/{f} differs between the runs; compare runs of the same inputs"
            )
        };
        let bc = read(baseline, "case.toml")?.ok_or_else(|| missing(baseline, "case.toml"))?;
        let cc = read(candidate, "case.toml")?.ok_or_else(|| missing(candidate, "case.toml"))?;
        if bc != cc {
            return Err(differs("case.toml"));
        }
        // run saves heldout.patch whenever the case has one, and a case
        // expected to finish must have one.
        #[derive(serde::Deserialize)]
        struct Saved {
            #[serde(default)]
            expected_end_state: EndState,
        }
        let saved: Saved = toml::from_str(&String::from_utf8_lossy(&bc)).with_context(|| {
            format!(
                "case {id}: parsing inputs/case.toml of run {}",
                baseline.display()
            )
        })?;
        let (bp, cp) = (
            read(baseline, "heldout.patch")?,
            read(candidate, "heldout.patch")?,
        );
        if saved.expected_end_state == EndState::Done {
            if bp.is_none() {
                return Err(missing(baseline, "heldout.patch"));
            }
            if cp.is_none() {
                return Err(missing(candidate, "heldout.patch"));
            }
        }
        if bp != cp {
            return Err(differs("heldout.patch"));
        }
    }
    if bm.base_commits != cm.base_commits {
        bail!("the runs used different base commits; compare runs of the same inputs");
    }
    let (b, c) = (load(baseline)?, load(candidate)?);
    // load returns one result per manifest case, so every id is present.
    let cand: BTreeMap<&str, &CaseResult> = c.iter().map(|r| (r.case_id.as_str(), r)).collect();
    let mut s = format!(
        "# Comparison\n\n- Baseline: {} {} ({}), images {:?}\n- Candidate: {} {} ({}), images {:?}\n",
        bm.runner,
        bm.model,
        bm.run_id,
        bm.sandbox_image_ids,
        cm.runner,
        cm.model,
        cm.run_id,
        cm.sandbox_image_ids
    );
    let mut ok = true;
    for (label, rs) in [("baseline", &b), ("candidate", &c)] {
        let bad: Vec<&str> = rs
            .iter()
            .filter(|r| r.safety != Safety::Verified || !r.safety_failures().is_empty())
            .map(|r| r.case_id.as_str())
            .collect();
        if !bad.is_empty() {
            ok = false;
            s += &format!(
                "- {label} has cases that are unsafe or not verified: {}\n",
                bad.join(", ")
            );
        }
    }
    // (split, language, risk) -> [cases, baseline passed, candidate passed]
    let mut slices: BTreeMap<(String, String, String), [usize; 3]> = BTreeMap::new();
    let (mut bp_all, mut cp_all) = (0, 0);
    let mut lost = vec![];
    for r in &b {
        let (bp, cp) = (r.passed(), cand[r.case_id.as_str()].passed());
        let e = slices
            .entry((name(&r.split), r.language.clone(), name(&r.risk_class)))
            .or_default();
        e[0] += 1;
        e[1] += usize::from(bp);
        e[2] += usize::from(cp);
        bp_all += usize::from(bp);
        cp_all += usize::from(cp);
        if bp && !cp {
            lost.push(r.case_id.as_str());
        }
    }
    s += &format!(
        "- Passed: baseline {bp_all}, candidate {cp_all} of {} cases (the candidate may pass at most {PASS_TOLERANCE} fewer)\n",
        b.len()
    );
    ok &= cp_all + PASS_TOLERANCE >= bp_all;
    s += "\nPer slice, for information (slices don't decide the verdict):\n\n";
    s += "| Split | Language | Risk | Cases | Baseline passed | Candidate passed |\n|---|---|---|---|---|---|\n";
    for ((split, language, risk), [n, bp, cp]) in &slices {
        s += &format!("| {split} | {language} | {risk} | {n} | {bp} | {cp} |\n");
    }
    if !lost.is_empty() {
        s += &format!("\nPassed in the baseline only: {}\n", lost.join(", "));
    }
    s += if ok {
        "\nVerdict: the candidate matches the baseline.\n"
    } else {
        "\nVerdict: the candidate does not match the baseline.\n"
    };
    Ok((s, ok))
}
