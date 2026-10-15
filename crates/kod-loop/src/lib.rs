//! kod-loop: Kod's own agent loop, run as a runner under kod-eval.
pub mod context;
pub mod tools;

use serde::{Deserialize, Serialize};

/// The same names as kod-eval's EndState.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndState {
    Done,
    Blocked,
    NeedsHuman,
    OverBudget,
}
