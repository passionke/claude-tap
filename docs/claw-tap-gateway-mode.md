# claw-tap gateway mode

Author: kejiqing

When `CLAW_CLUSTER_ID` and `CLAW_GATEWAY_DATABASE_URL` are set (same values as http-gateway-rs), claude-tap runs in **claw gateway mode**:

- Tap **connects to the same PostgreSQL** as http-gateway-rs (not via gateway HTTP).
- On a timer it reloads the **downstream LLM base URL** from the active row in PG (`gateway_llm_cluster_state` + `gateway_llm_cluster_revision` for `CLAW_CLUSTER_ID`) and proxies OpenAI-compatible traffic there.
- **Does not** fall back to `--tap-target`, `OPENAI_BASE_URL`, or `UPSTREAM_OPENAI_BASE_URL` — if PG has no active model, tap refuses to start and `/healthz` returns `ok: false`.
- `GET /healthz` returns `ok`, `clusterId`, `clusterHash` (algorithm matches `cluster_identity.rs` in claw-code). **`dbHost` is not included** in the public response (PG host stays internal; gateway verifies via `clusterHash` only).
- Poll interval: `CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS` (default 30). Admin “apply model” in gateway updates PG; tap picks it up on the next poll without restart.
- `--tap-upstream-config` and claw-code’s `claw-tap-upstream.json` file are **not** used in this mode.
- `OPENAI_BASE_URL` / `--tap-target` is only a **fallback** until PG has an active model or if a poll fails.

## Configuration

Copy [`.env.example`](../.env.example) to `.env` and set **only** the Mode B block (plus ports if using compose). Do not copy the entire claw-code `.env` into this repo.

## Example (proxy-only, port 8080)

```bash
export CLAW_CLUSTER_ID=local-dev
export CLAW_GATEWAY_DATABASE_URL=postgres://claw_gateway:secret@postgres:5432/claw_gateway

claude-tap --tap-no-launch --tap-host 0.0.0.0 --tap-port 8080 --tap-client codex
```

Gateway Admin registers `host` + `proxyPort` (8080), probes `GET http://{host}:8080/healthz`, and injects worker `OPENAI_BASE_URL` to that base URL.

See claw-code: `docs/claw-tap-integration-requirements.md`.
