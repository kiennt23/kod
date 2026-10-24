use super::*;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn record() -> store::Record {
    store::Record::new(
        RunPermit {
            run: "storage-run".into(),
            ticket: "acme/app#7".into(),
            repo: "acme/app".into(),
            base: "a".repeat(40),
        },
        1_800_000_000,
    )
}

fn pending(root: &Path) -> std::path::PathBuf {
    root.join(format!("{:x}.json.pending", Sha256::digest(b"storage-run")))
}

fn private_file(path: &Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn startup_discards_partial_write_and_preserves_committed_ambiguity() {
    // Given a committed issuance intent and an incomplete replacement after a crash.
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let store = store::Store::open(&root).unwrap();
    let mut committed = record();
    committed.pending = Some(committed.updated_at + 3660);
    committed.pr = Some(store::Intent {
        sha: "b".repeat(40),
        branch: committed.permit.branch(),
        base: "main".into(),
        url: None,
        created: None,
        blocked: true,
    });
    store.save(&committed).unwrap();
    private_file(&pending(&root), br#"{"version":1,"pending":null"#);
    drop(store);

    // When restarting and loading the lease directory.
    let restarted = store::Store::open(&root).unwrap();
    let loaded = restarted.load().unwrap();

    // Then only the committed intent survives and its pending slot is reusable.
    assert_eq!(loaded["storage-run"].pending, committed.pending);
    assert!(loaded["storage-run"].pr.as_ref().unwrap().blocked);
    assert!(!pending(&root).exists());
    restarted.save(&loaded["storage-run"]).unwrap();
}

#[test]
fn startup_discards_orphaned_partial_first_write() {
    // Given a crash before the first record was committed.
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    private_file(&pending(&root), b"{");
    let store = store::Store::open(&root).unwrap();

    // When loading records.
    let loaded = store.load().unwrap();

    // Then no uncommitted record is promoted.
    assert!(loaded.is_empty());
    assert!(!pending(&root).exists());
}

#[test]
fn save_replaces_stale_partial_write_under_held_lock() {
    // Given an interrupted save while this store retains the directory lock.
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let store = store::Store::open(&root).unwrap();
    private_file(&pending(&root), b"{");

    // When retrying the save.
    store.save(&record()).unwrap();

    // Then the newly committed record loads normally.
    assert_eq!(
        store.load().unwrap()["storage-run"].permit.run,
        "storage-run"
    );
    assert!(!pending(&root).exists());
}

#[test]
fn startup_rejects_untrusted_pending_entries_without_removing_them() {
    for case in ["unknown", "wrong-name", "public", "symlink"] {
        // Given an entry that is not a trusted pending lease file.
        let dir = directory();
        let root = dir.path().canonicalize().unwrap();
        let path = match case {
            "unknown" => root.join("unknown"),
            "wrong-name" => root.join("invalid.json.pending"),
            _ => pending(&root),
        };
        if case == "symlink" {
            std::os::unix::fs::symlink(root.join("missing"), &path).unwrap();
        } else {
            private_file(&path, b"{");
            if case == "public" {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let store = store::Store::open(&root).unwrap();

        // When loading the directory.
        let result = store.load();

        // Then the entry fails closed and remains untouched.
        assert!(result.is_err(), "{case}");
        assert!(fs::symlink_metadata(&path).is_ok(), "{case}");
    }
}

#[test]
fn concurrent_owner_cannot_clean_pending_files() {
    // Given a live owner with a partially written lease.
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let _owner = store::Store::open(&root).unwrap();
    private_file(&pending(&root), b"{");

    // When another store attempts to acquire ownership.
    let result = store::Store::open(&root);

    // Then lock acquisition fails without touching the active owner's write.
    assert!(result.is_err());
    assert_eq!(fs::read(pending(&root)).unwrap(), b"{");
}

#[test]
fn save_refuses_untrusted_pending_files() {
    // Given a pending filename occupied by an unsafe file or dangling symlink.
    for symlink in [false, true] {
        let dir = directory();
        let root = dir.path().canonicalize().unwrap();
        let store = store::Store::open(&root).unwrap();
        if symlink {
            std::os::unix::fs::symlink(root.join("missing"), pending(&root)).unwrap();
        } else {
            private_file(&pending(&root), b"{");
            fs::set_permissions(pending(&root), fs::Permissions::from_mode(0o644)).unwrap();
        }

        // When attempting to save a new record.
        let result = store.save(&record());

        // Then the unsafe entry remains and the save fails closed.
        assert!(result.is_err());
        assert!(fs::symlink_metadata(pending(&root)).is_ok());
    }
}

#[test]
fn old_records_load_with_default_concurrency_fields() {
    // Given a lease serialized before concurrency metadata was introduced.
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let store = store::Store::open(&root).unwrap();
    let mut value = serde_json::to_value(record()).unwrap();
    value.as_object_mut().unwrap().remove("generation");
    value["pr"] =
        serde_json::json!({"sha":"b".repeat(40),"branch":"branch","base":"main","url":null});
    private_file(
        &pending(&root).with_extension(""),
        &serde_json::to_vec(&value).unwrap(),
    );

    // When loading the existing record.
    let loaded = store.load().unwrap();

    // Then new fields have their backwards-compatible defaults.
    assert_eq!(loaded["storage-run"].generation, 0);
    let intent = loaded["storage-run"].pr.as_ref().unwrap();
    assert_eq!(intent.created, None);
    assert!(!intent.blocked);
}

#[test]
fn lease_expiry_accepts_bounded_skew_and_rejects_excess() {
    for (lifetime, accepted) in [(3720, true), (3721, false)] {
        // Given a lease with both token and unresolved issuance expiry.
        let dir = directory();
        let root = dir.path().canonicalize().unwrap();
        let store = store::Store::open(&root).unwrap();
        let mut record = record();
        record.pending = Some(record.updated_at + lifetime);
        record.tokens.push(store::Token {
            secret: "test-token".into(),
            expires: record.updated_at + lifetime,
            usable: false,
        });
        store.save(&record).unwrap();

        // When validating the persisted schema.
        let loaded = store.load();

        // Then only the configured skew window is accepted.
        assert_eq!(loaded.is_ok(), accepted, "{lifetime}");
    }
}

#[test]
fn signing_and_ownership_need_no_subprocesses() {
    for traditional in [true, false] {
        // Given a PKCS#1 (GitHub's download format) or PKCS#8 App key.
        let dir = directory();
        let root = dir.path().canonicalize().unwrap();
        let mut keygen = std::process::Command::new("openssl");
        keygen.args(["genrsa", "-out"]).arg(root.join("key.pem"));
        if traditional {
            keygen.arg("-traditional");
        }
        keygen.arg("2048");
        assert!(
            keygen
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        let public = root.join("pub.pem");
        assert!(
            std::process::Command::new("openssl")
                .args(["rsa", "-in"])
                .arg(root.join("key.pem"))
                .args(["-pubout", "-out"])
                .arg(&public)
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        fs::set_permissions(root.join("key.pem"), fs::Permissions::from_mode(0o600)).unwrap();

        // When ownership checks and signing run in a process with an empty PATH.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "github::tests::storage_review::signing_probe_child",
                "--nocapture",
            ])
            .env("PATH", "")
            .env("STORAGE_PROBE_ROOT", &root)
            .output()
            .unwrap();

        // Then they succeed in-process and the JWT verifies against the App key.
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let jwt = fs::read_to_string(root.join("jwt")).unwrap();
        super::objects::verify_jwt(&public, &jwt);
    }
}

#[test]
fn signing_probe_child() {
    let Some(root) = std::env::var_os("STORAGE_PROBE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let app = AppCredentials {
        app_id: 5,
        private_key: root.join("key.pem"),
    };
    store::Store::open(&root).unwrap().save(&record()).unwrap();
    signing::validate(&app.private_key).unwrap();
    let jwt = signing::jwt(&app, 1_800_000_000).unwrap();
    fs::write(root.join("jwt"), &jwt).unwrap();
    fs::set_permissions(&app.private_key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(signing::jwt(&app, 1_800_000_000).is_err());
}

#[test]
fn dropping_store_releases_lock_while_forked_child_has_not_exec_yet() {
    // Given an open store and a concurrently forked child that still shares its lock file
    // description (O_CLOEXEC only closes it at exec, as with any parallel Command::spawn).
    use std::{io::Read, os::fd::AsRawFd, os::unix::process::CommandExt};
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let store = store::Store::open(&root).unwrap();
    let (mut forked, signal) = std::io::pipe().unwrap();
    let fd = signal.as_raw_fd();
    let mut command = std::process::Command::new("true");
    // SAFETY: the hook only calls async-signal-safe write and nanosleep.
    unsafe {
        command.pre_exec(move || {
            libc::write(fd, b"x".as_ptr().cast(), 1);
            std::thread::sleep(std::time::Duration::from_secs(2));
            Ok(())
        });
    }
    let child = std::thread::spawn(move || command.status().unwrap());
    forked.read_exact(&mut [0]).unwrap();

    // When the owner drops the store and the host restarts immediately.
    drop(store);
    let reopened = store::Store::open(&root);

    // Then the lease directory is acquirable without waiting for the child.
    assert!(reopened.is_ok(), "{:#}", reopened.err().unwrap());
    drop(signal);
    assert!(child.join().unwrap().success());
}
