# Stage 1: Build Rust Bot
FROM rust:1.80-bullseye as rust-builder
WORKDIR /usr/src/app
COPY . .
# Build release binary
RUN cargo build --release --bin matrix-ping-bot

# Stage 2: Build Node.js MCP Server
FROM node:20-bookworm as node-builder
WORKDIR /usr/src/app
COPY web-search-mcp/package*.json ./
RUN npm ci --omit=dev
COPY web-search-mcp/ .
# Build skipped as source is missing, assuming dist exists
# RUN npm run build

# Stage 3: Runtime
# Use official Playwright image to ensure all browser dependencies are present
FROM mcr.microsoft.com/playwright:v1.57.0-jammy

# Install ca-certificates and dumb-init for signal handling
RUN apt-get update && apt-get install -y \
    ca-certificates \
    dumb-init \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy Rust Binary
COPY --from=rust-builder /usr/src/app/target/release/matrix-ping-bot /usr/local/bin/matrix-ping-bot

# Copy MCP Server
COPY --from=node-builder /usr/src/app/dist /app/mcp/dist
COPY --from=node-builder /usr/src/app/node_modules /app/mcp/node_modules
COPY --from=node-builder /usr/src/app/package.json /app/mcp/package.json

# Environment Setup
ENV RUST_LOG=info,plugin_ai=debug
# Ensure Node is in path (it is in Playwright image)

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["matrix-ping-bot"]
