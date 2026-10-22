ARG RUST_VERSION=1.99.0
FROM ghcr.io/gitleaks/gitleaks:v8.24.3@sha256:e1b35e12a8c6fa8901f060459cfb6b2fc4c484d3afbe3b029733a3bbfab07055 AS secrets
FROM rust:${RUST_VERSION}-bookworm
RUN rustup component add clippy \
 && cargo install --locked cargo-deny --version 0.20.2 \
 && useradd --uid 10001 --create-home gate
COPY --from=secrets /usr/bin/gitleaks /usr/local/bin/gitleaks
WORKDIR /opt/kod/source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
ENV CARGO_HOME=/opt/kod/cargo
RUN cargo fetch --locked && cargo deny fetch && rm -rf /opt/kod/source
COPY deny.toml /opt/kod/deny.toml
COPY images/gates/gitleaks.toml /opt/kod/gitleaks.toml
COPY --chmod=0755 images/gates/scan /opt/kod/scan
RUN chmod -R a+rX /opt/kod /usr/local/rustup
USER 10001:10001
WORKDIR /work
ENTRYPOINT ["/opt/kod/scan"]
