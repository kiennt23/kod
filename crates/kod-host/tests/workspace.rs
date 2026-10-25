use kod_host::workspace::supported_patch;
#[test]
fn unsupported_patch_intent_is_not_silently_lost() {
    for body in [
        "old mode 100644\nnew mode 100755\n",
        "rename from x\nrename to y\n",
        "copy from x\ncopy to y\n",
        "new file mode 120000\n",
        "new file mode 160000\n",
        "GIT binary patch\n",
        "Binary files a/x and b/x differ\n",
    ] {
        assert!(supported_patch(format!("diff --git a/x b/x\n{body}").as_bytes()).is_err());
    }
    assert!(supported_patch(b"diff --git a/x b/x\nindex aaa..bbb 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n").is_ok());
    assert!(
        supported_patch(
            b"diff --git a/x b/x\nindex aaa..bbb 100755\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"
        )
        .is_ok()
    );
    assert!(supported_patch(&[0xff]).is_err());
    assert!(supported_patch(b"\0").is_err());
}
#[test]
fn process_evidence_and_collection_cannot_be_forged_by_outcome() {
    use kod_host::runner::ready_to_collect;
    assert!(ready_to_collect(true, Some("0"), Some("0")).is_ok());
    assert!(ready_to_collect(false, Some("0"), Some("0")).is_err());
    assert!(ready_to_collect(true, Some("0"), None).is_err());
    assert!(ready_to_collect(true, Some("0"), Some("1")).is_err());
    assert!(ready_to_collect(true, Some("1"), Some("0")).is_err());
}
#[test]
fn supported_add_and_delete_patches_pass() {
    assert!(
        supported_patch(
            b"diff --git a/n b/n\nnew file mode 100644\nindex 0000000..bbb\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+new\n"
        )
        .is_ok()
    );
    for mode in ["100644", "100755"] {
        let patch = format!(
            "diff --git a/d b/d\ndeleted file mode {mode}\nindex aaa..0000000\n--- a/d\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n"
        );
        assert!(supported_patch(patch.as_bytes()).is_ok());
    }
}
