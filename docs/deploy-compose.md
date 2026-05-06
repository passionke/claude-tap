# Docker Compose deployment

This runs **claude-tap** in `--tap-no-launch` mode: only the proxy and optional live viewer are started inside the container. Install Claude Code / Codex / Cursor on client machines separately.

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

The compose file mounts `./traces` on the host to `/data/traces` in the container. JSONL and generated HTML viewers are written under dated subdirectories.

## Security

Do not expose ports 8080/3000 to the public internet without TLS termination (for example nginx or Traefik in front) and appropriate network restrictions. The proxy forwards API traffic and is not an authenticated application service.
