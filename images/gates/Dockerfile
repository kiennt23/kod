ARG RUST_VERSION
FROM ghcr.io/gitleaks/gitleaks:v8.24.3@sha256:e1b35e12a8c6fa8901f060459cfb6b2fc4c484d3afbe3b029733a3bbfab07055 AS secrets
FROM rust:${RUST_VERSION}-bookworm
ARG RUST_VERSION
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION}
RUN rustup component add clippy \
 && cargo install --locked cargo-deny --version 0.20.2 \
 && useradd --uid 10001 --create-home gate \
 && mkdir -p /work /prepared && chown gate:gate /work /prepared
COPY --from=secrets /usr/bin/gitleaks /usr/local/bin/gitleaks
WORKDIR /opt/kod/source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
ENV CARGO_HOME=/opt/kod/cargo
RUN cargo fetch --locked && cargo deny fetch && rm -rf /opt/kod/source
COPY deny.toml /opt/kod/deny.toml
COPY images/gates/gitleaks.toml /opt/kod/gitleaks.toml
COPY --chmod=0755 images/gates/scan /opt/kod/scan
COPY --chmod=0755 images/gates/prepare /opt/kod/prepare
RUN chmod -R a+rX /opt/kod /usr/local/rustup
USER 10001:10001
WORKDIR /work
ENTRYPOINT ["/opt/kod/scan"]
