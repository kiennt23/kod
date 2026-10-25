//! Bounded subprocesses: acceptance never waits unboundedly on docker, git, tar or openssl.
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

pub const SHORT: Duration = Duration::from_secs(30);
pub const LONG: Duration = Duration::from_secs(120);

/// Runs `cmd` to completion or kills it after `limit`. stdout/stderr are captured.
pub fn output(cmd: &mut Command, input: Option<&[u8]>, limit: Duration) -> Result<Output> {
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().context("stdout")?;
    let mut err = child.stderr.take().context("stderr")?;
    let (so, se) = (
        std::thread::spawn(move || {
            let mut b = vec![];
            let _ = out.read_to_end(&mut b);
            b
        }),
        std::thread::spawn(move || {
            let mut b = vec![];
            let _ = err.read_to_end(&mut b);
            b
        }),
    );
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().context("stdin")?;
        let bytes = bytes.to_vec();
        // Detached: a child that never reads must not block the bound.
        std::thread::spawn(move || stdin.write_all(&bytes));
    }
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            // Reader threads are detached: a grandchild holding the pipe cannot extend the bound.
            bail!("subprocess exceeded {}s and was killed", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        status,
        stdout: so.join().unwrap_or_default(),
        stderr: se.join().unwrap_or_default(),
    })
}

/// `output` that also requires a zero exit status.
pub fn success(cmd: &mut Command, limit: Duration) -> Result<bool> {
    Ok(output(cmd, None, limit)?.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hung_child_is_killed_at_the_bound() {
        let began = Instant::now();
        let e = output(
            Command::new("sleep").arg("30"),
            None,
            Duration::from_millis(300),
        );
        assert!(e.is_err() && began.elapsed() < Duration::from_secs(5));
        let ok = output(Command::new("echo").arg("hi"), None, SHORT).unwrap();
        assert_eq!(ok.stdout, b"hi\n");
    }
}
