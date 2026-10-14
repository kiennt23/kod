//! Embeds the git commit kod-eval was built from, for run manifests.
fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if let Some(sha) = sha {
        println!("cargo:rustc-env=KOD_GIT_SHA={sha}");
    }
    // ponytail: misses a dirty tree; add `git status` if builds from one matter.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
