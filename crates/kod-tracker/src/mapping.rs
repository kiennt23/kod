//! A tracker's mapping, from its registry file: workflow states to standard
//! states, custom fields to requirement ID and risk class, the trigger label
//! and the team group. Checked whole at load; any fault refuses the file.
use crate::State;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::{collections::HashMap, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    pub tracker: String,
    /// Workflow state to standard state.
    standard: HashMap<String, State>,
    /// Standard state to the workflow state `transition` moves to.
    target: HashMap<State, String>,
    /// The custom field holding the requirement ID.
    pub requirement_id_field: String,
    pub risk_class_field: String,
    pub trigger_label: String,
    pub team_group: String,
}

impl Mapping {
    /// Reads and checks a mapping file; errors name the file.
    pub fn load(path: &Path) -> Result<Mapping> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Mapping::parse(&path.display().to_string(), &text)
    }

    /// Checks a mapping. `at` names its source in errors.
    pub fn parse(at: &str, text: &str) -> Result<Mapping> {
        let v = crate::strict_json::parse(text).with_context(|| format!("{at}: not JSON"))?;
        let o = exact_keys(
            at,
            &v,
            &["tracker", "states", "fields", "trigger_label", "team_group"],
        )?;
        let names: Vec<&str> = State::ALL.iter().map(|s| s.name()).collect();
        let states = exact_keys(&format!("{at}: states"), &o["states"], &names)?;
        let mut standard = HashMap::new();
        let mut target = HashMap::new();
        for s in State::ALL {
            let at = format!("{at}: states.{}", s.name());
            let list = states[s.name()]
                .as_array()
                .filter(|l| !l.is_empty())
                .with_context(|| format!("{at} must be a non-empty list of workflow states"))?;
            for w in list {
                let w = name(&at, w)?;
                if standard.insert(w.clone(), s).is_some() {
                    bail!("{at}: workflow state {w:?} is mapped twice");
                }
                // The first one listed is where `transition` moves to.
                target.entry(s).or_insert(w);
            }
        }
        let fields = exact_keys(
            &format!("{at}: fields"),
            &o["fields"],
            &["requirement_id", "risk_class"],
        )?;
        let requirement_id_field = name(
            &format!("{at}: fields.requirement_id"),
            &fields["requirement_id"],
        )?;
        let risk_class_field = name(&format!("{at}: fields.risk_class"), &fields["risk_class"])?;
        if requirement_id_field == risk_class_field {
            bail!("{at}: fields.requirement_id and fields.risk_class are the same field");
        }
        Ok(Mapping {
            tracker: name(&format!("{at}: tracker"), &o["tracker"])?,
            standard,
            target,
            requirement_id_field,
            risk_class_field,
            trigger_label: name(&format!("{at}: trigger_label"), &o["trigger_label"])?,
            team_group: name(&format!("{at}: team_group"), &o["team_group"])?,
        })
    }

    /// The standard state of a workflow state; None if it is not mapped,
    /// which the adapter must treat as an error.
    pub fn standard(&self, workflow: &str) -> Option<State> {
        self.standard.get(workflow).copied()
    }

    /// The workflow state `transition` moves an issue to.
    pub fn target(&self, s: State) -> &str {
        &self.target[&s]
    }

    pub fn workflows(&self) -> impl Iterator<Item = &str> {
        self.standard.keys().map(String::as_str)
    }
}

/// A non-empty string with no spaces at the ends.
fn name(at: &str, v: &Value) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s == s.trim() => Ok(s.to_owned()),
        _ => bail!("{at} must be a non-empty name with no spaces at the ends"),
    }
}

/// An object with exactly these keys.
fn exact_keys<'a>(at: &str, v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let o = v
        .as_object()
        .with_context(|| format!("{at} must be an object"))?;
    if let Some(k) = o.keys().find(|k| !keys.contains(&k.as_str())) {
        bail!("{at}: unexpected key {k}");
    }
    if let Some(k) = keys.iter().find(|k| !o.contains_key(**k)) {
        bail!("{at}: missing key {k}");
    }
    Ok(o)
}
