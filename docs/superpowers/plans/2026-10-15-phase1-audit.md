# Phase 1.4 Audit Log (`kod-audit`) Implementation Plan

> **For agentic workers:** the execution mode is set per task in **Execution** below. *Subagent + review* tasks use superpowers:subagent-driven-development (a fresh implementer, then a fresh reviewer before the next task starts). *Native* tasks use superpowers:executing-plans in the orchestrating session. Every mode ends with one whole-branch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `kod-audit`, a Rust library and CLI for Kod's audit log. It provides an append-only, hash-chained SQLite table, redacts secrets before anything is stored, writes write-once checkpoints of the chain head to storage under a separate account, and deletes data only when the compliance profile, asked through kod-policy, allows it. A failed audit write refuses the guarded external call.

**Architecture:** A new workspace crate, `crates/kod-audit`, with one store type, `AuditLog`.
- `append` redacts the payload, then inserts one row in a `BEGIN IMMEDIATE` transaction. Each row's hash covers the previous row's hash.
- `before(record_type, payload, call)` appends first and runs `call` only if the append succeeded. Sub-plan 5's gateway wraps every external write in it.
- `checkpoint` verifies the log, then puts `{seq, hash, at}` into a `CheckpointTarget`. That is a two-method trait: the local `DirTarget` is used in tests and in production, pointed at a write-once mount owned by another account.
- `verify` recomputes the chain from a read snapshot and checks it against every checkpoint. The authoritative run is off-host, under the checkpoint account, against a copy of the log and that account's own view of the checkpoints (D12). The on-host run is an early warning.
- `purge` asks `Policy::decide` with `retention_delete` for each expired entry, in batches. It writes a deletion record first, then removes only the payloads that record lists, so the chain still verifies.
- A small `kod-audit` CLI runs `verify`, `checkpoint` and `purge` from timers.

**Tech Stack:** Rust 1.99.0 (edition 2024), rusqlite 0.40 with the `bundled` feature (SQLite compiled in), sha2 0.10, serde and serde_json, clap 4, anyhow, and `kod-policy` (sub-plan 3). Tests use tempfile.

**Spec:** `docs/kod-harness-design.md`, in particular:
- "Audit integrity"
- "Trust boundaries" (the audit store is in the trusted zone, and the gateway writes the audit record before each external call), plus the action and permission matrix rows `retention_delete` and publish
- "Configurable policies → compliance profile" (record types, retention, the strictest profile wins)
- "Ownership"

The phase decomposition is in `2026-10-14-phase1-overview.md`, sub-plan 4. The policy API comes from `2026-10-15-phase1-policy.md` (D3, D4, D12). Execution modes follow `docs/adr/0001-per-task-execution-mode.md`.

**Precondition:** kod-policy (sub-plan 3, branch `feat/phase1-policy`) is merged to main before Task 1 starts. Every task depends on `kod_policy::{Policy, Decision}` and the repo's `policy/` directory. This revision's executable blocks are validated against the real policy implementation from PR #13 (`d5aa0c8`). The executor repeats the tests against the merged crate; policy reason strings and semantics come from that crate.

**Out of scope here** (later sub-plans, see the overview):
- The tool gateway that calls `before` around every external write and refuses publish when it fails (5).
- The LLM gateway that hands redacted prompt and output capture to `append` (2).
- The dispatcher (6).
- Evidence export (7).
- Real compliance profile values.
- The real checkpoint storage and its account. That is the backup owner's decision; see Open items.

**Honest ceiling: tamper-evident, not tamper-proof.**
- Anyone with write access to the audit host can still change, delete or replace rows. The off-host verifier (D12) detects it whenever the change touches an entry at or before the last checkpoint.
- The on-host `verify` is only as trustworthy as the host: root can bind-mount a forged checkpoint directory over `/mnt/kod-checkpoints` and rewrite the whole log. Checkpoints that the audit host itself reads catch accidents, not an attacker. Integrity against host access rests on the off-host verifier, which depends on the checkpoint account (Open items).
- Entries written after the last checkpoint can be truncated without trace. That window is the checkpoint interval (D6).
- Someone with host access can append a well-formed deletion record (raw SQL or `purge` with a doctored policy directory) and clear payloads it lists. Rows and hashes stay. `verify` flags a deletion record that is malformed, covers a later entry or covers another record type, and the off-host verifier's `verify_retention` (D13) flags one whose `policy_version` isn't a released version or whose policy doesn't allow each deletion. Only someone who can also add a released policy directory on the checkpoint host gets past that.
- `purge` trusts the host clock. A clock set forward deletes payloads early; a clock behind the newest entry refuses to purge.
- Someone who controls both the audit host and the checkpoint account can rewrite everything.
- Redaction is pattern-based: it catches the shapes in D4, not every secret, and errs towards redacting harmless values under secret-sounding names.

## Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **SQLite** through rusqlite 0.40 with `bundled`, one file (default `/var/lib/kod/audit.db`), WAL mode, `synchronous=FULL`, 2 s busy timeout. Appends run in `BEGIN IMMEDIATE`, so concurrent writers (gateway, dispatcher, CLI) serialize and the chain stays linear. | Six people, our own hosts, one trusted audit host: SQLite has no server to run, patch, back up or authenticate, and tests and CI need no service. WAL handles several writer processes on one host. Postgres would add a server and credentials for a write rate of a few rows per second. Its INSERT-only role doesn't stop a host admin either: the integrity evidence comes from the checkpoints, not from the database. **Ceiling:** writers must be on the audit host, because SQLite over a network filesystem is unsafe. Move to Postgres when a writer has to run on another host. |
| D2 | One row per entry: `seq, at, record_type, payload, payload_sha256, hash`. `hash = sha256(prev_hash ‖ seq ‖ at ‖ len(record_type) ‖ record_type ‖ payload_sha256)`, with the integers as 8-byte little-endian and the hashes as raw bytes. Entry 1's `prev` is `GENESIS` (64 zeros). The chain covers the payload's hash, not the payload. | Retention can then null a payload while every hash still verifies. The length prefix stops record type and payload hash from sliding into each other. `seq` in the hash makes reordering or renumbering visible. |
| D3 | SQLite triggers refuse `DELETE`; refuse an `INSERT` (including `INSERT OR REPLACE`, whose delete fires no trigger) unless `seq` is the next number; and refuse any `UPDATE` other than setting `payload` to NULL when a `retention` entry with a higher seq, already in the chain, lists that entry under its record type. | They guard against mistakes and stray SQL, not against an attacker: anyone who can write the file can drop them. The tamper tests drop them first, as an attacker would. Binding the payload clear to a chained deletion record means even stray SQL can't erase a payload without leaving the record. |
| D4 | **Redaction before storage**: `append` rejects serialized payloads over `MAX_PAYLOAD_BYTES` (1 MiB) before redaction, rechecks the redacted size, and runs `redact_value` before taking the write lock, so a secret never reaches the file or the WAL. One function, `redact`, runs on every string value and every object key, and replaces with `[REDACTED]`: (a) a known credential prefix (Anthropic and OpenAI `sk-`, Stripe `sk_live_` `rk_live_`, GitHub `ghp_` `gho_` `ghu_` `ghs_` `ghr_` `github_pat_`, GitLab `glpat-`, Slack `xox?-`, AWS `AKIA` `ASIA`, Google `AIza`, JWT `eyJ`, `bearer ` in any case) not preceded by a letter or digit and followed by at least 16 token characters, so `KEY=ghp_…`, `--token=sk-…`, `path/ghp_…` and `?t=…` are caught; (b) the value of `name = value` or `name: value`, the name optionally quoted and spaces allowed around the separator, when the name, ignoring case, `_` and `-`, contains SECRET, TOKEN, PASS, PWD, AUTH, APIKEY, ACCESSKEY or PRIVATEKEY (env dumps, YAML, JSON text, headers such as `X-Api-Key:`, query strings; quoted values with their quotes; `Authorization: Basic …` takes scheme and credentials; all-digit values such as `max_tokens=4096` kept; `PWD`, `OLDPWD` and `author` are not secret names); (c) the value after a secret-sounding `--name ` flag, and `-u user:password`; (d) the password in `scheme://user:password@host`; (e) any `-----BEGIN …PRIVATE KEY…-----` block (PGP's `PRIVATE KEY BLOCK` included), to the end of the text if the END line is missing. `redact_value` also replaces a string under a secret-sounding JSON key, and suffixes `#n` when two keys redact to the same text. | Nonmatching prefixes are rejected before scanning their bodies, so long nonmatching token text does not repeatedly scan the suffix. It's the smallest thing that catches what agents actually leak (tokens in commands, env dumps, keys in files, credentials in clone URLs) without a regex dependency. The 16-character minimum keeps `task-list`, `sk-learn` and `ask-me-…` intact. **Ceiling:** an unknown format, or a bare password with no telling name around it, isn't caught. Add a prefix or name when a new shape shows up. |
| D5 | **Checkpoints** go through `trait CheckpointTarget { put, list }`. `put` must publish complete, durable contents without overwriting a prior sequence. `DirTarget` stages a private `.checkpoint-*` file, makes it read-only and fsyncs it, then uses `persist_noclobber` and fsyncs the directory. Readers ignore staging names. The production target must provide these semantics under a **separate account**. Validate atomic publication, no-overwrite, durability, and account separation on the selected storage before the integrity exit criterion. | Staging prevents partial files from appearing as checkpoints. The local adapter is a test fake until that storage contract is verified; read-only permissions do not stop a file's owner from changing them. A second `CheckpointTarget` can use storage-native atomic write-once publication. `checkpoint` verifies first and refuses a tampered log. |
| D6 | systemd timers on the audit host run `kod-audit checkpoint` **every 5 minutes**, `kod-audit verify` hourly and `kod-audit purge` daily for each record type. Any failure exits non-zero with `{"ok":false,"error":…}`, and `OnFailure=` alerts the owner. | The spec says "every few minutes". Five minutes bounds the untraceable truncation window. `verify` reads a snapshot and `purge` writes in batches, so neither holds the write lock long enough to time out a gateway append. The runbook section (Task 4) has the units. |
| D7 | **Fail closed**: `before(record_type, payload, call)` returns `Err` without running `call` whenever the append fails: the store is busy beyond 2 s, the disk is full, the file is unreadable, or the record type is invalid. `AuditLog::open` fails on a missing directory or a file that isn't a database. | This is the spec's failure rule: when the audit write fails, the external write is refused. It's a closure rather than a flag, so a caller can't run the call after forgetting to check. Sub-plan 5 wraps publish and every tracker or docs write in it. |
| D8 | **Logical retention clearing**: `purge(policy, record_type, now)` refuses when `now` is behind the newest entry. It takes the entries of that type that still have a payload, oldest first, in batches of `PURGE_BATCH` (500) per transaction. It asks `decide({"action":"retention_delete","record_type", "age_days"})` for each, with whole days rounded down, and stops at the first deny, which is always recorded as a `policy_decision`. For the allowed entries it appends one `retention` entry `{record_type, seqs, oldest_age_days, youngest_age_days, policy_version}`, then sets their `payload = NULL`. Only `purge` writes `retention` entries (`append` and `before` refuse that type), and they are never purged. Rows and hashes are never deleted. | This follows the matrix row: the decision lives in the policy (strictest profile wins, a missing value refuses), and the evidence is a deletion record in the chain with the policy version. Keeping rows and hashes means truncation detection survives retention. The placeholder profile refuses everything, so nothing can be deleted until the owner approves values and the storage-erasure/snapshot-expiry procedure. The CLI reports `payloads_cleared`, not physical deletions; WAL, freed pages and copies are outside this logical operation. |
| D9 | `verify` accepts a `retention` entry only if it has `record_type`, `seqs` and `policy_version`, and every listed seq is an earlier entry of that record type (never a `retention` entry). It reports `payload removed without a retention record` for any NULL payload no valid `retention` entry lists. | Otherwise a hand-written or forged deletion record would be a silent way to erase content. A malformed one can only come from raw writes, so failing `verify` on it is the right alert. |
| D10 | `record_decision(input, &Decision)` appends `run_audit` `{"kind":"policy_decision","input","decision"}`, where the decision carries `policy_version` (policy D4). Record types are lower-case letters and underscores. They are the keys of the compliance profile's `retention_days` (`run_audit`, `prompts`, `model_outputs`, …), plus `retention`. | One shape for every decision, so an auditor can find which rules decided. The record type naming matches the policy's `retention_days.<record_type>` lookup, so retention works per type without a mapping table. |
| D11 | `open` creates the database file mode `0600` before SQLite opens it, and SQLite gives the `-wal` and `-shm` files the same mode. The directory `/var/lib/kod` is `0700`, owned by the kod service user on the audit host. Only the kod user reads the live database. Owner and auditors read the snapshot the off-host verifier (D12) has just verified, on the checkpoint host (a private `/srv/kod-verify/run.*/audit.db` with a `VERIFIED` marker). Only the checkpoint account has access by default; grant named owner/auditor accounts read-only access to successful snapshots through the approved access procedure. | "Readable only by owner and auditors" is an OS permission, not an application feature. Payloads sit in the WAL before a checkpoint, so its mode matters as much as the database's. Root on the audit host could serve the verifier an honest copy while doctoring the live file, so auditors read the copy that was verified, not the live one. Build a reader role when auditors need more. |
| D12 | **Off-host verifier**: an hourly job under the independent checkpoint account first runs `capture-checkpoints --output NEW_DIR` against its trusted checkpoint store, then pulls a `VACUUM INTO` database copy into the same private run directory. It verifies that copy against the **frozen** checkpoint set and released policies, then writes `VERIFIED` only on success. | Freezing checkpoints before the DB snapshot prevents a legitimate later checkpoint from being mistaken for truncation. Every captured checkpoint must still match; never ignore anchors merely because they exceed the copied head. `captured_checkpoints_accept_an_older_snapshot_after_live_appends`, `captured_checkpoints_still_detect_a_truncated_snapshot` and the forged-mount test pin these boundaries. |
| D13 | **Released policies**: the checkpoint host keeps every released policy directory, one subdirectory per version, never deleted. `verify_retention(&[Policy])` (CLI `verify --released-policies DIR`) checks that each `retention` entry's `policy_version` is one of them and re-runs that policy's `decide` for every listed seq, with the age from the retention entry's `at` minus the entry's `at`. `purge` stamps its entries with its own `now`, so the ages match what it asked. | A forged but well-formed deletion record then fails automatically instead of waiting for an auditor. Keeping every released policy also makes the profiles behind each deletion recoverable: `policy_version` hashes `compliance.json`. |

## Execution

The modes follow [ADR 0001](../../adr/0001-per-task-execution-mode.md): a task that owns a safety guarantee or a Review Focus test runs as Subagent + review, and every other task runs Native. Every task here owns one, so every task is Subagent + review. The plan ends with one whole-branch review.

| Task | Mode | Why |
|---|---|---|
| 1. Crate and secret redaction | Subagent + review | Owns redaction before storage (Review Focus 4). |
| 2. Hash-chained store, decisions and the fail-closed guard | Subagent + review | Owns append-only, the chain, concurrent writers and the store-outage refusal (Review Focus 1 and 2). |
| 3. Checkpoints and verification | Subagent + review | Owns tamper and truncation detection and write-once checkpoints (Review Focus 3 and 5). |
| 4. Retention, the CLI and the runbook | Subagent + review | Owns retention through the policy, deletion evidence, the released-policy check and the placeholder refusal (Review Focus 5). |

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
2. **The store being down, locked by another writer, missing or not a database** must make the append fail, and the guarded external call must never run; and the audit job itself (verify, a large purge) must not be what locks it. Pinned by Task 2 `when_the_store_is_down_the_external_write_is_refused` and `a_store_that_cannot_be_opened_is_an_error`, Task 3 `verify_does_not_take_the_write_lock`, and Task 4 `large_purges_run_in_batches`. Task 2 `oversized_payloads_are_refused_before_waiting_for_the_write_lock` also requires over-limit caller payloads to fail before lock contention and before the external callback.
3. **A full rewrite with every hash recomputed, hidden behind a forged checkpoint mount, a replaced empty log, a junk checkpoint, or a checkpoint written while verify runs**: the off-host verify must fail, a tampered log must never be checkpointed, and a race or a foreign file must not raise a false alarm. Pinned by Task 3 `a_rewritten_chain_is_caught_by_the_checkpoint`, `the_off_host_verifier_catches_a_rewrite_behind_a_forged_mount`, `a_replaced_empty_log_is_caught`, `an_unreadable_checkpoint_store_fails_verification`, `a_checkpoint_taken_during_verify_is_not_reported_as_truncation` and `foreign_files_in_the_checkpoint_store_are_ignored`. Task 3's unit tests `staged_checkpoint_is_invisible_until_published` and `interrupted_staging_does_not_poison_the_store` cover publication; Task 4's two captured-checkpoint snapshot tests cover off-host ordering and real truncation.
4. **Secrets in odd places**: after `KEY=`, `--token=` or `/`, with spaces or a colon (`key = x`, YAML, JSON text, headers), in flags (`--password x`, `-u user:pass`), as Stripe keys or PGP private keys, in env dumps, clone URLs and query strings, in nested arrays and objects, as object keys, and a private key cut off before its END line. Ordinary text that resembles a prefix or assignment (`task-list`, `sk-learn`, `max_tokens=4096`, `host:8443/a@b`) must be left alone. Pinned by Task 1 `tokens_after_punctuation_are_redacted`, `names_match_without_separators_or_case`, `spaced_and_colon_assignments_are_redacted`, `credential_headers_are_redacted`, `credentials_in_command_flags_are_redacted`, `any_private_key_label_is_redacted`, `stripe_keys_are_redacted`, `sensitive_assignments_and_env_dumps_are_redacted`, `urls_are_redacted`, `every_string_and_key_in_a_payload_is_redacted`, `private_key_blocks_are_redacted_even_without_an_end` and `ordinary_text_is_left_alone`, and Task 2 `secrets_are_redacted_before_storage`. `long_non_matching_tokens_are_left_alone` must also complete under the five-second built-test-binary budget in Task 1.
5. **A forged deletion record, a payload erased by hand, a wrong clock, or a refusal mid-purge** must not lose evidence:
   - `append` refuses `retention`, the store refuses to clear an uncovered payload, `verify` flags a malformed or overreaching deletion record, and `verify_retention` flags a well-formed one that no released policy allows;
   - a NULL payload with no valid retention record is flagged;
   - a clock behind the newest entry refuses to purge, and a refusal after some deletions is recorded;
   - `retention` entries can't be purged.

   Pinned by Task 2 `retention_records_cannot_be_appended_directly` and `payloads_cannot_be_cleared_or_rows_replaced_through_sql`, Task 3 `a_forged_retention_record_does_not_excuse_a_removed_payload` and `a_removed_payload_without_a_retention_record_is_caught`, and Task 4 `a_forged_well_formed_retention_record_fails_automatically`, `a_purge_checks_out_only_against_its_released_policy`, `verify_checks_retention_records_against_the_released_policies`, `a_clock_behind_the_newest_entry_refuses_to_purge`, `a_refusal_after_some_deletions_is_recorded` and `retention_records_are_never_purged`.

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
//! touches an entry at or before the last checkpoint and `verify` reads
//! checkpoints the host cannot forge (run it under the checkpoint account,
//! against a copy of the log). Entries after the last checkpoint can be
//! truncated without trace.
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
    let pem = concat!("a\n-----BE", "GIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\nb");
    assert_eq!(redact(pem), format!("a\n{REDACTED}\nb"));
    let cut = concat!("a\n-----BE", "GIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA");
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
        "max_tokens=4096 and key=value",
        "https://example.com:8443/a@b and user@example.com",
        "PWD=/home/kod OLDPWD=/tmp author: Jane timeout = 30 port: 5432",
        "at 12:30 see https://example.com",
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn tokens_after_punctuation_are_redacted() {
    for (text, want) in [
        (
            concat!("GITHUB_TOKEN=gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
            format!("GITHUB_TOKEN={REDACTED}"),
        ),
        (
            concat!("export AWS_ACCESS_KEY_ID=AK", "IAIOSFODNN7EXAMPLE"),
            format!("export AWS_ACCESS_KEY_ID={REDACTED}"),
        ),
        (
            concat!("--token=sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz"),
            format!("--token={REDACTED}"),
        ),
        (
            concat!("path/gh", "p_abcdefghijklmnopqrstuvwxyz0123456789/x"),
            format!("path/{REDACTED}"),
        ),
        (
            "AUTHORIZATION: BEARER abcdefghijklmnopqrstuvwxyz",
            format!("AUTHORIZATION: {REDACTED}"),
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn sensitive_assignments_and_env_dumps_are_redacted() {
    let env = "HOME=/home/kod\nDB_PASSWORD=hunter2hunter2\nAPI_KEY='abc def'\nmy_secret=\"x\"\nPATH=/usr/bin";
    assert_eq!(
        redact(env),
        format!(
            "HOME=/home/kod\nDB_PASSWORD={REDACTED}\nAPI_KEY={REDACTED}\nmy_secret={REDACTED}\nPATH=/usr/bin"
        )
    );
    assert_eq!(
        redact("kod run --db-password=s3cret --verbose"),
        format!("kod run --db-password={REDACTED} --verbose")
    );
}

#[test]
fn urls_are_redacted() {
    assert_eq!(
        redact("clone https://kod:s3cretpass@git.example.com/repo.git now"),
        format!("clone https://kod:{REDACTED}@git.example.com/repo.git now")
    );
    assert_eq!(
        redact("GET https://api.example.com/v1?access_token=abc123xyz&page=2"),
        format!("GET https://api.example.com/v1?access_token={REDACTED}&page=2")
    );
}

#[test]
fn every_string_and_key_in_a_payload_is_redacted() {
    let v = json!({
        "prompt": concat!("use gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"),
        "nested": {"list": [1, concat!("xo", "xp-1234567890-abcdefghijklmnop"), null]},
        "password": "hunter2",
        (concat!("sk", "-ant-api03-abcdefghijklmnopqrstuvwxyz")): 1,
        (concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789")): 2
    });
    assert_eq!(
        redact_value(&v),
        json!({
            "prompt": format!("use {REDACTED}"),
            "nested": {"list": [1, REDACTED, null]},
            "password": REDACTED,
            (REDACTED): 2,
            (format!("{REDACTED}#2")): 1
        })
    );
}

#[test]
fn names_match_without_separators_or_case() {
    assert_eq!(
        redact("DB_PASS=x1 MYSQL_PWD=x2 db-auth=x3"),
        format!("DB_PASS={REDACTED} MYSQL_PWD={REDACTED} db-auth={REDACTED}")
    );
    assert_eq!(
        redact_value(&json!({"apiKey": "k", "auth": "a", "pass": "p", "max_tokens": 5})),
        json!({"apiKey": REDACTED, "auth": REDACTED, "pass": REDACTED, "max_tokens": 5})
    );
}

#[test]
fn spaced_and_colon_assignments_are_redacted() {
    for (text, want) in [
        (
            concat!("aws_secret_access_key = ", "wJalrXUtnFEMI/K7MDENG"),
            format!("aws_secret_access_key = {REDACTED}"),
        ),
        (
            r#"{"password": "hunter2", "user": "kod"}"#,
            format!(r#"{{"password": {REDACTED}, "user": "kod"}}"#),
        ),
        (
            "db:\n  password: hunter2\n  port: 5432",
            format!("db:\n  password: {REDACTED}\n  port: 5432"),
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn credential_headers_are_redacted() {
    for (text, want) in [
        (
            "Authorization: Basic dXNlcjpwYXNz",
            format!("Authorization: {REDACTED}"),
        ),
        (
            "authorization: basic dXNlcjpwYXNzd29yZA== x",
            format!("authorization: {REDACTED} x"),
        ),
        ("X-Api-Key: abc123def", format!("X-Api-Key: {REDACTED}")),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn credentials_in_command_flags_are_redacted() {
    for (text, want) in [
        (
            concat!("curl -u ", "kod:s3cret https://x"),
            format!("curl -u {REDACTED} https://x"),
        ),
        (
            "mysql --password hunter2 db",
            format!("mysql --password {REDACTED} db"),
        ),
        ("ls -u -l", "ls -u -l".to_string()),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn any_private_key_label_is_redacted() {
    let pgp = concat!("a\n-----BE", "GIN PGP PRIVATE KEY BLOCK-----\nlQOYBF\n-----END PGP PRIVATE KEY BLOCK-----\nb");
    assert_eq!(redact(pgp), format!("a\n{REDACTED}\nb"));
}

#[test]
fn stripe_keys_are_redacted() {
    for secret in [
        concat!("sk", "_live_abcdefghijklmnopqrstuvwx"),
        concat!("rk", "_live_abcdefghijklmnopqrstuvwx"),
    ] {
        assert_eq!(redact(&format!("key {secret}")), format!("key {REDACTED}"));
    }
}

#[test]
fn long_non_matching_tokens_are_left_alone() {
    let text = "_".repeat(128 * 1024);
    assert_eq!(redact(&text), text);
}
```

- [ ] **Step 3: Run it and see it fail**

Run: `cargo test -p kod-audit --test redact`
Expected: compile error, `file not found for module redact` (and `unresolved import kod_audit::redact`).

- [ ] **Step 4: Implement redaction**

Create `crates/kod-audit/src/redact.rs`:

```rust
//! Secret redaction, applied to every string and object key in a payload
//! before it is hashed and stored. Pattern-based: it catches the shapes listed
//! here, not every secret. Unknown formats pass through.
use serde_json::{Map, Value};

pub const REDACTED: &str = "[REDACTED]";

/// A token is a known prefix, not preceded by an ASCII letter or digit (so
/// `KEY=`, `--x=`, `/` and `?a=` all count as a start), followed by at least
/// MIN_BODY token characters. `bearer ` matches in any case.
const PREFIXES: [&str; 21] = [
    "sk-ant-",
    "sk-",
    "sk_live_",
    "rk_live_",
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
    "bearer ",
];
const MIN_BODY: usize = 16;
/// A name containing one of these, ignoring case, `_` and `-`, marks its
/// value as secret: `name=value` and `name: value` in env dumps, config files,
/// headers and query strings, `--name value` flags, and `"name": "value"` in
/// JSON payloads. It errs towards redacting (`bypass: x` goes too).
const SECRET_NAMES: [&str; 8] = [
    "SECRET",
    "TOKEN",
    "PASS",
    "PWD",
    "AUTH",
    "APIKEY",
    "ACCESSKEY",
    "PRIVATEKEY",
];

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_./+=".contains(&b)
}

fn is_secret_name(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .collect::<String>()
        .to_ascii_uppercase();
    // The working directory and authors are not secrets; Authorization is.
    if matches!(name.as_str(), "PWD" | "OLDPWD") {
        return false;
    }
    let name = name.replace("AUTHORIZATION", "AUTH").replace("AUTHOR", "");
    SECRET_NAMES.iter().any(|s| name.contains(s))
}

/// Redacts every string and object key in `v`, recursively. A string under a
/// secret-sounding key is replaced whole. Two keys that redact to the same
/// text are kept apart with a `#n` suffix.
pub fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, v) in o {
                let base = redact(k);
                let mut key = base.clone();
                let mut n = 1;
                while out.contains_key(&key) {
                    n += 1;
                    key = format!("{base}#{n}");
                }
                let v = match v {
                    Value::String(_) if is_secret_name(k) => Value::String(REDACTED.into()),
                    v => redact_value(v),
                };
                out.insert(key, v);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The one redaction function: PEM private keys, then known tokens, secret
/// assignments, secret flags and URL passwords.
pub fn redact(text: &str) -> String {
    redact_tokens(&redact_pem(text))
}

/// PEM private key blocks (any label containing PRIVATE KEY), from BEGIN to
/// the matching END. A block with no END
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
        if !label.contains("PRIVATE KEY") {
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
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let secret_end = token_at(text, i)
            .or_else(|| assigned_value_at(text, i))
            .or_else(|| flag_value_at(text, i))
            .or_else(|| url_password_at(text, i));
        match secret_end {
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

/// End of a known-prefix token starting at `i`.
fn token_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    PREFIXES.iter().find_map(|p| {
        let head = bytes.get(i..i + p.len())?;
        let hit = if *p == "bearer " {
            head.eq_ignore_ascii_case(p.as_bytes())
        } else {
            head == p.as_bytes()
        };
        if !hit {
            return None;
        }
        let body = bytes[i + p.len()..]
            .iter()
            .take_while(|b| is_token_byte(**b))
            .count();
        (body >= MIN_BODY).then_some(i + p.len() + body)
    })
}

/// End of a value starting at `i`: a quoted value with its quotes, otherwise
/// up to whitespace or `"'&;,`.
fn value_end(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes[i] {
        q @ (b'"' | b'\'') => text[i + 1..]
            .find(q as char)
            .map_or(text.len(), |e| i + e + 2),
        _ => {
            i + bytes[i..]
                .iter()
                .take_while(|b| !b.is_ascii_whitespace() && !b"\"'&;,".contains(b))
                .count()
        }
    }
}

/// End of the value of `name = value` or `name: value` starting at `i`, when
/// the name (optionally quoted) is secret-sounding. `Authorization: Basic x`
/// takes the scheme and the credentials. An all-digit value
/// (`max_tokens=4096`) is kept.
fn assigned_value_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let blank = |b: u8| b == b' ' || b == b'\t';
    if bytes[i].is_ascii_whitespace() {
        return None;
    }
    let mut j = i;
    while j > 0 && blank(bytes[j - 1]) {
        j -= 1;
    }
    if j == 0 || !b":=".contains(&bytes[j - 1]) {
        return None;
    }
    j -= 1;
    while j > 0 && blank(bytes[j - 1]) {
        j -= 1;
    }
    if j > 0 && b"\"'".contains(&bytes[j - 1]) {
        j -= 1;
    }
    let name_start = bytes[..j]
        .iter()
        .rposition(|b| !(b.is_ascii_alphanumeric() || b"_-".contains(b)))
        .map_or(0, |p| p + 1);
    if !is_secret_name(&text[name_start..j]) {
        return None;
    }
    let mut end = value_end(text, i);
    let scheme = text[i..end].to_ascii_lowercase();
    if matches!(scheme.as_str(), "basic" | "bearer" | "token" | "digest")
        && bytes.get(end) == Some(&b' ')
        && bytes.get(end + 1).is_some_and(|b| !b.is_ascii_whitespace())
    {
        end = value_end(text, end + 1);
    }
    let digits = bytes[i..end].iter().all(u8::is_ascii_digit);
    (end > i && !digits).then_some(end)
}

/// End of the value of `--name value` (a secret-sounding flag) or
/// `-u user:password` starting at `i`.
fn flag_value_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i < 2 || bytes[i - 1] != b' ' || bytes[i] == b'-' || bytes[i].is_ascii_whitespace() {
        return None;
    }
    let flag_start = bytes[..i - 1]
        .iter()
        .rposition(u8::is_ascii_whitespace)
        .map_or(0, |p| p + 1);
    let flag = &text[flag_start..i - 1];
    let end = value_end(text, i);
    let hit = match flag {
        "-u" | "--user" => text[i..end].contains(':'),
        f => f.starts_with("--") && is_secret_name(f),
    };
    hit.then_some(end)
}

/// End of the password in `scheme://user:password@host` starting at `i`, just
/// after the `:`.
fn url_password_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i == 0 || bytes[i - 1] != b':' {
        return None;
    }
    let user_start = text[..i - 1].rfind("://")? + 3;
    let user = &bytes[user_start..i - 1];
    if user
        .iter()
        .any(|b| b"/@:".contains(b) || b.is_ascii_whitespace())
    {
        return None;
    }
    let len = bytes[i..]
        .iter()
        .take_while(|b| !b"/@".contains(b) && !b.is_ascii_whitespace())
        .count();
    (len > 0 && bytes.get(i + len) == Some(&b'@')).then_some(i + len)
}
```

- [ ] **Step 5: Run it and see it pass**

Run: `cargo test -p kod-audit --test redact && cargo clippy -p kod-audit --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 14 passed, no warnings, no diff. Also run the built `redact` test binary's `long_non_matching_tokens_are_left_alone` test under `timeout 5s`, excluding Cargo compilation from the budget. This bounds processing of 128 KiB of nonmatching token characters; the previous suffix scan took quadratic time.

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
  - `now() -> Result<i64>` (unix seconds; a clock before 1970 is an error).
  - `AuditLog::open(&Path) -> Result<AuditLog>`.
  - `AuditLog::append(&self, &str, &Value) -> Result<u64>`, which refuses `RETENTION`.
  - `AuditLog::record_decision(&self, &Value, &Decision) -> Result<u64>`.
  - `AuditLog::before<T>(&self, &str, &Value, impl FnOnce() -> T) -> Result<T>`.
  - Crate-private helpers used by Tasks 3 and 4: `sha256_hex(&[u8]) -> String`, `prepare_payload(&Value) -> Result<String>`, `append_text_in(&Transaction, &str, &str, at: i64) -> Result<u64>` (`append` prepares before locking and passes `now()?`), and `AuditLog::tx(&self, TransactionBehavior, impl FnOnce(&Transaction) -> Result<T>) -> Result<T>` (one transaction: Immediate for writers, Deferred for readers). Task 4 adds `append_in` for retention's bounded internal payloads.
  - Table `audit(seq, at, record_type, payload, payload_sha256, hash)` with the triggers `audit_append_only`, `audit_no_delete` and `audit_only_purge`.

- [ ] **Step 1: Write the failing test**

Create `crates/kod-audit/tests/log.rs`:

```rust
use kod_audit::{AuditLog, GENESIS, RETENTION, RUN_AUDIT, entry_hash};
use kod_policy::Decision;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{cell::Cell, path::PathBuf, thread};
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
fn payloads_cannot_be_cleared_or_rows_replaced_through_sql() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    let c = Connection::open(f.db()).unwrap();
    for sql in [
        // No retention record covers seq 1.
        "UPDATE audit SET payload = NULL WHERE seq = 1",
        // REPLACE deletes without firing the delete trigger.
        "INSERT OR REPLACE INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (1, 0, 'run_audit', '{}', 'x', 'x')",
        "INSERT INTO audit (seq, at, record_type, payload, payload_sha256, hash)
         VALUES (9, 0, 'run_audit', '{}', 'x', 'x')",
    ] {
        let err = c.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
}

#[test]
fn retention_records_cannot_be_appended_directly() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let forged = json!({"kind": "retention_delete", "record_type": RUN_AUDIT, "seqs": [1]});
    assert!(f.log.append(RETENTION, &forged).is_err());
    assert!(f.log.before(RETENTION, &forged, || ()).is_err());
    assert_eq!(f.chain_len(), 1);
}

#[cfg(unix)]
#[test]
fn the_store_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    f.log
        .append(RUN_AUDIT, &json!({"prompt": "private"}))
        .unwrap();
    for name in ["audit.db", "audit.db-wal", "audit.db-shm"] {
        let mode = std::fs::metadata(f.dir.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

#[test]
fn concurrent_writers_keep_one_linear_chain() {
    let f = fixture();
    let db = f.db();
    thread::scope(|s| {
        for w in 0..4 {
            let log = AuditLog::open(&db).unwrap();
            s.spawn(move || {
                for n in 0..25 {
                    log.append(RUN_AUDIT, &json!({"w": w, "n": n})).unwrap();
                }
            });
        }
    });
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

#[test]
fn oversized_payloads_are_refused_before_waiting_for_the_write_lock() {
    let f = fixture();
    let blocker = Connection::open(f.db()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let called = Cell::new(false);
    let payload = json!({"output": "_".repeat(kod_audit::MAX_PAYLOAD_BYTES)});
    let err = f
        .log
        .before(RUN_AUDIT, &payload, || called.set(true))
        .unwrap_err();
    assert!(err.to_string().contains("payload exceeds"), "{err:#}");
    assert!(!called.get());
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.chain_len(), 0);
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p kod-audit --test log`
Expected: compile error, `unresolved imports kod_audit::AuditLog, kod_audit::GENESIS, kod_audit::RETENTION, kod_audit::RUN_AUDIT, kod_audit::entry_hash`.

- [ ] **Step 3: Implement the store**

Replace `crates/kod-audit/src/lib.rs` with:

```rust
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

use anyhow::{Context, Result, anyhow, bail};
use kod_policy::Decision;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The `prev` hash of entry 1.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Policy decisions and gateway calls.
pub const RUN_AUDIT: &str = "run_audit";
/// Deletion records, written only by `purge`. Never deleted themselves:
/// `verify` needs them to account for every removed payload.
pub const RETENTION: &str = "retention";
/// How long a write waits for another writer before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum serialized payload size, checked before redaction and locking.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit (
    seq INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    record_type TEXT NOT NULL,
    payload TEXT,
    payload_sha256 TEXT NOT NULL,
    hash TEXT NOT NULL
) STRICT;
CREATE TRIGGER IF NOT EXISTS audit_append_only BEFORE INSERT ON audit
WHEN NEW.seq IS NOT (SELECT COALESCE(MAX(seq), 0) + 1 FROM audit)
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_no_delete BEFORE DELETE ON audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS audit_only_purge BEFORE UPDATE ON audit
WHEN NEW.payload IS NOT NULL OR NEW.seq IS NOT OLD.seq OR NEW.at IS NOT OLD.at
  OR NEW.record_type IS NOT OLD.record_type
  OR NEW.payload_sha256 IS NOT OLD.payload_sha256 OR NEW.hash IS NOT OLD.hash
  OR NOT EXISTS (
    SELECT 1 FROM audit AS r, json_each(r.payload, '$.seqs') AS s
    WHERE r.record_type = 'retention' AND r.seq > OLD.seq
      AND json_extract(r.payload, '$.record_type') = OLD.record_type
      AND s.value = OLD.seq)
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

/// Unix seconds.
pub fn now() -> Result<i64> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before 1970")?;
    Ok(d.as_secs() as i64)
}

fn prepare_payload(payload: &Value) -> Result<String> {
    if serde_json::to_vec(payload)?.len() > MAX_PAYLOAD_BYTES {
        bail!("audit payload exceeds {MAX_PAYLOAD_BYTES} bytes");
    }
    let text = serde_json::to_string(&redact::redact_value(payload))?;
    if text.len() > MAX_PAYLOAD_BYTES {
        bail!("redacted audit payload exceeds {MAX_PAYLOAD_BYTES} bytes");
    }
    Ok(text)
}

fn append_text_in(tx: &Transaction, record_type: &str, text: &str, at: i64) -> Result<u64> {
    if record_type.is_empty()
        || !record_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        bail!("record type must be lower-case letters and underscores: {record_type:?}");
    }
    let (last, prev): (i64, String) = tx
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, GENESIS.to_string()));
    let seq = last + 1;
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
    /// The file is made 0600 before SQLite opens it, and SQLite gives the
    /// -wal and -shm files the same mode.
    pub fn open(path: &Path) -> Result<AuditLog> {
        let context = || format!("audit store {}", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .with_context(context)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let conn = Connection::open(path).with_context(context)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(AuditLog {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("audit log lock poisoned"))
    }

    /// Runs `f` in one transaction. Writers use Immediate, which takes the
    /// write lock up front; readers use Deferred, which in WAL mode reads a
    /// snapshot without blocking writers.
    pub(crate) fn tx<T>(
        &self,
        behavior: TransactionBehavior,
        f: impl FnOnce(&Transaction) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(behavior)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Redacts secrets in `payload`, then appends it. Returns the new seq.
    /// Refuses RETENTION: only `purge` writes deletion records.
    pub fn append(&self, record_type: &str, payload: &Value) -> Result<u64> {
        if record_type == RETENTION {
            bail!("retention records are written only by purge");
        }
        let text = prepare_payload(payload)?;
        self.tx(TransactionBehavior::Immediate, |tx| {
            append_text_in(tx, record_type, &text, now()?)
        })
    }

    /// Records a policy decision with its input and policy_version.
    pub fn record_decision(&self, input: &Value, decision: &Decision) -> Result<u64> {
        self.append(
            RUN_AUDIT,
            &json!({"kind": "policy_decision", "input": input, "decision": decision}),
        )
    }

    /// Writes the audit record, then runs `call`. If the write fails, `call`
    /// never runs: the external write is refused. This records the intent
    /// only; the caller appends the call's result as its own entry.
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
Expected: log 12 passed and redact 14 passed. `when_the_store_is_down_the_external_write_is_refused` takes about 2 s (the busy timeout).

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
  - Crate-private: `Row` and `AuditLog::rows(&self) -> Result<Vec<Row>>`, every row from one Deferred snapshot.
  - Test helpers: `common::{Fixture { dir, log, target }, fixture(), Fixture::db(), Fixture::attacker()}`; `attacker()` drops all three triggers.
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
        c.execute_batch(
            "DROP TRIGGER audit_no_delete; DROP TRIGGER audit_only_purge;
             DROP TRIGGER IF EXISTS audit_append_only;",
        )
        .unwrap();
        c
    }
}
```

Create `crates/kod-audit/tests/integrity.rs`:

```rust
mod common;

use anyhow::Result;
use common::fixture;
use kod_audit::{
    AuditLog, RETENTION, RUN_AUDIT,
    checkpoint::{Checkpoint, CheckpointTarget, DirTarget},
    entry_hash,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::cell::Cell;

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

/// Rewrites entry 2 and recomputes every hash after it, so the chain itself
/// is consistent again: only a checkpoint can show the change.
fn rewrite_entry_2(f: &common::Fixture) {
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
    let last: i64 = c
        .query_row("SELECT MAX(seq) FROM audit", [], |r| r.get(0))
        .unwrap();
    for seq in 2..=last {
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
}

#[test]
fn a_rewritten_chain_is_caught_by_the_checkpoint() {
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    rewrite_entry_2(&f);
    assert!(err(&f).contains("tampered: checkpoint at seq 3 does not match the log"));
    // And the tampered log is never checkpointed.
    assert!(f.log.checkpoint(&f.target).is_err());
}

#[test]
fn the_off_host_verifier_catches_a_rewrite_behind_a_forged_mount() {
    // f.target is the real write-once store under the checkpoint account.
    let f = filled(3);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    rewrite_entry_2(&f);
    // Root on the audit host points the on-host verify at a forged mount.
    let forged = DirTarget {
        dir: f.dir.path().join("forged"),
    };
    std::fs::create_dir(&forged.dir).unwrap();
    f.log.checkpoint(&forged).unwrap().unwrap();
    assert_eq!(f.log.verify(&forged).unwrap().entries, 3);
    // The off-host verifier checks a copy of the log against its own store.
    let copy = f.dir.path().join("copy.db");
    Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let e = AuditLog::open(&copy)
        .unwrap()
        .verify(&f.target)
        .unwrap_err();
    assert!(
        format!("{e:#}").contains("tampered: checkpoint at seq 3"),
        "{e:#}"
    );
}

/// Appends a correctly hashed retention entry through raw SQL, then clears
/// seq 1's payload: what a host attacker can do without going through purge.
fn forge_retention(f: &common::Fixture, payload: &Value) {
    let c = f.attacker();
    let (seq, prev): (i64, String) = c
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let text = payload.to_string();
    let ph = sha256_hex(&text);
    let hash = entry_hash(&prev, seq as u64 + 1, 0, RETENTION, &ph).unwrap();
    c.execute(
        "INSERT INTO audit VALUES (?1, 0, ?2, ?3, ?4, ?5)",
        params![seq + 1, RETENTION, text, ph, hash],
    )
    .unwrap();
    c.execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
        .unwrap();
}

#[test]
fn a_forged_retention_record_does_not_excuse_a_removed_payload() {
    for (payload, want) in [
        (json!({"seqs": [1]}), "malformed retention record"),
        (
            json!({"seqs": "no", "record_type": RUN_AUDIT, "policy_version": "v"}),
            "malformed retention record",
        ),
        (
            json!({"seqs": [1], "record_type": "prompts", "policy_version": "v"}),
            "not an earlier prompts entry",
        ),
        (
            json!({"seqs": [1, 9], "record_type": RUN_AUDIT, "policy_version": "v"}),
            "not an earlier run_audit entry",
        ),
    ] {
        let f = filled(2);
        forge_retention(&f, &payload);
        let e = format!("{:#}", f.log.verify(&f.target).unwrap_err());
        assert!(e.contains(want), "{payload}: {e}");
    }
}

#[test]
fn verify_does_not_take_the_write_lock() {
    let f = filled(2);
    let writer = Connection::open(f.db()).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 2);
    writer.execute_batch("ROLLBACK").unwrap();
}

/// On its first list, another process appends an entry and checkpoints it,
/// as the checkpoint timer can while verify runs.
struct Racing<'a> {
    f: &'a common::Fixture,
    raced: Cell<bool>,
}

impl CheckpointTarget for Racing<'_> {
    fn put(&self, cp: &Checkpoint) -> Result<()> {
        self.f.target.put(cp)
    }

    fn list(&self) -> Result<Vec<Checkpoint>> {
        if !self.raced.replace(true) {
            let other = AuditLog::open(&self.f.db())?;
            other.append(RUN_AUDIT, &json!({"n": "raced"}))?;
            other.checkpoint(&self.f.target)?;
        }
        self.f.target.list()
    }
}

#[test]
fn a_checkpoint_taken_during_verify_is_not_reported_as_truncation() {
    let f = filled(2);
    let racing = Racing {
        f: &f,
        raced: Cell::new(false),
    };
    f.log.verify(&racing).unwrap();
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 3);
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
    let junk = f.target.dir.join("00000000000000000007.json");
    std::fs::write(&junk, "not json").unwrap();
    assert!(err(&f).contains("unreadable checkpoint"));
    std::fs::remove_file(&junk).unwrap();
    std::fs::write(
        f.target.dir.join("00000000000000000009.json"),
        r#"{"seq":1,"hash":"00","at":0}"#,
    )
    .unwrap();
    assert!(err(&f).contains("names the wrong seq"));
    std::fs::remove_dir_all(&f.target.dir).unwrap();
    assert!(err(&f).contains("checkpoint directory"));
}

#[test]
fn foreign_files_in_the_checkpoint_store_are_ignored() {
    let f = filled(1);
    f.log.checkpoint(&f.target).unwrap().unwrap();
    for name in [".DS_Store", "lost+found", "notes.json", "1.json"] {
        std::fs::write(f.target.dir.join(name), "x").unwrap();
    }
    assert_eq!(f.log.verify(&f.target).unwrap().last_checkpoint, Some(1));
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
use tempfile::NamedTempFile;

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
    /// Publishes a complete, durable checkpoint atomically, without overwrite.
    fn put(&self, cp: &Checkpoint) -> Result<()>;
    /// Every stored checkpoint, in seq order. Fails on anything unreadable.
    fn list(&self) -> Result<Vec<Checkpoint>>;
}

/// One file per checkpoint, staged and synced before no-overwrite publication.
/// It is the test fake, and the production adapter when the directory is a
/// write-once mount owned by another account.
pub struct DirTarget {
    pub dir: PathBuf,
}

impl DirTarget {
    fn stage(&self, cp: &Checkpoint) -> Result<NamedTempFile> {
        let mut f = tempfile::Builder::new()
            .prefix(".checkpoint-")
            .tempfile_in(&self.dir)?;
        f.write_all(&serde_json::to_vec(cp)?)?;
        let mut perms = f.as_file().metadata()?.permissions();
        perms.set_readonly(true);
        f.as_file().set_permissions(perms)?;
        f.as_file().sync_all()?;
        Ok(f)
    }
}

impl CheckpointTarget for DirTarget {
    fn put(&self, cp: &Checkpoint) -> Result<()> {
        let path = self.dir.join(format!("{:020}.json", cp.seq));
        self.stage(cp)?
            .persist_noclobber(&path)
            .with_context(|| format!("write-once checkpoint {}", path.display()))?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Reads every `<20 digits>.json` file. Other names (`lost+found`,
    /// `.DS_Store`, NFS temp files) are not checkpoints and are skipped.
    fn list(&self) -> Result<Vec<Checkpoint>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("checkpoint directory {}", self.dir.display()))?
        {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_checkpoint = name
                .strip_suffix(".json")
                .is_some_and(|stem| stem.len() == 20 && stem.bytes().all(|b| b.is_ascii_digit()));
            if !is_checkpoint {
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_checkpoint_is_invisible_until_published() {
        let dir = tempfile::tempdir().unwrap();
        let target = DirTarget {
            dir: dir.path().to_owned(),
        };
        let cp = Checkpoint {
            seq: 1,
            hash: "a".repeat(64),
            at: 0,
        };
        let staged = target.stage(&cp).unwrap();
        assert!(target.list().unwrap().is_empty());
        drop(staged);
        assert!(target.list().unwrap().is_empty());
        target.put(&cp).unwrap();
        assert_eq!(target.list().unwrap(), vec![cp.clone()]);
        assert!(target.put(&cp).is_err());
        assert_eq!(target.list().unwrap(), vec![cp]);
    }

    #[test]
    fn interrupted_staging_does_not_poison_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let target = DirTarget {
            dir: dir.path().to_owned(),
        };
        fs::write(dir.path().join(".checkpoint-interrupted"), "{\"seq\":").unwrap();
        let cp = Checkpoint {
            seq: 1,
            hash: "a".repeat(64),
            at: 0,
        };
        target.put(&cp).unwrap();
        assert_eq!(target.list().unwrap(), vec![cp]);
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
use anyhow::{Result, bail};
use rusqlite::TransactionBehavior;
use serde_json::Value;
use std::collections::HashSet;

/// One audit row: seq, at, record_type, payload, payload_sha256, hash.
pub(crate) type Row = (u64, i64, String, Option<String>, String, String);

/// What `verify` checked.
#[derive(Debug, PartialEq)]
pub struct Verified {
    pub entries: u64,
    /// Hash of the last entry, or GENESIS for an empty log.
    pub head: String,
    pub last_checkpoint: Option<u64>,
}

impl AuditLog {
    /// Every row, from one snapshot (no write lock).
    pub(crate) fn rows(&self) -> Result<Vec<Row>> {
        // ponytail: reads every row on each run; page through it if the log
        // grows past what fits in memory.
        self.tx(TransactionBehavior::Deferred, |tx| {
            let mut stmt = tx.prepare(
                "SELECT seq, at, record_type, payload, payload_sha256, hash FROM audit ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Checks the whole chain, then every checkpoint against it. Any mismatch
    /// is an error naming the first problem found. Reads a snapshot, so it
    /// never holds the write lock. Run under the checkpoint account against a
    /// copy of the log, it is the authoritative check; on the audit host it is
    /// an early warning, since that host controls its own view of the
    /// checkpoints.
    pub fn verify(&self, target: &dyn CheckpointTarget) -> Result<Verified> {
        // Checkpoints first: one written while the rows are read then refers
        // to an entry inside the snapshot, not past its end.
        let checkpoints = target.list()?;
        let rows = self.rows()?;
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
                    // A deletion record covers only earlier entries of the
                    // record type it names. purge writes nothing else, so
                    // anything else is tampering.
                    let v: Value = serde_json::from_str(p)?;
                    let (Some(covered), Some(seqs), Some(_)) = (
                        v["record_type"].as_str(),
                        v["seqs"].as_array(),
                        v["policy_version"].as_str(),
                    ) else {
                        bail!("tampered at seq {seq}: malformed retention record");
                    };
                    for s in seqs {
                        let earlier = s
                            .as_u64()
                            .filter(|s| (1..*seq).contains(s))
                            .filter(|s| rows[*s as usize - 1].2 == covered && covered != RETENTION);
                        let Some(s) = earlier else {
                            bail!(
                                "tampered at seq {seq}: retention record covers {s}, \
                                 which is not an earlier {covered} entry"
                            );
                        };
                        purged.insert(s);
                    }
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
            at: now()?,
        };
        target.put(&cp)?;
        Ok(Some(cp))
    }
}
```

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test -p kod-audit && cargo clippy -p kod-audit --all-targets -- -D warnings && cargo fmt --all --check`
Expected: checkpoint unit tests 2 passed, integrity 15 passed, log 12 passed, redact 14 passed.

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
  - From Tasks 2 and 3: `AuditLog::{tx, rows, verify, checkpoint}`, `prepare_payload`, `append_text_in`, `RETENTION`, `RUN_AUDIT`, `now`, `checkpoint::DirTarget` and `common::fixture`. This task adds the `append_in` wrapper for retention records.
  - From kod-policy: `Policy::load(&Path) -> Result<Policy>`, `Policy::version(&self) -> &str`, `Policy::decide(&self, &Value) -> Decision`.
  - The repo's `policy/` directory, whose placeholder `compliance.json` is `{"profiles":{"placeholder":{}}}`.
- Produces:
  - `AuditLog::purge(&self, &Policy, record_type: &str, now: i64) -> Result<usize>` and `PURGE_BATCH: usize` (500).
  - `AuditLog::verify_retention(&self, released: &[Policy]) -> Result<usize>`, the number of retention entries checked.
  - The binary `kod-audit [--db PATH] --checkpoints DIR (verify [--released-policies DIR] | checkpoint | capture-checkpoints --output NEW_DIR | purge [--policy DIR] --record-type TYPE)`, which prints one JSON line and exits non-zero on any error. `capture-checkpoints` never opens the database.

- [ ] **Step 1: Write the failing tests**

Create `crates/kod-audit/tests/retention.rs`:

```rust
mod common;

use common::fixture;
use kod_audit::{GENESIS, PURGE_BATCH, RETENTION, RUN_AUDIT, entry_hash, now};
use kod_policy::Policy;
use rusqlite::{Connection, OptionalExtension, params};
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

/// Appends an entry dated `at` through raw SQL with a correct hash, since
/// `append` always stamps the current time.
fn append_dated(db: &Path, at: i64) {
    append_raw(db, at, RUN_AUDIT, r#"{"dated":true}"#);
}

/// Appends any entry through raw SQL, triggers intact, with a correct hash:
/// what someone who knows the (public) hash construction can do.
fn append_raw(db: &Path, at: i64, record_type: &str, text: &str) {
    use sha2::{Digest, Sha256};
    let c = Connection::open(db).unwrap();
    let (seq, prev): (i64, String) = c
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
        .unwrap_or((0, GENESIS.to_string()));
    let ph: String = Sha256::digest(text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let hash = entry_hash(&prev, seq as u64 + 1, at, record_type, &ph).unwrap();
    c.execute(
        "INSERT INTO audit VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq + 1, at, record_type, text, ph, hash],
    )
    .unwrap();
}

#[test]
fn the_placeholder_profile_refuses_every_deletion_and_records_why() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let policy = Policy::load(&policy_dir()).unwrap();
    let in_a_century = now().unwrap() + 36_500 * DAY;
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
        f.log
            .purge(&policy, RUN_AUDIT, now().unwrap() + 31 * DAY)
            .unwrap(),
        0
    );
    // 91 days: entries 1-3 are old enough. The refusal, written at 31 days,
    // is 60 days old, so it is refused in turn (entry 6) and kept.
    assert_eq!(
        f.log
            .purge(&policy, RUN_AUDIT, now().unwrap() + 91 * DAY)
            .unwrap(),
        3
    );

    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 7);
    for i in [0, 1, 2] {
        assert_eq!(rows[i].2, None, "seq {}", rows[i].0);
    }
    assert_eq!(rows[3].2.as_deref(), Some(r#"{"p":"kept"}"#));
    assert!(rows[4].2.is_some() && rows[5].2.is_some());
    assert_eq!(rows[6].1, RETENTION);
    let deletion: Value = serde_json::from_str(rows[6].2.as_ref().unwrap()).unwrap();
    assert_eq!(deletion["kind"], "retention_delete");
    assert_eq!(deletion["seqs"], json!([1, 2, 3]));
    assert_eq!(deletion["record_type"], RUN_AUDIT);
    assert_eq!(deletion["policy_version"], policy.version());
    assert_eq!(f.log.verify(&f.target).unwrap().entries, 7);
    assert_eq!(f.log.verify_retention(&[policy]).unwrap(), 1);
}

#[test]
fn retention_records_are_never_purged() {
    let f = fixture();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"retention": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert!(f.log.purge(&policy, RETENTION, now().unwrap()).is_err());
}

#[test]
fn a_clock_behind_the_newest_entry_refuses_to_purge() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    let err = f
        .log
        .purge(&policy, RUN_AUDIT, now().unwrap() - DAY)
        .unwrap_err();
    assert!(err.to_string().contains("clock is behind"), "{err:#}");
    assert!(payloads(&f.db())[0].2.is_some());
}

#[test]
fn a_refusal_after_some_deletions_is_recorded() {
    let f = fixture();
    let t = now().unwrap();
    for days in [40, 40, 10] {
        append_dated(&f.db(), t - days * DAY);
    }
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 30}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, t).unwrap(), 2);
    let rows = payloads(&f.db());
    assert_eq!(rows.len(), 5);
    assert!(rows[2].2.is_some());
    // Entry 4 is the refusal for entry 3; entry 5 lists the deletions.
    let refusal: Value = serde_json::from_str(rows[3].2.as_ref().unwrap()).unwrap();
    assert_eq!(refusal["decision"]["allow"], false);
    assert_eq!(refusal["input"]["age_days"], 10);
    let deletion: Value = serde_json::from_str(rows[4].2.as_ref().unwrap()).unwrap();
    assert_eq!(deletion["seqs"], json!([1, 2]));
    f.log.verify(&f.target).unwrap();
}

#[test]
fn large_purges_run_in_batches() {
    let f = fixture();
    for n in 0..=PURGE_BATCH {
        f.log.append(RUN_AUDIT, &json!({"n": n})).unwrap();
    }
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(
        f.log.purge(&policy, RUN_AUDIT, now().unwrap()).unwrap(),
        PURGE_BATCH + 1
    );
    let rows = payloads(&f.db());
    assert_eq!(rows.iter().filter(|r| r.1 == RETENTION).count(), 2);
    assert!(rows.iter().all(|r| r.1 == RETENTION || r.2.is_none()));
    f.log.verify(&f.target).unwrap();
}

#[test]
fn a_purge_checks_out_only_against_its_released_policy() {
    let f = fixture();
    let t = now().unwrap();
    append_dated(&f.db(), t - 40 * DAY);
    let dir =
        policy_with_compliance(json!({"profiles": {"a": {"retention_days": {"run_audit": 30}}}}));
    let policy = Policy::load(dir.path()).unwrap();
    assert_eq!(f.log.purge(&policy, RUN_AUDIT, t).unwrap(), 1);
    assert_eq!(f.log.verify_retention(&[policy]).unwrap(), 1);
    let placeholder = Policy::load(&policy_dir()).unwrap();
    let err = f.log.verify_retention(&[placeholder]).unwrap_err();
    assert!(
        err.to_string().contains("not a released version"),
        "{err:#}"
    );
}

#[test]
fn a_forged_well_formed_retention_record_fails_automatically() {
    let placeholder = Policy::load(&policy_dir()).unwrap();
    for (version, want) in [
        ("sha256:forged;regorus=0.13.0", "not a released version"),
        (placeholder.version(), "does not allow deleting seq 1"),
    ] {
        let f = fixture();
        f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
        f.log.checkpoint(&f.target).unwrap().unwrap();
        let forged = json!({"kind": "retention_delete", "record_type": RUN_AUDIT,
                            "seqs": [1], "policy_version": version});
        append_raw(&f.db(), now().unwrap(), RETENTION, &forged.to_string());
        // The triggers let the payload go: a chained retention entry covers it.
        Connection::open(f.db())
            .unwrap()
            .execute("UPDATE audit SET payload = NULL WHERE seq = 1", [])
            .unwrap();
        f.log.verify(&f.target).unwrap();
        let err = f
            .log
            .verify_retention(std::slice::from_ref(&placeholder))
            .unwrap_err();
        assert!(err.to_string().contains("tampered at seq 2"), "{err:#}");
        assert!(err.to_string().contains(want), "{err:#}");
    }
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
    run_at(&f.db(), &f.target.dir, args)
}

fn run_at(db: &Path, checkpoints: &Path, args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_kod-audit"))
        .arg("--db")
        .arg(db)
        .arg("--checkpoints")
        .arg(checkpoints)
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
    assert_eq!(v, json!({"ok": true, "payloads_cleared": 0}));
}

#[test]
fn captured_checkpoints_accept_an_older_snapshot_after_live_appends() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let frozen = f.dir.path().join("frozen");
    let missing_db = f.dir.path().join("not-yet.db");
    let (ok, v) = run_at(
        &missing_db,
        &f.target.dir,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(ok, "{v}");
    assert!(!missing_db.exists());
    let snapshot = f.dir.path().join("snapshot.db");
    rusqlite::Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let (ok, v) = run_at(&snapshot, &frozen, &["verify"]);
    assert!(ok, "{v}");
    assert_eq!(v["entries"], 1);
    assert_eq!(v["last_checkpoint"], 1);
}

#[test]
fn captured_checkpoints_still_detect_a_truncated_snapshot() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let snapshot = f.dir.path().join("snapshot.db");
    rusqlite::Connection::open(f.db())
        .unwrap()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    f.log.append(RUN_AUDIT, &json!({"n": 2})).unwrap();
    f.log.checkpoint(&f.target).unwrap();
    let frozen = f.dir.path().join("frozen");
    let (ok, v) = run(
        &f,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(ok, "{v}");
    let (ok, v) = run_at(&snapshot, &frozen, &["verify"]);
    assert!(!ok, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("checkpoint at seq 2"),
        "{v}"
    );
}

#[test]
fn checkpoint_capture_never_reuses_a_previous_directory() {
    let f = fixture();
    let frozen = f.dir.path().join("frozen");
    std::fs::create_dir(&frozen).unwrap();
    let (ok, v) = run(
        &f,
        &["capture-checkpoints", "--output", frozen.to_str().unwrap()],
    );
    assert!(!ok, "{v}");
}

#[test]
fn verify_checks_retention_records_against_the_released_policies() {
    let f = fixture();
    f.log.append(RUN_AUDIT, &json!({"n": 1})).unwrap();
    let released = f.dir.path().join("released");
    std::fs::create_dir(&released).unwrap();
    let flag = ["verify", "--released-policies", released.to_str().unwrap()];
    // No retention records yet: nothing to check against.
    let (ok, v) = run(&f, &flag);
    assert!(ok, "{v}");
    // One policy version allowing immediate deletion, released and used.
    let policy = released.join("v1");
    std::fs::create_dir(&policy).unwrap();
    for entry in
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy")).unwrap()
    {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, policy.join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::write(
        policy.join("compliance.json"),
        json!({"profiles": {"a": {"retention_days": {"run_audit": 0}}}}).to_string(),
    )
    .unwrap();
    let (ok, v) = run(
        &f,
        &[
            "purge",
            "--policy",
            policy.to_str().unwrap(),
            "--record-type",
            RUN_AUDIT,
        ],
    );
    assert!(ok, "{v}");
    let (ok, v) = run(&f, &flag);
    assert!(ok, "{v}");
    assert_eq!(v["retention_records"], 1);
    // Withdrawn from the released set: the record no longer checks out.
    std::fs::remove_dir_all(&policy).unwrap();
    let (ok, v) = run(&f, &flag);
    assert!(!ok);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("not a released version"),
        "{v}"
    );
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p kod-audit --test retention --test cli`
Expected: compile errors, `unresolved import kod_audit::PURGE_BATCH`, `no method named purge found for struct kod_audit::AuditLog` (and `verify_retention`), and in cli.rs `environment variable CARGO_BIN_EXE_kod-audit not defined`.

- [ ] **Step 3: Implement retention and the CLI**

In `crates/kod-audit/src/lib.rs`, add `mod retention;` on the line after `pub mod redact;`, and `pub use retention::PURGE_BATCH;` on the line before `pub use verify::Verified;`. Add this helper for retention's bounded, internally generated records (ordinary caller payloads are prepared before locking by `append`):

```rust
pub(crate) fn append_in(
    tx: &Transaction,
    record_type: &str,
    payload: &Value,
    at: i64,
) -> Result<u64> {
    append_text_in(tx, record_type, &prepare_payload(payload)?, at)
}
```

Create `crates/kod-audit/src/retention.rs`:

```rust
use crate::{AuditLog, RETENTION, RUN_AUDIT, append_in};
use anyhow::{Result, bail};
use kod_policy::Policy;
use rusqlite::{Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

/// Payloads logically cleared per transaction, so a purge never holds the write lock
/// long enough to time out a gateway append.
pub const PURGE_BATCH: usize = 500;

impl AuditLog {
    /// Deletes the payloads of `record_type` entries whose retention has
    /// ended, oldest first, asking the policy about each one. The first
    /// refusal is recorded and ends the purge. Works in batches of
    /// PURGE_BATCH; each batch appends one RETENTION entry listing what it
    /// deletes, before clearing the payloads (the store refuses to clear a
    /// payload no retention entry covers). Rows and hashes stay, so the chain
    /// still verifies. Refuses to run when `now` is behind the newest entry:
    /// the clock is wrong. Returns how many payloads were logically cleared,
    /// not how many were erased from storage or expired from backups.
    pub fn purge(&self, policy: &Policy, record_type: &str, now: i64) -> Result<usize> {
        if record_type == RETENTION {
            bail!("retention records are kept for the life of the log");
        }
        let mut deleted = 0;
        loop {
            let (n, refused) = self.tx(TransactionBehavior::Immediate, |tx| {
                purge_batch(tx, policy, record_type, now)
            })?;
            deleted += n;
            if refused || n < PURGE_BATCH {
                return Ok(deleted);
            }
        }
    }

    /// Checks every retention entry against the released policies: its
    /// policy_version must be one of theirs, and that policy must allow
    /// deleting each entry it lists at the age the entry had when the
    /// retention entry was written. A forged but well-formed deletion record
    /// fails here. Run by the off-host verifier after `verify`, with one
    /// loaded policy per released version. Returns how many retention
    /// entries it checked.
    pub fn verify_retention(&self, released: &[Policy]) -> Result<usize> {
        let rows = self.rows()?;
        let mut checked = 0;
        for (seq, at, record_type, payload, _, _) in &rows {
            let (RETENTION, Some(p)) = (record_type.as_str(), payload) else {
                continue;
            };
            let v: Value = serde_json::from_str(p)?;
            let version = v["policy_version"].as_str().unwrap_or_default();
            let Some(policy) = released.iter().find(|p| p.version() == version) else {
                bail!(
                    "tampered at seq {seq}: retention record cites policy {version:?}, \
                     which is not a released version"
                );
            };
            for s in v["seqs"].as_array().into_iter().flatten() {
                let Some(row) = s
                    .as_u64()
                    .and_then(|s| rows.get((s as usize).wrapping_sub(1)))
                else {
                    bail!("tampered at seq {seq}: malformed retention record");
                };
                let age = (at - row.1) / 86_400;
                let input =
                    json!({"action": "retention_delete", "record_type": row.2, "age_days": age});
                if !policy.decide(&input).allow {
                    bail!(
                        "tampered at seq {seq}: policy {version} does not allow deleting \
                         seq {s} at {age} days"
                    );
                }
            }
            checked += 1;
        }
        Ok(checked)
    }
}

/// One batch: returns how many payloads it cleared and whether the policy
/// refused one.
fn purge_batch(
    tx: &Transaction,
    policy: &Policy,
    record_type: &str,
    now: i64,
) -> Result<(usize, bool)> {
    let newest: Option<i64> = tx.query_row("SELECT MAX(at) FROM audit", [], |r| r.get(0))?;
    if newest.is_some_and(|at| now < at) {
        bail!("clock is behind the newest audit entry; refusing to purge");
    }
    let candidates = tx
        .prepare(
            "SELECT seq, at FROM audit WHERE record_type = ?1 AND payload IS NOT NULL
             ORDER BY at, seq LIMIT ?2",
        )?
        .query_map(params![record_type, PURGE_BATCH as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seqs = Vec::new();
    let mut ages = Vec::new();
    let mut last = None;
    let mut refused = false;
    for (seq, at) in candidates {
        // Whole days, rounded down; never negative, since now >= every `at`.
        let age = (now - at) / 86_400;
        let input =
            json!({"action": "retention_delete", "record_type": record_type, "age_days": age});
        let decision = policy.decide(&input);
        if !decision.allow {
            append_in(
                tx,
                RUN_AUDIT,
                &json!({"kind": "policy_decision", "input": input, "decision": decision}),
                now,
            )?;
            refused = true;
            break;
        }
        seqs.push(seq);
        ages.push(age);
        last = Some(decision);
    }
    if let Some(decision) = last {
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
            now,
        )?;
        for seq in &seqs {
            tx.execute("UPDATE audit SET payload = NULL WHERE seq = ?1", [seq])?;
        }
    }
    Ok((seqs.len(), refused))
}
```

Create `crates/kod-audit/src/main.rs`:

```rust
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kod_audit::{
    AuditLog,
    checkpoint::{CheckpointTarget, DirTarget},
    now,
};
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
    /// Freeze the trusted checkpoint set before taking an off-host DB snapshot
    CaptureCheckpoints {
        /// New private directory; existing directories are refused
        #[arg(long)]
        output: PathBuf,
    },
    /// Check the chain and every checkpoint
    Verify {
        /// Directory holding one policy directory per released version; when
        /// given, every retention record is checked against them too
        #[arg(long)]
        released_policies: Option<PathBuf>,
    },
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
    let target = DirTarget {
        dir: cli.checkpoints,
    };
    Ok(match cli.command {
        Command::CaptureCheckpoints { output } => {
            use std::os::unix::fs::DirBuilderExt;
            let checkpoints = target.list()?;
            std::fs::DirBuilder::new().mode(0o700).create(&output)?;
            let frozen = DirTarget { dir: output };
            for cp in &checkpoints {
                frozen.put(cp)?;
            }
            json!({"ok": true, "checkpoints": checkpoints.len()})
        }
        Command::Verify { released_policies } => {
            let log = AuditLog::open(&cli.db)?;
            let v = log.verify(&target)?;
            let mut out =
                json!({"ok": true, "entries": v.entries, "last_checkpoint": v.last_checkpoint});
            if let Some(dir) = released_policies {
                let released = std::fs::read_dir(&dir)
                    .with_context(|| format!("released policies {}", dir.display()))?
                    .map(|e| Policy::load(&e?.path()))
                    .collect::<Result<Vec<_>>>()?;
                out["retention_records"] = log.verify_retention(&released)?.into();
            }
            out
        }
        Command::Checkpoint => {
            let log = AuditLog::open(&cli.db)?;
            json!({"ok": true, "checkpoint": log.checkpoint(&target)?})
        }
        Command::Purge {
            policy,
            record_type,
        } => {
            let log = AuditLog::open(&cli.db)?;
            // Separate transactions: an entry appended in between is checked
            // by the next verify, and purge clears only payloads its own
            // retention records cover.
            log.verify(&target)?;
            let policy = Policy::load(&policy)?;
            json!({"ok": true, "payloads_cleared": log.purge(&policy, &record_type, now()?)?})
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
Expected: checkpoint unit tests 2, cli 6, integrity 15, log 12, redact 14 and retention 8 passed (57 audit tests), plus every other crate's tests.

- [ ] **Step 5: Add the runbook section**

Append to `docs/phase1-runbook.md`:

````markdown
## Audit log

Prerequisites: `kod-audit` on both hosts, the `sqlite3` CLI on the audit host, and an ssh key that lets the checkpoint account log in to the audit host as `kod`.

`kod-audit` runs on the audit host as the `kod` user. The database is `/var/lib/kod/audit.db`; the directory is mode 0700 and the database, `-wal` and `-shm` files are 0600. Checkpoints go to `/mnt/kod-checkpoints`, a mount owned by the checkpoint account: `kod` can create files there but can't change or delete them.

Timers on the audit host (systemd, each with `OnFailure=` alerting the owner):

```
# kod-audit-checkpoint.timer: OnCalendar=*:0/5
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints checkpoint
# kod-audit-verify.timer: OnCalendar=hourly (early warning only)
ExecStart=/usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints verify
# kod-audit-purge.timer: OnCalendar=daily, one purge per record type in the
# compliance profile, refused and recorded while it is a placeholder;
# every type is tried and the unit fails at the end if any failed
ExecStart=/bin/sh -c 'rc=0; for t in run_audit prompts model_outputs; do /usr/local/bin/kod-audit --checkpoints /mnt/kod-checkpoints purge --policy /etc/kod/policy --record-type "$t" || rc=1; done; exit $rc'
```

The authoritative check runs on the checkpoint host, under the checkpoint account. It freezes its trusted checkpoint set **before** requesting the database snapshot. Checkpoints published later belong to the next check; a copied database must cover every frozen checkpoint, so actual truncation is still refused. Hourly, with `OnFailure=` alerting the owner:

```
set -eu
umask 077
verify_work=$(mktemp -d /srv/kod-verify/run.XXXXXX)
kod-audit --checkpoints /srv/kod-checkpoints capture-checkpoints --output "$verify_work/checkpoints"
ssh kod@audit-host rm -f /var/lib/kod/snap.db
printf "VACUUM INTO '/var/lib/kod/snap.db';" | ssh kod@audit-host sqlite3 /var/lib/kod/audit.db
scp kod@audit-host:/var/lib/kod/snap.db "$verify_work/audit.db"
kod-audit --db "$verify_work/audit.db" --checkpoints "$verify_work/checkpoints" verify --released-policies /srv/kod-policies
touch "$verify_work/VERIFIED"
```

`/srv/kod-policies` holds every released `policy/` directory, one subdirectory per version (for example the release tag), copied there when it is released and never deleted: each `retention` record is checked against the policy it names. Owner and auditors read only run directories with a `VERIFIED` marker, never the live database. Run this job as one systemd oneshot so remote snapshot creation cannot overlap another invocation.

Retention currently means **logical payload clearing**, not storage erasure. `payloads_cleared` counts SQL values set to NULL; old bytes can remain in WAL frames, freed pages, the remote `snap.db`, these run directories, and backups. Keep the placeholder profile refusing deletions until the owner approves both retention values and a documented erasure/copy-expiry procedure. That procedure must name the SQLite secure-delete/checkpoint or compaction strategy and its verification, the owner and expiry of every snapshot/backup, and how failed or offline copies are handled. A WAL checkpoint alone does not erase freed pages or copies. Expire failed run directories too; retain checkpoint hashes separately from payload-bearing copies. Test storage recovery and copy expiry with synthetic markers before enabling a non-placeholder profile. This plan does not implement or claim physical erasure.

Checkpoints that only the audit host reads catch accidents, not someone with root there: root can mount a forged directory over `/mnt/kod-checkpoints`. Until the checkpoint account and this job exist, the integrity exit criterion is not met.

A failed `verify`, on either host, means the log was changed. Stop publishing (kill switch), keep the database and the checkpoint directory as they are, and tell the owner. The error names the first bad entry. A `retention` record that is malformed also blocks every later purge (the store's trigger can't read it), which fails closed; `verify` names it, and it is handled as tampering like any other.
````

- [ ] **Step 6: Commit**

```bash
git add crates/kod-audit docs/phase1-runbook.md
git commit -m "kod-audit: retention through the compliance profile, CLI and runbook"
```

## Whole-branch review

After Task 4, one fresh reviewer reads the whole branch against the spec sections listed under **Spec** and against this plan's Decisions and Review Focus. The reviewer checks in particular:

- Every insert goes through `append_text_in` inside `AuditLog::tx`; `purge` calls it through `append_in`. Ordinary caller payloads are size-checked and redacted before acquiring the write lock. Only `purge` writes `retention` entries, and the only `UPDATE` is its `payload = NULL`, after the `retention` entry that lists those seqs.
- Redaction runs before storage; the in-memory serialization used for the size check is never persisted. No code path stores an unredacted payload.
- `before` cannot run its closure after a failed append, and no error is swallowed (no `let _ =`, no `.ok()` on a write).
- `verify` lists checkpoints before reading a Deferred snapshot, checks the seq continuity, the payload hash, the entry hash, every `retention` entry's shape and coverage, the NULL payloads and every checkpoint, and `checkpoint` refuses a log that fails it.
- Redaction is the one `redact` function, applied to values and keys.
- `verify_retention` fails for a `policy_version` not in the released set, and `purge` stamps its entries with the `now` it decided with.
- `Cargo.lock` adds only rusqlite, libsqlite3-sys and their listed dependencies, and CI's cargo-deny step passes.

## Self-review

- **Spec coverage.**
  - Append-only, hash-chained: Task 2 (D2, D3).
  - Checkpoints every few minutes to write-once storage under a different account: Task 3 (D5) and the runbook timer in Task 4 (D6). Checked from the other account's side: D12, D13 and the runbook job.
  - Modification and truncation show up against the checkpoints: Task 3 tests.
  - Tamper evidence, not tamper-proofing: the Honest ceiling section, and `truncation_after_the_last_checkpoint_is_not_detectable` pins the ceiling.
  - Prompts and outputs redacted before storage: Task 1 and Task 2 (D4).
  - Readable only by owner and auditors: D11, 0600 database, WAL and shm files (`the_store_files_are_private`).
  - SQL-visible payloads cleared at the end of the profile's retention: Task 4 (D8). Storage erasure and snapshot/backup expiry remain a prerequisite for enabling a non-placeholder profile.
  - Audit write fails, so the external write is refused: Task 2 `before` (D7). Gateway wiring is sub-plan 5.
  - Tampering, truncation and store-outage tests: Tasks 2 and 3.
  - Each decision recorded with its policy version: `record_decision` (Task 2, D10).
  - `retention_delete` matrix row: the policy decides, the evidence is a deletion record with record type, ages and policy version, a missing value refuses, and the placeholder refuses all (Task 4). The row also lists the profiles; see Open items.
  - Coverage of gateway calls, approvals, merges, commands and diffs is the callers' job (sub-plans 2, 5, 6 and 7), through `append` and `before`.
- **Placeholders.** None in the steps. The only placeholder is the compliance profile, which is the policy repo's file, not this crate's.
- **Type consistency.**
  - The names in each Interfaces block match the code: `append_in(.., at)`, `rows`, `verify_retention`, `tx(TransactionBehavior, _)`, `now() -> Result<i64>`, `sha256_hex`, `entry_hash`, `Verified`, `CheckpointTarget::{put, list}`, `DirTarget { dir }`, `purge(&Policy, &str, i64)` and `PURGE_BATCH`.
  - Test helpers: `common::fixture` and `Fixture::{db, attacker}` are used by integrity.rs and cli.rs. log.rs has its own `Fixture` without a target, and retention.rs has its own policy helpers.
  - The code in this plan is byte-identical to a scratch workspace in which every task's end state passed `cargo fmt --check`, `cargo clippy --all-targets -D warnings` and `cargo test`.
- **Review Focus.** Each of the five lines names tests in its owning task, and those tests are in that task's code above.

## Open items

- **Checkpoint storage, its account and the off-host verifier** (the backup owner, per the overview). **Blocks the integrity exit criterion.** Validate D5's complete publication, durability, no-overwrite and account-separation contract on the selected storage. A directory adapter is suitable only if the deployment actually supplies that contract, including permitted staging and directory sync; an S3 Object Lock target needs a second adapter. The local fake and read-only modes do not establish protection from host access. Run the D12 job from the independent account before claiming that protection.
- **Compliance profile values and storage/copy expiry**: `retention_days` per record type in `policy/compliance.json`, plus the approved SQLite erasure and snapshot/backup-expiry procedure described in the runbook. Keep the placeholder refusal until both are settled. A successful logical purge is not evidence of physical erasure.
- **Auditor access**: accounts in the auditors group on the checkpoint host (D11), once auditors are named.
- **Profiles in the deletion record**: the matrix lists record type, age, profiles and policy version. The policy API exposes no profile list, so the record carries `policy_version`, which pins `compliance.json`; the released policy directories kept under D13 give the profiles back. The spec owner accepts that, or the policy crate adds a profile list and `purge` records it.
- **Eval-suite cases**: the spec puts tampering, truncation and store-outage cases in the eval suite. This plan covers them as crate tests, which meet the overview's row 4 exit criteria. Wiring them into `crates/kod-eval` waits until the eval suite runs harness cases.
