use kod_tracker::{Tracker, fake::sandbox};

#[test]
fn concurrent_status_updates_preserve_human_comments_and_one_owned_comment() {
    let (tracker, _) = sandbox();
    tracker
        .comment("FAKE-1", "Kod status: human-authored text")
        .unwrap();
    std::thread::scope(|scope| {
        for n in 0..16 {
            let tracker = &tracker;
            scope.spawn(move || {
                tracker
                    .status_comment("FAKE-1", &format!("status {n}"))
                    .unwrap()
            });
        }
    });
    let comments = tracker.comments("FAKE-1");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0], "Kod status: human-authored text");
    assert!(comments[1].starts_with("status "));
    assert!(tracker.status_comment("FAKE-404", "status").is_err());
}
