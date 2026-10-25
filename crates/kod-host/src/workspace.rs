pub fn supported_patch(bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::ensure;
    ensure!(
        bytes.len() <= kod_tool_gateway::MAX_PUBLISH_BYTES * 4,
        "patch too large"
    );
    let text = std::str::from_utf8(bytes)?;
    ensure!(!text.contains('\0'), "binary patch");
    let mut headers = true;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            headers = true;
            continue;
        }
        if line.starts_with("@@ ") {
            headers = false;
            continue;
        }
        if !headers {
            continue;
        }
        ensure!(
            ![
                "old mode ",
                "new mode ",
                "rename from ",
                "rename to ",
                "copy from ",
                "copy to ",
                "GIT binary patch",
                "Binary files "
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix)),
            "unsupported patch header"
        );
        if let Some(mode) = line.strip_prefix("new file mode ") {
            ensure!(mode == "100644", "unsupported new file mode");
        }
        if let Some(mode) = line.strip_prefix("deleted file mode ") {
            ensure!(
                matches!(mode, "100644" | "100755"),
                "unsupported deleted file mode"
            );
        }
        if line.starts_with("index ")
            && let Some(mode) = line.split_whitespace().nth(2)
        {
            ensure!(
                matches!(mode, "100644" | "100755"),
                "unsupported indexed mode"
            );
        }
    }
    Ok(())
}

use anyhow::{Context, Result, ensure};
use kod_tool_gateway::forge::{Change, Collected, Workspace};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Mutex,
};
#[derive(Default)]
pub struct CollectedWorkspace {
    entries: Mutex<HashMap<String, Collected>>,
}
impl CollectedWorkspace {
    pub(crate) fn insert(&self, run: &str, collected: Collected) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = entries.get(run) {
            ensure!(old == &collected, "collected result changed");
        } else {
            entries.insert(run.into(), collected);
        }
        Ok(())
    }
}
#[cfg(feature = "test-support")]
impl CollectedWorkspace {
    pub fn insert_for_test(&self, run: &str, collected: Collected) -> Result<()> {
        self.insert(run, collected)
    }
}
impl Workspace for CollectedWorkspace {
    fn collected(&self, run: &str) -> Result<Collected> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(run)
            .cloned()
            .context("no trusted collected result")
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    base: String,
    files: Vec<Entry>,
}
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Entry {
    Blob(Blob),
    Deleted(Deleted),
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Blob {
    path: String,
    mode: String,
    blob: usize,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Deleted {
    path: String,
    deleted: bool,
}
pub(crate) fn read_export(dir: &Path, base: &str) -> Result<Collected> {
    use kod_eval::connected::regular_bytes;
    regular_bytes(&dir.join("export_ok"), 64)?;
    let manifest: Manifest =
        crate::config::strict_json(&regular_bytes(&dir.join("manifest.json"), 1 << 20)?)?;
    ensure!(
        manifest.version == 1 && manifest.base == base && manifest.files.len() <= 1000,
        "invalid export binding"
    );
    let mut seen = HashSet::new();
    let mut changes = Vec::new();
    let mut total = 0;
    for (index, entry) in manifest.files.into_iter().enumerate() {
        let (path, content) = match entry {
            Entry::Deleted(e) => {
                ensure!(e.deleted, "invalid deletion");
                (e.path, None)
            }
            Entry::Blob(e) => {
                ensure!(
                    e.blob == index && matches!(e.mode.as_str(), "100644" | "100755"),
                    "invalid blob"
                );
                let bytes = regular_bytes(&dir.join(index.to_string()), (8 << 20) - total)?;
                total += bytes.len();
                let text = String::from_utf8(bytes)?;
                ensure!(!text.contains('\0'), "binary blob");
                (e.path, Some(text))
            }
        };
        ensure!(
            !path.is_empty()
                && !path.contains('\0')
                && path.split('/').all(|p| !p.is_empty()
                    && p != "."
                    && p != ".."
                    && !p.eq_ignore_ascii_case(".git")),
            "invalid path"
        );
        ensure!(seen.insert(path.clone()), "duplicate path");
        changes.push(Change { path, content });
    }
    Ok(Collected { changes, test_evidence: "Trusted networkless collection and index export succeeded. Project tests were not run by the host.".into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_is_insert_or_compare_and_missing_is_error() {
        let workspace = CollectedWorkspace::default();
        assert!(workspace.collected("run-1").is_err());
        let c = Collected {
            changes: vec![],
            test_evidence: "host".into(),
        };
        workspace.insert("run-1", c.clone()).unwrap();
        workspace.insert("run-1", c.clone()).unwrap();
        assert!(
            workspace
                .insert(
                    "run-1",
                    Collected {
                        test_evidence: "other".into(),
                        ..c.clone()
                    }
                )
                .is_err()
        );
        assert_eq!(workspace.collected("run-1").unwrap(), c);
    }
    #[test]
    fn forged_manifest_and_wrong_base_cannot_be_collected() {
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("export_ok"), "").unwrap();
        for manifest in [
            r#"{"version":1,"base":"wrong","files":[]}"#,
            r#"{"version":1,"base":"base","files":[{"path":"../secret","mode":"100644","blob":0}]}"#,
            r#"{"version":1,"version":1,"base":"base","files":[]}"#,
            r#"{"version":1,"base":"base","files":[{"path":"x","mode":"120000","blob":0}]}"#,
        ] {
            std::fs::write(out.path().join("manifest.json"), manifest).unwrap();
            std::fs::write(out.path().join("0"), "text").unwrap();
            assert!(read_export(out.path(), "base").is_err());
        }
        std::fs::remove_file(out.path().join("0")).unwrap();
        std::os::unix::fs::symlink("manifest.json", out.path().join("0")).unwrap();
        std::fs::write(
            out.path().join("manifest.json"),
            r#"{"version":1,"base":"base","files":[{"path":"x","mode":"100644","blob":0}]}"#,
        )
        .unwrap();
        assert!(read_export(out.path(), "base").is_err());
    }
}
