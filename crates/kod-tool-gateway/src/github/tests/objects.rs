use super::fixture::Remote;
use axum::{body::Body, response::Response};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
pub(super) fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "kod-agent")
        .env("GIT_AUTHOR_EMAIL", "agent@kod.invalid")
        .env("GIT_COMMITTER_NAME", "kod-agent")
        .env("GIT_COMMITTER_EMAIL", "agent@kod.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
pub(super) fn seed(repo: &Path) {
    std::fs::create_dir(repo).unwrap();
    git(repo, &["init", "--initial-branch=main"], None);
    std::fs::create_dir(repo.join("bin")).unwrap();
    for (p, text) in [
        ("bin/check", "#!/bin/sh\nexit 0\n"),
        ("old.txt", "old\n"),
        ("untouched.txt", "keep\n"),
    ] {
        std::fs::write(repo.join(p), text).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        repo.join("bin/check"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git(repo, &["add", "."], None);
    git(repo, &["commit", "-m", "base"], None);
}
fn response(status: u16, body: Value) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(body.to_string()))
        .unwrap()
}
pub(super) fn handle(s: &mut Remote, method: &str, path: &str, body: &Value) -> Response {
    let p = path.trim_start_matches("/repos/acme/app/git/");
    if method == "GET" {
        if let Some(sha) = p.strip_prefix("commits/") {
            if !super::super::sha(sha)
                || !Command::new("git")
                    .arg("-C")
                    .arg(&s.repo)
                    .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            {
                return response(404, json!({}));
            }
            return response(
                200,
                json!({"sha":sha,"tree":{"sha":git(&s.repo,&["rev-parse",&format!("{sha}^{{tree}}")],None)}}),
            );
        }
        if let Some(sha) = p.strip_prefix("trees/") {
            let tree = git(&s.repo, &["ls-tree", "-r", "-t", sha], None);
            let entries: Vec<Value> = tree
                .lines()
                .map(|line| {
                    let (head, path) = line.split_once('\t').unwrap();
                    let v: Vec<_> = head.split(' ').collect();
                    json!({"mode":v[0],"type":v[1],"sha":v[2],"path":path})
                })
                .collect();
            return response(
                200,
                json!({"sha":sha,"truncated":s.truncated,"tree":entries}),
            );
        }
        if let Some(sha) = p.strip_prefix("blobs/") {
            let out = Command::new("git")
                .arg("-C")
                .arg(&s.repo)
                .args(["cat-file", "blob", sha])
                .output()
                .unwrap();
            return response(
                200,
                json!({"sha":sha,"encoding":"base64","content":STANDARD.encode(&out.stdout),"size":out.stdout.len()}),
            );
        }
    }
    if method == "POST" && p == "trees" {
        git(
            &s.repo,
            &["read-tree", body["base_tree"].as_str().unwrap()],
            None,
        );
        for entry in body["tree"].as_array().unwrap() {
            let path = entry["path"].as_str().unwrap();
            if let Some(text) = entry.get("content") {
                let sha = git(
                    &s.repo,
                    &["hash-object", "-w", "--stdin"],
                    Some(text.as_str().unwrap().as_bytes()),
                );
                git(
                    &s.repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        entry["mode"].as_str().unwrap(),
                        &sha,
                        path,
                    ],
                    None,
                );
            } else {
                git(
                    &s.repo,
                    &["update-index", "--force-remove", "--", path],
                    None,
                );
            }
        }
        return response(201, json!({"sha":git(&s.repo,&["write-tree"],None)}));
    }
    if method == "POST" && p == "commits" {
        let mut command = Command::new("git");
        command.arg("-C").arg(&s.repo).args([
            "commit-tree",
            body["tree"].as_str().unwrap(),
            "-p",
            body["parents"][0].as_str().unwrap(),
        ]);
        for (prefix, field) in [("AUTHOR", "author"), ("COMMITTER", "committer")] {
            for (suffix, key) in [("NAME", "name"), ("EMAIL", "email"), ("DATE", "date")] {
                command.env(
                    format!("GIT_{prefix}_{suffix}"),
                    body[field][key].as_str().unwrap(),
                );
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body["message"].as_str().unwrap().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        return response(
            201,
            json!({"sha":String::from_utf8(out.stdout).unwrap().trim()}),
        );
    }
    response(404, json!({}))
}
pub(super) fn verify_jwt(public: &Path, jwt: &str) {
    let parts: Vec<_> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["iss"], 5);
    assert_eq!(claims["iat"], 1_799_999_940i64);
    assert_eq!(claims["exp"], 1_800_000_540i64);
    let sig = public.with_extension("sig");
    std::fs::write(&sig, URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    let mut p = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(public)
        .arg("-signature")
        .arg(sig)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    p.stdin
        .take()
        .unwrap()
        .write_all(format!("{}.{}", parts[0], parts[1]).as_bytes())
        .unwrap();
    assert!(p.wait().unwrap().success());
}
