# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY assets/mcp-target-ops.ico ./assets/mcp-target-ops.ico
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --locked --release \
    && rm -rf src

COPY assets ./assets
COPY src ./src
RUN touch src/main.rs \
    && cargo build --locked --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssh-client \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/mcp-target-ops /usr/local/bin/mcp-target-ops

EXPOSE 8765

ENTRYPOINT ["mcp-target-ops"]
