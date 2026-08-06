"""LiveViewerServer - SSE-based real-time trace viewer."""

from __future__ import annotations

import asyncio
import copy
import json
from collections import OrderedDict, deque
from pathlib import Path
from urllib.parse import unquote

from aiohttp import web

from claude_tap.session_index import SessionIndex

# Cap in-memory SSE replay per watched session (full history: use /api/sessions/traces).
_SSE_REPLAY_MAX = 1000
# Default Live UI session LRU size (--tap-max-sessions / CLAUDE_TAP_MAX_SESSIONS).
DEFAULT_MAX_SESSIONS = 1000
# Env for Live UI session LRU (same knob as --tap-max-sessions). Author: kejiqing
MAX_SESSIONS_ENV = "CLAUDE_TAP_MAX_SESSIONS"


def resolve_max_sessions(raw: int | str | None = None) -> int:
    """Resolve Live session LRU size from explicit value or ``CLAUDE_TAP_MAX_SESSIONS``."""
    if raw is None:
        import os

        env_raw = os.environ.get(MAX_SESSIONS_ENV, "").strip()
        raw = env_raw if env_raw else DEFAULT_MAX_SESSIONS
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return DEFAULT_MAX_SESSIONS
    return value if value >= 1 else DEFAULT_MAX_SESSIONS


def normalize_live_prefix_path(prefix_path: str) -> str:
    """Normalize external prefix path for live viewer JS (e.g. e2b subpath routing)."""
    raw = str(prefix_path or "").strip()
    if not raw:
        return ""
    return raw.rstrip("/") if raw.startswith("/") else f"/{raw.rstrip('/')}"


class LiveViewerServer:
    """HTTP server for real-time trace viewing via SSE."""

    def __init__(
        self,
        output_dir: Path,
        session_index: SessionIndex,
        port: int = 0,
        host: str = "127.0.0.1",
        prefix_path: str = "",
        max_sessions: int | None = None,
    ):
        self.output_dir = Path(output_dir)
        self.session_index = session_index
        self.port = port
        self.host = host
        self.prefix_path = normalize_live_prefix_path(prefix_path)
        self.max_sessions = resolve_max_sessions(max_sessions)
        self._sse_clients: list[tuple[web.StreamResponse, str]] = []
        # Only sessions touched by Live UI (LRU). Author: kejiqing
        self._session_buffers: OrderedDict[str, deque] = OrderedDict()
        self._lock = asyncio.Lock()
        self._runner: web.AppRunner | None = None
        self._actual_port: int = 0
        self._shutdown_event = asyncio.Event()

    def _active_sse_sessions(self) -> set[str]:
        return {sf for _, sf in self._sse_clients}

    def _evict_lru(self) -> None:
        """Drop oldest buffers that have no live SSE subscriber."""
        active = self._active_sse_sessions()
        while len(self._session_buffers) > self.max_sessions:
            victim = None
            for sid in self._session_buffers:
                if sid not in active:
                    victim = sid
                    break
            if victim is None:
                break
            del self._session_buffers[victim]

    def _open_session_buffer(self, claw_session_id: str) -> deque:
        """Mark session as watched by UI; create empty buffer if needed."""
        if claw_session_id in self._session_buffers:
            self._session_buffers.move_to_end(claw_session_id)
            return self._session_buffers[claw_session_id]
        buf: deque = deque(maxlen=_SSE_REPLAY_MAX)
        self._session_buffers[claw_session_id] = buf
        self._evict_lru()
        return buf

    def _buffer_append_if_watched(self, claw_session_id: str, record: dict) -> None:
        """Append only when Live UI has opened this session (no silent RAM growth)."""
        if claw_session_id not in self._session_buffers:
            return
        self._session_buffers.move_to_end(claw_session_id)
        self._session_buffers[claw_session_id].append(record)

    async def start(self) -> int:
        """Start the viewer server and return the actual port."""
        app = web.Application()
        app.router.add_get("/", self._handle_index)
        app.router.add_get("/events", self._handle_sse)
        app.router.add_get("/records", self._handle_records)
        app.router.add_get("/api/sessions", self._handle_api_sessions)
        app.router.add_get("/api/sessions/traces", self._handle_api_session_traces)
        app.router.add_get("/api/sessions/full", self._handle_api_session_full)

        self._runner = web.AppRunner(app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self.host, self.port)
        await site.start()

        try:
            self._actual_port = site._server.sockets[0].getsockname()[1]
        except (AttributeError, IndexError, OSError):
            self._actual_port = self.port

        return self._actual_port

    async def stop(self) -> None:
        """Stop the viewer server."""
        self._shutdown_event.set()
        for client, _ in self._sse_clients:
            try:
                await client.write_eof()
            except Exception:
                pass
        self._sse_clients.clear()
        self._session_buffers.clear()

        if self._runner:
            await self._runner.cleanup()

    async def broadcast(self, record: dict) -> None:
        """Push to connected SSE clients; buffer only if UI already watches the session."""
        sid = record.get("claw_session_id")
        if not sid:
            return
        sid_s = str(sid)

        async with self._lock:
            self._buffer_append_if_watched(sid_s, record)

        data = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
        message = f"data: {data}\n\n"

        disconnected: list[web.StreamResponse] = []
        for client, sess_filter in self._sse_clients:
            if sess_filter != sid_s:
                continue
            try:
                await client.write(message.encode("utf-8"))
            except (ConnectionError, ConnectionResetError, Exception):
                disconnected.append(client)

        if disconnected:
            self._sse_clients[:] = [(c, sf) for c, sf in self._sse_clients if c not in disconnected]

    @property
    def url(self) -> str:
        """Return the viewer URL."""
        return f"http://{self.host}:{self._actual_port}"

    async def _handle_index(self, request: web.Request) -> web.Response:
        """Serve the viewer HTML with live mode enabled."""
        template = Path(__file__).parent / "viewer.html"
        if not template.exists():
            return web.Response(status=404, text="viewer.html not found")

        html = template.read_text(encoding="utf-8")
        live_js = (
            "const LIVE_MODE = true;\n"
            f"const LIVE_PREFIX_PATH = {json.dumps(self.prefix_path)};\n"
            "const EMBEDDED_TRACE_DATA = [];\n"
            'const __TRACE_JSONL_PATH__ = "";\n'
            'const __TRACE_HTML_PATH__ = "";\n'
        )
        marker = "/* CLAUDETAP_LIVE_CONFIG */\n"
        if marker in html:
            html = html.replace(marker, live_js, 1)
        else:
            # Older templates: inject before first script utility line
            html = html.replace(
                "<script>\nconst $ = s =>",
                f"<script>\n{live_js}\nconst $ = s =>",
                1,
            )
        return web.Response(text=html, content_type="text/html")

    def _require_session(self, request: web.Request) -> str | None:
        qs = request.rel_url.query
        raw = (qs.get("session") or qs.get("claw_session_id") or "").strip()
        return raw if raw else None

    async def _handle_sse(self, request: web.Request) -> web.StreamResponse:
        """SSE endpoint for live trace updates (requires ``?session=``)."""
        sess_filter = self._require_session(request)
        if not sess_filter:
            return web.Response(
                status=400,
                text="session or claw_session_id query parameter is required",
            )

        resp = web.StreamResponse(
            status=200,
            headers={
                "Content-Type": "text/event-stream",
                "Cache-Control": "no-cache",
                "Connection": "keep-alive",
                "Access-Control-Allow-Origin": "*",
            },
        )
        await resp.prepare(request)

        async with self._lock:
            buf = self._open_session_buffer(sess_filter)
            for record in buf:
                data = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
                await resp.write(f"data: {data}\n\n".encode("utf-8"))

        self._sse_clients.append((resp, sess_filter))

        try:
            while not self._shutdown_event.is_set():
                try:
                    await asyncio.wait_for(self._shutdown_event.wait(), timeout=30)
                except asyncio.TimeoutError:
                    pass
                if self._shutdown_event.is_set():
                    break
                try:
                    await resp.write(b": keepalive\n\n")
                except (ConnectionError, ConnectionResetError, RuntimeError):
                    break
        except asyncio.CancelledError:
            pass
        finally:
            self._sse_clients[:] = [(c, sf) for c, sf in self._sse_clients if c is not resp]

        return resp

    async def _handle_records(self, request: web.Request) -> web.Response:
        """Return in-memory records for one session (requires ``?session=``).

        Opening this endpoint marks the session as watched so subsequent
        broadcasts are buffered (history still comes from disk JSONL APIs).
        """
        sess = self._require_session(request)
        if not sess:
            return web.Response(
                status=400,
                text="session or claw_session_id query parameter is required",
            )
        async with self._lock:
            buf = list(self._open_session_buffer(sess))
            return web.json_response(buf)

    async def _handle_api_sessions(self, request: web.Request) -> web.Response:
        """Paginated session list (``limit`` default 100, ``offset`` default 0)."""
        qs = request.rel_url.query
        try:
            limit = min(500, max(1, int(qs.get("limit", "100"))))
        except ValueError:
            limit = 100
        try:
            offset = max(0, int(qs.get("offset", "0")))
        except ValueError:
            offset = 0

        rows, total = self.session_index.list_sessions(limit, offset)
        sessions = [
            {
                "claw_session_id": r.claw_session_id,
                "storage_slug": r.storage_slug,
                "jsonl_relpath": r.jsonl_relpath,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
                "first_calendar_date": r.first_calendar_date,
                "last_calendar_date": r.last_calendar_date,
                "last_turn": r.last_turn,
            }
            for r in rows
        ]
        return web.json_response({"sessions": sessions, "total": total, "limit": limit, "offset": offset})

    async def _handle_api_session_traces(self, request: web.Request) -> web.Response:
        """Load JSONL records for one ``claw_session_id`` (query ``session=``).

        Supports incremental polling via ``since_turn`` (exclusive).
        """
        raw_q = request.rel_url.query.get("session") or request.rel_url.query.get("claw_session_id") or ""
        claw_session_id = unquote(raw_q.strip())
        if not claw_session_id:
            return web.Response(
                status=400,
                text="session or claw_session_id query parameter is required",
            )
        try:
            since_turn = int(request.rel_url.query.get("since_turn", "0"))
        except ValueError:
            since_turn = 0
        if since_turn < 0:
            since_turn = 0

        return web.json_response(self._load_session_records(claw_session_id, since_turn=since_turn))

    async def _handle_api_session_full(self, request: web.Request) -> web.Response:
        """Load full records for one session while stripping stream event noise."""
        raw_q = request.rel_url.query.get("session") or request.rel_url.query.get("claw_session_id") or ""
        claw_session_id = unquote(raw_q.strip())
        if not claw_session_id:
            return web.Response(
                status=400,
                text="session or claw_session_id query parameter is required",
            )
        try:
            since_turn = int(request.rel_url.query.get("since_turn", "0"))
        except ValueError:
            since_turn = 0
        if since_turn < 0:
            since_turn = 0

        raw_records = self._load_session_records(claw_session_id, since_turn=since_turn)
        filtered: list[dict] = []
        for rec in raw_records:
            if not isinstance(rec, dict):
                continue
            clean = copy.deepcopy(rec)
            response = clean.get("response")
            if isinstance(response, dict):
                response.pop("sse_events", None)
                response.pop("ws_events", None)
            filtered.append(clean)
        return web.json_response(filtered)

    def _load_session_records(self, claw_session_id: str, since_turn: int = 0) -> list[dict]:
        """Read one session's JSONL records, optionally filtering by turn."""
        row = self.session_index.get_session(claw_session_id)
        if not row:
            return []

        path = self.output_dir / row.jsonl_relpath
        if not path.is_file():
            return []

        records: list[dict] = []
        try:
            text = path.read_text(encoding="utf-8")
            for line in text.splitlines():
                line = line.strip()
                if not line:
                    continue
                rec = json.loads(line)
                if not isinstance(rec, dict):
                    continue
                if since_turn > 0:
                    turn = rec.get("turn")
                    try:
                        turn_i = int(turn)
                    except (TypeError, ValueError):
                        continue
                    if turn_i <= since_turn:
                        continue
                records.append(rec)
        except (OSError, json.JSONDecodeError):
            return []
        return records
