"""Route trace records to per-session JSONL files by claw_session_id."""

from __future__ import annotations

import asyncio
import hashlib
from pathlib import Path
from typing import TYPE_CHECKING

from claude_tap.claw_session import DEFAULT_CLAW_SESSION_ID, sanitize_filename_suffix
from claude_tap.trace import TraceWriter

if TYPE_CHECKING:
    from claude_tap.live import LiveViewerServer


class SessionTraceDispatcher:
    """Lazy TraceWriter per session id; per-session turn allocation and stats aggregation."""

    def __init__(
        self,
        date_dir: Path,
        time_str: str,
        live_server: "LiveViewerServer | None" = None,
    ) -> None:
        self._date_dir = date_dir
        self._time_str = time_str
        self._live_server = live_server
        self._lock = asyncio.Lock()
        self._writers: dict[str, TraceWriter] = {}
        self._raw_to_slug: dict[str, str] = {}
        self._slug_to_raw: dict[str, str] = {}
        self._turns: dict[str, int] = {}

    def attach_live_server(self, live_server: "LiveViewerServer | None") -> None:
        """Set live broadcast target (e.g. after LiveViewerServer.start). New writers pick this up."""
        self._live_server = live_server

    def _make_unique_slug(self, raw: str) -> str:
        base = sanitize_filename_suffix(raw)
        if base not in self._slug_to_raw:
            return base
        if self._slug_to_raw.get(base) == raw:
            return base
        digest = hashlib.sha256(raw.encode("utf-8")).hexdigest()[:8]
        return f"{base}_{digest}"

    def _slug_for(self, raw_claw_session_id: str) -> str:
        if raw_claw_session_id in self._raw_to_slug:
            return self._raw_to_slug[raw_claw_session_id]
        slug = self._make_unique_slug(raw_claw_session_id)
        self._raw_to_slug[raw_claw_session_id] = slug
        self._slug_to_raw[slug] = raw_claw_session_id
        return slug

    def jsonl_path_for_slug(self, slug: str) -> Path:
        return self._date_dir / f"trace_{self._time_str}__{slug}.jsonl"

    async def alloc_turn(self, raw_claw_session_id: str) -> int:
        """Allocate next turn index for this session (1-based within that session)."""
        slug = self._slug_for(raw_claw_session_id)
        async with self._lock:
            self._turns[slug] = self._turns.get(slug, 0) + 1
            return self._turns[slug]

    async def _ensure_writer(self, raw_claw_session_id: str) -> TraceWriter:
        slug = self._slug_for(raw_claw_session_id)
        if slug in self._writers:
            return self._writers[slug]
        async with self._lock:
            if slug in self._writers:
                return self._writers[slug]
            path = self.jsonl_path_for_slug(slug)
            writer = TraceWriter(path, live_server=self._live_server)
            self._writers[slug] = writer
            return writer

    async def write(self, raw_claw_session_id: str, record: dict) -> None:
        """Persist record (must already include ``turn``); sets ``claw_session_id``."""
        record["claw_session_id"] = raw_claw_session_id
        writer = await self._ensure_writer(raw_claw_session_id)
        await writer.write(record)

    def close(self) -> None:
        for w in self._writers.values():
            w.close()

    def iter_session_paths(self) -> list[Path]:
        """JSONL paths in stable order (sorted by slug)."""
        return sorted(w.path for w in self._writers.values())

    def default_primary_trace_path(self) -> Path:
        """Path used for legacy single-file UX when listing primary trace (anonymous bucket)."""
        return self.jsonl_path_for_slug(self._slug_for(DEFAULT_CLAW_SESSION_ID))

    def total_record_count(self) -> int:
        """Total persisted records across all session writers."""
        return sum(w.count for w in self._writers.values())

    def get_summary(self) -> dict:
        """Aggregate statistics across all session writers."""
        api_calls = 0
        total_input = total_output = cache_read = cache_create = 0
        models_used: dict[str, int] = {}
        for w in self._writers.values():
            s = w.get_summary()
            api_calls += s["api_calls"]
            total_input += s["input_tokens"]
            total_output += s["output_tokens"]
            cache_read += s["cache_read_tokens"]
            cache_create += s["cache_create_tokens"]
            for m, c in s["models_used"].items():
                models_used[m] = models_used.get(m, 0) + c
        return {
            "api_calls": api_calls,
            "input_tokens": total_input,
            "output_tokens": total_output,
            "cache_read_tokens": cache_read,
            "cache_create_tokens": cache_create,
            "models_used": models_used,
        }
