"""Tests for live viewer external prefix path support."""

from __future__ import annotations

import re
from pathlib import Path

import aiohttp
import pytest

from claude_tap.live import LiveViewerServer, normalize_live_prefix_path
from claude_tap.session_index import SessionIndex


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("", ""),
        ("   ", ""),
        ("/foo/", "/foo"),
        ("/e2b/3000/sbx_test", "/e2b/3000/sbx_test"),
        ("e2b/3000/sbx_test/", "/e2b/3000/sbx_test"),
    ],
)
def test_normalize_live_prefix_path(raw: str, expected: str) -> None:
    assert normalize_live_prefix_path(raw) == expected


def test_viewer_html_live_api_uses_live_url_helper() -> None:
    html = Path("claude_tap/viewer.html").read_text(encoding="utf-8")
    live_block = html.split("function startLivePoll", 1)[1].split("function initLiveMode", 1)[0]
    assert "function liveUrl(path)" in html
    assert "liveUrl('/api/sessions/traces?')" in live_block
    assert "liveUrl('/events?')" in live_block
    assert "liveUrl(`/api/sessions?limit=100" in live_block
    assert "liveUrl('/api/sessions/full?')" in live_block
    assert not re.search(r"""fetch\(['"`]/api/""", live_block)
    assert "liveUrl('/events?')" in live_block


@pytest.mark.asyncio
async def test_live_viewer_injects_prefix_path(tmp_path: Path) -> None:
    idx = SessionIndex(tmp_path)
    prefix = "/e2b/3000/sbx_test"
    server = LiveViewerServer(tmp_path, idx, port=0, host="127.0.0.1", prefix_path=prefix)
    port = await server.start()
    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(f"http://127.0.0.1:{port}/") as resp:
                assert resp.status == 200
                html = await resp.text()
                assert 'const LIVE_PREFIX_PATH = "/e2b/3000/sbx_test";' in html

            async with session.get(f"http://127.0.0.1:{port}/api/sessions?limit=1") as resp:
                assert resp.status == 200
                body = await resp.json()
                assert "sessions" in body
    finally:
        await server.stop()
        idx.close()
