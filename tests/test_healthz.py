"""Tests for GET /healthz in claw gateway mode. Author: kejiqing"""

import json

import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from claude_tap.cluster_identity import local_cluster_identity
from claude_tap.health import healthz_handler


@pytest.mark.asyncio
async def test_healthz_returns_cluster_fields():
    identity = local_cluster_identity("local-dev", "postgres://gw:pw@postgres:5432/claw_gateway")
    app = web.Application()
    app["claw_cluster_identity"] = identity
    app.router.add_get("/healthz", healthz_handler)

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/healthz")
        assert resp.status == 200
        body = json.loads(await resp.text())
        assert body["ok"] is True
        assert body["clusterId"] == "local-dev"
        assert "dbHost" not in body
        assert body["clusterHash"] == identity.cluster_hash
