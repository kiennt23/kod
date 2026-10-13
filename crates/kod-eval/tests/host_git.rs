//! Host git must not run anything the host's config or a repository's hooks
//! name, since score_work holds the agent's .gitattributes. One test per
//! binary: it sets an environment variable for the whole process.
use kod_eval::workspace::{git, prepare};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn host_git_ignores_global_config_and_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");
    let global = tmp.path().join("gitconfig");
    fs::write(
        &global,
        format!(
            "[filter \"probe\"]\n\tclean = \"touch {0}; cat\"\n[core]\n\thooksPath = {1}\n",
            marker.display(),
            tmp.path().join("hooks").display()
        ),
    )
    .unwrap();
    let hook = tmp.path().join("hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &global) };

    let m = tmp.path().join("mirror");
    fs::create_dir_all(&m).unwrap();
    git(&m, &["init", "-q"]).unwrap();
    // A filter named by the agent's .gitattributes, defined in the host config.
    fs::write(m.join(".gitattributes"), "* filter=probe\n").unwrap();
    fs::write(m.join("a.txt"), "a\n").unwrap();
    git(&m, &["add", "-A"]).unwrap();
    git(
        &m,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "base",
        ],
    )
    .unwrap();
    // A repository hook, too.
    let local_hook = m.join(".git/hooks/pre-commit");
    fs::copy(&hook, &local_hook).unwrap();
    git(
        &m,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    )
    .unwrap();
    let base = git(&m, &["rev-parse", "HEAD"]).unwrap();
    prepare(&m, base.trim(), &tmp.path().join("w")).unwrap();
    assert!(!marker.exists(), "host git ran a filter or hook");
}
