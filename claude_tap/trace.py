"""TraceWriter – async JSONL writer with statistics."""

from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path
from typing import TYPE_CHECKING, TextIO

if TYPE_CHECKING:
    from claude_tap.live import LiveViewerServer


class TraceWriter:
    """Writes trace records to a JSONL file and accumulates statistics.

    File handles may be released after idle time and re-opened on the next write.
    Author: kejiqing
    """

    def __init__(self, path: Path, live_server: "LiveViewerServer | None" = None):
        self.path = path
        self._lock = asyncio.Lock()
        self.count = 0
        # Token statistics
        self.total_input_tokens = 0
        self.total_output_tokens = 0
        self.total_cache_read_tokens = 0
        self.total_cache_create_tokens = 0
        self.models_used: dict[str, int] = {}
        self._live_server = live_server
        self._file: TextIO | None = None
        self._last_used = time.monotonic()
        self._ensure_open_unlocked()

    @property
    def is_open(self) -> bool:
        return self._file is not None and not self._file.closed

    @property
    def idle_seconds(self) -> float:
        return time.monotonic() - self._last_used

    def _ensure_open_unlocked(self) -> None:
        """Open append handle if missing/closed (caller holds ``_lock`` or ctor)."""
        if self._file is not None and not self._file.closed:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._file = open(self.path, "a", encoding="utf-8")

    def release_fd(self) -> None:
        """Flush and close the file handle; writer state/stats are kept.

        Must not race with ``write``; callers should hold ``_lock`` or ensure idle.
        """
        if self._file is not None and not self._file.closed:
            self._file.flush()
            self._file.close()
        self._file = None

    async def release_fd_async(self) -> None:
        """Close FD under the writer lock (safe vs concurrent ``write``)."""
        async with self._lock:
            self.release_fd()

    async def write(self, record: dict) -> None:
        """Write a record and update statistics."""
        async with self._lock:
            self._ensure_open_unlocked()
            assert self._file is not None
            self._file.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
            self._file.flush()
            self.count += 1
            self._update_stats(record)
            self._last_used = time.monotonic()

        # Broadcast to live viewer if enabled
        if self._live_server:
            await self._live_server.broadcast(record)

    def close(self) -> None:
        """Flush and close the JSONL file."""
        self.release_fd()

    def _update_stats(self, record: dict) -> None:
        """Extract token usage from record and update totals."""
        req_body = record.get("request", {}).get("body", {})
        model = req_body.get("model", "unknown") if isinstance(req_body, dict) else "unknown"
        self.models_used[model] = self.models_used.get(model, 0) + 1

        resp_body = record.get("response", {}).get("body", {})
        usage = resp_body.get("usage", {}) if isinstance(resp_body, dict) else {}
        if not usage and isinstance(resp_body, dict):
            usage = resp_body

        input_tokens = usage.get("input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_create = usage.get("cache_creation_input_tokens", 0)

        self.total_input_tokens += input_tokens
        self.total_output_tokens += output_tokens
        self.total_cache_read_tokens += cache_read
        self.total_cache_create_tokens += cache_create

    def get_summary(self) -> dict:
        """Return a summary of the trace statistics."""
        return {
            "api_calls": self.count,
            "input_tokens": self.total_input_tokens,
            "output_tokens": self.total_output_tokens,
            "cache_read_tokens": self.total_cache_read_tokens,
            "cache_create_tokens": self.total_cache_create_tokens,
            "models_used": self.models_used,
        }
