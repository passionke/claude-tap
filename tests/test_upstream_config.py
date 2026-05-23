"""Tests for hot-reloadable upstream config file."""

from __future__ import annotations

import json
import time
from pathlib import Path

import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from claude_tap.proxy import proxy_handler
from claude_tap.session_dispatcher import SessionTraceDispatcher
from claude_tap.session_index import SessionIndex
from claude_tap.upstream_config import (
    UpstreamConfigStore,
    parse_upstream_config_text,
    resolve_upstream,
    strip_path_prefix_for,
)


def test_parse_upstream_config_text_accepts_target_and_target_url() -> None:
    assert parse_upstream_config_text('{"target": "https://api.example.com/"}') == "https://api.example.com"
    assert parse_upstream_config_text('{"target_url": "https://x.test"}') == "https://x.test"


def test_strip_path_prefix_for_codex_non_openai() -> None:
    assert strip_path_prefix_for("codex", "https://chatgpt.com/backend-api/codex") == "/v1"
    assert strip_path_prefix_for("codex", "https://api.openai.com") == ""


def test_upstream_config_store_reload_on_mtime(tmp_path: Path) -> None:
    config = tmp_path / "upstream.json"
    config.write_text(json.dumps({"target": "https://first.example"}), encoding="utf-8")
    store = UpstreamConfigStore(client="claude", config_path=config, fallback_target="https://fallback.example")
    assert store.load_initial() is True
    assert store.snapshot().target == "https://first.example"

    config.write_text(json.dumps({"target": "https://second.example"}), encoding="utf-8")
    time.sleep(0.05)
    assert store.reload_if_changed() is True
    assert store.snapshot().target == "https://second.example"


def test_upstream_config_invalid_file_keeps_previous(tmp_path: Path) -> None:
    config = tmp_path / "upstream.json"
    config.write_text(json.dumps({"target": "https://ok.example"}), encoding="utf-8")
    store = UpstreamConfigStore(client="claude", config_path=config, fallback_target="https://fallback.example")
    store.load_initial()

    config.write_text("{not json", encoding="utf-8")
    time.sleep(0.05)
    assert store.reload_if_changed() is False
    assert store.snapshot().target == "https://ok.example"


@pytest.mark.asyncio
async def test_proxy_uses_reloaded_upstream_per_request(tmp_path: Path) -> None:
    seen_hosts: list[str] = []

    async def upstream_handler(request: web.Request) -> web.Response:
        seen_hosts.append(request.host)
        return web.json_response({"host": request.host})

    upstream_app = web.Application()
    upstream_app.router.add_post("/v1/chat/completions", upstream_handler)
    upstream_runner = web.AppRunner(upstream_app)
    await upstream_runner.setup()
    upstream_site = web.TCPSite(upstream_runner, "127.0.0.1", 0)
    await upstream_site.start()
    port = upstream_site._server.sockets[0].getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    config = tmp_path / "upstream.json"
    config.write_text(json.dumps({"target": base}), encoding="utf-8")
    store = UpstreamConfigStore(client="claude", config_path=config, fallback_target="http://127.0.0.1:1")
    store.load_initial()

    trace_dir = tmp_path / "traces"
    trace_dir.mkdir()
    session_index = SessionIndex(trace_dir)
    dispatcher = SessionTraceDispatcher(trace_dir, session_index, live_server=None)

    import aiohttp

    session = aiohttp.ClientSession(auto_decompress=False)
    proxy_app = web.Application()
    proxy_app["trace_ctx"] = {
        "target_url": "http://127.0.0.1:1",
        "upstream": store,
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
        resp = await client.post("/v1/chat/completions", json={"model": "m", "messages": []})
        assert resp.status == 200
        assert seen_hosts == [f"127.0.0.1:{port}"]

        # Point config at a host that does not listen; next request should fail fast.
        config.write_text(json.dumps({"target": "http://127.0.0.1:1"}), encoding="utf-8")
        time.sleep(0.05)
        assert store.reload_if_changed() is True
        assert resolve_upstream(proxy_app["trace_ctx"]).target == "http://127.0.0.1:1"

        resp2 = await client.post("/v1/chat/completions", json={"model": "m", "messages": []})
        assert resp2.status == 502
    finally:
        await client.close()
        await session.close()
        dispatcher.close()
        session_index.close()
        await upstream_runner.cleanup()
