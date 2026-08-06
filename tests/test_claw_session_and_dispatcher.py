"""Unit tests for claw-session routing and SessionTraceDispatcher."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from claude_tap.claw_session import (
    CLAW_SESSION_HEADER,
    extract_claw_session_id,
    sanitize_filename_suffix,
    strip_claw_session_header,
)
from claude_tap.live import LiveViewerServer
from claude_tap.session_index import SessionIndex
from tests.conftest import make_trace_dispatcher


def test_extract_none_when_missing():
    assert extract_claw_session_id({}) is None


def test_extract_and_strip_case_insensitive():
    h = {"Claw-Session-Id": "sess-alpha"}
    assert extract_claw_session_id(h) == "sess-alpha"
    fwd = dict(h)
    strip_claw_session_header(fwd)
    assert CLAW_SESSION_HEADER not in [k.lower() for k in fwd]


def test_extract_blank_is_none():
    assert extract_claw_session_id({"claw-session-id": "  "}) is None


def test_sanitize_truncates_long_id():
    long_id = "x" * 200
    s = sanitize_filename_suffix(long_id)
    assert len(s) <= 64
    assert "x" in s


@pytest.mark.asyncio
async def test_dispatcher_splits_sessions(tmp_path: Path):
    d = make_trace_dispatcher(tmp_path)
    r1 = {"request_id": "a", "turn": 1}
    r2 = {"request_id": "b", "turn": 1}
    await d.write("sess-one", r1)
    await d.write("sess-two", r2)
    d.close()
    paths = sorted(tmp_path.glob("sessions/*/trace.jsonl"))
    assert len(paths) == 2
    by_session = {}
    for p in paths:
        rec = json.loads(p.read_text(encoding="utf-8").strip().splitlines()[0])
        by_session[rec["claw_session_id"]] = p
    assert set(by_session) == {"sess-one", "sess-two"}


@pytest.mark.asyncio
async def test_live_sse_filters_by_session(tmp_path: Path):
    idx = SessionIndex(tmp_path)
    srv = LiveViewerServer(tmp_path, idx, port=0, host="127.0.0.1")
    port = await srv.start()
    try:
        import aiohttp

        async with aiohttp.ClientSession() as session:
            # UI open marks session watched before broadcasts buffer. Author: kejiqing
            async with session.get(f"http://127.0.0.1:{port}/records?session=A") as resp:
                assert await resp.json() == []

            await srv.broadcast({"request_id": "1", "claw_session_id": "A"})
            await srv.broadcast({"request_id": "2", "claw_session_id": "B"})

            async with session.get(f"http://127.0.0.1:{port}/records?session=A") as resp:
                rows = await resp.json()
                assert len(rows) == 1
                assert rows[0]["claw_session_id"] == "A"

            # B never opened via UI — still empty until watch
            async with session.get(f"http://127.0.0.1:{port}/records?session=B") as resp:
                assert await resp.json() == []
    finally:
        await srv.stop()
        idx.close()


@pytest.mark.asyncio
async def test_live_buffer_skips_unwatched_and_lru(tmp_path: Path):
    """Broadcast without UI must not grow RAM; LRU caps watched sessions."""
    idx = SessionIndex(tmp_path)
    srv = LiveViewerServer(tmp_path, idx, port=0, host="127.0.0.1", max_sessions=2)
    try:
        await srv.broadcast({"request_id": "x", "claw_session_id": "ghost"})
        assert "ghost" not in srv._session_buffers

        async with srv._lock:
            srv._open_session_buffer("s1")
            srv._open_session_buffer("s2")
            srv._open_session_buffer("s3")
        assert list(srv._session_buffers.keys()) == ["s2", "s3"]
        assert srv.max_sessions == 2

        await srv.broadcast({"request_id": "1", "claw_session_id": "s2"})
        assert len(srv._session_buffers["s2"]) == 1
    finally:
        await srv.stop()
        idx.close()


def test_resolve_max_sessions_env(monkeypatch):
    from claude_tap.live import MAX_SESSIONS_ENV, resolve_max_sessions

    monkeypatch.delenv(MAX_SESSIONS_ENV, raising=False)
    assert resolve_max_sessions() == 1000
    monkeypatch.setenv(MAX_SESSIONS_ENV, "42")
    assert resolve_max_sessions() == 42
    assert resolve_max_sessions(7) == 7
    monkeypatch.setenv(MAX_SESSIONS_ENV, "0")
    assert resolve_max_sessions() == 1000


@pytest.mark.asyncio
async def test_alloc_turn_per_session(tmp_path: Path):
    d = make_trace_dispatcher(tmp_path)
    assert await d.alloc_turn("s1") == 1
    assert await d.alloc_turn("s1") == 2
    assert await d.alloc_turn("s2") == 1
    d.close()

    d2 = make_trace_dispatcher(tmp_path)
    assert await d2.alloc_turn("s1") == 3
    assert await d2.alloc_turn("s2") == 2
    d2.close()


@pytest.mark.asyncio
async def test_trace_writer_reopens_after_fd_release(tmp_path: Path):
    from claude_tap.trace import TraceWriter

    path = tmp_path / "sessions" / "s" / "trace.jsonl"
    w = TraceWriter(path)
    await w.write({"turn": 1, "claw_session_id": "s"})
    assert w.is_open
    w.release_fd()
    assert not w.is_open
    await w.write({"turn": 2, "claw_session_id": "s"})
    assert w.is_open
    w.close()
    lines = path.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 2
    assert json.loads(lines[1])["turn"] == 2


@pytest.mark.asyncio
async def test_dispatcher_reclaims_idle_writer_fd(tmp_path: Path):
    from claude_tap.session_dispatcher import SessionTraceDispatcher

    idx = SessionIndex(tmp_path)
    d = SessionTraceDispatcher(tmp_path, idx, writer_idle_seconds=0.01)
    await d.write("idle-sess", {"turn": 1, "request_id": "a"})
    writer = d._writers[d._slug_for("idle-sess")]
    assert writer.is_open
    writer._last_used -= 1.0
    await d.write("other", {"turn": 1, "request_id": "b"})
    assert not writer.is_open
    await d.write("idle-sess", {"turn": 2, "request_id": "c"})
    assert writer.is_open
    d.close()
    idx.close()
    path = tmp_path / "sessions" / d._slug_for("idle-sess") / "trace.jsonl"
    assert len(path.read_text(encoding="utf-8").strip().splitlines()) == 2
