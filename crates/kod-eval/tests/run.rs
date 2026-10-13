use kod_eval::{case::EndState, run::decide, runner::Runner};

#[test]
fn timeouts_kills_and_missing_output_map_to_end_states() {
    let r = Runner::Script {
        command: "true".into(),
    };
    assert_eq!(decide(&r, true, Some(0), "").0, EndState::OverBudget);
    assert_eq!(decide(&r, false, Some(137), "").0, EndState::OverBudget); // OOM kill
    assert_eq!(decide(&r, false, None, "").0, EndState::NeedsHuman);
    assert_eq!(decide(&r, false, Some(0), "").0, EndState::Done);
}
