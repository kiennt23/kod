# Phase 1.4 Audit Log (`kod-audit`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development (a fresh implementer, then a fresh reviewer before the next task starts). *Native* tasks use superpowers:executing-plans in the orchestrating session. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `kod-audit`, a Rust library and CLI for Kod's audit log. It provides an append-only, hash-chained SQLite table, redacts secrets before anything is stored, writes write-once checkpoints of the chain head to storage under a separate account, and deletes data only when the compliance profile, asked through kod-policy, allows it. A failed audit write refuses the guarded external call.

**Architecture:** A new workspace crate, `crates/kod-audit`, with one store type, `AuditLog`.
- `append` redacts the payload, then inserts one row in a `BEGIN IMMEDIATE` transaction. Each row's hash covers the previous row's hash.
- `before(record_type, payload, call)` appends first and runs `call` only if the append succeeded. Sub-plan 5's gateway wraps every external write in it.
- `checkpoint` verifies the log, then puts `{seq, hash, at}` into a `CheckpointTarget`. That is a two-method trait: the local `DirTarget` is used in tests and in production, pointed at a write-once mount owned by another account.
- `verify` recomputes the chain and checks it against every checkpoint.
- `purge` asks `Policy::decide` with `retention_delete` for each expired entry. It removes only the payload, so the chain still verifies, and it records the deletion in the chain.
- A small `kod-audit` CLI runs `verify`, `checkpoint` and `purge` from a timer.

**Tech Stack:** Rust 1.99.0 (edition 2024), rusqlite 0.40 with the `bundled` feature (SQLite compiled in), sha2 0.10, serde and serde_json, clap 4, anyhow, and `kod-policy` (sub-plan 3). Tests use tempfile.

**Spec:** `docs/kod-harness-design.md`, in particular:
- "Audit integrity"
- "Trust boundaries" (the audit store is in the trusted zone, and the gateway writes the audit record before each external call), plus the action and permission matrix rows `retention_delete` and publish
- "Configurable policies → compliance profile" (record types, retention, the strictest profile wins)
- "Ownership"

The phase decomposition is in `2026-10-14-phase1-overview.md`, sub-plan 4. The policy API comes from `2026-10-15-phase1-policy.md` (D3, D4, D12). Execution modes follow `docs/adr/0001-per-task-execution-mode.md`.

**Precondition:** kod-policy (sub-plan 3, branch `feat/phase1-policy`) is merged to main before Task 1 starts. Every task depends on `kod_policy::{Policy, Decision}` and the repo's `policy/` directory. This plan's code was compiled and tested against a stub crate that implements only the policy plan's API (`Policy::load`, `version`, `decide`, `Decision { allow, reasons, policy_version }`) and its D12 `retention_delete` rule, with the same deny reasons. The executor runs the tests against the real crate; if a reason string differs, the policy plan is the source of truth.

**Out of scope here** (later sub-plans, see the overview):
- The tool gateway that calls `before` around every external write and refuses publish when it fails (5).
- The LLM gateway that hands redacted prompt and output capture to `append` (2).
- The dispatcher (6).
- Evidence export (7).
- Real compliance profile values.
- The real checkpoint storage and its account. That is the backup owner's decision; see Open items.

**Honest ceiling: tamper-evident, not tamper-proof.**
- Anyone with write access to the audit host can still change, delete or replace rows. `verify` detects it whenever the change touches an entry at or before the last checkpoint.
- Entries written after the last checkpoint can be truncated without trace. That window is the checkpoint interval (D6).
- Someone who controls both the audit host and the checkpoint account can rewrite everything.
- Redaction is pattern-based: it catches the known credential formats and private key blocks, not every secret.

## Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **SQLite** through rusqlite 0.40 with `bundled`, one file (default `/var/lib/kod/audit.db`), WAL mode, `synchronous=FULL`, 2 s busy timeout. Appends run in `BEGIN IMMEDIATE`, so concurrent writers (gateway, dispatcher, CLI) serialize and the chain stays linear. | Six people, our own hosts, one trusted audit host: SQLite has no server to run, patch, back up or authenticate, and tests and CI need no service. WAL handles several writer processes on one host. Postgres would add a server and credentials for a write rate of a few rows per second. Its INSERT-only role doesn't stop a host admin either: the integrity evidence comes from the checkpoints, not from the database. **Ceiling:** writers must be on the audit host, because SQLite over a network filesystem is unsafe. Move to Postgres when a writer has to run on another host. |
| D2 | One row per entry: `seq, at, record_type, payload, payload_sha256, hash`. `hash = sha256(prev_hash ‖ seq ‖ at ‖ len(record_type) ‖ record_type ‖ payload_sha256)`, with the integers as 8-byte little-endian and the hashes as raw bytes. Entry 1's `prev` is `GENESIS` (64 zeros). The chain covers the payload's hash, not the payload. | Retention can then null a payload while every hash still verifies. The length prefix stops record type and payload hash from sliding into each other. `seq` in the hash makes reordering or renumbering visible. |
| D3 | SQLite triggers refuse `DELETE`, and refuse any `UPDATE` other than setting `payload` to NULL. | They guard against mistakes and stray SQL, not against an attacker: anyone who can write the file can drop them. The tamper tests drop them first, as an attacker would. |
| D4 | **Redaction before storage**: `append` runs `redact_value` on the payload before serializing it, so a secret never reaches the file or the WAL. It replaces every string value (not object keys) that matches a known credential prefix (Anthropic and OpenAI `sk-`, GitHub `ghp_` `gho_` `ghu_` `ghs_` `ghr_` `github_pat_`, GitLab `glpat-`, Slack `xox?-`, AWS `AKIA` `ASIA`, Google `AIza`, JWT `eyJ`, `Bearer `) followed by at least 16 token characters at a word start, and any `-----BEGIN … PRIVATE KEY-----` block (to the end of the text if the END line is missing). The replacement is `[REDACTED]`. | It's the smallest thing that catches what agents actually leak (tokens in commands, env dumps, keys in files) without a regex dependency. The 16-character minimum and the word-start rule keep `task-list`, `sk-learn` and `ask-me-…` intact. **Ceiling:** an unknown format or a bare password isn't caught. Add a prefix when a new credential type shows up. |
| D5 | **Checkpoints** go through `trait CheckpointTarget { put, list }`. `DirTarget` writes `{seq:020}.json` with `create_new` (an existing file is never overwritten), fsyncs it and makes it read-only. `list` fails on any file it can't parse, or whose name and `seq` disagree. In production the directory is a mount owned by a **separate account**: the kod service user may create files there but can't modify or delete them (for example an NFS export on the backup host with the immutable or append-only attribute set by its own cron, or an S3 Object Lock bucket via a second `CheckpointTarget`). | The trait is two methods, so a second target is easy to add once the storage is chosen, and tests need no cloud. `create_new` plus read-only makes the local fake write-once for the kod user. The separate account is what makes it real. `checkpoint` verifies first and refuses to checkpoint a tampered log, so a tampered head never becomes the trusted reference. |
| D6 | A systemd timer runs `kod-audit checkpoint` **every 5 minutes** and `kod-audit verify` hourly. Any failure exits non-zero with `{"ok":false,"error":…}`, and the timer's failure alert reaches the owner. | The spec says "every few minutes". Five minutes bounds the untraceable truncation window. The runbook section (Task 4) has the units. |
| D7 | **Fail closed**: `before(record_type, payload, call)` returns `Err` without running `call` whenever the append fails: the store is busy beyond 2 s, the disk is full, the file is unreadable, or the record type is invalid. `AuditLog::open` fails on a missing directory or a file that isn't a database. | This is the spec's failure rule: when the audit write fails, the external write is refused. It's a closure rather than a flag, so a caller can't run the call after forgetting to check. Sub-plan 5 wraps publish and every tracker or docs write in it. |
| D8 | **Retention**: `purge(policy, record_type, now)` takes the entries of that type that still have a payload, oldest first. It asks `decide({"action":"retention_delete","record_type", "age_days"})` for each, with whole days rounded down and a future timestamp counting as 0, and stops at the first deny. Allowed entries get `payload = NULL`. Then one `retention` entry records `{record_type, seqs, oldest_age_days, youngest_age_days, policy_version}`. If nothing is allowed, the refusal is recorded as a `policy_decision`. `retention` entries are never purged. Rows and hashes are never deleted. | This follows the matrix row: the decision lives in the policy (strictest profile wins, a missing value refuses), and the evidence is a deletion record in the chain with the policy version. Keeping rows and hashes means truncation detection survives retention. The placeholder profile refuses everything, so nothing can be deleted until the owner sets values. |
| D9 | `verify` reports `payload removed without a retention record` for any NULL payload whose seq isn't listed in some `retention` entry. | Otherwise nulling a payload, which the trigger allows, would be a silent way to erase content. |
| D10 | `record_decision(input, &Decision)` appends `run_audit` `{"kind":"policy_decision","input","decision"}`, where the decision carries `policy_version` (policy D4). Record types are lower-case letters and underscores. They are the keys of the compliance profile's `retention_days` (`run_audit`, `prompts`, `model_outputs`, …), plus `retention`. | One shape for every decision, so an auditor can find which rules decided. The record type naming matches the policy's `retention_days.<record_type>` lookup, so retention works per type without a mapping table. |
| D11 | The database file is mode `0600`, owned by the kod service user on the audit host. Owner and auditor read access goes through that host's account permissions. | "Readable only by owner and auditors" on a single host is an OS permission, not an application feature. Build a reader role when auditors need remote access. |

## Execution

The modes follow [ADR 0001](../../adr/0001-per-task-execution-mode.md): a task that owns a safety guarantee or a Review Focus test runs as Subagent + review, and every other task runs Native. Every task here owns one, so every task is Subagent + review. The plan ends with one whole-branch review.

| Task | Mode | Why |
|---|---|---|
| 1. Crate and secret redaction | Subagent + review | Owns redaction before storage (Review Focus 4). |
| 2. Hash-chained store, decisions and the fail-closed guard | Subagent + review | Owns append-only, the chain, concurrent writers and the store-outage refusal (Review Focus 1 and 2). |
| 3. Checkpoints and verification | Subagent + review | Owns tamper and truncation detection and write-once checkpoints (Review Focus 3 and 5). |
| 4. Retention, the CLI and the runbook | Subagent + review | Owns retention through the policy, deletion evidence and the placeholder refusal (Review Focus 5). |

## Global Constraints

- Toolchain: Rust 1.99.0 from `rust-toolchain.toml`, edition 2024, `publish = false` on every crate.
- CI must pass: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`, and cargo-deny with the existing `deny.toml`, which is not changed.
- New dependencies, in kod-audit only: `rusqlite = { version = "0.40", features = ["bundled"] }` and `sha2 = "0.10"`.
  - rusqlite pulls in libsqlite3-sys (MIT, compiles SQLite from source, needs a C compiler, which macOS and our Linux hosts have).
  - It also pulls in hashlink, fallible-iterator, fallible-streaming-iterator and bitflags (MIT or Apache-2.0) and foldhash (Zlib).
  - sha2 is the version kod-policy already uses.
  - Every license is on the allow list.
- `anyhow`, `clap`, `serde`, `serde_json` and the dev-dependency `tempfile` are already in the workspace.
- Fail closed: every error is returned, never logged and ignored. Nothing in this crate retries an append.
- No gateway and no dispatcher code. `before` is the hook those sub-plans call.
- Commit messages are plain, with no trailers.

## Review Focus

1. **Several processes writing at once** (the gateway, the dispatcher and the CLI on one host) must produce one linear chain with no forks or gaps. Pinned by Task 2 `concurrent_writers_keep_one_linear_chain`.
2. **The store being down, locked by another writer, missing or not a database** must make the append fail, and the guarded external call must never run. Pinned by Task 2 `when_the_store_is_down_the_external_write_is_refused` and `a_store_that_cannot_be_opened_is_an_error`.
3. **A full rewrite with every hash recomputed, a replaced empty log, or a junk file in the checkpoint directory** must fail `verify`, and a tampered log must never be checkpointed. Pinned by Task 3 `a_rewritten_chain_is_caught_by_the_checkpoint`, `a_replaced_empty_log_is_caught` and `an_unreadable_checkpoint_store_fails_verification`.
4. **Secrets in odd places**: inside nested arrays and objects, a private key cut off before its END line, and a token used as an object key (keys are kept; see the test). Ordinary prose that resembles a prefix (`task-list`, `sk-learn`) must be left alone. Pinned by Task 1 `every_string_in_a_payload_is_redacted`, `private_key_blocks_are_redacted_even_without_an_end` and `ordinary_text_is_left_alone`, and Task 2 `secrets_are_redacted_before_storage`.
5. **A clock set back, a payload erased by hand, or a request to purge deletion records** must not lose evidence:
   - an entry dated in the future is 0 days old;
   - a NULL payload with no retention record is flagged;
   - `retention` entries can't be purged.

   Pinned by Task 4 `entries_dated_in_the_future_count_as_zero_days_old` and `retention_records_are_never_purged`, and Task 3 `a_removed_payload_without_a_retention_record_is_caught`.

---

## File Structure

```
Cargo.toml                          modify: add crates/kod-audit to members
crates/kod-audit/
  Cargo.toml                        create (Task 1)
  src/lib.rs                        create (Task 1), filled in by Task 2, module lines added in Tasks 3 and 4
  src/redact.rs                     create (Task 1): redact, redact_value
  src/checkpoint.rs                 create (Task 3): Checkpoint, CheckpointTarget, DirTarget
  src/verify.rs                     create (Task 3): AuditLog::verify, AuditLog::checkpoint, Verified
  src/retention.rs                  create (Task 4): AuditLog::purge
  src/main.rs                       create (Task 4): the kod-audit CLI
  tests/redact.rs                   create (Task 1)
  tests/log.rs                      create (Task 2)
  tests/common/mod.rs               create (Task 3): fixture(), Fixture::db(), Fixture::attacker()
  tests/integrity.rs                create (Task 3)
  tests/retention.rs                create (Task 4)
  tests/cli.rs                      create (Task 4)
docs/phase1-runbook.md              modify (Task 4): "Audit log" section with the timers
```

This plan's own PR links it from row 4 of the overview.

---

### Task 1: Crate and secret redaction

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `crates/kod-audit/Cargo.toml`, `crates/kod-audit/src/lib.rs`, `crates/kod-audit/src/redact.rs`
- Test: `crates/kod-audit/tests/redact.rs`

**Interfaces:**
- Consumes: nothing from this plan. The Cargo.toml names `kod-policy` (merged, see Precondition).
- Produces: `kod_audit::redact::{REDACTED: &str, redact(&str) -> String, redact_value(&serde_json::Value) -> serde_json::Value}`.

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add `"crates/kod-audit"` to `members`. Keep the list sorted; after sub-plan 3 it also contains `"crates/kod-policy"`.

Create `crates/kod-audit/Cargo.toml`:

```toml
[package]
name = "kod-audit"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
kod-policy = { path = "../kod-policy" }
rusqlite = { version = "0.40", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"

[dev-dependencies]
tempfile = "3"
```

Create `crates/kod-audit/src/lib.rs`:

```rust
//! Kod's audit log: an append-only, hash-chained SQLite table with secret
//! redaction before storage, external write-once checkpoints of the chain
//! head, and retention decided by the compliance profile through kod-policy.
//!
//! Tamper-evident, not tamper-proof. Anyone with write access to the host can
//! still change or delete rows; `verify` shows it, as long as the change
//! touches an entry at or before the last checkpoint. Entries after the last
//! checkpoint can be truncated without trace.
pub mod redact;
```

- [ ] **Step 2: Write the failing test**

Create `crates/kod-audit/tests/redact.rs`:

```rust
use kod_audit::redact::{REDACTED, redact, redact_value};
use serde_json::json;

// The fake credentials are split with concat! so secret scanners, GitHub push
// protection among them, don't flag this file.
#[test]
fn known_credential_formats_are_redacted() {
    for secret in [
        concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("sk", "-proj-abcdefghijklmnopqrstuvwx"),
        concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("gi", "thub_pat_11ABCDEFG0123456789_abcdefghijklmnop"),
        concat!("gl", "pat-abcdefghijklmnopqrst"),
        concat!("xo", "xb-1234567890-abcdefghijklmnop"),
        concat!("AK", "IAIOSFODNN7EXAMPLE"),
        concat!("AI", "zaSyA-abcdefghijklmnopqrstuvwxyz012345"),
        concat!(
            "ey",
            "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abcdefghijklmnop"
        ),
        "Bearer abcdefghijklmnopqrstuvwxyz",
    ] {
        let text = format!("token: {secret} end");
        assert_eq!(redact(&text), format!("token: {REDACTED} end"), "{secret}");
    }
}

#[test]
fn private_key_blocks_are_redacted_even_without_an_end() {
    let pem =
        "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\nb";
    assert_eq!(redact(pem), format!("a\n{REDACTED}\nb"));
    let cut = "a\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA";
    assert_eq!(redact(cut), format!("a\n{REDACTED}"));
    let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    assert_eq!(redact(cert), cert);
}

#[test]
fn ordinary_text_is_left_alone() {
    for text in [
        "task-list and sk-learn are fine",
        "ask-me-anything-about-the-weather-today",
        "the AKIA prefix alone",
        "Bearer short",
        "unicode: héllo wörld ✓ sk-",
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn every_string_in_a_payload_is_redacted() {
    let v = json!({
        "prompt": "use ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "nested": {"list": [1, concat!("xo", "xp-1234567890-abcdefghijklmnop"), null]},
        (concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz")): true
    });
    assert_eq!(
        redact_value(&v),
        json!({
            "prompt": format!("use {REDACTED}"),
            "nested": {"list": [1, REDACTED, null]},
            (concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz")): true
        })
    );
}
```

- [ ] **Step 3: Run it and see it fail**

Run: `cargo test -p kod-audit --test redact`
Expected: compile error, `file not found for module redact` (and `unresolved import kod_audit::redact`).

- [ ] **Step 4: Implement redaction**

Create `crates/kod-audit/src/redact.rs`:

```rust
//! Secret redaction, applied to every string in a payload before it is hashed
//! and stored. Pattern-based: it catches the credential formats listed here,
//! not every secret. Unknown formats pass through.
use serde_json::Value;

pub const REDACTED: &str = "[REDACTED]";

/// A token is a known prefix, at the start of a word, followed by at least
/// MIN_BODY token characters.
const PREFIXES: [&str; 20] = [
    "sk-ant-",
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "eyJ",
    "Bearer ",
    "bearer ",
];
const MIN_BODY: usize = 16;

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_./+=".contains(&b)
}

/// Redacts every string value in `v`, recursively. Object keys are kept.
pub fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), redact_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn redact(text: &str) -> String {
    redact_tokens(&redact_pem(text))
}

/// PEM private key blocks, from BEGIN to the matching END. A block with no END
/// is redacted to the end of the text.
fn redact_pem(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN ") {
        let after = &rest[start + 11..];
        let Some(label_len) = after.find("-----") else {
            break;
        };
        let label = &after[..label_len];
        if !label.ends_with("PRIVATE KEY") {
            out.push_str(&rest[..start + 11]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        let end = format!("-----END {label}-----");
        rest = match after.find(&end) {
            Some(i) => &after[i + end.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn redact_tokens(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let at_word_start = i == 0 || !is_token_byte(bytes[i - 1]);
        let token_end = PREFIXES
            .iter()
            .filter(|p| at_word_start && text[i..].starts_with(*p))
            .map(|p| {
                let body = text[i + p.len()..]
                    .bytes()
                    .take_while(|b| is_token_byte(*b))
                    .count();
                (body, i + p.len() + body)
            })
            .find(|(body, _)| *body >= MIN_BODY)
            .map(|(_, end)| end);
        match token_end {
            Some(end) => {
                out.push_str(REDACTED);
                i = end;
            }
            None => {
                let c = text[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}
```

- [ ] **Step 5: Run it and see it pass**

Run: `cargo test -p kod-audit --test redact && cargo clippy -p kod-audit --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 4 passed, no warnings, no diff.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kod-audit
git commit -m "kod-audit: crate skeleton and secret redaction"
```

### Task 2: Hash-chained store, decisions and the fail-closed guard

**Files:**
- Modify: `crates/kod-audit/src/lib.rs` (replace the whole file)
- Test: `crates/kod-audit/tests/log.rs`

**Interfaces:**
- Consumes: `redact::redact_value` (Task 1); `kod_policy::Decision { allow: bool, reasons: Vec<String>, policy_version: String }` (Serialize).
- Produces:
  - `GENESIS`, `RUN_AUDIT = "run_audit"`, `RETENTION = "retention"`, `BUSY_TIMEOUT`.
  - `entry_hash(prev: &str, seq: u64, at: i64, record_type: &str, payload_sha256: &str) -> Result<String>`.
  - `now() -> i64` (unix seconds).
  - `AuditLog::open(&Path) -> Result<AuditLog>`.
  - `AuditLog::append(&self, &str, &Value) -> Result<u64>`.
  - `AuditLog::record_decision(&self, &Value, &Decision) -> Result<u64>`.
  - `AuditLog::before<T>(&self, &str, &Value, impl FnOnce() -> T) -> Result<T>`.
  - Crate-private helpers used by Tasks 3 and 4: `sha256_hex(&[u8]) -> String`, `append_in(&Transaction, &str, &Value) -> Result<u64>`, and `AuditLog::tx(&self, impl FnOnce(&Transaction) -> Result<T>) -> Result<T>` (one IMMEDIATE transaction).
  - Table `audit(seq, at, record_type, payload, payload_sha256, hash)` with the triggers `audit_no_delete` and `audit_only_purge`.

- [ ] **Step 1: Write the failing test**

Create `crates/kod-audit/tests/log.rs`:

```rust
use kod_audit::{AuditLog, GENESIS, RUN_AUDIT, entry_hash};
use kod_policy::Decision;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{cell::Cell, path::PathBuf, sync::Arc, thread};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    log: AuditLog,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(&dir.path().join("audit.db")).unwrap();
    Fixture { dir, log }
}

impl Fixture {
    fn db(&self) -> PathBuf {
        self.dir.path().join("audit.db")
    }

    /// Recomputes every hash from GENESIS and returns the number of entries.
    fn chain_len(&self) -> u64 {
        let c = Connection::open(self.db()).unwrap();
        let rows: Vec<(i64, i64, String, String, String)> = c
            .prepare("SELECT seq, at, record_type, payload_sha256, hash FROM audit ORDER BY seq")
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut prev = GENESIS.to_string();
        for (i, (seq, at, rt, ph, hash)) in rows.iter().enumerate() {
            assert_eq!(*seq, i as i64 + 1);
            assert_eq!(entry_hash(&prev, *seq as u64, *at, rt, ph).unwrap(), *hash);
            prev = hash.clone();
        }
        rows.len() as u64
    }
}

#[test]
fn entries_form_a_chain_from_genesis() {
    let f = fixture();
    assert_eq!(f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap(), 1);
    assert_eq!(f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap(), 2);
    assert_eq!(f.chain_len(), 2);
}

#[test]
fn secrets_are_redacted_before_storage() {
    let f = fixture();
    let secret = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");
    f.log
        .append("prompts", &json!({"prompt": format!("push with {secret}")}))
        .unwrap();
    drop(f.log);
    let mut raw = Vec::new();
    for name in ["audit.db", "audit.db-wal"] {
        if let Ok(b) = std::fs::read(f.dir.path().join(name)) {
            raw.extend(b);
        }
    }
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("[REDACTED]"));
    assert!(!raw.contains(secret));
}

#[test]
fn a_bad_record_type_is_refused() {
    let f = fixture();
    for rt in ["", "Run_Audit", "run audit", "run-audit"] {
        assert!(f.log.append(rt, &json!({})).is_err(), "{rt:?}");
    }
}

#[test]
fn decisions_are_recorded_with_their_policy_version() {
    let f = fixture();
    let decision = Decision {
        allow: false,
        reasons: vec!["publish: the run is canceled".into()],
        policy_version: "sha256:abc;regorus=0.13.0".into(),
    };
    f.log
        .record_decision(&json!({"action": "publish"}), &decision)
        .unwrap();
    let c = Connection::open(f.db()).unwrap();
    let payload: String = c
        .query_row("SELECT payload FROM audit WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    let v: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["kind"], "policy_decision");
    assert_eq!(v["input"]["action"], "publish");
    assert_eq!(v["decision"]["allow"], false);
    assert_eq!(v["decision"]["policy_version"], "sha256:abc;regorus=0.13.0");
}

#[test]
fn rows_cannot_be_changed_or_deleted_through_sql() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let c = Connection::open(f.db()).unwrap();
    for sql in [
        "DELETE FROM audit",
        "UPDATE audit SET payload = '{}'",
        "UPDATE audit SET at = 0",
        "UPDATE audit SET hash = 'x'",
    ] {
        let err = c.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
}

#[test]
fn concurrent_writers_keep_one_linear_chain() {
    let f = fixture();
    let db = f.db();
    let handles: Vec<_> = (0..4)
        .map(|w| {
            let log = Arc::new(AuditLog::open(&db).unwrap());
            thread::spawn(move || {
                for n in 0..25 {
                    log.append(RUN_AUDIT, &json!({"w": w, "n": n})).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(f.chain_len(), 100);
}

#[test]
fn when_the_store_is_down_the_external_write_is_refused() {
    let f = fixture();
    let called = Cell::new(false);
    assert!(
        f.log
            .before(RUN_AUDIT, &json!({"call": "publish"}), || called.set(true))
            .is_ok()
    );
    assert!(called.replace(false));

    // Another holder takes the write lock and keeps it: the store is unavailable.
    let blocker = Connection::open(f.db()).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let err = f
        .log
        .before(RUN_AUDIT, &json!({"call": "publish"}), || called.set(true))
        .unwrap_err();
    assert!(err.to_string().contains("locked"), "{err:#}");
    assert!(!called.get());
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.chain_len(), 1);
}

#[test]
fn a_store_that_cannot_be_opened_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(AuditLog::open(&dir.path().join("missing/audit.db")).is_err());
    std::fs::write(dir.path().join("junk.db"), "not a database, just junk text").unwrap();
    assert!(AuditLog::open(&dir.path().join("junk.db")).is_err());
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p kod-audit --test log`
Expected: compile error, `unresolved imports kod_audit::AuditLog, kod_audit::GENESIS, kod_audit::RUN_AUDIT, kod_audit::entry_hash`.

- [ ] **Step 3: Implement the store**

Replace `crates/kod-audit/src/lib.rs` with:

```rust
//! Kod's audit log: an append-only, hash-chained SQLite table with secret
//! redaction before storage, external write-once checkpoints of the chain
//! head, and retention decided by the compliance profile through kod-policy.
//!
//! Tamper-evident, not tamper-proof. Anyone with write access to the host can
//! still change or delete rows; `verify` shows it, as long as the change
//! touches an entry at or before the last checkpoint. Entries after the last
//! checkpoint can be truncated without trace.
pub mod redact;

use anyhow::{Context, Result, anyhow, bail};
use kod_policy::Decision;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The `prev` hash of entry 1.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Policy decisions and gateway calls.
pub const RUN_AUDIT: &str = "run_audit";
/// Deletion records. Never deleted themselves: `verify` needs them to account
/// for every removed payload.
pub const RETENTION: &str = "retention";
/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit (
    seq INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    record_type TEXT NOT NULL,
    payload TEXT,
    payload_sha256 TEXT NOT NULL,
    hash TEXT NOT NULL
) STRICT;
CREATE TRIGGER IF NOT EXISTS audit_no_delete BEFORE DELETE ON audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_only_purge BEFORE UPDATE ON audit
WHEN NEW.payload IS NOT NULL OR NEW.seq IS NOT OLD.seq OR NEW.at IS NOT OLD.at
  OR NEW.record_type IS NOT OLD.record_type
  OR NEW.payload_sha256 IS NOT OLD.payload_sha256 OR NEW.hash IS NOT OLD.hash
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
";

pub struct AuditLog {
    conn: Mutex<Connection>,
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.len() != 64 {
        bail!("not a sha256 hex string: {s:?}");
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("{e}: {s:?}")))
        .collect()
}

/// sha256(prev || seq || at || len(record_type) || record_type || payload_sha256),
/// with the integers as little-endian 8-byte values and the hashes as raw bytes.
pub fn entry_hash(
    prev: &str,
    seq: u64,
    at: i64,
    record_type: &str,
    payload_sha256: &str,
) -> Result<String> {
    let mut h = Sha256::new();
    h.update(unhex(prev)?);
    h.update(seq.to_le_bytes());
    h.update(at.to_le_bytes());
    h.update((record_type.len() as u64).to_le_bytes());
    h.update(record_type.as_bytes());
    h.update(unhex(payload_sha256)?);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn append_in(tx: &Transaction, record_type: &str, payload: &Value) -> Result<u64> {
    if record_type.is_empty()
        || !record_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        bail!("record type must be lower-case letters and underscores: {record_type:?}");
    }
    let text = serde_json::to_string(&redact::redact_value(payload))?;
    let (last, prev): (i64, String) = tx
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, GENESIS.to_string()));
    let seq = last + 1;
    let at = now();
    let payload_sha256 = sha256_hex(text.as_bytes());
    let hash = entry_hash(&prev, seq as u64, at, record_type, &payload_sha256)?;
    tx.execute(
        "INSERT INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq, at, record_type, text, payload_sha256, hash],
    )?;
    Ok(seq as u64)
}

impl AuditLog {
    /// Opens or creates the log. WAL with synchronous=FULL, so an entry that
    /// `append` returned survives a crash.
    pub fn open(path: &Path) -> Result<AuditLog> {
        let conn =
            Connection::open(path).with_context(|| format!("audit store {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(AuditLog {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn tx<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("audit log lock poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Redacts secrets in `payload`, then appends it. Returns the new seq.
    pub fn append(&self, record_type: &str, payload: &Value) -> Result<u64> {
        self.tx(|tx| append_in(tx, record_type, payload))
    }

    /// Records a policy decision with its input and policy_version.
    pub fn record_decision(&self, input: &Value, decision: &Decision) -> Result<u64> {
        self.append(
            RUN_AUDIT,
            &json!({"kind": "policy_decision", "input": input, "decision": decision}),
        )
    }

    /// Writes the audit record, then runs `call`. If the write fails, `call`
    /// never runs: the external write is refused.
    pub fn before<T>(
        &self,
        record_type: &str,
        payload: &Value,
        call: impl FnOnce() -> T,
    ) -> Result<T> {
        self.append(record_type, payload)?;
        Ok(call())
    }
}
```

- [ ] **Step 4: Run it and see it pass**

Run: `cargo test -p kod-audit && cargo clippy -p kod-audit --all-targets -- -D warnings && cargo fmt --all --check`
Expected: log 8 passed and redact 4 passed. `when_the_store_is_down_the_external_write_is_refused` takes about 2 s (the busy timeout).

- [ ] **Step 5: Commit**

```bash
git add crates/kod-audit
git commit -m "kod-audit: append-only hash-chained store and fail-closed guard"
```

### Task 3: Checkpoints and verification

**Files:**
- Modify: `crates/kod-audit/src/lib.rs` (module lines)
- Create: `crates/kod-audit/src/checkpoint.rs`, `crates/kod-audit/src/verify.rs`
- Test: `crates/kod-audit/tests/common/mod.rs`, `crates/kod-audit/tests/integrity.rs`

**Interfaces:**
- Consumes: `AuditLog::{open, append, tx}`, `GENESIS`, `RETENTION`, `entry_hash`, `now`, `sha256_hex` (Task 2).
- Produces:
  - `checkpoint::Checkpoint { seq: u64, hash: String, at: i64 }` (Serialize, Deserialize, PartialEq).
  - `checkpoint::CheckpointTarget { fn put(&self, &Checkpoint) -> Result<()>; fn list(&self) -> Result<Vec<Checkpoint>>; }`, where `list` is sorted by seq.
  - `checkpoint::DirTarget { pub dir: PathBuf }`.
  - `Verified { entries: u64, head: String, last_checkpoint: Option<u64> }`.
  - `AuditLog::verify(&self, &dyn CheckpointTarget) -> Result<Verified>`.
  - `AuditLog::checkpoint(&self, &dyn CheckpointTarget) -> Result<Option<Checkpoint>>`.
  - Test helpers: `common::{Fixture { dir, log, target }, fixture(), Fixture::db(), Fixture::attacker()}`.
  - Error texts that Task 4's CLI test matches: `tampered at seq N: …`, `truncated: checkpoint at seq N but the log ends at seq M`, `chain broken: …`.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-audit/tests/common/mod.rs`:

```rust
#![allow(dead_code)]
use kod_audit::{AuditLog, checkpoint::DirTarget};
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::TempDir;

/// A fresh log and an empty checkpoint directory in one temp dir.
pub struct Fixture {
    pub dir: TempDir,
    pub log: AuditLog,
    pub target: DirTarget,
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(&dir.path().join("audit.db")).unwrap();
    let cps = dir.path().join("checkpoints");
    std::fs::create_dir(&cps).unwrap();
    Fixture {
        log,
        target: DirTarget { dir: cps },
        dir,
    }
}

impl Fixture {
    pub fn db(&self) -> PathBuf {
        self.dir.path().join("audit.db")
    }

    /// A raw connection with the append-only triggers dropped: what someone
    /// with write access to the host can do.
    pub fn attacker(&self) -> Connection {
        let c = Connection::open(self.db()).unwrap();
        c.execute_batch("DROP TRIGGER audit_no_delete; DROP TRIGGER audit_only_purge;")
            .unwrap();
        c
    }
}
```

Create `crates/kod-audit/tests/integrity.rs`:

```rust
mod common;

use common::fixture;
use kod_audit::{
    RUN_AUDIT,
    checkpoint::{Checkpoint, CheckpointTarget},
    entry_hash,
};
use serde_json::json;

fn filled(n: u64) -> common::Fixture {
    let f = fixture();
    for i in 0..n {
        f.log.append(RUN_AUDIT, &json!({"n": i})).unwrap();
    }
    f
}

fn err(f: &common::Fixture) -> String {
    format!("{:#}", f.log.verify(&f.target).unwrap_err())
}

#[test]
fn checkpoints_are_write_once() {
    let f = filled(3);
    let cp = f.log.checkpoint(&f.target).unwrap().unwrap();
    assert_eq!(cp.seq, 3);
    // Nothing new: no second checkpoint, and no error.
    assert_eq!(f.log.checkpoint(&f.target).unwrap(), None);
    let forged = Checkpoint {
        hash: "f".repeat(64),
        ..cp.clone()
    };
    assert!(f.target.put(&forged).is_err());
    assert_eq!(f.target.list().unwrap(), vec![cp]);
    f.log.append(RUN_AUDIT, &json!({"n": 3})).unwrap();
    assert_eq!(f.log.checkpoint(&f.target).unwrap().unwrap().seq, 4);
    assert_eq!(f.log.verify(&f.target).unwrap().last_checkpoint, Some(4));
}

#[test]
fn an_edited_payload_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET payload = '{\"n\":99}' WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 2: payload does not match"));
}

#[test]
fn an_edited_entry_with_a_matching_payload_hash_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET at = at + 1 WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 2: entry hash"));
}

#[test]
fn a_removed_payload_without_a_retention_record_is_caught() {
    let f = filled(3);
    f.attacker()
        .execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
        .unwrap();
    assert!(err(&f).contains("tampered at seq 1: payload removed without a retention record"));
}

#[test]
fn a_deleted_middle_entry_breaks_the_chain() {
    let f = filled(3);
    f.attacker()
        .execute("DELETE FROM audit WHERE seq = 2", [])
        .unwrap();
    assert!(err(&f).contains("chain broken: expected seq 2, found seq 3"));
}

#[test]
fn a_rewritten_chain_is_caught_by_the_checkpoint() {
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    // Rewrite entry 2 and recompute every hash after it, so the chain itself
    // is consistent again.
    let c = f.attacker();
    c.execute(
        "UPDATE audit SET payload = '{\"n\":99}', payload_sha256 = ?1 WHERE seq = 2",
        [sha256_hex("{\"n\":99}")],
    )
    .unwrap();
    let mut prev = c
        .query_row("SELECT hash FROM audit WHERE seq = 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    for seq in 2..=3i64 {
        let (at, rt, ph): (i64, String, String) = c
            .query_row(
                "SELECT at, record_type, payload_sha256 FROM audit WHERE seq = ?1",
                [seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let h = entry_hash(&prev, seq as u64, at, &rt, &ph).unwrap();
        c.execute("UPDATE audit SET hash = ?1 WHERE seq = ?2", (&h, seq))
            .unwrap();
        prev = h;
    }
    assert!(err(&f).contains("tampered: checkpoint at seq 3 does not match the log"));
    // And the tampered log is never checkpointed.
    assert!(f.log.checkpoint(&f.target).is_err());
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn truncation_before_the_last_checkpoint_is_caught() {
    let f = filled(5);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.attacker()
        .execute("DELETE FROM audit WHERE seq > 3", [])
        .unwrap();
    assert!(err(&f).contains("truncated: checkpoint at seq 5 but the log ends at seq 3"));
}

#[test]
fn truncation_after_the_last_checkpoint_is_not_detectable() {
    // The honest ceiling: entries newer than the last checkpoint can vanish.
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 3})).unwrap();
    f.attacker()
        .execute("DELETE FROM audit WHERE seq = 4", [])
        .unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 3);
}

#[test]
fn a_replaced_empty_log_is_caught() {
    let f = filled(2);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    f.attacker().execute("DELETE FROM audit", []).unwrap();
    assert!(err(&f).contains("truncated: checkpoint at seq 2 but the log ends at seq 0"));
}

#[test]
fn an_unreadable_checkpoint_store_fails_verification() {
    let f = filled(1);
    std::fs::write(f.target.dir.join("junk.json"), "not json").unwrap();
    assert!(err(&f).contains("unreadable checkpoint"));
    std::fs::remove_file(f.target.dir.join("junk.json")).unwrap();
    std::fs::write(
        f.target.dir.join("00000000000000000009.json"),
        r#"{"seq":1,"hash":"00","at":0}"#,
    )
    .unwrap();
    assert!(err(&f).contains("names the wrong seq"));
    std::fs::remove_dir_all(&f.target.dir).unwrap();
    assert!(err(&f).contains("checkpoint directory"));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p kod-audit --test integrity`
Expected: compile error, `could not find checkpoint in kod_audit`.

- [ ] **Step 3: Implement checkpoints and verification**

In `crates/kod-audit/src/lib.rs`, replace the line `pub mod redact;` with:

```rust
pub mod checkpoint;
pub mod redact;
mod verify;

pub use verify::Verified;
```

Create `crates/kod-audit/src/checkpoint.rs`:

```rust
//! External checkpoints of the chain head. The target must be write-once and
//! held under a different account from the harness hosts: that separation,
//! not this code, is what makes a rewrite of the log detectable.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Sequence number of the head entry when the checkpoint was taken.
    pub seq: u64,
    /// That entry's chain hash, lower-case hex.
    pub hash: String,
    /// Unix seconds.
    pub at: i64,
}

pub trait CheckpointTarget {
    /// Stores a checkpoint. Must fail if one with the same seq exists.
    fn put(&self, cp: &Checkpoint) -> Result<()>;
    /// Every stored checkpoint, in seq order. Fails on anything unreadable.
    fn list(&self) -> Result<Vec<Checkpoint>>;
}

/// One file per checkpoint, created with create_new and then made read-only.
/// It is the test fake, and the production adapter when the directory is a
/// write-once mount owned by another account.
pub struct DirTarget {
    pub dir: PathBuf,
}

impl CheckpointTarget for DirTarget {
    fn put(&self, cp: &Checkpoint) -> Result<()> {
        let path = self.dir.join(format!("{:020}.json", cp.seq));
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("write-once checkpoint {}", path.display()))?;
        f.write_all(&serde_json::to_vec(cp)?)?;
        f.sync_all()?;
        let mut perms = f.metadata()?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms)?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<Checkpoint>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("checkpoint directory {}", self.dir.display()))?
        {
            let path = entry?.path();
            let text = fs::read_to_string(&path)?;
            let cp: Checkpoint = serde_json::from_str(&text)
                .with_context(|| format!("unreadable checkpoint {}", path.display()))?;
            if path.file_name() != Some(format!("{:020}.json", cp.seq).as_ref()) {
                bail!("checkpoint {} names the wrong seq", path.display());
            }
            out.push(cp);
        }
        out.sort_by_key(|cp| cp.seq);
        Ok(out)
    }
}
```

Create `crates/kod-audit/src/verify.rs`:

```rust
use crate::{
    AuditLog, GENESIS, RETENTION,
    checkpoint::{Checkpoint, CheckpointTarget},
    entry_hash, now, sha256_hex,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashSet;

/// What `verify` checked.
#[derive(Debug, PartialEq)]
pub struct Verified {
    pub entries: u64,
    /// Hash of the last entry, or GENESIS for an empty log.
    pub head: String,
    pub last_checkpoint: Option<u64>,
}

impl AuditLog {
    /// Checks the whole chain, then every checkpoint against it. Any mismatch
    /// is an error naming the first problem found.
    pub fn verify(&self, target: &dyn CheckpointTarget) -> Result<Verified> {
        // ponytail: reads every row on each run; page through it if the log
        // grows past what fits in memory.
        let rows = self.tx(|tx| {
            let mut stmt = tx.prepare(
                "SELECT seq, at, record_type, payload, payload_sha256, hash FROM audit ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let mut hashes = Vec::with_capacity(rows.len());
        let mut removed = Vec::new();
        let mut purged = HashSet::new();
        let mut prev = GENESIS.to_string();
        for (i, (seq, at, record_type, payload, payload_sha256, hash)) in rows.iter().enumerate() {
            let expected = i as u64 + 1;
            if *seq != expected {
                bail!("chain broken: expected seq {expected}, found seq {seq}");
            }
            match payload {
                Some(p) if sha256_hex(p.as_bytes()) != *payload_sha256 => {
                    bail!("tampered at seq {seq}: payload does not match its hash")
                }
                Some(p) if record_type == RETENTION => {
                    let v: Value = serde_json::from_str(p)?;
                    let seqs = v["seqs"]
                        .as_array()
                        .with_context(|| format!("retention record {seq} has no seqs"))?;
                    purged.extend(seqs.iter().filter_map(Value::as_u64));
                }
                Some(_) => {}
                None => removed.push(*seq),
            }
            if entry_hash(&prev, *seq, *at, record_type, payload_sha256)? != *hash {
                bail!("tampered at seq {seq}: entry hash does not match the chain");
            }
            prev = hash.clone();
            hashes.push(hash);
        }
        if let Some(seq) = removed.iter().find(|s| !purged.contains(s)) {
            bail!("tampered at seq {seq}: payload removed without a retention record");
        }
        let checkpoints = target.list()?;
        for cp in &checkpoints {
            match hashes.get((cp.seq as usize).wrapping_sub(1)) {
                None => bail!(
                    "truncated: checkpoint at seq {} but the log ends at seq {}",
                    cp.seq,
                    hashes.len()
                ),
                Some(h) if **h != cp.hash => {
                    bail!(
                        "tampered: checkpoint at seq {} does not match the log",
                        cp.seq
                    )
                }
                Some(_) => {}
            }
        }
        Ok(Verified {
            entries: hashes.len() as u64,
            head: prev,
            last_checkpoint: checkpoints.last().map(|cp| cp.seq),
        })
    }

    /// Verifies the log, then writes its head to `target`. Refuses to
    /// checkpoint a log that fails verification. Returns None when the log is
    /// empty or the head is already checkpointed.
    pub fn checkpoint(&self, target: &dyn CheckpointTarget) -> Result<Option<Checkpoint>> {
        let v = self.verify(target)?;
        if v.entries == 0 || v.last_checkpoint == Some(v.entries) {
            return Ok(None);
        }
        let cp = Checkpoint {
            seq: v.entries,
            hash: v.head,
            at: now(),
        };
        target.put(&cp)?;
        Ok(Some(cp))
    }
}
```

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test -p kod-audit && cargo clippy -p kod-audit --all-targets -- -D warnings && cargo fmt --all --check`
Expected: integrity 10 passed, log 8 passed, redact 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/kod-audit
git commit -m "kod-audit: write-once checkpoints and tamper and truncation checks"
```

### Task 4: Retention, the CLI and the runbook

**Files:**
- Modify: `crates/kod-audit/src/lib.rs` (one module line), `docs/phase1-runbook.md`
- Create: `crates/kod-audit/src/retention.rs`, `crates/kod-audit/src/main.rs`
- Test: `crates/kod-audit/tests/retention.rs`, `crates/kod-audit/tests/cli.rs`

**Interfaces:**
- Consumes:
  - From Tasks 2 and 3: `AuditLog::{tx, verify, checkpoint}`, `append_in`, `RETENTION`, `RUN_AUDIT`, `now`, `checkpoint::DirTarget` and `common::fixture`.
  - From kod-policy: `Policy::load(&Path) -> Result<Policy>`, `Policy::version(&self) -> &str`, `Policy::decide(&self, &Value) -> Decision`.
  - The repo's `policy/` directory, whose placeholder `compliance.json` is `{"profiles":{"placeholder":{}}}`.
- Produces:
  - `AuditLog::purge(&self, &Policy, record_type: &str, now: i64) -> Result<usize>`.
  - The binary `kod-audit [--db PATH] --checkpoints DIR (verify | checkpoint | purge [--policy DIR] --record-type TYPE)`, which prints one JSON line and exits non-zero on any error.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-audit/tests/retention.rs`:

```rust
mod common;

use common::fixture;
use kod_audit::{RETENTION, RUN_AUDIT, now};
use kod_policy::Policy;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const DAY: i64 = 86_400;

/// The repo's policy directory (kod-policy, sub-plan 3).
fn policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")
}

/// A temp copy of the policy directory with compliance.json replaced.
fn policy_with_compliance(compliance: Value) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(policy_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(dir.path().join("compliance.json"), compliance.to_string()).unwrap();
    dir
}

fn payloads(db: &Path) -> Vec<(i64, String, Option<String>)> {
    Connection::open(db)
        .unwrap()
        .prepare("SELECT seq, record_type, payload FROM audit ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn the_placeholder_profile_refuses_every_deletion_and_records_why() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let policy = Policy::load(&policy_dir()).unwrap();
    let in_a_century = now() + 36_500 * DAY;
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, in_a_century).unwrap(), 0);
    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 2);
    assert!(rows[0].2.is_some());
    let refusal: Value = serde_json::from_str(rows[1].2.as_ref().unwrap()).unwrap();
    assert_eq!(refusal["kind"], "policy_decision");
    assert_eq!(refusal["input"]["action"], "retention_delete");
    assert_eq!(refusal["decision"]["allow"], false);
    assert!(
        refusal["decision"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("placeholder has no retention_days.run_audit")
    );
    assert_eq!(refusal["decision"]["policy_version"], policy.version());
    f.log.verify(&f.target).unwrap();
}

#[test]
fn expired_payloads_are_deleted_and_the_chain_still_verifies() {
    let f = fixture();
    for n in 0..3 {
        f.log.append(RUN_AUDIT, &json!({"n": n})).unwrap();
    }
    f.log.append("prompts", &json!({"p": "kept"})).unwrap();
    f.log.checkpoint(&f.target).unwrap().unwrap();
    let dir = policy_with_compliance(
        json!({"profiles": {"a": {"retention_days": {"run_audit": 30, "prompts": 30}},
                            "b": {"retention_days": {"run_audit": 90, "prompts": 30}}}}),
    );
    let policy = Policy::load(dir.path()).unwrap();

    // 31 days: profile b still needs 90, so nothing goes, and the refusal is
    // recorded as entry 5.
    assert_eq!(
        f.log.purge(&policy, RUN_AUDIT, now() + 31 * DAY).unwrap(),
        0
    );
    // 91 days: every run_audit payload is old enough, the refusal included.
    assert_eq!(
        f.log.purge(&policy, RUN_AUDIT, now() + 91 * DAY).unwrap(),
        4
    );

    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 6);
    for i in [0, 1, 2, 4] {
        assert_eq!(rows[i].2, None, "seq {}", rows[i].0);
    }
    assert_eq!(rows[3].2.as_deref(), Some(r#"{"p":"kept"}"#));
    assert_eq!(rows[5].1, RETENTION);
    let deletion: Value = serde_json::from_str(rows[5].2.as_ref().unwrap()).unwrap();
    assert_eq!(deletion["kind"], "retention_delete");
    assert_eq!(deletion["seqs"], json!([1, 2, 3, 5]));
    assert_eq!(deletion["record_type"], RUN_AUDIT);
    assert_eq!(deletion["policy_version"], policy.version());
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 6);
}

#[test]
fn retention_records_are_never_purged() {
    let f = fixture();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"retention": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert!(f.log.purge(&policy, RETENTION, now()).is_err());
}

#[test]
fn entries_dated_in_the_future_count_as_zero_days_old() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 1}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    // The clock is set back a year: nothing is old enough.
    assert_eq!(
        f.log.purge(&policy, RUN_AUDIT, now() - 365 * DAY).unwrap(),
        0
    );
    assert!(payloads(&f.db())[0].2.is_some());
}
```

Create `crates/kod-audit/tests/cli.rs`:

```rust
mod common;

use common::fixture;
use kod_audit::RUN_AUDIT;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

/// Runs the kod-audit binary against the fixture; returns (exit ok, stdout JSON).
fn run(f: &common::Fixture, args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_kod-audit"))
        .arg("--db")
        .arg(f.db())
        .arg("--checkpoints")
        .arg(&f.target.dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

#[test]
fn checkpoint_and_verify_succeed_then_tampering_exits_non_zero() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    let (ok, v) = run(&f, &["checkpoint"]);
    assert!(ok, "{v}");
    assert_eq!(v["checkpoint"]["seq"], 2);
    let (ok, v) = run(&f, &["verify"]);
    assert!(ok, "{v}");
    assert_eq!(v["entries"], 2);
    f.attacker()
        .execute("UPDATE audit SET payload = '{}' WHERE seq = 1", [])
        .unwrap();
    let (ok, v) = run(&f, &["verify"]);
    assert!(!ok);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("tampered at seq 1"));
    let (ok, _) = run(&f, &["checkpoint"]);
    assert!(!ok);
}

#[test]
fn purge_refuses_under_the_placeholder_profile() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let policy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy");
    let policy = policy.to_str().unwrap();
    let (ok, v) = run(
        &f,
        &["purge", "--policy", policy, "--record-type", RUN_AUDIT],
    );
    assert!(ok, "{v}");
    assert_eq!(v, json!({"ok": true, "deleted": 0}));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p kod-audit --test retention --test cli`
Expected: compile errors, `no method named purge found for struct kod_audit::AuditLog`, and in cli.rs `environment variable CARGO_BIN_EXE_kod-audit not defined`.

- [ ] **Step 3: Implement retention and the CLI**

In `crates/kod-audit/src/lib.rs`, add `mod retention;` on the line after `pub mod redact;`.

Create `crates/kod-audit/src/retention.rs`:

```rust
use crate::{AuditLog, RETENTION, RUN_AUDIT, append_in};
use anyhow::{Result, bail};
use kod_policy::Policy;
use serde_json::json;

impl AuditLog {
    /// Deletes the payloads of `record_type` entries whose retention has
    /// ended, oldest first, asking the policy about each one and stopping at
    /// the first refusal. Rows and hashes stay, so the chain still verifies;
    /// one RETENTION entry lists what was deleted. When nothing is deleted,
    /// the refusal is recorded instead. Returns how many payloads were deleted.
    pub fn purge(&self, policy: &Policy, record_type: &str, now: i64) -> Result<usize> {
        if record_type == RETENTION {
            bail!("retention records are kept for the life of the log");
        }
        self.tx(|tx| {
            let candidates = tx
                .prepare(
                    "SELECT seq, at FROM audit WHERE record_type = ?1 AND payload IS NOT NULL
                     ORDER BY at, seq",
                )?
                .query_map([record_type], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut seqs = Vec::new();
            let mut ages = Vec::new();
            let mut last = None;
            for (seq, at) in candidates {
                // Whole days, rounded down; an entry from the future is 0 days old.
                let age = (now - at).max(0) / 86_400;
                let input =
                    json!({"action": "retention_delete", "record_type": record_type, "age_days": age});
                let decision = policy.decide(&input);
                if !decision.allow {
                    if seqs.is_empty() {
                        append_in(
                            tx,
                            RUN_AUDIT,
                            &json!({"kind": "policy_decision", "input": input, "decision": decision}),
                        )?;
                    }
                    break;
                }
                seqs.push(seq);
                ages.push(age);
                last = Some(decision);
            }
            let Some(decision) = last else {
                return Ok(0);
            };
            for seq in &seqs {
                tx.execute("UPDATE audit SET payload = NULL WHERE seq = ?1", [seq])?;
            }
            append_in(
                tx,
                RETENTION,
                &json!({
                    "kind": "retention_delete",
                    "record_type": record_type,
                    "seqs": seqs,
                    "oldest_age_days": ages.first(),
                    "youngest_age_days": ages.last(),
                    "policy_version": decision.policy_version,
                }),
            )?;
            Ok(seqs.len())
        })
    }
}
```

Create `crates/kod-audit/src/main.rs`:

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};
use kod_audit::{AuditLog, checkpoint::DirTarget, now};
use kod_policy::Policy;
use serde_json::json;
use std::{path::PathBuf, process::ExitCode};

/// Kod's audit log. Prints one JSON line; exits non-zero on any failure.
#[derive(Parser)]
#[command(name = "kod-audit", version)]
struct Cli {
    #[arg(long, default_value = "/var/lib/kod/audit.db")]
    db: PathBuf,
    /// Write-once checkpoint directory, owned by a separate account
    #[arg(long)]
    checkpoints: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the chain and every checkpoint
    Verify,
    /// Verify, then write the chain head as a new checkpoint
    Checkpoint,
    /// Verify, then delete payloads whose retention has ended
    Purge {
        #[arg(long, default_value = "/etc/kod/policy")]
        policy: PathBuf,
        #[arg(long)]
        record_type: String,
    },
}

fn run(cli: Cli) -> Result<serde_json::Value> {
    let log = AuditLog::open(&cli.db)?;
    let target = DirTarget {
        dir: cli.checkpoints,
    };
    Ok(match cli.command {
        Command::Verify => {
            let v = log.verify(&target)?;
            json!({"ok": true, "entries": v.entries, "last_checkpoint": v.last_checkpoint})
        }
        Command::Checkpoint => json!({"ok": true, "checkpoint": log.checkpoint(&target)?}),
        Command::Purge {
            policy,
            record_type,
        } => {
            log.verify(&target)?;
            let policy = Policy::load(&policy)?;
            json!({"ok": true, "deleted": log.purge(&policy, &record_type, now())?})
        }
    })
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", json!({"ok": false, "error": format!("{e:#}")}));
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 4: Run everything and see it pass**

Run: `cargo test --all && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check`
Expected: cli 2, integrity 10, log 8, redact 4 and retention 4 passed, plus every other crate's tests.

- [ ] **Step 5: Add the runbook section**

Append to `docs/phase1-runbook.md`:

````markdown
## Audit log

`kod-audit` runs on the audit host as the `kod` user. The database is `/var/lib/kod/audit.db` (mode 0600). Checkpoints go to `/mnt/kod-checkpoints`, a mount owned by the checkpoint account: `kod` can create files there but can't change or delete them.

Timers (systemd, both with `OnFailure=` alerting the owner):

```
# kod-audit-checkpoint.timer: OnCalendar=*:0/5
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints checkpoint
# kod-audit-verify.timer: OnCalendar=hourly
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints verify
```

Retention, per record type (refused while the compliance profile is a placeholder; the refusal is recorded):

```
kod-audit --checkpoints /mnt/kod-checkpoints purge --policy /etc/kod/policy --record-type run_audit
```

A failed `verify` means the log was changed. Stop publishing (kill switch), keep the database and the checkpoint directory as they are, and tell the owner. The error names the first bad entry.
````

- [ ] **Step 6: Commit**

```bash
git add crates/kod-audit docs/phase1-runbook.md
git commit -m "kod-audit: retention through the compliance profile, CLI and runbook"
```

## Whole-branch review

After Task 4, one fresh reviewer reads the whole branch against the spec sections listed under **Spec** and against this plan's Decisions and Review Focus. The reviewer checks in particular:

- Every write to the table goes through `append_in` inside `AuditLog::tx`. The only `UPDATE` is retention's `payload = NULL`.
- Redaction runs before serialization, and no code path stores an unredacted payload.
- `before` cannot run its closure after a failed append, and no error is swallowed (no `let _ =`, no `.ok()` on a write).
- `verify` checks the seq continuity, the payload hash, the entry hash, the NULL payloads and every checkpoint, and `checkpoint` refuses a log that fails it.
- `Cargo.lock` adds only rusqlite, libsqlite3-sys and their listed dependencies, and CI's cargo-deny step passes.

## Self-review

- **Spec coverage.**
  - Append-only, hash-chained: Task 2 (D2, D3).
  - Checkpoints every few minutes to write-once storage under a different account: Task 3 (D5) and the runbook timer in Task 4 (D6).
  - Modification and truncation show up against the checkpoints: Task 3 tests.
  - Tamper evidence, not tamper-proofing: the Honest ceiling section, and `truncation_after_the_last_checkpoint_is_not_detectable` pins the ceiling.
  - Prompts and outputs redacted before storage: Task 1 and Task 2 (D4).
  - Readable only by owner and auditors: D11, file mode 0600.
  - Deleted at the end of the profile's retention: Task 4 (D8).
  - Audit write fails, so the external write is refused: Task 2 `before` (D7). Gateway wiring is sub-plan 5.
  - Tampering, truncation and store-outage tests: Tasks 2 and 3.
  - Each decision recorded with its policy version: `record_decision` (Task 2, D10).
  - `retention_delete` matrix row: the policy decides, the evidence is a deletion record with record type, ages and policy version, a missing value refuses, and the placeholder refuses all (Task 4).
  - Coverage of gateway calls, approvals, merges, commands and diffs is the callers' job (sub-plans 2, 5, 6 and 7), through `append` and `before`.
- **Placeholders.** None in the steps. The only placeholder is the compliance profile, which is the policy repo's file, not this crate's.
- **Type consistency.**
  - The names in each Interfaces block match the code: `append_in`, `tx`, `sha256_hex`, `entry_hash`, `Verified`, `CheckpointTarget::{put, list}`, `DirTarget { dir }` and `purge(&Policy, &str, i64)`.
  - Test helpers: `common::fixture` and `Fixture::{db, attacker}` are used by integrity.rs and cli.rs. log.rs has its own `Fixture` without a target, and retention.rs has its own policy helpers.
  - The code in this plan is byte-identical to a scratch workspace in which every task's end state passed `cargo fmt --check`, `cargo clippy --all-targets -D warnings` and `cargo test`.
- **Review Focus.** Each of the five lines names tests in its owning task, and those tests are in that task's code above.

## Open items

- **Checkpoint storage and its account** (the backup owner, per the overview). It could be a WORM directory on the backup host owned by a separate account (`DirTarget`, no new code) or an S3 Object Lock bucket (a second `CheckpointTarget`). Until it's set up, checkpoints on the same host only catch accidents, not someone with host access.
- **Compliance profile values**: `retention_days` per record type in `policy/compliance.json`. Until then `purge` refuses and records why.
- **Auditor access**: shell access to the audit host under D11, or a read-only export, whichever the owner prefers once auditors are named.
