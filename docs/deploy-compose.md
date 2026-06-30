# Docker Compose deployment

This runs **claude-tap** in `--tap-no-launch` mode: only the proxy and optional live viewer are started inside the container. Install Claude Code / Codex / Cursor on client machines separately.

## Environment

claude-tap only needs a small env surface — not the full claw-code gateway stack file.

```bash
cp .env.example .env
# edit .env (ports, optional CLAW_CLUSTER_ID + CLAW_GATEWAY_DATABASE_URL for gateway mode)
```

| Variable | Default | Purpose |
|----------|---------|---------|
| `CLAUDE_TAP_PORT` | `8080` | Proxy listen port |
| `CLAUDE_TAP_LIVE_PORT` | `3000` | Live viewer port |
| `CLAW_CLUSTER_ID` | — | Cluster label for `/healthz` (must match gateway) |
| `CLAW_GATEWAY_DATABASE_URL` | — | **Tap connects here**; periodic read of active LLM upstream **and API key** |
| `CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS` | `30` | How often tap refreshes upstream URL and API key from PG |

In gateway mode tap **does not** use `OPENAI_BASE_URL` or `--tap-target` for upstream routing. The LLM API key is loaded from PostgreSQL and injected on upstream requests (client `Authorization` / `x-api-key` headers are replaced when a DB key exists). See [claw-tap-gateway-mode.md](claw-tap-gateway-mode.md).

Gateway, pool, worker, and Git settings belong in **claw-code** (`deploy/stack/env.local.example`), not here.

## Build and run

```bash
docker compose up --build
```

Or with Podman:

```bash
podman-compose up --build
```

## Ports

| Port | Purpose |
|------|---------|
| 8080 | Reverse proxy (HTTP). Point `ANTHROPIC_BASE_URL` or `OPENAI_BASE_URL` here (see README). |
| 3000 | Live viewer (SSE). Open `http://<host>:3000/`; filter by agent session with `?session=<claw-session-id>` (same value as the `claw-session-id` request header). |

## Traces

The compose file mounts `./traces` on the host to `/data/traces` in the container. JSONL traces live under `sessions/<storage-slug>/trace.jsonl` (only when clients send `claw-session-id`). Session listing uses `claude_tap_sessions.sqlite3` in the same directory. Optional `trace.html` files are written next to each JSONL.

## Security

Do not expose ports 8080/3000 to the public internet without TLS termination (for example nginx or Traefik in front) and appropriate network restrictions. The proxy forwards API traffic and is not an authenticated application service.

## ACR / private registry (CI)

For faster pulls in China, CI can push release images to Aliyun ACR (or any Docker registry) via workflow `.github/workflows/claude-tap-acr.yaml` — same pattern as [claw-code `claw-code-acr.yaml`](https://github.com/passionke/claw-code/blob/main/.github/workflows/claw-code-acr.yaml).

**Trigger:** push tag `v*` / `release-v*` (same as `publish.yml`), or manual **workflow_dispatch**.

**GitHub setup** (reuse the `claw-acr` environment from claw-code if you already have it):

| Kind | Name | Example |
|------|------|---------|
| Variable | `ACR_REGISTRY` | `crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/my-ns` |
| Variable | `CONTAINER_BASE_REGISTRY` (optional) | `docker.1ms.run` |
| Variable | `ACR_GITHUB_ENVIRONMENT` (optional) | `claw-acr` |
| Secret | `ACR_USERNAME` | RAM user or registry token username |
| Secret | `ACR_PASSWORD` | Registry password |

**Pulled image name:** `${ACR_REGISTRY}/claw-tap:<tag>` (also `:latest`, `:sha-<git-sha>`).

Example:

```bash
docker pull crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/my-ns/claw-tap:v0.0.7
docker run --rm -p 8080:8080 -p 3000:3000 \
  -v "$(pwd)/traces:/data/traces" \
  crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/my-ns/claw-tap:v0.0.7
```

GHCR images from `publish.yml` remain available as `ghcr.io/<owner>/claude-tap:<tag>` when you need them.
