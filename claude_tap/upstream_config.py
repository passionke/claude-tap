"""Hot-reloadable upstream target from a JSON config file (reverse proxy mode)."""

from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import dataclass
from pathlib import Path

log = logging.getLogger("claude-tap")


def strip_path_prefix_for(client: str, target: str) -> str:
    """Codex path prefix stripping when target is not api.openai.com."""
    return "/v1" if client == "codex" and "api.openai.com" not in target else ""


@dataclass(frozen=True)
class UpstreamSnapshot:
    target: str
    strip_path_prefix: str


def parse_upstream_config_text(text: str) -> str:
    """Parse ``target`` or ``target_url`` from JSON config body."""
    data = json.loads(text)
    if not isinstance(data, dict):
        raise ValueError("config root must be a JSON object")
    raw = data.get("target") if data.get("target") is not None else data.get("target_url")
    if not isinstance(raw, str) or not raw.strip():
        raise ValueError('config must include non-empty "target" or "target_url"')
    return raw.strip().rstrip("/")


@dataclass
class UpstreamConfigStore:
    """Mutable upstream settings; each HTTP request reads a fresh snapshot."""

    client: str
    config_path: Path
    fallback_target: str
    _snapshot: UpstreamSnapshot | None = None
    _mtime_ns: int | None = None

    def __post_init__(self) -> None:
        self._apply_target(self.fallback_target)

    def snapshot(self) -> UpstreamSnapshot:
        assert self._snapshot is not None
        return self._snapshot

    def load_initial(self) -> bool:
        """Load config file on startup if present; otherwise keep CLI fallback."""
        return self._reload_if_changed(force=True)

    def reload_if_changed(self) -> bool:
        """Reload when the config file mtime changes. Returns True if target updated."""
        return self._reload_if_changed(force=False)

    def _apply_target(self, target: str) -> None:
        normalized = target.strip().rstrip("/")
        self._snapshot = UpstreamSnapshot(
            target=normalized,
            strip_path_prefix=strip_path_prefix_for(self.client, normalized),
        )

    def _reload_if_changed(self, *, force: bool) -> bool:
        path = self.config_path
        if not path.is_file():
            if force:
                log.info("Upstream config %s not found; using --tap-target %s", path, self.fallback_target)
            return False

        try:
            stat = path.stat()
        except OSError as exc:
            log.warning("Cannot stat upstream config %s: %s", path, exc)
            return False

        mtime_ns = stat.st_mtime_ns
        if not force and mtime_ns == self._mtime_ns:
            return False

        try:
            text = path.read_text(encoding="utf-8")
            target = parse_upstream_config_text(text)
        except (OSError, json.JSONDecodeError, ValueError) as exc:
            log.warning("Ignoring invalid upstream config %s: %s", path, exc)
            self._mtime_ns = mtime_ns
            return False

        previous = self._snapshot.target if self._snapshot else ""
        self._mtime_ns = mtime_ns
        self._apply_target(target)
        if target != previous:
            log.info("Upstream target -> %s (from %s)", target, path)
            return True
        return False


async def poll_upstream_config(store: UpstreamConfigStore, interval_seconds: float) -> None:
    """Background task: poll config file mtime and reload upstream target."""
    interval = max(0.2, interval_seconds)
    while True:
        await asyncio.sleep(interval)
        store.reload_if_changed()


def resolve_upstream(ctx: dict) -> UpstreamSnapshot:
    """Resolve upstream for one request (hot-reload store or static trace_ctx)."""
    store = ctx.get("upstream")
    if store is not None:
        return store.snapshot()
    return UpstreamSnapshot(
        target=ctx["target_url"],
        strip_path_prefix=ctx.get("strip_path_prefix", ""),
    )
