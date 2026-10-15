//! Kod's audit log: an append-only, hash-chained SQLite table with secret
//! redaction before storage, external write-once checkpoints of the chain
//! head, and retention decided by the compliance profile through kod-policy.
//!
//! Tamper-evident, not tamper-proof. Anyone with write access to the host can
//! still change or delete rows; `verify` shows it, as long as the change
//! touches an entry at or before the last checkpoint and `verify` reads
//! checkpoints the host cannot forge (run it under the checkpoint account,
//! against a copy of the log). Entries after the last checkpoint can be
//! truncated without trace.
pub mod redact;
