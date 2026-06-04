"""clawTap health endpoint for http-gateway-rs probe. Author: kejiqing"""

from __future__ import annotations

import json

from aiohttp import web

from claude_tap.cluster_identity import ClusterIdentity, health_json_body


async def healthz_handler(request: web.Request) -> web.Response:
    identity: ClusterIdentity | None = request.app.get("claw_cluster_identity")
    if identity is None:
        return web.Response(status=503, text="claw cluster identity not configured")
    store = request.app.get("gateway_upstream_store")
    llm_ready = store.is_ready() if store is not None else True
    body = health_json_body(identity, ok=llm_ready)
    return web.Response(
        status=200,
        text=json.dumps(body),
        content_type="application/json",
    )
