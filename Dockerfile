# Stage 1: Build Rust Bot
FROM rust:1.80-bullseye AS rust-builder
WORKDIR /usr/src/app
COPY . .
RUN cargo build --release --bin matrix-ping-bot

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy Rust Binary
COPY --from=rust-builder /usr/src/app/target/release/matrix-ping-bot /usr/local/bin/matrix-ping-bot

# Create app user with home directory (needed for npm) and data directory
RUN mkdir -p /data && \
    useradd -r -u 10001 -m -d /home/app app && \
    chown -R app:app /data /app /home/app

USER app

ENV RUST_LOG=info,plugin_ai=debug \
    MATRIX_STORE=/app/data/store \
    MATRIX_SESSION_FILE=/app/data/session.json

ENTRYPOINT ["matrix-ping-bot"]
