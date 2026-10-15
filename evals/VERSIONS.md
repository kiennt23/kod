# Pinned versions

Everything a baseline depends on. Change a version here and in the build
command together, and rerun the baseline.

| Component | Version | Pinned how |
|---|---|---|
| Rust toolchain (harness) | 1.99.0 | `rust-toolchain.toml` |
| Sandbox base image (Rust) | `rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6` | digest |
| Node.js | 22.23.3 | `NODE_VERSION`, checked against nodejs.org SHASUMS256.txt |
| Claude Code | 2.1.296 | `CLAUDE_CODE_VERSION` (needs Node.js 22 or newer) |
| cargo-nextest | 0.9.148 | `NEXTEST_VERSION` (no checksum yet) |
| Model id | to be chosen at the smoke test | `--model` |

## Build

    docker build -t kod-sandbox-rust \
      --build-arg RUST_VERSION=1.99.0 \
      --build-arg BASE=rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 \
      --build-arg NODE_VERSION=22.23.3 \
      --build-arg CLAUDE_CODE_VERSION=2.1.296 \
      --build-arg NEXTEST_VERSION=0.9.148 \
      -f images/sandbox/Dockerfile .

Verified on 2026-10-14: `claude --version` runs as the `agent` user and prints
`2.1.296 (Claude Code)`.
`RUST_VERSION` builds kod-loop into the image; keep it equal to `rust-toolchain.toml`.
