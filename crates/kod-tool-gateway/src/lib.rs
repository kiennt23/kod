//! Kod's tool gateway: the MCP endpoint a run's sandbox reaches for tracker
//! and docs tools. Every call is checked by kod-policy and recorded in the
//! audit log before it acts; a call that can't be decided or recorded
//! doesn't happen. A canceled run gets nothing.
use anyhow::bail;
use docs::Docs;
use kod_audit::{AuditLog, RUN_AUDIT, redact::REDACTED};
use kod_policy::Policy;
use kod_tracker::{Issue, Rework, ReworkKind, State, Tracker};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub mod docs;

/// A run token is at least this many lower-case hex characters, as in the
/// LLM gateway.
pub const MIN_TOKEN_LEN: usize = 32;
/// The moves the agent may make on its own ticket. Review and done are the
/// dispatcher's, on publish.
pub const AGENT_TRANSITIONS: [(State, State); 2] = [
    (State::InProgress, State::Blocked),
    (State::Blocked, State::InProgress),
];

/// A run the gateway serves. `base` is the commit its workspace started from.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: String,
    pub ticket: String,
    pub repo: String,
    pub base: String,
    /// The fixed classes admitted for both model routing and document reads.
    pub data_classes: Vec<String>,
    /// The rework requests Kod itself linked to the ticket before this run,
    /// from the dispatcher's records: a snapshot taken at start, never the
    /// ticket's live links, which any human can change.
    pub linked_tickets: Vec<String>,
    /// Unix seconds. The commit date, so a retried publish makes the same
    /// commit.
    pub started_at: i64,
}

#[derive(Clone)]
struct Live {
    run: Run,
    /// Rework requests this run created; it may read and comment on them.
    created: Vec<String>,
}

impl Live {
    fn linked(&self) -> Vec<String> {
        let mut all = self.run.linked_tickets.clone();
        all.extend(self.created.iter().cloned());
        all
    }
}

/// Whether a run is stopped (canceled, or ended) in the dispatcher's
/// database. The gateway asks before every call; an error counts as
/// stopped.
pub type Stopped = Arc<dyn Fn(&str) -> anyhow::Result<bool> + Send + Sync>;

/// The adapters behind the tools. Each holds its own credentials; none of
/// them is ever handed to the sandbox.
pub struct Adapters {
    pub tracker: Arc<dyn Tracker>,
    pub docs: Arc<dyn Docs>,
    pub stopped: Stopped,
}

pub struct Gateway {
    policy: Arc<Policy>,
    audit: Arc<AuditLog>,
    adapters: Adapters,
    /// Keyed by the token's SHA-256, as in the LLM gateway.
    runs: Mutex<HashMap<String, Live>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key(token: &str) -> String {
    hex(&Sha256::digest(token))
}

/// A non-empty single line with no spaces at the ends.
fn named(s: &str) -> bool {
    !s.is_empty() && s == s.trim() && !s.contains(['\n', '\r'])
}

fn lower_hex(s: &str, min: usize) -> bool {
    s.len() >= min && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Error text made safe for an MCP reply, an audit record or a log: the
/// audit log's redaction, then run tokens in `/r/{token}` paths.
pub fn scrub(text: &str) -> String {
    let text = kod_audit::redact::redact(text);
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(i) = rest.find("/r/") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let n = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        if n >= MIN_TOKEN_LEN {
            out.push_str(REDACTED);
            rest = &rest[n..];
        }
    }
    out.push_str(rest);
    out
}

type Reply = Result<Value, String>;

fn text<'a>(args: &'a Value, k: &str) -> Result<&'a str, String> {
    args[k]
        .as_str()
        .ok_or_else(|| format!("{k} must be a string"))
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "key": i.key,
        "title": i.title,
        "body": i.body,
        "acceptance_criteria": i.acceptance_criteria,
        "requirement_id": i.requirement_id,
        "risk_class": i.risk_class,
        "approved_versions": i.approved_versions,
        "links": i.links,
        "state": i.state.name(),
    })
}

impl Gateway {
    pub fn new(
        policy: Arc<Policy>,
        audit: Arc<AuditLog>,
        adapters: Adapters,
    ) -> anyhow::Result<Gateway> {
        Ok(Gateway {
            policy,
            audit,
            adapters,
            runs: Mutex::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Live>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts serving a run under its token.
    pub fn start_run(&self, token: &str, run: Run) -> anyhow::Result<()> {
        if !lower_hex(token, MIN_TOKEN_LEN) {
            bail!("a run token must be at least {MIN_TOKEN_LEN} lower-case hex characters");
        }
        for (k, v) in [
            ("id", &run.id),
            ("ticket", &run.ticket),
            ("repo", &run.repo),
        ] {
            if !named(v) {
                bail!("run {}: {k} must be a name", run.id);
            }
        }
        if !lower_hex(&run.base, 40) || run.base.len() != 40 {
            bail!("run {}: base must be a 40-character commit SHA", run.id);
        }
        if run.data_classes.is_empty() || !run.data_classes.iter().all(|c| named(c)) {
            bail!(
                "run {}: data_classes must be a non-empty list of names",
                run.id
            );
        }
        if !run.linked_tickets.iter().all(|t| named(t)) {
            bail!("run {}: linked_tickets must be names", run.id);
        }
        let mut runs = self.lock();
        if runs.values().any(|l| l.run.id == run.id) || runs.contains_key(&key(token)) {
            bail!("run {} is already started", run.id);
        }
        runs.insert(
            key(token),
            Live {
                run,
                created: vec![],
            },
        );
        Ok(())
    }

    /// Stops serving a run: its token stops working. Safe to repeat. The
    /// dispatcher calls it when the run ends, however it ends.
    pub fn end_run(&self, run_id: &str) -> anyhow::Result<()> {
        self.lock().retain(|_, l| l.run.id != run_id);
        Ok(())
    }

    /// The kill switch: stop serving every run.
    pub fn end_all(&self) -> anyhow::Result<()> {
        self.lock().clear();
        Ok(())
    }

    /// The run, or None for an unknown token.
    pub fn run(&self, token: &str) -> Option<Run> {
        self.lock().get(&key(token)).map(|l| l.run.clone())
    }

    fn live(&self, token: &str) -> Option<Live> {
        self.lock().get(&key(token)).cloned()
    }

    fn update(&self, run_id: &str, f: impl FnOnce(&mut Live)) {
        if let Some(l) = self.lock().values_mut().find(|l| l.run.id == run_id) {
            f(l);
        }
    }

    /// Refuses unless the dispatcher's database says the run is live.
    fn not_stopped(&self, run_id: &str) -> Result<(), String> {
        match (self.adapters.stopped)(run_id) {
            Ok(false) => Ok(()),
            Ok(true) => Err("refused: the run is canceled".into()),
            Err(e) => Err(scrub(&format!(
                "refused: the run's state could not be read: {e:#}"
            ))),
        }
    }

    /// Runs one tool call for the run with this token. Blocking. Every
    /// error is scrubbed.
    pub fn call(&self, token: &str, tool: &str, args: &Value) -> Reply {
        let live = self.live(token).ok_or("unknown run")?;
        self.not_stopped(&live.run.id)?;
        match tool {
            "get_issue" => self.get_issue(&live, args),
            "read_doc" => self.read_doc(&live, args),
            "comment" => self.comment(&live, args),
            "transition" => self.transition(&live, args),
            "create_linked" => self.create_linked(&live, args),
            other => Err(format!("unknown tool {other:?}")),
        }
        .map_err(|e| scrub(&e))
    }

    /// Asks the policy and records the decision. Refuses if the record can't
    /// be written or the policy says no.
    fn allow(&self, input: Value) -> Result<(), String> {
        let d = self.policy.decide(&input);
        self.audit
            .record_decision(&input, &d)
            .map_err(|e| format!("refused: the audit write failed: {e:#}"))?;
        if d.allow {
            Ok(())
        } else {
            Err(format!("refused by policy: {}", d.reasons.join("; ")))
        }
    }

    /// Writes the intent with its exact target, then runs `call`; a failed
    /// write means `call` never runs. The result is recorded after.
    fn act<T>(
        &self,
        run: &Run,
        tool: &str,
        target: Value,
        call: impl FnOnce() -> anyhow::Result<T>,
    ) -> Result<T, String> {
        let intent = json!({"kind": "tool_call", "run": run.id, "tool": tool, "target": target});
        let r = self
            .audit
            .before(RUN_AUDIT, &intent, call)
            .map_err(|e| scrub(&format!("refused: the audit write failed: {e:#}")))?;
        let result = r
            .as_ref()
            .map(|_| "ok".to_owned())
            .unwrap_or_else(|e| scrub(&format!("{e:#}")));
        // The call already happened; a failed result record can't undo it.
        let _ = self.audit.append(
            RUN_AUDIT,
            &json!({"kind": "tool_result", "run": run.id, "tool": tool, "result": result}),
        );
        r.map_err(|e| scrub(&format!("{tool} failed: {e:#}")))
    }

    /// The run's own ticket, read for its linked docs, its state and the PR
    /// text.
    fn own_issue(&self, run: &Run) -> Result<Issue, String> {
        self.adapters
            .tracker
            .get_issue(&run.ticket)
            .map_err(|e| format!("reading the run's ticket failed: {e:#}"))
    }

    fn get_issue(&self, live: &Live, args: &Value) -> Reply {
        let (run, k) = (&live.run, text(args, "key")?);
        let target = json!({"kind": "ticket", "id": k});
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "linked_tickets": live.linked()},
            "target": target,
        }))?;
        let t = &self.adapters.tracker;
        let i = self.act(run, "get_issue", target, || t.get_issue(k))?;
        Ok(issue_json(&i))
    }

    fn read_doc(&self, live: &Live, args: &Value) -> Reply {
        let (run, id) = (&live.run, text(args, "id")?);
        let issue = self.own_issue(run)?;
        let target = json!({"kind": "doc", "id": id});
        self.allow(json!({
            "action": "read",
            "actor": {"kind": "agent"},
            "run": {"id": run.id, "ticket": run.ticket, "linked_docs": issue.approved_versions},
            "target": target,
        }))?;
        let docs = &self.adapters.docs;
        let doc = self.act(run, "read_doc", target, || {
            let doc = docs.read(id)?;
            if !run.data_classes.contains(&doc.class) {
                bail!("document is outside the run's admitted data classes; start a newly classified run");
            }
            Ok(doc)
        })?;
        Ok(json!({"id": id, "text": doc.text}))
    }

    fn write_input(&self, live: &Live, kind: &str, target: Value) -> Result<(), String> {
        let run = &live.run;
        self.allow(json!({
            "action": "tracker_write",
            "kind": kind,
            "actor": {"kind": "agent"},
            // `not_stopped` already checked the dispatcher's database.
            "run": {"id": run.id, "ticket": run.ticket, "canceled": false, "linked_tickets": live.linked()},
            "target": target,
        }))
    }

    fn comment(&self, live: &Live, args: &Value) -> Reply {
        let (k, body) = (text(args, "key")?, text(args, "text")?);
        let target = json!({"ticket": k});
        self.write_input(live, "comment", target.clone())?;
        let t = &self.adapters.tracker;
        self.act(&live.run, "comment", target, || t.comment(k, body))?;
        Ok(json!({"ok": true}))
    }

    fn transition(&self, live: &Live, args: &Value) -> Reply {
        let (run, k, name) = (&live.run, text(args, "key")?, text(args, "state")?);
        let to = State::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| format!("unknown state {name:?}"))?;
        if k != run.ticket {
            return Err("the agent may move only its own ticket".into());
        }
        let from = self.own_issue(run)?.state;
        if !AGENT_TRANSITIONS.contains(&(from, to)) {
            return Err(format!(
                "the agent may not move its ticket from {} to {name}",
                from.name()
            ));
        }
        let target = json!({"ticket": k, "state": name});
        self.write_input(live, "transition", json!({"ticket": k}))?;
        let t = &self.adapters.tracker;
        self.act(run, "transition", target, || t.transition(k, to))?;
        Ok(json!({"ok": true}))
    }

    fn create_linked(&self, live: &Live, args: &Value) -> Reply {
        let run = &live.run;
        let kind = match text(args, "kind")? {
            "question" => ReworkKind::Question,
            "amendment" => ReworkKind::Amendment,
            "replan" => ReworkKind::Replan,
            other => return Err(format!("unknown kind {other:?}")),
        };
        let evidence = args["evidence"]
            .as_array()
            .and_then(|l| l.iter().map(|e| e.as_str().map(str::to_owned)).collect())
            .ok_or("evidence must be a list of strings")?;
        let fields = Rework {
            stage: text(args, "stage")?.to_owned(),
            artifact: text(args, "artifact")?.to_owned(),
            version: text(args, "version")?.to_owned(),
            text: text(args, "text")?.to_owned(),
            evidence,
        };
        let target = json!({"parent": run.ticket});
        self.write_input(live, "create_linked", target.clone())?;
        let t = &self.adapters.tracker;
        let key = self.act(run, "create_linked", target, || {
            t.create_linked(&run.ticket, kind, &fields)
        })?;
        self.update(&run.id, |l| l.created.push(key.clone()));
        Ok(json!({"key": key}))
    }
}
