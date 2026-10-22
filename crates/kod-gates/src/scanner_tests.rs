use super::*;
use std::{os::unix::fs::PermissionsExt, process::Command as Process};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn mutable_images_and_incomplete_scan_suites_are_refused() {
    assert!(ScanConfig::rust("rust:latest").validate().is_err());
    let mut cfg = ScanConfig::rust(&format!("sha256:{}", "a".repeat(64)));
    cfg.validate().unwrap();
    cfg.commands.remove("secret-scan");
    assert!(cfg.validate().is_err());
}

fn docker_fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, DockerScanner) {
    let image = std::env::var("KOD_GATE_TEST_IMAGE")
        .expect("build images/gates/Dockerfile and set KOD_GATE_TEST_IMAGE to its digest");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("fixture");
    std::fs::create_dir(&root).unwrap();
    for (path, text) in files {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), text).unwrap();
    }
    let archive = dir.path().join("source.tar.gz");
    assert!(
        Process::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .arg("fixture")
            .status()
            .unwrap()
            .success()
    );
    std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o444)).unwrap();
    let docker = PathBuf::from(
        std::env::var("KOD_DOCKER_BIN").unwrap_or_else(|_| "/usr/local/bin/docker".into()),
    );
    (
        dir,
        DockerScanner {
            config: ScanConfig::rust(&image),
            archive,
            sha: SHA.into(),
            docker,
            cache: None,
        },
    )
}

fn container(scanner: &DockerScanner, name: &str) -> Result<()> {
    scanner.config.validate()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(scanner.run_container(name))
}

#[test]
fn real_containers_are_isolated_and_nonzero_exit_and_timeout_fail() {
    if std::env::var("KOD_DOCKER_TESTS").as_deref() != Ok("1") {
        return;
    }
    let (_dir, mut scanner) = docker_fixture(&[("marker", "original\n")]);
    scanner.config.commands.insert("tests".into(), vec!["/bin/sh".into(), "-ec".into(),
        "test \"$(id -u)\" = 10001; test -z \"${KOD_GITHUB_TOKEN:-}\"; test ! -S /var/run/docker.sock; test \"$(cat marker)\" = original; test \"$(stat -f -c %T /work)\" != tmpfs; ! touch /opt/kod/host-write; ! touch /input/source.tar.gz; test -z \"$(ip route 2>/dev/null)\"; echo changed > marker".into()]);
    container(&scanner, "tests").unwrap();
    container(&scanner, "tests").unwrap();
    scanner.config.commands.insert(
        "tests".into(),
        vec!["/bin/sh".into(), "-c".into(), "exit 19".into()],
    );
    assert!(container(&scanner, "tests").is_err());
    scanner.config.timeout_secs = 1;
    scanner
        .config
        .commands
        .insert("tests".into(), vec!["/bin/sleep".into(), "30".into()]);
    assert!(
        container(&scanner, "tests")
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
}

#[test]
fn real_rust_tests_sast_and_secret_scans_reject_seeded_bad_sources() {
    if std::env::var("KOD_DOCKER_TESTS").as_deref() != Ok("1") {
        return;
    }
    let manifest = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"MIT\"\n";
    let lock = "version = 4\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n";
    let (_dir, scanner) = docker_fixture(&[
        ("Cargo.toml", manifest),
        ("Cargo.lock", lock),
        (
            "src/lib.rs",
            "pub fn answer() -> u8 { 42 }\n#[test] fn correct() { assert_eq!(answer(), 42); }\n",
        ),
    ]);
    container(&scanner, "tests").unwrap();
    container(&scanner, "sast").unwrap();
    container(&scanner, "secret-scan").unwrap();
    container(&scanner, "dependency-scan").unwrap();
    let (_dir, scanner) = docker_fixture(&[
        ("Cargo.toml", manifest),
        ("Cargo.lock", lock),
        (
            "src/lib.rs",
            "pub fn answer() -> u8 { let unused = 1; 42 }\n#[test] fn wrong() { assert_eq!(answer(), 0); }\n",
        ),
    ]);
    assert!(container(&scanner, "tests").is_err());
    assert!(container(&scanner, "sast").is_err());
    let secret = format!(
        "token = \"ghp_{}\" # gitleaks:allow\n",
        "aB7xZ9qK2mN5pR8sT1vW4yU6cD0eF3gH5jL7"
    );
    let (_dir, scanner) = docker_fixture(&[
        ("secret.txt", &secret),
        (".gitleaks.toml", "[allowlist]\npaths = ['.*']\n"),
        (".gitleaksignore", "secret.txt\n"),
    ]);
    assert!(container(&scanner, "secret-scan").is_err());
    let (_dir, scanner) = docker_fixture(&[
        (
            "Cargo.toml",
            &format!("{manifest}\n[dependencies]\nforbidden = {{ path = \"forbidden\" }}\n"),
        ),
        (
            "Cargo.lock",
            "version = 4\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\ndependencies = [\"forbidden\"]\n[[package]]\nname = \"forbidden\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub fn answer() {}"),
        (
            "forbidden/Cargo.toml",
            "[package]\nname = \"forbidden\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"GPL-3.0-only\"\n",
        ),
        ("forbidden/src/lib.rs", "pub fn answer() {}"),
        ("deny.toml", "[licenses]\nallow = [\"GPL-3.0-only\"]\n"),
    ]);
    assert!(container(&scanner, "dependency-scan").is_err());
}

#[test]
fn source_must_match_the_target_sha_before_docker_is_started() {
    let scanner = DockerScanner {
        config: ScanConfig::rust(&format!("sha256:{}", "a".repeat(64))),
        archive: "/does-not-exist".into(),
        sha: SHA.into(),
        docker: "/does-not-exist".into(),
        cache: None,
    };
    assert!(
        scanner
            .scan("tests", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap_err()
            .to_string()
            .contains("SHA")
    );
}

#[test]
fn unlabelled_and_sandbox_daemons_are_refused() {
    for labels in [
        "[]",
        "[\"kod.role=sandbox\"]",
        "[\"kod.role=gate\",\"kod.role=sandbox\"]",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let docker = dir.path().join("docker");
        std::fs::write(&docker, format!("#!/bin/sh\nprintf '%s' '{labels}'\n")).unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scanner = DockerScanner {
            config: ScanConfig::rust(&format!("sha256:{}", "a".repeat(64))),
            archive: dir.path().join("absent-archive"),
            sha: SHA.into(),
            docker,
            cache: None,
        };
        assert!(
            scanner
                .scan("tests", SHA)
                .unwrap_err()
                .to_string()
                .contains("exclusive kod.role=gate")
        );
    }
}

#[test]
fn container_names_are_unique_without_a_temporary_directory() {
    let a = resource_name(SHA).unwrap();
    let b = resource_name(SHA).unwrap();
    assert_ne!(a, b);
    assert!(a.starts_with("kod-gate-aaaaaaaa-"));
}

#[test]
fn real_preparation_fetches_new_dependencies_and_stale_caches_are_refused() {
    if std::env::var("KOD_DOCKER_TESTS").as_deref() != Ok("1") {
        return;
    }
    let (dir, mut scanner) = docker_fixture(&[
        (
            "Cargo.toml",
            "[package]\nname=\"fresh\"\nversion=\"0.1.0\"\nedition=\"2024\"\nlicense=\"MIT\"\n[dependencies]\neither=\"=1.13.0\"\n",
        ),
        (
            "src/lib.rs",
            "pub fn answer() -> either::Either<u8,u8> { either::Left(42) }",
        ),
    ]);
    assert!(
        Process::new("cargo")
            .args(["generate-lockfile", "--manifest-path"])
            .arg(dir.path().join("fixture/Cargo.toml"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(dir.path().join("fixture/.cargo-marker"), "untrusted").unwrap();
    std::fs::create_dir(dir.path().join("fixture/.cargo")).unwrap();
    std::fs::write(
        dir.path().join("fixture/.cargo/config.toml"),
        "invalid TOML [[[\n",
    )
    .unwrap();
    std::fs::set_permissions(&scanner.archive, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        Process::new("tar")
            .arg("-czf")
            .arg(&scanner.archive)
            .arg("-C")
            .arg(dir.path())
            .arg("fixture")
            .status()
            .unwrap()
            .success()
    );
    std::fs::set_permissions(&scanner.archive, std::fs::Permissions::from_mode(0o444)).unwrap();
    let network = resource_name(SHA).unwrap();
    scanner.config.package_network = network.clone();
    scanner.cache = Some(resource_name(SHA).unwrap());
    assert!(
        Process::new(&scanner.docker)
            .args(["network", "create", &network])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(scanner.prepare_container());
    if let Err(error) = result {
        assert!(
            Process::new(&scanner.docker)
                .args(["network", "rm", &network])
                .status()
                .unwrap()
                .success()
        );
        panic!("{error:#}");
    }
    scanner.config.commands.insert("tests".into(), vec!["/bin/sh".into(), "-ec".into(),
        "rm -rf .cargo; test -d /prepared/cargo/advisory-dbs; /usr/local/cargo/bin/cargo test --locked --offline".into()]);
    let scan = container(&scanner, "tests");
    let cache = scanner.cache.as_ref().unwrap();
    let stale = Process::new(&scanner.docker)
        .args([
            "run",
            "--rm",
            "--network=none",
            "--entrypoint=/bin/sh",
            "--mount",
            &format!("type=volume,source={cache},target=/prepared"),
            &scanner.config.image,
            "-c",
            "echo 0 > /prepared/at",
        ])
        .status()
        .unwrap();
    let rejected = container(&scanner, "tests");
    scanner.remove_cache().unwrap();
    assert!(
        Process::new(&scanner.docker)
            .args(["network", "rm", &network])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    scan.unwrap();
    assert!(stale.success());
    assert!(rejected.is_err());
}
