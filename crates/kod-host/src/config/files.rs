use anyhow::{Context, Result, ensure};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};
const LIMIT: u64 = 1024 * 1024;

fn owner(metadata: &Metadata) -> Result<()> {
    // SAFETY: geteuid has no arguments, pointers, or caller preconditions.
    let uid = unsafe { libc::geteuid() };
    ensure!(metadata.uid() == uid, "path must belong to the host owner");
    Ok(())
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "configured path must be absolute");
    let mut prefix = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "path must be canonical"
        );
        prefix.push(component);
        let metadata = std::fs::symlink_metadata(&prefix).context("configured path is missing")?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "symlink path component refused"
        );
        ensure!(
            !metadata.is_dir() || metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0,
            "writable path ancestor permits replacement"
        );
    }
    Ok(path.canonicalize()?)
}
pub(super) fn private_dir(path: &Path) -> Result<PathBuf> {
    let path = canonical(path)?;
    let metadata = std::fs::metadata(&path)?;
    owner(&metadata)?;
    ensure!(
        metadata.is_dir() && metadata.mode() & 0o777 == 0o700,
        "directory must be owner-only mode 0700"
    );
    Ok(path)
}
fn private_metadata(metadata: &Metadata) -> Result<()> {
    owner(metadata)?;
    ensure!(
        metadata.is_file() && metadata.mode() & 0o077 == 0,
        "secret must be an owner-only regular file"
    );
    Ok(())
}
pub fn private_file(path: &Path) -> Result<()> {
    private_metadata(&std::fs::symlink_metadata(path)?)
}
fn open(path: &Path) -> Result<File> {
    canonical(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "configured path must be a regular file"
    );
    Ok(file)
}
fn bounded(file: File) -> Result<Vec<u8>> {
    ensure!(
        file.metadata()?.len() <= LIMIT,
        "configuration file exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= LIMIT,
        "configuration file exceeds 1 MiB"
    );
    Ok(bytes)
}
pub fn read_local(path: &Path) -> Result<Vec<u8>> {
    bounded(open(path)?)
}
pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    private_dir(path.parent().context("missing private parent")?)?;
    let file = open(path)?;
    private_metadata(&file.metadata()?)?;
    bounded(file)
}
pub(super) fn database(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "database path must be absolute");
    let parent = private_dir(path.parent().context("missing database parent")?)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            private_file(path)?;
            canonical(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(parent.join(path.file_name().context("missing database filename")?))
        }
        Err(error) => Err(error.into()),
    }
}
