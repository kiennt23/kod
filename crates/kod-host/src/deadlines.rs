use std::time::Duration;
pub const MODEL_CANCEL: Duration = Duration::from_millis(2000);
pub const TERM_GRACE: Duration = Duration::from_millis(5000);
pub const ABSENCE: Duration = Duration::from_millis(10000);
pub const WORKER_TICK: Duration = Duration::from_millis(250);
pub const STATUS_DEBOUNCE: Duration = Duration::from_millis(1000);
pub const RECONCILE: Duration = Duration::from_millis(30000);
pub const SOURCE: Duration = Duration::from_millis(10000);
pub const SETUP_WAIT: Duration = Duration::from_millis(2000);
