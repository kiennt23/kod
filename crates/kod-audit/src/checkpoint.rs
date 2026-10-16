//! External checkpoints of the chain head. The target must be write-once and
//! held under a different account from the harness hosts: that separation,
//! not this code, is what makes a rewrite of the log detectable.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};
use tempfile::NamedTempFile;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Sequence number of the head entry when the checkpoint was taken.
    pub seq: u64,
    /// That entry's chain hash, lower-case hex.
    pub hash: String,
    /// Unix seconds.
    pub at: i64,
}

pub trait CheckpointTarget {
    /// Publishes a complete, durable checkpoint atomically, without overwrite.
    fn put(&self, cp: &Checkpoint) -> Result<()>;
    /// Every stored checkpoint, in seq order. Fails on anything unreadable.
    fn list(&self) -> Result<Vec<Checkpoint>>;
}

/// One file per checkpoint, staged and synced before no-overwrite publication.
/// It is the test fake, and the production adapter when the directory is a
/// write-once mount owned by another account.
pub struct DirTarget {
    pub dir: PathBuf,
}

impl DirTarget {
    fn stage(&self, cp: &Checkpoint) -> Result<NamedTempFile> {
        let mut f = tempfile::Builder::new()
            .prefix(".checkpoint-")
            .tempfile_in(&self.dir)?;
        f.write_all(&serde_json::to_vec(cp)?)?;
        let mut perms = f.as_file().metadata()?.permissions();
        perms.set_readonly(true);
        f.as_file().set_permissions(perms)?;
        f.as_file().sync_all()?;
        Ok(f)
    }
}

impl CheckpointTarget for DirTarget {
    fn put(&self, cp: &Checkpoint) -> Result<()> {
        let path = self.dir.join(format!("{:020}.json", cp.seq));
        self.stage(cp)?
            .persist_noclobber(&path)
            .with_context(|| format!("write-once checkpoint {}", path.display()))?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Reads every `<20 digits>.json` file. Other names (`lost+found`,
    /// `.DS_Store`, NFS temp files) are not checkpoints and are skipped.
    fn list(&self) -> Result<Vec<Checkpoint>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("checkpoint directory {}", self.dir.display()))?
        {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_checkpoint = name
                .strip_suffix(".json")
                .is_some_and(|stem| stem.len() == 20 && stem.bytes().all(|b| b.is_ascii_digit()));
            if !is_checkpoint {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let cp: Checkpoint = serde_json::from_str(&text)
                .with_context(|| format!("unreadable checkpoint {}", path.display()))?;
            if path.file_name() != Some(format!("{:020}.json", cp.seq).as_ref()) {
                bail!("checkpoint {} names the wrong seq", path.display());
            }
            out.push(cp);
        }
        out.sort_by_key(|cp| cp.seq);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_checkpoint_is_invisible_until_published() {
        let dir = tempfile::tempdir().unwrap();
        let target = DirTarget {
            dir: dir.path().to_owned(),
        };
        let cp = Checkpoint {
            seq: 1,
            hash: "a".repeat(64),
            at: 0,
        };
        let staged = target.stage(&cp).unwrap();
        assert!(target.list().unwrap().is_empty());
        drop(staged);
        assert!(target.list().unwrap().is_empty());
        target.put(&cp).unwrap();
        assert_eq!(target.list().unwrap(), vec![cp.clone()]);
        assert!(target.put(&cp).is_err());
        assert_eq!(target.list().unwrap(), vec![cp]);
    }

    #[test]
    fn interrupted_staging_does_not_poison_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let target = DirTarget {
            dir: dir.path().to_owned(),
        };
        fs::write(dir.path().join(".checkpoint-interrupted"), "{\"seq\":").unwrap();
        let cp = Checkpoint {
            seq: 1,
            hash: "a".repeat(64),
            at: 0,
        };
        target.put(&cp).unwrap();
        assert_eq!(target.list().unwrap(), vec![cp]);
    }
}
