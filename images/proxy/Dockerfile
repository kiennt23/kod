ARG RUST_VERSION
FROM rust:${RUST_VERSION}-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kod-llm-proxy

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/kod-llm-proxy /kod-llm-proxy
EXPOSE 8080
ENTRYPOINT ["/kod-llm-proxy"]
