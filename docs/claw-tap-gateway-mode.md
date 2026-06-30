# claw-tap gateway mode

Author: kejiqing

When `CLAW_CLUSTER_ID` and `CLAW_GATEWAY_DATABASE_URL` are set (same values as http-gateway-rs), claude-tap runs in **claw gateway mode**.

## Overview

- Tap **connects to the same PostgreSQL** as http-gateway-rs (not via gateway HTTP).
- On a timer it reloads the active LLM from PG (`gateway_llm_cluster_state` + `gateway_llm_cluster_revision` for `CLAW_CLUSTER_ID`).
- Tap proxies OpenAI-compatible traffic to the configured downstream LLM base URL.
- **Does not** use `--tap-target`, `OPENAI_BASE_URL`, `UPSTREAM_OPENAI_BASE_URL`, or `--tap-upstream-config` / `claw-tap-upstream.json`.
- If PG has no active model, tap refuses to start and `/healthz` returns `ok: false`.
- Poll interval: `CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS` (default 30). Gateway Admin “apply model” updates PG; tap picks it up on the next poll without restart.

## API key management

Since **v0.0.11**, the upstream LLM API key is **managed in PostgreSQL**, not by worker clients.

| Source | Table / column |
|--------|----------------|
| Cluster schema (preferred) | `gateway_llm_cluster_model.api_key_ciphertext` (AES-GCM, keyed by `cluster_id`) |
| Legacy singleton schema | `gateway_global_settings.llm_model_api_keys_json` |

When forwarding HTTP or WebSocket requests to the upstream LLM:

1. Tap loads and decrypts the active model’s API key from PG.
2. If the DB key is non-empty, tap **replaces** client `Authorization` and `x-api-key` headers with the DB key.
3. Header format depends on `--tap-client`:
   - `codex` and other OpenAI-compatible clients → `Authorization: Bearer <db-key>`
   - `claude` → `x-api-key: <db-key>`
4. If the DB key is empty, client auth headers are forwarded unchanged (backward-compatible fallback).

**Implications for workers**

- Workers only need `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` pointing at tap (e.g. `http://tap-host:8080/v1`).
- Workers do **not** need a valid upstream LLM API key; gateway Admin owns the key in PG.
- Client-supplied keys (e.g. worker `OPENAI_API_KEY`) are **not** sent to the real LLM when a DB key exists.
- Trace files still **redact** auth headers in recorded requests.

## Configuration

Copy [`.env.example`](../.env.example) to `.env` and set the **Mode B** block (plus ports if using compose). Do not copy the entire claw-code `.env` into this repo.

| Variable | Required | Purpose |
|----------|----------|---------|
| `CLAW_CLUSTER_ID` | Yes | Cluster label; must match http-gateway-rs |
| `CLAW_GATEWAY_DATABASE_URL` | Yes | PostgreSQL URL (same DB as gateway) |
| `CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS` | No | Upstream + API key refresh interval (default `30`) |
| `CLAUDE_TAP_PORT` | No | Proxy listen port (default `8080`) |

## Example (proxy-only, port 8080)

```bash
export CLAW_CLUSTER_ID=local-dev
export CLAW_GATEWAY_DATABASE_URL=postgres://claw_gateway:secret@postgres:5432/claw_gateway

claude-tap --tap-no-launch --tap-host 0.0.0.0 --tap-port 8080 --tap-client codex
```

Gateway Admin registers `host` + `proxyPort` (8080), probes `GET http://{host}:8080/healthz`, and injects worker `OPENAI_BASE_URL` to that base URL.

Worker example (no upstream LLM key required on the worker):

```bash
export OPENAI_BASE_URL=http://tap-host:8080/v1
codex -c 'openai_base_url="http://tap-host:8080/v1"'
```

## Health check

`GET /healthz` returns:

- `ok` — tap has a loaded active LLM from PG
- `clusterId` — same as `CLAW_CLUSTER_ID`
- `clusterHash` — matches `cluster_identity.rs` in claw-code (gateway verifies registration)

`dbHost` is **not** included in the public response.

## Docker Compose

See [deploy-compose.md](deploy-compose.md) for container deployment with the same env variables.

## Related

- claw-code: `docs/claw-tap-integration-requirements.md`
- Docker: [deploy-compose.md](deploy-compose.md)
