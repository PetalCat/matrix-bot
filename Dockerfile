# syntax=docker/dockerfile:1

# ---------- Build stage ----------
FROM rust:1-bullseye AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libsqlite3-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Cache deps
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs
RUN cargo build --release || true

# Build actual app
COPY . .
RUN cargo build --release

# ---------- Runtime stage ----------
FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl1.1 libsqlite3-0 && \
    rm -rf /var/lib/apt/lists/* && \
    useradd -r -u 10001 -m app

WORKDIR /app
COPY --from=builder /src/target/release/matrix-ping-bot /usr/local/bin/matrix-ping-bot

# Create default writable dirs
RUN mkdir -p /data && chown -R app:app /data

USER app

# Env-driven config; mount your config at /app/config.yaml or set MATRIX_CONFIG
ENV MATRIX_STORE=/data/store \
    MATRIX_SESSION_FILE=/data/session.json

ENTRYPOINT ["/usr/local/bin/matrix-ping-bot"]
# Example: --homeserver ... --username ... --password ... --config /app/config.yaml
