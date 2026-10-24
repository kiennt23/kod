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

pub async fn recover_and_attach(
    runtime: Arc<crate::runtime::Runtime>,
    open_dispatcher: impl FnOnce() -> Result<kod_dispatcher::Dispatcher> + Send + 'static,
) -> Result<Arc<kod_dispatcher::Dispatcher>> {
    tokio::task::spawn_blocking(move || {
        let dispatcher = Arc::new(recover_then_open(
            || runtime.recover_local(),
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
