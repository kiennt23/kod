//! Startup, route mounting and shutdown ordering for the connected host.
use anyhow::{Result, bail};
use axum::Router;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

pub fn recover_then_open<T>(
    recover: impl FnOnce() -> Result<()>,
    open: impl FnOnce() -> Result<T>,
) -> Result<T> {
    recover()?;
    open()
}

/// Runs every phase in order even when an earlier one failed.
pub fn cleanup_all(
    quiesce: impl FnOnce() -> Result<()>,
    revoke: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let outcomes = [quiesce(), revoke(), stop()];
    let errors: Vec<String> = outcomes
        .into_iter()
        .filter_map(Result::err)
        .map(|e| kod_tool_gateway::scrub(&format!("{e:#}")))
        .collect();
    if !errors.is_empty() {
        bail!("cleanup remains pending: {}", errors.join("; "));
    }
    Ok(())
}

/// Public ingress gets only `control`; tool and model routes stay internal.
pub fn split_routes(control: Router, tools: Router, model: Router) -> (Router, Router) {
    (control, tools.fallback_service(model))
}

pub fn instance_lock(private_root: &std::path::Path) -> Result<std::fs::File> {
    use anyhow::{Context, ensure};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let root = std::fs::symlink_metadata(private_root)?;
    ensure!(
        root.is_dir() && !root.file_type().is_symlink() && root.permissions().mode() & 0o077 == 0,
        "host root must be private"
    );
    let path = private_root.join("host.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(m) => ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "invalid host lock file"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o077 == 0,
        "host lock must be private"
    );
    file.try_lock().context("another host owns this runtime")?;
    Ok(file)
}

/// Revocation and stop confirmation are polled, not awaited: the first call
/// starts remote work and reports pending. Retry until confirmed or the bound.
pub fn retry_until_confirmed(mut f: impl FnMut() -> Result<()>) -> Result<()> {
    let end = std::time::Instant::now() + CONFIRM_WITHIN;
    loop {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) if std::time::Instant::now() >= end => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}
const CONFIRM_WITHIN: std::time::Duration = std::time::Duration::from_secs(20);

pub async fn recover_and_attach(
    runtime: Arc<crate::runtime::Runtime>,
    open_dispatcher: impl FnOnce() -> Result<kod_dispatcher::Dispatcher> + Send + 'static,
) -> Result<Arc<kod_dispatcher::Dispatcher>> {
    tokio::task::spawn_blocking(move || {
        let dispatcher = Arc::new(recover_then_open(
            || retry_until_confirmed(|| runtime.recover_local()),
            open_dispatcher,
        )?);
        runtime.attach(Arc::downgrade(&dispatcher))?;
        Ok(dispatcher)
    })
    .await?
}

pub async fn dispatch_worker(
    dispatcher: Arc<kod_dispatcher::Dispatcher>,
    ready: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            _ = tick.tick() => {
                let d = dispatcher.clone();
                let allow = ready.load(Ordering::Acquire);
                let result = tokio::task::spawn_blocking(move || d.work_once_with_admission(allow)).await;
                let error = match result {
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    ready.store(false, Ordering::Release);
                    eprintln!("maintenance retry: {}", kod_tool_gateway::scrub(&error));
                }
            }
        }
    }
}

/// Everything `serve` takes from outside the config files. The tracker is
/// built by the caller because its credentials are not part of the host config.
#[derive(Clone)]
pub struct Parts {
    pub tracker: Arc<dyn kod_tracker::Tracker>,
    pub artifacts: Arc<crate::artifacts::Artifacts>,
    pub base: Arc<dyn Fn() -> Result<String> + Send + Sync>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub execute: crate::runtime::Execute,
    /// Called with the name of each startup/shutdown phase as it begins.
    pub trace: Arc<dyn Fn(&str) + Send + Sync>,
}

pub fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Production parts over an already-built tracker: mirror head, wall clock,
/// the real runner.
pub fn production_parts(
    config: &Arc<crate::config::FsConfig>,
    tracker: Arc<dyn kod_tracker::Tracker>,
) -> Result<Parts> {
    let artifacts = Arc::new(crate::artifacts::Artifacts::new(
        config.paths.approval_manifest.clone(),
        &config.paths.policy_dir,
    )?);
    let (mirror, branch) = (
        config.paths.mirror.clone(),
        config.github.product.base_branch.clone(),
    );
    Ok(Parts {
        tracker,
        artifacts,
        base: Arc::new(move || crate::production::mirror_base(&mirror, &branch)),
        now: Arc::new(system_now),
        execute: crate::production::execute(config.clone()),
        trace: Arc::new(|_| {}),
    })
}

pub struct Service {
    pub runtime: Arc<crate::runtime::Runtime>,
    pub registry: kod_llm_proxy::RunRegistry,
    pub dispatcher: Arc<kod_dispatcher::Dispatcher>,
    pub ingress: std::net::SocketAddr,
    pub gateway: std::net::SocketAddr,
    ready: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
    worker: tokio::task::JoinHandle<Result<()>>,
    listeners: Vec<tokio::task::JoinHandle<()>>,
    trace: Arc<dyn Fn(&str) + Send + Sync>,
    _lock: std::fs::File,
}

fn private_subdir(path: &std::path::Path) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path.to_owned())
}

struct Assembled {
    runtime: Arc<crate::runtime::Runtime>,
    registry: kod_llm_proxy::RunRegistry,
    tool: Arc<kod_tool_gateway::Gateway>,
    model_router: Router,
    policy: Arc<kod_policy::Policy>,
    audit: Arc<kod_audit::AuditLog>,
    dcfg: kod_dispatcher::Config,
    cli_keys: Vec<(String, Vec<u8>)>,
}

/// Blocking: GitHub's HTTP client owns a runtime that must not be built or dropped on an async thread.
fn assemble(config: &Arc<crate::config::FsConfig>, parts: Parts) -> Result<Assembled> {
    use anyhow::Context;
    let p = &config.paths;
    let m = &config.model;
    // Recovery reads the dispatcher database before the dispatcher opens it.
    drop(kod_dispatcher::store::open(&p.dispatcher_db)?);
    let policy = Arc::new(kod_policy::Policy::load(&p.policy_dir).context("loading policy")?);
    let audit = Arc::new(kod_audit::AuditLog::open(&p.audit_db).context("opening audit log")?);
    let key = String::from_utf8(crate::config::read_private(&m.api_key_file)?)?;
    let models = m.models.clone();
    let prices = kod_llm_proxy::config::parse_prices(
        &String::from_utf8(crate::config::read_local(&m.prices)?)?,
        &models,
    )?;
    let (model_router, registry) = kod_llm_proxy::app_with_registry(kod_llm_proxy::Config {
        endpoint: kod_llm_proxy::Endpoint {
            name: m.endpoint_name.clone(),
            upstream: m.upstream.trim_end_matches('/').to_owned(),
            api_key: key.trim().parse().context("invalid model api key")?,
        },
        allowed_paths: kod_llm_proxy::config::parse_paths(&m.allowed_paths)?,
        limits: kod_llm_proxy::Limits {
            allowed_models: models,
            max_tokens: m.max_tokens,
            max_body: m.max_body,
            max_requests: m.max_requests,
            max_concurrent: m.max_concurrent,
        },
        prices,
        runs: Default::default(),
        policy: policy.clone(),
        audit: audit.clone(),
    });
    registry.set_reservation_sink(Arc::new(crate::runtime::model_sink::DurableSink(
        p.dispatcher_db.clone(),
    )))?;
    let github = Arc::new(kod_tool_gateway::github::GitHub::new(
        config.github.product.clone(),
        config.github.app.clone(),
        private_subdir(&p.credential_dir.join("publish"))?,
    )?);
    let workspace = Arc::new(crate::workspace::CollectedWorkspace::default());
    let gate = Arc::new(kod_eval::connected::SpawnGate::default());
    let tool = Arc::new(kod_tool_gateway::Gateway::new(
        policy.clone(),
        audit.clone(),
        kod_tool_gateway::Adapters {
            tracker: parts.tracker.clone(),
            docs: parts.artifacts.clone(),
            stopped: crate::runtime::stopped_reader(p.dispatcher_db.clone()),
            broker: github.clone(),
            forge: github.clone(),
            workspace: workspace.clone(),
        },
        kod_tool_gateway::forge::Identity {
            name: config.github.agent_name.clone(),
            email: config.github.agent_email.clone(),
        },
    )?);
    let resolve = crate::production::resolve(
        config.clone(),
        crate::production::Sources {
            tracker: parts.tracker.clone(),
            artifacts: parts.artifacts.clone(),
            base: parts.base.clone(),
            now: parts.now.clone(),
        },
    );
    let runtime = crate::runtime::Runtime::new(
        config.clone(),
        crate::runtime::RuntimeAdapters {
            tool: tool.clone(),
            model: registry.clone(),
            github,
            workspace,
            gate,
            resolve,
            execute: parts.execute.clone(),
        },
    );
    let dcfg = kod_dispatcher::Config::load(&config.dispatcher_config)?;
    let mut cli_keys = vec![(
        dcfg.owner.clone(),
        crate::config::read_private(&config.github.owner_key_file)?,
    )];
    if let (Some(b), Some(f)) = (&dcfg.backup, &config.github.backup_key_file) {
        cli_keys.push((b.clone(), crate::config::read_private(f)?));
    }
    // Required sources must answer before anything is opened.
    parts.artifacts.current_manifest()?;
    (parts.base)()?;
    Ok(Assembled {
        runtime,
        registry,
        tool,
        model_router,
        policy,
        audit,
        dcfg,
        cli_keys,
    })
}

impl Service {
    /// Order: lock, orphan/lease reconcile + tombstones + dispatcher recovery
    /// + confirmed cleanup (one `recover_and_attach`), then listeners, then ready.
    ///   Any failure after recovery starts runs the cleanup path before returning.
    pub async fn start(config: Arc<crate::config::FsConfig>, parts: Parts) -> Result<Service> {
        use anyhow::Context;
        let trace = parts.trace.clone();
        let t = |p: &str| trace(p);
        t("lock");
        let root = config
            .paths
            .dispatcher_db
            .parent()
            .context("dispatcher_db has no parent")?;
        let lock = instance_lock(root)?;
        let (cfg, tr) = (config.clone(), parts.tracker.clone());
        let parts2 = Parts {
            trace: trace.clone(),
            ..parts
        };
        let asm = tokio::task::spawn_blocking(move || assemble(&cfg, parts2))
            .await
            .context("assembly panicked")??;
        let Assembled {
            runtime,
            registry,
            tool,
            model_router,
            policy,
            audit,
            dcfg,
            cli_keys,
        } = asm;
        let (p, parts_tracker) = (&config.paths, tr);
        let ready = Arc::new(AtomicBool::new(false));
        let guard = {
            let (ready, rt) = (ready.clone(), Arc::downgrade(&runtime));
            Arc::new(move || {
                ready.load(Ordering::Acquire) && rt.upgrade().is_some_and(|r| !r.cleanup_pending())
            })
        };
        t("recover");
        let (db, tracker, gateway) = (p.dispatcher_db.clone(), parts_tracker, runtime.clone());
        let tr = trace.clone();
        let dispatcher = recover_and_attach(runtime.clone(), move || {
            tr("dispatcher");
            let mut d =
                kod_dispatcher::Dispatcher::open(dcfg, &db, Ok(policy), audit, tracker, gateway)?
                    .with_admission_guard(guard);
            for (login, key) in cli_keys {
                d = d.with_cli_key(&login, &key);
            }
            Ok(d)
        })
        .await
        .inspect_err(|_| {
            // The cleanup failed or is pending; the next start retries from durable identities.
        })?;
        let (stop, stop_rx) = watch::channel(false);
        let build = async {
            t("bind");
            let n = &config.networking;
            let ingress = tokio::net::TcpListener::bind(n.ingress_listen).await?;
            let gateway = tokio::net::TcpListener::bind(n.gateway_listen).await?;
            Ok::<_, anyhow::Error>((ingress, gateway))
        };
        let (ingress_l, gateway_l) = match build.await {
            Ok(l) => l,
            Err(e) => {
                let _ = shutdown_cleanup(&runtime, &trace).await;
                let _ = tokio::task::spawn_blocking(move || drop((runtime, dispatcher))).await;
                return Err(e);
            }
        };
        let (control, tools) = (
            kod_dispatcher::router(dispatcher.clone()),
            kod_tool_gateway::router(tool, config.networking.accepted_hosts.clone()),
        );
        let (public, internal) = split_routes(control, tools, model_router);
        let (ia, ga) = (ingress_l.local_addr()?, gateway_l.local_addr()?);
        let serve = |l: tokio::net::TcpListener, r: Router, mut s: watch::Receiver<bool>| {
            tokio::spawn(async move {
                let _ = axum::serve(l, r)
                    .with_graceful_shutdown(async move {
                        let _ = s.wait_for(|v| *v).await;
                    })
                    .await;
            })
        };
        let listeners = vec![
            serve(ingress_l, public, stop_rx.clone()),
            serve(gateway_l, internal, stop_rx.clone()),
        ];
        let worker = tokio::spawn(dispatch_worker(dispatcher.clone(), ready.clone(), stop_rx));
        ready.store(true, Ordering::Release);
        t("ready");
        Ok(Service {
            runtime,
            registry,
            dispatcher: dispatcher.clone(),
            ingress: ia,
            gateway: ga,
            ready,
            stop,
            worker,
            listeners,
            trace,
            _lock: lock,
        })
    }

    /// Admission is open and no revocation or container stop is pending.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) && !self.runtime.cleanup_pending()
    }

    /// SIGTERM path: close admission, stop the worker, quiesce/revoke/stop with
    /// every phase attempted, then close the listeners. Err while cleanup is pending.
    pub async fn shutdown(self) -> Result<()> {
        (self.trace)("close-admission");
        self.ready.store(false, Ordering::Release);
        (self.trace)("stop-worker");
        let _ = self.stop.send(true);
        let _ = self.worker.await;
        let result = shutdown_cleanup(&self.runtime, &self.trace).await;
        (self.trace)("stop-listeners");
        for l in self.listeners {
            let _ = l.await;
        }
        // The last owners hold a blocking HTTP client; drop them off the async threads.
        let Service {
            runtime,
            registry,
            dispatcher,
            ..
        } = self;
        let _ = tokio::task::spawn_blocking(move || drop((runtime, registry, dispatcher))).await;
        result
    }
}

async fn shutdown_cleanup(
    runtime: &Arc<crate::runtime::Runtime>,
    trace: &Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<()> {
    let (rt, tr) = (runtime.clone(), trace.clone());
    tokio::task::spawn_blocking(move || {
        cleanup_all(
            || {
                tr("quiesce");
                rt.quiesce()
            },
            || {
                tr("revoke");
                retry_until_confirmed(|| rt.revoke_all())
            },
            || {
                tr("stop");
                retry_until_confirmed(|| kod_dispatcher::Gateway::stop_all(rt.as_ref()))
            },
        )
    })
    .await?
}

/// Start, wait for `term` (SIGTERM in `main`), then shut down in order.
pub async fn serve(
    config: Arc<crate::config::FsConfig>,
    parts: Parts,
    term: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let service = Service::start(config, parts).await?;
    term.await;
    service.shutdown().await
}

/// `serve` plus the 30 s status refresh: once at startup and then every `RECONCILE`. The loop
/// is stopped before shutdown begins and never joined, so a stuck status call cannot hold up kill.
pub async fn serve_refreshing(
    config: Arc<crate::config::FsConfig>,
    parts: Parts,
    tracker: Arc<kod_tracker::github::GitHub>,
    term: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let service = Service::start(config.clone(), parts.clone()).await?;
    let refresher = start_refresh(&config, &parts, tracker)?;
    term.await;
    refresher.stop();
    service.shutdown().await
}

fn start_refresh(
    config: &Arc<crate::config::FsConfig>,
    parts: &Parts,
    tracker: Arc<kod_tracker::github::GitHub>,
) -> Result<crate::refresh::Refresher> {
    let dir = private_subdir(&config.paths.credential_dir.join("refresh"))?;
    let scratch = private_subdir(&config.paths.credential_dir.join("refresh-records"))?;
    let product = Arc::new(kod_tool_gateway::github::GitHub::new(
        config.github.product.clone(),
        config.github.app.clone(),
        &dir,
    )?);
    let source = crate::refresh::Timed::production(Arc::new(crate::refresh::WorkflowSource {
        tracker: parts.tracker.clone(),
        artifacts: parts.artifacts.clone(),
        dispatcher_db: config.paths.dispatcher_db.clone(),
        scratch,
        open: Arc::new(move || {
            Ok(product
                .open_pulls()?
                .into_iter()
                .map(|p| p.branch)
                .collect())
        }),
    }));
    let db = config.paths.dispatcher_db.clone();
    let (cfg, audit_db) = (config.clone(), config.paths.audit_db.clone());
    let db2 = db.clone();
    Ok(crate::refresh::Refresher::spawn(
        move || {
            kod_dispatcher::store::open(&db2)
                .and_then(|c| crate::refresh::stories(&c))
                .unwrap_or_default()
        },
        move |story| {
            let mut c = kod_dispatcher::store::open(&db)?;
            let audit = kod_audit::AuditLog::open(&audit_db)?;
            crate::refresh::refresh_story(&cfg, &tracker, &source, &audit, &mut c, story)
        },
        crate::deadlines::RECONCILE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
    };
    use std::cell::RefCell;
    use tower::ServiceExt;

    #[test]
    fn failed_recovery_does_not_open_dispatcher_or_enable_admission() {
        let events = RefCell::new(Vec::new());
        let result = recover_then_open(
            || {
                events.borrow_mut().push("recover");
                anyhow::bail!("revoke pending")
            },
            || {
                events.borrow_mut().push("open");
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["recover"]);
    }

    #[test]
    fn shutdown_attempts_stop_even_when_quiesce_and_revoke_fail() {
        let events = RefCell::new(Vec::new());
        let result = cleanup_all(
            || {
                events.borrow_mut().push("quiesce");
                anyhow::bail!("store unavailable")
            },
            || {
                events.borrow_mut().push("revoke");
                anyhow::bail!("provider unavailable")
            },
            || {
                events.borrow_mut().push("stop");
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(*events.borrow(), ["quiesce", "revoke", "stop"]);
    }

    #[test]
    fn second_host_cannot_own_the_same_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = instance_lock(dir.path()).unwrap();
        assert!(instance_lock(dir.path()).is_err());
        drop(first);
        // A concurrently forked child can briefly hold the descriptor until it execs.
        let reacquired = (0..50).any(|_| {
            instance_lock(dir.path()).is_ok() || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(reacquired);
    }

    #[test]
    fn the_lock_refuses_a_public_root_and_a_symlinked_lock_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(instance_lock(dir.path()).is_err());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("host.lock")).unwrap();
        assert!(instance_lock(dir.path()).is_err());
    }

    #[tokio::test]
    async fn model_and_tools_are_not_mounted_on_public_ingress() {
        let control = axum::Router::new()
            .route("/webhook", post(|| async { StatusCode::ACCEPTED }))
            .route("/command", post(|| async { StatusCode::FORBIDDEN }));
        let tools =
            axum::Router::new().route("/r/{token}/mcp", post(|| async { StatusCode::NO_CONTENT }));
        let model = axum::Router::new().fallback(|| async { StatusCode::UNAUTHORIZED });
        let (ingress, internal) = split_routes(control, tools, model);
        for path in ["/r/known/mcp", "/r/known/v1/messages"] {
            let request = Request::post(path).body(Body::empty()).unwrap();
            assert_eq!(
                ingress.clone().oneshot(request).await.unwrap().status(),
                StatusCode::NOT_FOUND
            );
        }
        let request = Request::post("/r/known/mcp").body(Body::empty()).unwrap();
        assert_eq!(
            internal.clone().oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let request = Request::post("/r/known/v1/messages")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            internal.oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let request = Request::post("/command").body(Body::empty()).unwrap();
        assert_eq!(
            ingress.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}
