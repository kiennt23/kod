use super::RunPermit;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Token {
    pub secret: String,
    pub expires: i64,
    #[serde(skip)]
    pub usable: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    pub sha: String,
    pub branch: String,
    pub base: String,
    pub url: Option<String>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub blocked: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    #[serde(default)]
    pub generation: u64,
    pub permit: RunPermit,
    pub tokens: Vec<Token>,
    pub pending: Option<i64>,
    pub expected: Option<String>,
    pub input: Option<String>,
    pub pr: Option<Intent>,
    pub updated_at: i64,
}
impl Record {
    pub fn new(permit: RunPermit, now: i64) -> Self {
        Self {
            version: 1,
            generation: 0,
            permit,
            tokens: vec![],
            pending: None,
            expected: None,
            input: None,
            pr: None,
            updated_at: now,
        }
    }
}
pub(super) struct Store {
    root: PathBuf,
    _lock: File,
}
pub(super) fn owner() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}
pub(super) fn checked(path: &Path, directory: bool) -> Result<std::fs::Metadata> {
    let m = std::fs::symlink_metadata(path).context("private path unavailable")?;
    ensure!(
        !m.file_type().is_symlink()
            && (if directory { m.is_dir() } else { m.is_file() })
            && m.uid() == owner()
            && m.mode() & 0o077 == 0,
        "private path must be owner-only and not a symlink"
    );
    Ok(m)
}
pub(super) fn ancestors(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "private path must be absolute");
    for p in path.ancestors().skip(1) {
        let m = std::fs::symlink_metadata(p)?;
        ensure!(
            m.is_dir()
                && !m.file_type().is_symlink()
                && (m.uid() == 0 || m.uid() == owner())
                && (m.mode() & 0o022 == 0 || (m.uid() == 0 && m.mode() & 0o1000 != 0)),
            "untrusted private path ancestor"
        );
    }
    Ok(())
}
fn filename(run: &str) -> String {
    format!("{:x}.json", Sha256::digest(run.as_bytes()))
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        ancestors(root)?;
        checked(root, true)?;
        let path = root.join("owner.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        checked(&path, false)?;
        lock.try_lock()
            .context("GitHub lease directory already owned")?;
        Ok(Self {
            root: root.to_owned(),
            _lock: lock,
        })
    }
    pub fn load(&self) -> Result<BTreeMap<String, Record>> {
        let mut records = BTreeMap::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let p = entry.path();
            checked(&p, false)?;
            if entry.file_name() == "owner.lock" {
                continue;
            }
            if p.extension().is_some_and(|e| e == "pending") {
                self.remove_pending(&p)?;
                continue;
            }
            ensure!(
                p.extension().is_some_and(|e| e == "json"),
                "unexpected lease directory entry"
            );
            let mut bytes = Vec::new();
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&p)?
                .take((16 << 20) + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 16 << 20, "lease too large");
            let record: Record = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid GitHub lease"))?;
            ensure!(
                record.version == 1
                    && p.file_name()
                        .is_some_and(|f| f == filename(&record.permit.run).as_str())
                    && record.updated_at > 0
                    && record.pending.is_none_or(|until| until > record.updated_at
                        && until <= record.updated_at.saturating_add(3720))
                    && record.input.as_ref().is_none_or(|hash| hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    && record.tokens.iter().all(|t| !t.secret.is_empty()
                        && t.expires > 0
                        && t.expires <= record.updated_at.saturating_add(3720)),
                "invalid GitHub lease identity"
            );
            ensure!(
                records.insert(record.permit.run.clone(), record).is_none(),
                "duplicate lease"
            );
        }
        Ok(records)
    }
    fn remove_pending(&self, path: &Path) -> Result<()> {
        ensure!(
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json.pending"))
                .is_some_and(|hash| hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "unexpected lease directory entry"
        );
        checked(path, false)?;
        std::fs::remove_file(path)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub fn remove(&self, run: &str) -> Result<()> {
        let path = self.root.join(filename(run));
        checked(&path, false)?;
        std::fs::remove_file(&path)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub fn save(&self, record: &Record) -> Result<()> {
        checked(&self.root, true)?;
        let dest = self.root.join(filename(&record.permit.run));
        if dest.exists() {
            checked(&dest, false)?;
        }
        let tmp = self
            .root
            .join(format!("{}.pending", filename(&record.permit.run)));
        match std::fs::symlink_metadata(&tmp) {
            Ok(_) => self.remove_pending(&tmp)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        let result = (|| -> Result<()> {
            f.write_all(&serde_json::to_vec(record)?)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &dest)?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() && tmp.exists() {
            self.remove_pending(&tmp)?;
        }
        result.context("cannot persist GitHub lease")
    }
}
