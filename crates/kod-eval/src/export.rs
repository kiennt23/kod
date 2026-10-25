use anyhow::{Context, Result, ensure};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};
const BYTES: usize = 8 << 20;
fn git_bytes(repo: &Path, args: &[&str], cap: usize) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .context("missing git stdout")?
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > cap {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!(
        bytes.len() <= cap && status.success(),
        "bounded git export failed"
    );
    Ok(bytes)
}
pub fn export_index(repo: &Path, out: &Path, base: &str) -> Result<()> {
    ensure!(
        out.is_dir() && std::fs::read_dir(out)?.next().is_none(),
        "export directory must be empty"
    );
    ensure!(
        base.len() == 40
            && base
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid export base"
    );
    let raw = git_bytes(
        repo,
        &[
            "diff",
            "--cached",
            "--raw",
            "-z",
            "--no-abbrev",
            "--no-renames",
            "HEAD",
        ],
        BYTES,
    )?;
    let mut fields = raw.split(|b| *b == 0);
    let mut files = Vec::new();
    let mut total = 0;
    let mut seen = std::collections::HashSet::new();
    while let Some(header) = fields.next() {
        if header.is_empty() {
            break;
        }
        ensure!(files.len() < 1000, "too many export files");
        let header = std::str::from_utf8(header)?;
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(parts.len() == 5, "invalid raw change");
        let old = parts[0].strip_prefix(':').context("missing mode prefix")?;
        let new = parts[1];
        let status = parts[4];
        ensure!(matches!(status, "A" | "M" | "D"), "unsupported change kind");
        let path = std::str::from_utf8(fields.next().context("missing path")?)?;
        ensure!(
            !path.is_empty()
                && path.split('/').all(|p| !p.is_empty()
                    && p != "."
                    && p != ".."
                    && !p.eq_ignore_ascii_case(".git")),
            "invalid export path"
        );
        ensure!(seen.insert(path.to_owned()), "duplicate export path");
        match status {
            "A" => ensure!(old == "000000" && new == "100644", "unsupported added mode"),
            "M" => ensure!(
                matches!(old, "100644" | "100755") && old == new,
                "changed file mode"
            ),
            "D" => ensure!(
                matches!(old, "100644" | "100755") && new == "000000",
                "unsupported deleted mode"
            ),
            _ => unreachable!(),
        }
        if status == "D" {
            files.push(serde_json::json!({"path":path,"deleted":true}));
            continue;
        }
        let oid = parts[3];
        ensure!(
            oid.len() == 40
                && oid
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid export blob id"
        );
        let blob = git_bytes(repo, &["cat-file", "blob", oid], BYTES - total)?;
        let text = std::str::from_utf8(&blob)?;
        ensure!(!text.contains('\0'), "binary export");
        total += blob.len();
        let id = files.len();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out.join(id.to_string()))?;
        f.write_all(&blob)?;
        f.sync_all()?;
        files.push(serde_json::json!({"path":path,"mode":new,"blob":id}));
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"base":base,"files":files}))?;
    std::fs::write(out.join("manifest.json"), bytes)?;
    Ok(())
}
