# Matrix Ping Bot

Simple Matrix bot using the Matrix Rust SDK with end‑to‑end encryption enabled. It joins rooms (unless disabled), relays messages between configured room clusters, and replies "pong" to messages containing `!ping`.

## Features

- E2EE enabled with persistent SQLite store
- Session restore (no need to log in every run)
- Auto‑join on invites (toggle with `--no-autojoin`)
- Room cluster relaying between room IDs/aliases
- Experimental: dynamic plugins via WebAssembly components (enable with cargo feature "wasm-plugins")

## Requirements

- Rust
- Optional (for WASM plugins):
  - Build with cargo feature: cargo run --features wasm-plugins -- …
  - Install Wasmtime CLI for local testing/debugging (not required at runtime): macOS: brew install wasmtime
  - WASI Preview 2 is provided by the embedded runtime; no separate WASI install is required.

## WASM Plugins (experimental)

This bot can discover and load external plugins compiled as WebAssembly components. The host uses Wasmtime with WASI Preview 2 and a small WIT-defined interface for plugin I/O.

- WIT contract lives at: wit/plugin.wit
- World: matrix-plugin
- Guest exports:
  - plugin.get-spec() -> plugin-spec
  - plugin.help() -> string
  - plugin.run(run-request) -> result<(), string>
- Host imports:
  - host-io.send-text(text: string) -> result<(), string>
  - WASI Preview 2 (stdio, env, clocks, random, filesystem, streams)

### Enabling and running

- Build/run with feature flag:
  - cargo run --features wasm-plugins -- [your usual args]
- Discovery directory:
  - Set WASM_PLUGINS_DIR to a directory containing .wasm or .cwasm components.
  - Fallback order if unset: PLUGINS_DIR, then ./plugins, then ./tools.
- Each discovered file registers a plugin whose id is derived from the file name (e.g., plugins/echo.wasm => id "echo").

### Configuration and triggers

- Per-plugin config file (optional):
  - Place YAML at: <PLUGINS_DIR>/<plugin_id>/config.yaml
  - This file is merged into the plugin’s default config (as provided by the component’s get-spec).
- Triggers:
  - Commands and mentions can be provided by the component via get-spec, or configured in your bot config and merged at startup.
- Enable/disable at runtime:
  - Use the built-in tools manager: !tools list | enable <id> | disable <id>

### Authoring a plugin

- Target: WebAssembly component using WASI Preview 2.
- Implement the matrix-plugin world from wit/plugin.wit:
  - Return defaults from plugin.get-spec (id, enabled, dev-only, triggers, config-yaml).
  - Provide a short help string from plugin.help.
  - Implement plugin.run to handle a trigger; use host-io.send-text to post replies.

Notes:
- This feature set is evolving. If the feature is not enabled or a component is not fully wired, the bot will register the plugin but may emit a placeholder message on invocation.
