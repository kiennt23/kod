//! A tag-only sandbox image must block Docker mode and the report must say why (this hid a CI failure).
#[path = "common/connected.rs"]
mod connected;

#[test]
fn tag_only_image_blocks_with_a_reason() {
    // Own test binary with one test, so setting the env var cannot race another test.
    unsafe { std::env::set_var("KOD_CONNECTED_IMAGE", "kod-connected-sandbox") };
    let h = connected::ConnectedFixture::local();
    let (out, report) = h.accept();
    assert_eq!(out.status.code(), Some(2), "report: {report}");
    assert_eq!(report["blocked"][0], "docker_prerequisites");
    assert_eq!(
        report["blocked_reasons"][0],
        "docker_prerequisites: image_digest"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("image_digest"));
}
