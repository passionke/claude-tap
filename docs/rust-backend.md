# Rust backend (claude-tap)

Author: kejiqing

Docker and the primary server runtime are implemented in Rust under `crates/claude-tap`.

## Build / run

```bash
cargo build --release -p claude-tap
./target/release/claude-tap --tap-no-launch --tap-host 0.0.0.0 --tap-port 8080 --tap-live --tap-live-port 3000 --tap-output-dir ./.traces
```

Forward (CONNECT MITM) example:

```bash
./target/release/claude-tap --tap-proxy-mode forward --tap-no-launch --tap-host 127.0.0.1 --tap-port 8080 --tap-output-dir ./.traces
```

## Tests

```bash
cargo test --workspace
```

## Live memory model

- Disk JSONL + SQLite are the source of truth.
- `/events` SSE pushes **new** records only (no RAM replay on connect).
- Viewer **Load from disk** loads `/api/sessions/traces` on demand.
- `CLAUDE_TAP_MAX_SESSIONS` is no longer a memory control.

## Scope (online / claw-code)

Reverse and forward proxy paths aim for Python parity (HTTP/SSE streaming, WebSocket upgrade relay, gateway cluster + legacy singleton, Live disk-backed chunks, export, HTML viewer on exit).

Explicitly **out of scope** for this build:

1. PyPI auto-update check
2. Cursor transcript import
