#[path = "../src/acceptance/lifecycle/stop.rs"]
mod stop;
mod deadlines {
    pub use kod_host::deadlines::*;
}

#[test]
#[ignore = "requires real Docker; run explicitly with --ignored"]
fn real_term_resistant_container_is_killed_and_absent_within_ten_seconds() {
    assert_eq!(
        std::env::var("KOD_DOCKER_TESTS").as_deref(),
        Ok("1"),
        "explicit Docker acceptance required"
    );
    let image = std::env::var("KOD_ACCEPTANCE_STOP_IMAGE")
        .unwrap_or_else(|_| "kod-sandbox-test:latest".into());
    let elapsed = stop::resistant_container(&image).unwrap();
    println!(
        "{{\"scenario\":\"real-docker-term-kill-absence\",\"outcome\":\"pass\",\"stop_ms\":{},\"term_grace_ms\":5000,\"absence_deadline_ms\":10000,\"absent\":true}}",
        elapsed.as_millis()
    );
}
