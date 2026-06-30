"""Tests for gateway-managed API key injection in PostgreSQL mode. Author: kejiqing"""

from __future__ import annotations

from pathlib import Path

import aiohttp
import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from claude_tap.gateway_llm import ActiveLlmRuntime
from claude_tap.gateway_upstream import GatewayLlmUpstreamStore, apply_gateway_auth_headers
from claude_tap.proxy import proxy_handler
from claude_tap.session_dispatcher import SessionTraceDispatcher
from claude_tap.session_index import SessionIndex
from claude_tap.upstream_config import UpstreamSnapshot


def test_apply_gateway_auth_headers_codex_replaces_client_bearer() -> None:
    headers = {
        "Authorization": "Bearer client-key-should-not-forward",
        "Content-Type": "application/json",
    }
    apply_gateway_auth_headers(headers, client="codex", api_key="db-managed-key")
    assert headers["Authorization"] == "Bearer db-managed-key"
    assert "x-api-key" not in {k.lower() for k in headers}


def test_apply_gateway_auth_headers_claude_replaces_x_api_key() -> None:
    headers = {
        "x-api-key": "client-key-should-not-forward",
        "anthropic-version": "2023-06-01",
    }
    apply_gateway_auth_headers(headers, client="claude", api_key="db-managed-key")
    assert headers["x-api-key"] == "db-managed-key"
    assert "authorization" not in {k.lower() for k in headers}


def test_apply_gateway_auth_headers_empty_key_leaves_client_headers() -> None:
    headers = {"Authorization": "Bearer keep-me"}
    apply_gateway_auth_headers(headers, client="codex", api_key="")
    assert headers["Authorization"] == "Bearer keep-me"


def test_apply_gateway_auth_headers_strips_both_auth_headers_before_set() -> None:
    headers = {
        "x-api-key": "old-x",
        "Authorization": "Bearer old-bearer",
    }
    apply_gateway_auth_headers(headers, client="codex", api_key="db-key")
    assert headers == {"Authorization": "Bearer db-key"}


@pytest.mark.asyncio
async def test_proxy_gateway_mode_uses_db_api_key_for_upstream(tmp_path: Path) -> None:
    captured_auth: list[str] = []

    async def upstream_handler(request: web.Request) -> web.Response:
        captured_auth.append(request.headers.get("Authorization", ""))
        return web.json_response({"ok": True})

    upstream_app = web.Application()
    upstream_app.router.add_post("/v1/chat/completions", upstream_handler)
    upstream_runner = web.AppRunner(upstream_app)
    await upstream_runner.setup()
    upstream_site = web.TCPSite(upstream_runner, "127.0.0.1", 0)
    await upstream_site.start()
    port = upstream_site._server.sockets[0].getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    store = GatewayLlmUpstreamStore(client="codex", database_url="postgres://unused", cluster_id="local-dev")
    store._runtime = ActiveLlmRuntime(
        model_id="llm-1",
        model_rev="rev-1",
        base_model_url=base,
        model_name="test-model",
        api_key="db-managed-key",
    )
    store._snapshot = UpstreamSnapshot(target=base, strip_path_prefix="")

    trace_dir = tmp_path / "traces"
    trace_dir.mkdir()
    session_index = SessionIndex(trace_dir)
    dispatcher = SessionTraceDispatcher(trace_dir, session_index, live_server=None)

    session = aiohttp.ClientSession(auto_decompress=False)
    proxy_app = web.Application()
    proxy_app["trace_ctx"] = {
        "upstream": store,
        "trace_dispatcher": dispatcher,
        "session": session,
        "force_http": False,
    }
    proxy_app.router.add_route("*", "/{path_info:.*}", proxy_handler)
    proxy_server = TestServer(proxy_app)
    client = TestClient(proxy_server)
    await client.start_server()

    try:
        resp = await client.post(
            "/v1/chat/completions",
            json={"model": "m", "messages": []},
            headers={"Authorization": "Bearer wrong-client-key"},
        )
        assert resp.status == 200
        assert captured_auth == ["Bearer db-managed-key"]
    finally:
        await client.close()
        await session.close()
        dispatcher.close()
        session_index.close()
        await upstream_runner.cleanup()


@pytest.mark.asyncio
async def test_proxy_gateway_mode_claude_uses_db_x_api_key(tmp_path: Path) -> None:
    captured_key: list[str] = []

    async def upstream_handler(request: web.Request) -> web.Response:
        captured_key.append(request.headers.get("x-api-key", ""))
        return web.json_response({"content": []})

    upstream_app = web.Application()
    upstream_app.router.add_post("/v1/messages", upstream_handler)
    upstream_runner = web.AppRunner(upstream_app)
    await upstream_runner.setup()
    upstream_site = web.TCPSite(upstream_runner, "127.0.0.1", 0)
    await upstream_site.start()
    port = upstream_site._server.sockets[0].getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    store = GatewayLlmUpstreamStore(client="claude", database_url="postgres://unused", cluster_id="local-dev")
    store._runtime = ActiveLlmRuntime(
        model_id="llm-1",
        model_rev="rev-1",
        base_model_url=base,
        model_name="claude-test",
        api_key="db-claude-key",
    )
    store._snapshot = UpstreamSnapshot(target=base, strip_path_prefix="")

    trace_dir = tmp_path / "traces"
    trace_dir.mkdir()
    session_index = SessionIndex(trace_dir)
    dispatcher = SessionTraceDispatcher(trace_dir, session_index, live_server=None)

    session = aiohttp.ClientSession(auto_decompress=False)
    proxy_app = web.Application()
    proxy_app["trace_ctx"] = {
        "upstream": store,
        "trace_dispatcher": dispatcher,
        "session": session,
        "force_http": False,
    }
    proxy_app.router.add_route("*", "/{path_info:.*}", proxy_handler)
    proxy_server = TestServer(proxy_app)
    client = TestClient(proxy_server)
    await client.start_server()

    try:
        resp = await client.post(
            "/v1/messages",
            json={"model": "m", "max_tokens": 1, "messages": []},
            headers={"x-api-key": "wrong-client-key"},
        )
        assert resp.status == 200
        assert captured_key == ["db-claude-key"]
    finally:
        await client.close()
        await session.close()
        dispatcher.close()
        session_index.close()
        await upstream_runner.cleanup()


@pytest.mark.asyncio
async def test_proxy_non_gateway_mode_keeps_client_auth(tmp_path: Path) -> None:
    captured_auth: list[str] = []

    async def upstream_handler(request: web.Request) -> web.Response:
        captured_auth.append(request.headers.get("Authorization", ""))
        return web.json_response({"ok": True})

    upstream_app = web.Application()
    upstream_app.router.add_post("/v1/chat/completions", upstream_handler)
    upstream_runner = web.AppRunner(upstream_app)
    await upstream_runner.setup()
    upstream_site = web.TCPSite(upstream_runner, "127.0.0.1", 0)
    await upstream_site.start()
    port = upstream_site._server.sockets[0].getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    trace_dir = tmp_path / "traces"
    trace_dir.mkdir()
    session_index = SessionIndex(trace_dir)
    dispatcher = SessionTraceDispatcher(trace_dir, session_index, live_server=None)

    session = aiohttp.ClientSession(auto_decompress=False)
    proxy_app = web.Application()
    proxy_app["trace_ctx"] = {
        "target_url": base,
        "trace_dispatcher": dispatcher,
        "session": session,
        "strip_path_prefix": "",
        "force_http": False,
    }
    proxy_app.router.add_route("*", "/{path_info:.*}", proxy_handler)
    proxy_server = TestServer(proxy_app)
    client = TestClient(proxy_server)
    await client.start_server()

    try:
        resp = await client.post(
            "/v1/chat/completions",
            json={"model": "m", "messages": []},
            headers={"Authorization": "Bearer client-key"},
        )
        assert resp.status == 200
        assert captured_auth == ["Bearer client-key"]
    finally:
        await client.close()
        await session.close()
        dispatcher.close()
        session_index.close()
        await upstream_runner.cleanup()
