"""Unit tests for claw-session routing and SessionTraceDispatcher."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from claude_tap.claw_session import (
    CLAW_SESSION_HEADER,
    DEFAULT_CLAW_SESSION_ID,
    extract_claw_session_id,
    sanitize_filename_suffix,
    strip_claw_session_header,
)
from claude_tap.session_dispatcher import SessionTraceDispatcher


def test_extract_default_when_missing():
    assert extract_claw_session_id({}) == DEFAULT_CLAW_SESSION_ID


def test_extract_and_strip_case_insensitive():
    h = {"Claw-Session-Id": "sess-alpha"}
    assert extract_claw_session_id(h) == "sess-alpha"
    fwd = dict(h)
    strip_claw_session_header(fwd)
    assert CLAW_SESSION_HEADER not in [k.lower() for k in fwd]


def test_extract_blank_falls_back_to_default():
    assert extract_claw_session_id({"claw-session-id": "  "}) == DEFAULT_CLAW_SESSION_ID


def test_sanitize_truncates_long_id():
    long_id = "x" * 200
    s = sanitize_filename_suffix(long_id)
    assert len(s) <= 64
    assert "x" in s


@pytest.mark.asyncio
async def test_dispatcher_splits_sessions(tmp_path: Path):
    dd = tmp_path / "2099-07-01"
    dd.mkdir()
    d = SessionTraceDispatcher(dd, "143022", live_server=None)
    r1 = {"request_id": "a", "turn": 1}
    r2 = {"request_id": "b", "turn": 1}
    await d.write("sess-one", r1)
    await d.write("sess-two", r2)
    d.close()
    paths = sorted(dd.glob("trace_*.jsonl"))
    assert len(paths) == 2
    by_session = {}
    for p in paths:
        rec = json.loads(p.read_text(encoding="utf-8").strip().splitlines()[0])
        by_session[rec["claw_session_id"]] = p
    assert set(by_session) == {"sess-one", "sess-two"}


@pytest.mark.asyncio
async def test_live_sse_filters_by_session(tmp_path: Path):
    from claude_tap.live import LiveViewerServer

    tp = tmp_path / "anchor.jsonl"
    srv = LiveViewerServer(tp, port=0, host="127.0.0.1", output_dir=None)
    port = await srv.start()
    try:
        await srv.broadcast({"request_id": "1", "claw_session_id": "A"})
        await srv.broadcast({"request_id": "2", "claw_session_id": "B"})

        import aiohttp

        async with aiohttp.ClientSession() as session:
            async with session.get(f"http://127.0.0.1:{port}/records?session=A") as resp:
                rows = await resp.json()
                assert len(rows) == 1
                assert rows[0]["claw_session_id"] == "A"
    finally:
        await srv.stop()


@pytest.mark.asyncio
async def test_alloc_turn_per_session(tmp_path: Path):
    dd = tmp_path / "2099-07-02"
    dd.mkdir()
    d = SessionTraceDispatcher(dd, "090000", live_server=None)
    assert await d.alloc_turn("s1") == 1
    assert await d.alloc_turn("s1") == 2
    assert await d.alloc_turn("s2") == 1
    d.close()
