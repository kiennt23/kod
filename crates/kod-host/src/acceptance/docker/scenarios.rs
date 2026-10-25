//! Scenarios that drive the real gateway, proxy, collector, refresh and gate-container code
//! against the running host. Each returns named checks; a scenario passes only if all hold.
use super::{
    fixture::{Fixture, private_write},
    provider::Remote,
};
use crate::{
    acceptance::{Observation, ObservationCode, Outcome, Scenario},
    config::FsConfig,
    records::{self, Open},
    refresh::{Timed, WorkflowSource, refresh_story, rework_records},
    workspace::{CollectedWorkspace, read_export, supported_patch},
};
use anyhow::{Context, Result, ensure};
use kod_status::records::{Records, Snapshot, Source};
use kod_tool_gateway::docs::Docs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

pub type Checks = Vec<(&'static str, bool)>;
const TICKET: &str = "acme/app#7";

pub struct Ctx {
    pub config: Arc<FsConfig>,
    pub gateway: Arc<kod_tool_gateway::Gateway>,
    pub workspace: Arc<CollectedWorkspace>,
    pub registry: kod_llm_proxy::RunRegistry,
    pub addr: std::net::SocketAddr,
    pub remote: Arc<Mutex<Remote>>,
    pub api: String,
}

impl Ctx {
    fn remote(&self) -> std::sync::MutexGuard<'_, Remote> {
        self.remote.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn writes(&self) -> usize {
        self.remote()
            .requests
            .iter()
            .filter(|(m, p, _)| m != "GET" && p.starts_with("/repos/"))
            .count()
    }
    /// Non-GET provider requests whose path or body mentions `needle`.
    fn writes_with(&self, needle: &str) -> usize {
        self.remote()
            .requests
            .iter()
            .filter(|(m, p, b)| {
                m != "GET" && (p.contains(needle) || b.to_string().contains(needle))
            })
            .count()
    }
    fn models(&self) -> usize {
        self.remote().models.len()
    }
    fn db(&self) -> Result<rusqlite::Connection> {
        kod_dispatcher::store::open(&self.config.paths.dispatcher_db)
    }
    fn tracker(&self) -> Result<kod_tracker::github::GitHub> {
        let c = &self.config;
        kod_tracker::github::GitHub::with_test_api(
            c.github.tracker.clone(),
            kod_tracker::mapping::Mapping::load(&c.github.mapping)?,
            "connected-fixture-token",
            "connected-fixture-webhook",
            &self.api,
        )
    }
    /// A live run row plus gateway and proxy registration under a fresh token.
    fn start(&self, ticket: &str, budget: u64) -> Result<(String, String)> {
        let db = self.db()?;
        db.execute(
            "INSERT INTO runs (ticket, revision, state) VALUES (?1, 'acceptance', 'running')",
            [ticket],
        )?;
        let rowid = db.last_insert_rowid();
        let id = kod_dispatcher::run_id(rowid);
        let token = format!(
            "{:x}",
            Sha256::digest(format!("{id}{:?}", std::time::SystemTime::now()))
        );
        self.gateway.start_run(
            &token,
            kod_tool_gateway::Run {
                id: id.clone(),
                ticket: ticket.into(),
                repo: "acme/app".into(),
                base: crate::production::mirror_base(&self.config.paths.mirror, "main")?,
                data_classes: vec!["placeholder".into()],
                pins: [("design".to_owned(), "design-v1".to_owned())].into(),
                linked_tickets: vec![],
                started_at: chrono::Utc::now().timestamp(),
            },
        )?;
        self.registry.register(
            &token,
            kod_llm_proxy::config::RunSpec {
                id: id.clone(),
                ticket: ticket.into(),
                repo: "acme/app".into(),
                data_classes: vec!["placeholder".into()],
                budget: kod_llm_proxy::config::Budget {
                    max_tokens: budget,
                    max_cost_micro_usd: 100_000,
                },
            },
        )?;
        Ok((id, token))
    }
    fn finish(&self, id: &str) -> Result<()> {
        self.gateway.end_run(id)?;
        let _ = self.registry.revoke(id);
        self.db()?
            .execute("UPDATE runs SET state='canceled' WHERE seq=?1", [&id[4..]])?;
        Ok(())
    }
    fn decisions(&self) -> Result<Vec<Value>> {
        let c = rusqlite::Connection::open(&self.config.paths.audit_db)?;
        let mut s = c.prepare("SELECT payload FROM audit ORDER BY seq")?;
        let rows = s
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .iter()
            .filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .filter(|v| v["kind"] == "policy_decision")
            .collect())
    }
}

fn refused_over_spend(c: &Ctx, run: &str) -> bool {
    let Ok(rows) = rusqlite::Connection::open(&c.config.paths.audit_db).and_then(|a| {
        let mut s = a.prepare("SELECT payload FROM audit")?;
        s.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
    }) else {
        return false;
    };
    rows.iter()
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .any(|v| v["kind"] == "gateway_refusal" && v["run"] == run && v["event"] == "over_spend")
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("client")
}
fn mcp(c: &Ctx, token: &str, host: Option<&str>) -> Result<u16> {
    let mut r = http()
        .post(format!("http://{}/r/{token}/mcp", c.addr))
        .header("content-type", "application/json")
        .body("{}");
    if let Some(h) = host {
        r = r.header("host", h);
    }
    Ok(r.send()?.status().as_u16())
}
fn model(c: &Ctx, token: &str, path: &str, max_tokens: u64) -> Result<u16> {
    let r = http()
        .post(format!("http://{}/r/{token}{path}", c.addr))
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(
            json!({"model":"fixture-model","max_tokens":max_tokens,"messages":[{"role":"user","content":"hi"}]})
                .to_string(),
        )
        .send()?;
    let code = r.status().as_u16();
    Ok(code)
}
fn err_has<T>(r: &Result<T, String>, text: &str) -> bool {
    r.as_ref().err().is_some_and(|e| e.contains(text))
}

pub fn gateway_safety(c: &Ctx) -> Result<Checks> {
    let mut k: Checks = vec![];
    let (id, token) = c.start(TICKET, 100_000)?;
    let call = |tool: &str, args: Value| c.gateway.call(&token, tool, &args);
    // Unauthorized delegation: the real policy refuses a high-risk or path-raised ticket.
    let policy = kod_policy::Policy::load(&c.config.paths.policy_dir)?;
    let ticket = |class: &str, path: &str| {
        policy
            .decide(&json!({"action":"delegate","ticket":{"repo":"acme/app","risk_class":class,"paths":[path],"acceptance_criteria":true,"bounces":0},"wip_in_progress":0}))
            .allow
    };
    k.push((
        "delegation-low-allowed-high-and-protected-path-denied",
        ticket("low", "docs/readme.md")
            && !ticket("high", "docs/readme.md")
            && !ticket("low", "auth/login.rs"),
    ));
    // Unauthorized access: another ticket and an unpinned document, both audited as denied.
    let other = call("get_issue", json!({"key":"acme/app#8"}));
    let secret = call("read_doc", json!({"id":"planted-secret"}));
    let own = call("read_doc", json!({"id":"design-v1"}));
    let denied = |target: &str| {
        c.decisions().is_ok_and(|d| {
            d.iter()
                .any(|v| v["input"]["target"]["id"] == target && v["decision"]["allow"] == false)
        })
    };
    k.push((
        "unowned-ticket-and-unpinned-doc-denied-and-audited",
        err_has(&other, "policy")
            && err_has(&secret, "policy")
            && own.is_ok()
            && denied("acme/app#8")
            && denied("planted-secret"),
    ));
    // Unlisted egress: a Host outside the list and a model path outside the allow list.
    let before = c.models();
    let host = mcp(c, &token, Some("evil.example"))?;
    let path = model(c, &token, "/v1/other", 10)?;
    k.push((
        "unlisted-host-and-model-path-refused",
        !(200..300).contains(&host)
            && host != 404
            && (400..500).contains(&path)
            && c.models() == before,
    ));
    // Forged metadata: sandbox-supplied identity is ignored; the decision binds the registered run.
    let forged = call(
        "get_issue",
        json!({"key":TICKET,"actor":{"kind":"human"},"run":{"id":"run-1","ticket":"acme/app#99"}}),
    );
    k.push((
        "forged-actor-and-run-ignored",
        forged.is_ok()
            && c.decisions()?.iter().rev().any(|v| {
                v["input"]["target"]["id"] == TICKET
                    && v["input"]["run"]["id"] == id.as_str()
                    && v["input"]["actor"]["kind"] == "agent"
            }),
    ));
    // Over budget: a request above the run budget never reaches the upstream; an in-budget one does.
    let (small_id, small) = (c.start("acme/app#8", 5)).map_err(|e| e.context("second run"))?;
    let before = c.models();
    let over = model(c, &small, "/v1/messages", 100)?;
    let held = c.models() == before;
    k.push((
        "over-budget-refused-before-upstream",
        over == 403 && held && refused_over_spend(c, &small_id),
    ));
    c.finish(&small_id)?;
    // Protected path: publishing a change under .github/ is refused by policy; nothing is written.
    c.workspace.insert_for_test(
        &id,
        kod_tool_gateway::forge::Collected {
            changes: vec![kod_tool_gateway::forge::Change {
                path: ".github/workflows/ci.yml".into(),
                content: Some("on: push\n".into()),
            }],
            test_evidence: String::new(),
        },
    )?;
    let writes = c.writes_with("/git/") + c.writes_with("/pulls");
    let published = c.gateway.publish(&id);
    k.push((
        "protected-path-publish-refused",
        err_has(&published, "high") && c.writes_with("/git/") + c.writes_with("/pulls") == writes,
    ));
    // Audit outage: with the audit store write-locked the call is refused before any external write.
    let lock = rusqlite::Connection::open(&c.config.paths.audit_db)?;
    lock.execute_batch("BEGIN IMMEDIATE")?;
    let outage = call("comment", json!({"key":TICKET,"text":"outage-marker"}));
    lock.execute_batch("ROLLBACK")?;
    let after = call("comment", json!({"key":TICKET,"text":"recovered-marker"}));
    k.push((
        "audit-outage-refuses-before-external-write",
        err_has(&outage, "audit")
            && c.writes_with("outage-marker") == 0
            && after.is_ok()
            && c.writes_with("recovered-marker") == 1,
    ));
    // Wrong and ended tokens.
    let wrong = format!("{:x}", Sha256::digest("wrong"));
    let ended_token = token.clone();
    c.finish(&id)?;
    k.push((
        "wrong-and-ended-tokens-refused",
        mcp(c, &wrong, None)? == 404
            && mcp(c, &ended_token, None)? == 404
            && err_has(
                &c.gateway.call(&wrong, "get_issue", &json!({"key":TICKET})),
                "unknown run",
            )
            && err_has(
                &c.gateway
                    .call(&ended_token, "get_issue", &json!({"key":TICKET})),
                "unknown run",
            ),
    ));
    Ok(k)
}

fn tar_work(dir: &Path, build: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let tree = dir.join("tree");
    std::fs::create_dir(&tree)?;
    build(&tree)?;
    let ok = super::proc::success(
        std::process::Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-cf")
            .arg(dir.join("agent_out/work.tar"))
            .arg("-C")
            .arg(&tree)
            .arg("."),
        super::proc::SHORT,
    )?;
    ensure!(ok, "tar failed");
    std::fs::remove_dir_all(&tree)?;
    Ok(())
}

/// The real collector containers on an agent output the test controls, then the host reader.
fn collect_pipeline(
    c: &Ctx,
    n: u64,
    build: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<String>> {
    use std::os::unix::fs::DirBuilderExt;
    let cfg = &c.config;
    let run = format!("run-{n}");
    let dir = cfg.paths.run_root.join(&run);
    let base = crate::production::mirror_base(&cfg.paths.mirror, "main")?;
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let result = (|| {
        std::fs::create_dir(dir.join("agent_out"))?;
        std::fs::write(dir.join("agent_out/exit_code"), "0\n")?;
        std::fs::write(dir.join("agent_out/agent_procs"), "0\n")?;
        tar_work(&dir, build)?;
        let spec = kod_eval::connected::ConnectedSpec {
            run: run.clone(),
            base: base.clone(),
            mirror: cfg.paths.mirror.clone(),
            dir: dir.clone(),
            agent_image: cfg.sandbox.agent_image.clone(),
            collector_image: cfg.sandbox.collector_image.clone(),
            network: cfg.networking.sandbox_network.clone(),
            model: cfg.model.selected_model.clone(),
            max_tokens: cfg.model.max_tokens,
            budget: kod_eval::case::Budget {
                max_turns: cfg.sandbox.max_turns,
                timeout_secs: cfg.sandbox.wall_seconds,
                memory: cfg.sandbox.memory.clone(),
                disk: cfg.sandbox.disk.clone(),
                cpus: cfg.sandbox.cpus,
            },
            prompt: String::new(),
            model_url: String::new(),
            mcp_url: String::new(),
        };
        let gate = kod_eval::connected::SpawnGate::default();
        let live = || Ok(false);
        let collected =
            kod_eval::connected::collect(&spec, &gate, &live).context("collect stage")?;
        let patch = kod_eval::connected::regular_bytes(&collected.join("diff.patch"), 32 << 20)?;
        supported_patch(&patch).context("patch stage")?;
        let exported = kod_eval::connected::export(&spec, &gate, &live).context("export stage")?;
        let out = read_export(&exported, &base).context("read stage")?;
        Ok(out.changes.into_iter().map(|c| c.path).collect())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn write(root: &Path, path: &str, text: &str) -> Result<()> {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().context("parent")?)?;
    std::fs::write(p, text)?;
    Ok(())
}

pub fn collector_integrity(c: &Ctx) -> Result<Checks> {
    let mut k: Checks = vec![];
    let seq = 9000 + u64::from(std::process::id() % 900);
    // .git tampering and a nested repository never reach the export.
    let paths = collect_pipeline(c, seq, |t| {
        write(t, "answer.txt", "42\n")?;
        write(t, ".git/config", "[core]\n\thooksPath = /tmp\n")?;
        write(t, ".git/hooks/pre-commit", "#!/bin/sh\nexit 1\n")?;
        write(t, "sub/.git/HEAD", "ref: refs/heads/x\n")?;
        write(t, "sub/note.txt", "kept\n")
    });
    k.push((
        "dot-git-tampering-dropped",
        paths.as_ref().is_ok_and(|p| {
            p.iter().any(|x| x == "answer.txt")
                && p.iter().all(|x| !x.split('/').any(|s| s == ".git"))
        }),
    ));
    // A symlink escaping the work tree stops the pipeline before any result is inserted.
    let escape = collect_pipeline(c, seq + 1000, |t| {
        write(t, "answer.txt", "42\n")?;
        std::os::unix::fs::symlink("/etc/passwd", t.join("escape"))?;
        Ok(())
    });
    k.push(("symlink-escape-rejected", escape.is_err()));
    // Forged export directories and fabricated patches are refused by the host reader.
    let base = crate::production::mirror_base(&c.config.paths.mirror, "main")?;
    let forged = |manifest: Value, link: bool| -> bool {
        let Ok(dir) = tempfile::tempdir() else {
            return false;
        };
        let d = dir.path();
        let _ = std::fs::write(d.join("export_ok"), "");
        let _ = std::fs::write(d.join("manifest.json"), manifest.to_string());
        if link {
            let _ = std::os::unix::fs::symlink("/etc/passwd", d.join("0"));
        } else {
            let _ = std::fs::write(d.join("0"), "x\n");
        }
        read_export(d, &base).is_err()
    };
    let file = |path: &str| json!({"path":path,"mode":"100644","blob":0});
    let m = |base: &str, f: Value| json!({"version":1,"base":base,"files":[f]});
    k.push((
        "forged-export-manifest-rejected",
        forged(m("0".repeat(40).as_str(), file("a.txt")), false)
            && forged(m(&base, file(".git/config")), false)
            && forged(m(&base, file("../escape")), false)
            && forged(m(&base, file("a.txt")), true),
    ));
    let patch =
        |extra: &str| format!("diff --git a/a b/a\n{extra}--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n");
    k.push((
        "fabricated-patch-rejected",
        supported_patch(patch("").as_bytes()).is_ok()
            && supported_patch(patch("new mode 100755\n").as_bytes()).is_err()
            && supported_patch(patch("rename from a\n").as_bytes()).is_err()
            && supported_patch(b"diff --git a/a b/a\nGIT binary patch\n\0").is_err(),
    ));
    Ok(k)
}

fn recorder() -> (Arc<Mutex<Vec<String>>>, RecordingForge) {
    let log = Arc::new(Mutex::new(vec![]));
    (log.clone(), RecordingForge(log))
}
pub struct RecordingForge(Arc<Mutex<Vec<String>>>);
impl kod_gates::Forge for RecordingForge {
    fn pull(&self, _: u64) -> Result<kod_gates::PullRequest> {
        anyhow::bail!("not used")
    }
    fn report(&self, sha: &str, check: &kod_gates::Check) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("log"))?
            .push(format!("{sha}:{:?}", check.conclusion));
        Ok(())
    }
}

/// Replaces the EXISTING `design` link's revision (no new link), so old pins become superseded.
fn advance_design(manifest: &mut Value, text: &str) -> Result<()> {
    let entry = manifest["artifacts"]
        .as_array_mut()
        .context("artifacts")?
        .iter_mut()
        .find(|a| a["link"] == "design")
        .context("design link")?;
    entry["revision"] = "design-v2".into();
    entry["path"] = "design2.txt".into();
    entry["sha256"] = format!("{:x}", Sha256::digest(text)).into();
    Ok(())
}

pub fn approval_refresh(c: &Ctx) -> Result<Checks> {
    use std::os::unix::fs::PermissionsExt;
    let mut k: Checks = vec![];
    let cfg = &c.config;
    let active = &cfg.paths.approval_manifest;
    let old = std::fs::read(active)?;
    let mut manifest: Value = serde_json::from_slice(&old)?;
    let text = "Approved fixture design, second revision";
    advance_design(&mut manifest, text)?;
    let stage = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
    std::fs::set_permissions(stage.path(), std::fs::Permissions::from_mode(0o700))?;
    let candidate = stage.path().join("manifest.json");
    for a in manifest["artifacts"].as_array().context("artifacts")? {
        let rel = a["path"].as_str().context("path")?;
        if rel != "design2.txt" {
            std::fs::copy(
                active.parent().context("dir")?.join(rel),
                stage.path().join(rel),
            )?;
            std::fs::set_permissions(
                stage.path().join(rel),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
    }
    for (name, body) in [
        ("design2.txt", text.to_owned()),
        ("manifest.json", manifest.to_string()),
    ] {
        std::fs::write(stage.path().join(name), body)?;
        std::fs::set_permissions(
            stage.path().join(name),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    let audit = kod_audit::AuditLog::open(&stage.path().join("audit.log"))?;
    let gates = kod_gates::Config {
        repo: "acme/app".into(),
        protect_all: true,
        protected: vec![],
        codeowners: vec!["kiennt23".into()],
    };
    let head_a = "a".repeat(40);
    let head_b = "b".repeat(40);
    let open = |head: String| {
        move || {
            Ok(vec![Open {
                story: TICKET.into(),
                head: head.clone(),
            }])
        }
    };
    // A failed invalidation keeps the old manifest and installs nothing.
    let result = records::activate_from(cfg, &candidate, &open(head_a.clone()), &|_| {
        anyhow::bail!("forge unavailable")
    });
    k.push((
        "failed-invalidation-keeps-old-manifest",
        result.is_err()
            && std::fs::read(active)? == old
            && !active.parent().context("dir")?.join("design2.txt").exists(),
    ));
    // The head changed since the first attempt: the retry reads the live head, invalidates it
    // to pending through the audited report path, and only then installs the new revision.
    let (log, forge) = recorder();
    let result = records::activate_from(cfg, &candidate, &open(head_b.clone()), &|h| {
        kod_gates::invalidate(&gates, &audit, &forge, h)
    });
    let log = log.lock().map_err(|_| anyhow::anyhow!("log"))?.clone();
    k.push((
        "changed-head-invalidated-to-pending-then-activated",
        result.is_ok()
            && !log.is_empty()
            && log
                .iter()
                .all(|l| l.starts_with(&head_b) && l.ends_with("Pending"))
            && std::fs::read(active)? == std::fs::read(&candidate)?
            && active.parent().context("dir")?.join("design2.txt").exists(),
    ));
    // The new approved revision is readable and old content stays immutable; the required `design`
    // link now resolves to v2, so a v1 pin is superseded and refused while a v2 pin is accepted.
    let artifacts = crate::artifacts::Artifacts::new(active.clone(), &cfg.paths.policy_dir)?;
    let design = ["design".to_owned()];
    k.push((
        "new-revision-active",
        artifacts.read("design-v2").is_ok_and(|d| d.text == text),
    ));
    k.push((
        "required-link-advanced-and-old-pin-stale",
        kod_gates::Artifacts::current(&artifacts, "design").is_ok_and(|r| r == "design-v2")
            && kod_gates::check_pins(&["design-v1".into()], &design, &artifacts).is_err()
            && kod_gates::check_pins(&["design-v2".into()], &design, &artifacts).is_ok(),
    ));
    Ok(k)
}

struct Rows {
    tracker: kod_tracker::github::GitHub,
    db: PathBuf,
}
impl Source for Rows {
    fn load(&self, story: &str) -> Result<Records> {
        use kod_tracker::Tracker;
        let c = kod_dispatcher::store::open(&self.db)?;
        Ok(Records {
            snapshot: Snapshot {
                story: self.tracker.get_issue(story)?,
                tickets: vec![],
                artifacts: vec![],
                pulls: vec![],
                rework: rework_records(&c, story)?,
            },
            runs: vec![],
        })
    }
}

pub fn current_source_fail_closed(c: &Ctx) -> Result<Checks> {
    let mut k: Checks = vec![];
    let cfg = &c.config;
    let dir = tempfile::tempdir()?;
    let audit = kod_audit::AuditLog::open(&dir.path().join("audit.log"))?;
    let mut db = c.db()?;
    let tracker = c.tracker()?;
    let writes = c.writes();
    // An open agent pull request for the story fails closed instead of rendering stale status.
    let open = WorkflowSource {
        tracker: Arc::new(c.tracker()?),
        artifacts: Arc::new(crate::artifacts::Artifacts::new(
            cfg.paths.approval_manifest.clone(),
            &cfg.paths.policy_dir,
        )?),
        dispatcher_db: cfg.paths.dispatcher_db.clone(),
        scratch: dir.path().to_owned(),
        open: Arc::new(|| Ok(vec![format!("agent/{TICKET}-run-3")])),
    };
    let e = refresh_story(cfg, &tracker, &open, &audit, &mut db, TICKET);
    k.push((
        "open-pull-request-refuses-status-write",
        e.as_ref()
            .err()
            .is_some_and(|e| format!("{e:#}").contains("evidence is unavailable"))
            && c.writes() == writes,
    ));
    // A source that does not answer within the deadline is an error, never a cached success.
    struct Slow;
    impl Source for Slow {
        fn load(&self, _: &str) -> Result<Records> {
            std::thread::sleep(Duration::from_millis(600));
            anyhow::bail!("late")
        }
    }
    let started = std::time::Instant::now();
    let slow = Timed::new(Arc::new(Slow), Duration::from_millis(50));
    let e = refresh_story(cfg, &tracker, &slow, &audit, &mut db, TICKET);
    k.push((
        "source-read-timeout-refuses-without-stale-fallback",
        e.as_ref()
            .err()
            .is_some_and(|e| format!("{e:#}").contains("timed out"))
            && started.elapsed() < Duration::from_millis(500)
            && c.writes() == writes,
    ));
    // An upstream outage while reading the story also refuses.
    c.remote().fail = true;
    let rows = Rows {
        tracker: c.tracker()?,
        db: cfg.paths.dispatcher_db.clone(),
    };
    let e = refresh_story(cfg, &tracker, &rows, &audit, &mut db, TICKET);
    c.remote().fail = false;
    k.push(("source-outage-refuses", e.is_err() && c.writes() == writes));
    Ok(k)
}

pub fn status_comment_reconciliation(c: &Ctx) -> Result<Checks> {
    let mut k: Checks = vec![];
    let cfg = &c.config;
    let dir = tempfile::tempdir()?;
    let audit = kod_audit::AuditLog::open(&dir.path().join("audit.log"))?;
    let mut db = c.db()?;
    let rows = |c: &Ctx| -> Result<Rows> {
        Ok(Rows {
            tracker: c.tracker()?,
            db: cfg.paths.dispatcher_db.clone(),
        })
    };
    let posts = |c: &Ctx| {
        c.remote()
            .requests
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "/repos/acme/app/issues/7/comments")
            .count()
    };
    let before = posts(c);
    // The create reaches the provider but its response is lost.
    c.remote().lose_create = true;
    let first = refresh_story(cfg, &c.tracker()?, &rows(c)?, &audit, &mut db, TICKET);
    c.remote().lose_create = false;
    k.push((
        "lost-response-is-an-error",
        first.is_err() && posts(c) == before + 1,
    ));
    // After a restart (fresh tracker, same durable intent) it reconciles to the one owned comment.
    let again = refresh_story(cfg, &c.tracker()?, &rows(c)?, &audit, &mut db, TICKET);
    let owned = c.remote().comments.len();
    k.push((
        "restart-reconciles-without-a-second-comment",
        again.is_ok() && posts(c) == before + 1 && owned == 1,
    ));
    // A later refresh updates that comment in place.
    let third = refresh_story(cfg, &c.tracker()?, &rows(c)?, &audit, &mut db, TICKET);
    k.push((
        "later-refresh-updates-in-place",
        third.is_ok() && posts(c) == before + 1 && c.remote().comments.len() == 1,
    ));
    Ok(k)
}

pub fn gate_container_isolation(c: &Ctx) -> Result<Checks> {
    let mut k: Checks = vec![];
    let dir = tempfile::tempdir()?;
    let archive = dir.path().join("source.tar.gz");
    write(dir.path(), "src/marker", "original\n")?;
    ensure!(
        super::proc::success(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(dir.path())
                .arg("src"),
            super::proc::SHORT
        )?,
        "tar failed"
    );
    let sha = "a".repeat(40);
    let scanner = kod_gates::scanner::DockerScanner {
        config: kod_gates::scanner::ScanConfig::rust(&c.config.sandbox.collector_image),
        archive,
        sha: sha.clone(),
        docker: "docker".into(),
        cache: None,
    };
    let name = kod_gates::scanner::resource_name(&sha)?;
    let mut args = scanner.container_args(&name, false)?;
    let flags = [
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--user=10001:10001",
        "--pull=never",
    ];
    k.push((
        "isolation-flags-present",
        flags.iter().all(|f| args.iter().any(|a| a == f)),
    ));
    for a in &mut args {
        if a.starts_with("--entrypoint=") {
            *a = "--entrypoint=/bin/sh".into();
        }
    }
    args.extend([
        "-ec".into(),
        "test \"$(id -u)\" = 10001; test -z \"${KOD_GITHUB_TOKEN:-}${KOD_MODEL_KEY:-}\"; \
         test ! -S /var/run/docker.sock; ! touch /rootfile; ! touch /input/source.tar.gz; \
         test \"$(ls /sys/class/net | tr -d '\\n')\" = lo"
            .into(),
    ]);
    use super::proc::{LONG, SHORT, output};
    let docker = |a: &[&str]| output(std::process::Command::new("docker").args(a), None, SHORT);
    // A hung or killed `docker run` client may leave its container behind: always remove by name.
    let run = output(std::process::Command::new("docker").args(&args), None, LONG);
    let removed = docker(&["rm", "--force", "--volumes", &name]);
    let listed = docker(&["ps", "-a", "-q", "--filter", &format!("name={name}")]);
    k.push((
        "gate-container-ran-isolated",
        run.is_ok_and(|r| r.status.success()),
    ));
    k.push((
        "gate-container-absent-afterwards",
        removed.is_ok() && listed.is_ok_and(|l| l.status.success() && l.stdout.is_empty()),
    ));
    Ok(k)
}

pub(super) fn scenario(name: &str, checks: Result<Checks>, evidence: &Path) -> Scenario {
    let (ok, count, detail) = match checks {
        Ok(k) if !k.is_empty() => (
            k.iter().all(|(_, ok)| *ok),
            k.iter().filter(|(_, ok)| *ok).count(),
            json!(
                k.iter()
                    .map(|(n, ok)| json!({"check":n,"ok":ok}))
                    .collect::<Vec<_>>()
            ),
        ),
        Ok(_) => (false, 0, json!("no checks")),
        Err(e) => (false, 0, json!(format!("{e:#}"))),
    };
    if !ok {
        eprintln!("scenario {name} failed: {detail}");
    }
    let _ = private_write(
        &evidence.join(format!("scenario-{name}.json")),
        detail.to_string().as_bytes(),
    );
    Scenario {
        name: name.into(),
        outcome: if ok { Outcome::Pass } else { Outcome::Fail },
        observations: vec![Observation {
            code: ObservationCode::Observed,
            count: u64::try_from(count).ok(),
            ..Default::default()
        }],
    }
}

pub async fn run(h: &Fixture, evidence: &Path) -> Result<Vec<Scenario>> {
    let service = h.service.as_ref().context("service missing")?;
    let ctx = Arc::new(Ctx {
        config: h.config.clone(),
        gateway: service.runtime.tool().clone(),
        workspace: service.runtime.workspace().clone(),
        registry: service.registry.clone(),
        addr: service.gateway,
        remote: h.provider.state.clone(),
        api: h.provider.api.clone(),
    });
    let evidence = evidence.to_owned();
    tokio::task::spawn_blocking(move || {
        type Run = fn(&Ctx) -> Result<Checks>;
        let all: [(&str, Run); 6] = [
            ("connected-gateway-safety", gateway_safety),
            ("collector-integrity", collector_integrity),
            ("approval-refresh", approval_refresh),
            ("current-source-fail-closed", current_source_fail_closed),
            (
                "status-comment-reconciliation",
                status_comment_reconciliation,
            ),
            ("gate-container-isolation", gate_container_isolation),
        ];
        all.iter()
            .map(|(name, f)| scenario(name, f(&ctx), &evidence))
            .collect()
    })
    .await
    .context("scenario worker")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Stub(Value);
    impl kod_gates::Artifacts for Stub {
        fn current(&self, link: &str) -> Result<String> {
            let a = self.0["artifacts"].as_array().context("artifacts")?;
            let a = a.iter().find(|a| a["link"] == link).context("unknown")?;
            Ok(a["revision"].as_str().context("revision")?.to_owned())
        }
    }
    #[test]
    fn refresh_advances_the_required_link_and_stales_old_pins() {
        let mut m = json!({"artifacts":[
            {"link":"design","revision":"design-v1","path":"design.txt","sha256":"x"},
            {"link":"secret","revision":"planted-secret","path":"secret.txt","sha256":"y"}]});
        advance_design(&mut m, "v2").unwrap();
        assert_eq!(m["artifacts"].as_array().unwrap().len(), 2);
        let (stub, design) = (Stub(m), ["design".to_owned()]);
        assert!(kod_gates::check_pins(&["design-v1".into()], &design, &stub).is_err());
        assert!(kod_gates::check_pins(&["design-v2".into()], &design, &stub).is_ok());
    }
}
