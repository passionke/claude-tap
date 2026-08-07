"""Live traces strip stream chunks; Ajax stream-events loads one turn. Author: kejiqing"""

from __future__ import annotations

from pathlib import Path

import aiohttp
import pytest

from claude_tap.live import LiveViewerServer
from claude_tap.session_dispatcher import SessionTraceDispatcher
from claude_tap.session_index import SessionIndex


@pytest.mark.asyncio
async def test_traces_strip_chunks_stream_events_ajax(tmp_path: Path) -> None:
    idx = SessionIndex(tmp_path)
    disp = SessionTraceDispatcher(tmp_path, idx, live_server=None)
    turn = await disp.alloc_turn("sess-lazy")
    await disp.write(
        "sess-lazy",
        {
            "turn": turn,
            "response": {
                "status": 200,
                "body": {"ok": True},
                "sse_events": [
                    {"event": "message_delta", "data": {"t": "hi"}},
                    {"event": "message_stop", "data": {}},
                ],
            },
        },
    )
    server = LiveViewerServer(tmp_path, idx, port=0, host="127.0.0.1")
    port = await server.start()
    try:
        async with aiohttp.ClientSession() as session:
            async with session.get(
                f"http://127.0.0.1:{port}/api/sessions/traces",
                params={"session": "sess-lazy"},
            ) as resp:
                assert resp.status == 200
                rows = await resp.json()
                assert len(rows) == 1
                assert "sse_events" not in rows[0]["response"]
                assert rows[0]["response"]["sse_event_count"] == 2

            async with session.get(
                f"http://127.0.0.1:{port}/api/sessions/stream-events",
                params={"session": "sess-lazy", "turn": str(turn)},
            ) as resp:
                assert resp.status == 200
                body = await resp.json()
                assert len(body["sse_events"]) == 2
                assert body["sse_events"][0]["event"] == "message_delta"
    finally:
        await server.stop()
        idx.close()
