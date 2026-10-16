#![allow(dead_code)]
use kod_audit::{AuditLog, checkpoint::DirTarget};
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::TempDir;

/// A fresh log and an empty checkpoint directory in one temp dir.
pub struct Fixture {
    pub dir: TempDir,
    pub log: AuditLog,
    pub target: DirTarget,
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(&dir.path().join("audit.db")).unwrap();
    let cps = dir.path().join("checkpoints");
    std::fs::create_dir(&cps).unwrap();
    Fixture {
        log,
        target: DirTarget { dir: cps },
        dir,
    }
}

impl Fixture {
    pub fn db(&self) -> PathBuf {
        self.dir.path().join("audit.db")
    }

    /// A raw connection with the append-only triggers dropped: what someone
    /// with write access to the host can do.
    pub fn attacker(&self) -> Connection {
        let c = Connection::open(self.db()).unwrap();
        c.execute_batch(
            "DROP TRIGGER audit_no_delete; DROP TRIGGER audit_only_purge;
             DROP TRIGGER IF EXISTS audit_append_only;",
        )
        .unwrap();
        c
    }
}
