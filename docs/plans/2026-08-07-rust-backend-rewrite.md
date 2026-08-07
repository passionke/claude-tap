# Rust rewrite plan (active)

Author: kejiqing

Status: **in progress / Docker path complete**

See Cursor plan `rust_backend_rewrite` and [rust-backend.md](../rust-backend.md).

## Done

- [x] Cargo workspace + CLI + unit tests
- [x] Reverse proxy + JSONL/SQLite + SSE reassembler
- [x] Live disk-first (no RAM replay) + viewer Load from disk
- [x] Gateway PG / healthz / AES-GCM auth rewrite
- [x] Forward MITM CA + CONNECT path
- [x] Rust Dockerfile + CI `cargo test`
- [x] Idle RSS ~10MB (target ≤50MB)

## Remaining (transitional)

- [ ] PyPI / local CLI default fully switched to Rust binary distribution
- [ ] Remove Python runtime package once browser/script tests no longer import it
