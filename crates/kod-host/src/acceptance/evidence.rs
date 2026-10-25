use anyhow::{Context, Result, ensure};
use std::{
    fs::{DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

pub fn create(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = path.parent().context("missing evidence parent")?;
    let mut prefix = PathBuf::new();
    for part in parent.components() {
        ensure!(
            !matches!(part, Component::ParentDir | Component::CurDir),
            "noncanonical evidence path"
        );
        prefix.push(part);
        ensure!(
            !std::fs::symlink_metadata(&prefix)?.file_type().is_symlink(),
            "evidence symlink refused"
        );
    }
    DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.join("report.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path.join("report.json"))?;
    Ok(())
}
