use anyhow::{Context, Result, ensure};
use std::{
    process::Command,
    time::{Duration, Instant},
};

/// Exercises the production stop path against an owned, TERM-resistant container.
/// Returns measured stop time only after Docker confirms absence.
pub fn resistant_container(image: &str) -> Result<Duration> {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let run = format!("run-{}", u64::try_from(suffix % u128::from(u64::MAX))?);
    let name = kod_eval::connected::names(&run)?
        .into_iter()
        .next()
        .context("missing container name")?;
    let created = Command::new("docker")
        .args([
            "run",
            "--detach",
            "--rm",
            "--network",
            "none",
            "--name",
            &name,
            "--label",
            &format!("kod.run={run}"),
            "--entrypoint",
            "sh",
            image,
            "-c",
            "trap '' TERM; echo ready; while :; do sleep 1; done",
        ])
        .output()?;
    ensure!(
        created.status.success(),
        "resistant container creation failed"
    );
    let result = (|| {
        // The emitted marker proves the TERM trap is installed before stop.
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let logs = Command::new("docker").args(["logs", &name]).output()?;
            ensure!(logs.status.success(), "container readiness query failed");
            if logs.stdout.starts_with(b"ready") {
                break;
            }
            ensure!(
                Instant::now() < ready_deadline,
                "container readiness timed out"
            );
            std::thread::yield_now();
        }
        let began = Instant::now();
        kod_eval::connected::stop_owned(
            &run,
            std::slice::from_ref(&name),
            crate::deadlines::TERM_GRACE,
            crate::deadlines::ABSENCE,
        )?;
        let elapsed = began.elapsed();
        ensure!(
            elapsed >= crate::deadlines::TERM_GRACE,
            "TERM-resistant container stopped before KILL grace"
        );
        ensure!(
            elapsed <= crate::deadlines::ABSENCE,
            "container absence exceeded deadline"
        );
        let inventory = Command::new("docker")
            .args(["ps", "-aq", "--filter", &format!("name=^/{name}$")])
            .output()?;
        ensure!(
            inventory.status.success() && inventory.stdout.is_empty(),
            "Docker did not confirm absence"
        );
        Ok(elapsed)
    })();
    if result.is_err() {
        let cleanup = Command::new("docker").args(["rm", "-f", &name]).output()?;
        ensure!(
            cleanup.status.success(),
            "failed stop also left cleanup unconfirmed"
        );
    }
    result
}
