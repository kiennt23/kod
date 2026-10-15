# One image per product language. BASE is that language's toolchain image,
# for example rust:<version>-bookworm; it must be Debian bookworm or newer
# with glibc, because kod-loop is built on rust:<RUST_VERSION>-slim-bookworm.
# Build from the repo root: docker build -f images/sandbox/Dockerfile .
# RUST_VERSION is the harness toolchain (rust-toolchain.toml). Claude Code is
# installed only when CLAUDE_CODE_VERSION is set, on Node.js NODE_VERSION from
# nodejs.org (checked against its SHASUMS256.txt; distribution Node.js is too
# old), and cargo-nextest only when NEXTEST_VERSION is set (Rust images).
# EXTRA_PACKAGES is for test images.
ARG RUST_VERSION
ARG BASE=debian:bookworm-slim

FROM rust:${RUST_VERSION}-slim-bookworm AS loop
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
RUN cargo build --release --locked -p kod-loop

FROM ${BASE}
ARG CLAUDE_CODE_VERSION=""
ARG EXTRA_PACKAGES=""
ARG NEXTEST_VERSION=""
ARG NODE_VERSION=22.23.3
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates util-linux procps ${NEXTEST_VERSION:+curl} ${CLAUDE_CODE_VERSION:+curl xz-utils} ${EXTRA_PACKAGES} \
 && if [ -n "$CLAUDE_CODE_VERSION" ]; then \
      case "$(dpkg --print-architecture)" in arm64) a=arm64 ;; *) a=x64 ;; esac \
      && n="node-v${NODE_VERSION}-linux-$a.tar.xz" \
      && cd /tmp && curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/$n" \
      && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" | grep " $n\$" | sha256sum -c - \
      && tar -xJf "$n" -C /usr/local --strip-components=1 && rm "$n" && cd / \
      && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    fi \
 && if [ -n "$NEXTEST_VERSION" ]; then \
      case "$(dpkg --print-architecture)" in arm64) p=linux-arm ;; *) p=linux ;; esac \
      && curl -fLsS "https://get.nexte.st/${NEXTEST_VERSION}/$p" | tar zxf - -C /usr/local/bin; \
    fi \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 agent
COPY images/sandbox/nextest.toml /etc/kod/nextest.toml
COPY --from=loop /src/target/release/kod-loop /usr/local/bin/kod-loop
COPY crates/kod-loop/system.md /etc/kod/kod-loop/system.md
# Cargo's own state (registry cache, locks) goes in /tmp, which the agent can
# write in both agent and score mode.
ENV CARGO_HOME=/tmp/cargo
COPY --chmod=0755 images/sandbox/entrypoint.sh /usr/local/bin/kod-entrypoint
ENTRYPOINT ["/usr/local/bin/kod-entrypoint"]
