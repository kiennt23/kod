use super::{ConnectedSpec, SpawnGate, launch, names};
use anyhow::{Result, ensure};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};
pub fn regular_bytes(path: &Path, cap: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= u64::try_from(cap)?,
        "invalid bounded regular file"
    );
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && meta.dev() == opened.dev() && meta.ino() == opened.ino(),
        "file changed while opening"
    );
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap)? + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "file too large");
    Ok(bytes)
}
fn phase(
    spec: &ConnectedSpec,
    gate: &SpawnGate,
    stopped: &dyn Fn() -> Result<bool>,
    export: bool,
) -> Result<PathBuf> {
    let kind = if export { "export" } else { "collect" };
    let out = spec.dir.join(format!("{kind}_out"));
    std::fs::create_dir(&out)?;
    let base = spec.dir.join("base_tree");
    if !export {
        crate::workspace::extract(&spec.mirror, &spec.base, &base)?;
    }
    let input = spec
        .dir
        .join(if export { "collect_out" } else { "agent_out" });
    let meta = std::fs::metadata(&spec.dir)?;
    let mut budget = spec.budget.clone();
    budget.memory = crate::sandbox::collect_memory(&budget);
    let names = names(&spec.run)?;
    let name = &names[if export { 2 } else { 1 }];
    let mut args = crate::sandbox::budget_args(
        name,
        &spec.run,
        &budget,
        "none",
        &crate::sandbox::Mounts {
            read_only: &[
                (&base, "/in"),
                (&input, if export { "/patch" } else { "/agent" }),
            ],
            out: &out,
            tmpfs: &["/tmp", "/base", "/wt"],
        },
        (meta.uid(), meta.gid()),
        &[("KOD_BASE".into(), spec.base.clone())],
    );
    args.extend([
        spec.collector_image.clone(),
        if export {
            "collect-export".into()
        } else {
            "collect".into()
        },
    ]);
    ensure!(
        launch(
            &args,
            name,
            Duration::from_secs(spec.budget.timeout_secs),
            gate,
            stopped
        )?
        .success,
        "collector failed"
    );
    regular_bytes(&out.join(format!("{kind}_ok")), 64)?;
    if !export {
        regular_bytes(&out.join("diff.patch"), 32 << 20)?;
    }
    Ok(out)
}
pub fn collect(
    spec: &ConnectedSpec,
    gate: &SpawnGate,
    stopped: &dyn Fn() -> Result<bool>,
) -> Result<PathBuf> {
    let output = spec.dir.join("agent_out");
    let exit = regular_bytes(&output.join("exit_code"), 64)?;
    let procs = regular_bytes(&output.join("agent_procs"), 64)?;
    ensure!(std::str::from_utf8(&exit)?.trim() == "0", "agent failed");
    crate::run::process_gate(Some(std::str::from_utf8(&procs)?)).map_err(anyhow::Error::msg)?;
    ensure!(
        !super::process::present(
            &names(&spec.run)?[0],
            std::time::Instant::now() + super::ABSENCE
        )?,
        "agent still present"
    );
    phase(spec, gate, stopped, false)
}
pub fn export(
    spec: &ConnectedSpec,
    gate: &SpawnGate,
    stopped: &dyn Fn() -> Result<bool>,
) -> Result<PathBuf> {
    phase(spec, gate, stopped, true)
}
