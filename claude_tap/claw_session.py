"""Claw session ID extraction and normalization for multi-session traces."""

from __future__ import annotations

import hashlib
import re
from collections.abc import Mapping

# Header clients send to route traces; stripped before forwarding upstream.
CLAW_SESSION_HEADER = "claw-session-id"

# Bucket for requests without the header (backward compatible).
DEFAULT_CLAW_SESSION_ID = "_anonymous"

_MAX_SLUG_LEN = 48


def extract_claw_session_id(headers: Mapping[str, str]) -> str:
    """Return the claw session id from headers, or DEFAULT_CLAW_SESSION_ID if absent or blank."""
    for key, value in headers.items():
        if key.lower() == CLAW_SESSION_HEADER:
            if isinstance(value, str):
                stripped = value.strip()
                return stripped if stripped else DEFAULT_CLAW_SESSION_ID
            return DEFAULT_CLAW_SESSION_ID
    return DEFAULT_CLAW_SESSION_ID


def strip_claw_session_header(headers: dict[str, str]) -> None:
    """Remove claw-session-id from a mutable header dict (any key casing)."""
    to_drop = [k for k in list(headers.keys()) if k.lower() == CLAW_SESSION_HEADER]
    for k in to_drop:
        del headers[k]


def sanitize_filename_suffix(raw: str) -> str:
    """Map a session id to a short filesystem-safe component (may truncate)."""
    if raw == DEFAULT_CLAW_SESSION_ID:
        return "anonymous"
    compact = re.sub(r"[^a-zA-Z0-9._-]+", "_", raw.strip()).strip("._-")
    if not compact:
        compact = "session"
    if len(compact) > _MAX_SLUG_LEN:
        digest = hashlib.sha256(raw.encode("utf-8")).hexdigest()[:12]
        head = compact[: max(8, _MAX_SLUG_LEN - 13)]
        compact = f"{head}_{digest}"
    return compact
