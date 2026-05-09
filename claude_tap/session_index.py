"""SQLite index: claw_session_id, storage paths, and session listing."""

from __future__ import annotations

import shutil
import sqlite3
import threading
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

SESSIONS_DB_FILENAME = "claude_tap_sessions.sqlite3"
SESSIONS_SUBDIR = "sessions"


def _utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass(frozen=True)
class SessionRow:
    claw_session_id: str
    storage_slug: str
    jsonl_relpath: str
    created_at: str
    updated_at: str
    first_calendar_date: str | None
    last_calendar_date: str | None
    last_turn: int


class SessionIndex:
    """Thread-safe SQLite session registry under ``output_dir``."""

    def __init__(self, output_dir: Path) -> None:
        self._output_dir = Path(output_dir)
        self._output_dir.mkdir(parents=True, exist_ok=True)
        self._db_path = self._output_dir / SESSIONS_DB_FILENAME
        self._lock = threading.Lock()
        self._closed = False
        self._conn = sqlite3.connect(self._db_path, check_same_thread=False)
        self._conn.row_factory = sqlite3.Row
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._init_schema()

    def _init_schema(self) -> None:
        self._conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS sessions (
              claw_session_id TEXT PRIMARY KEY,
              storage_slug TEXT UNIQUE NOT NULL,
              jsonl_relpath TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              first_calendar_date TEXT,
              last_calendar_date TEXT,
              last_turn INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_updated
              ON sessions(updated_at DESC);
            """
        )
        self._conn.commit()

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            self._conn.close()

    @property
    def output_dir(self) -> Path:
        return self._output_dir

    def get_last_turn(self, claw_session_id: str) -> int:
        with self._lock:
            cur = self._conn.execute(
                "SELECT last_turn FROM sessions WHERE claw_session_id = ?",
                (claw_session_id,),
            )
            row = cur.fetchone()
            return int(row[0]) if row else 0

    def upsert_session_row(
        self,
        claw_session_id: str,
        storage_slug: str,
        jsonl_relpath: str,
    ) -> None:
        now = _utc_now_iso()
        cal = datetime.now(timezone.utc).date().isoformat()
        with self._lock:
            self._conn.execute(
                """
                INSERT INTO sessions (
                  claw_session_id, storage_slug, jsonl_relpath,
                  created_at, updated_at, first_calendar_date, last_calendar_date, last_turn
                ) VALUES (?, ?, ?, ?, ?, ?, ?, 0)
                ON CONFLICT(claw_session_id) DO UPDATE SET
                  updated_at = excluded.updated_at
                """,
                (claw_session_id, storage_slug, jsonl_relpath, now, now, cal, cal),
            )
            self._conn.commit()

    def record_write(self, claw_session_id: str, turn: int) -> None:
        now = _utc_now_iso()
        cal = datetime.now(timezone.utc).date().isoformat()
        with self._lock:
            cur = self._conn.execute(
                "SELECT last_turn FROM sessions WHERE claw_session_id = ?",
                (claw_session_id,),
            )
            row = cur.fetchone()
            prev = int(row[0]) if row else 0
            new_last = max(prev, int(turn))
            self._conn.execute(
                """
                UPDATE sessions SET
                  updated_at = ?,
                  last_turn = ?,
                  last_calendar_date = ?,
                  first_calendar_date = COALESCE(first_calendar_date, ?)
                WHERE claw_session_id = ?
                """,
                (now, new_last, cal, cal, claw_session_id),
            )
            self._conn.commit()

    def get_session(self, claw_session_id: str) -> SessionRow | None:
        with self._lock:
            cur = self._conn.execute(
                "SELECT * FROM sessions WHERE claw_session_id = ?",
                (claw_session_id,),
            )
            r = cur.fetchone()
            if not r:
                return None
            return _row_to_session(r)

    def list_sessions(self, limit: int, offset: int) -> tuple[list[SessionRow], int]:
        with self._lock:
            cur = self._conn.execute("SELECT COUNT(*) FROM sessions")
            total = int(cur.fetchone()[0])
            cur = self._conn.execute(
                """
                SELECT * FROM sessions
                ORDER BY updated_at DESC
                LIMIT ? OFFSET ?
                """,
                (limit, offset),
            )
            rows = [_row_to_session(r) for r in cur.fetchall()]
            return rows, total

    def session_count(self) -> int:
        with self._lock:
            cur = self._conn.execute("SELECT COUNT(*) FROM sessions")
            return int(cur.fetchone()[0])

    def delete_oldest_sessions(self, count: int) -> int:
        """Remove ``count`` sessions with smallest ``updated_at``. Returns deleted count."""
        if count <= 0:
            return 0
        deleted = 0
        with self._lock:
            cur = self._conn.execute(
                """
                SELECT claw_session_id, jsonl_relpath FROM sessions
                ORDER BY updated_at ASC
                LIMIT ?
                """,
                (count,),
            )
            victims = [(str(r[0]), str(r[1])) for r in cur.fetchall()]
            for cid, relpath in victims:
                self._conn.execute("DELETE FROM sessions WHERE claw_session_id = ?", (cid,))
                deleted += 1
                abs_path = self._output_dir / relpath
                parent = abs_path.parent
                if parent.is_dir() and parent.name != SESSIONS_SUBDIR:
                    try:
                        shutil.rmtree(parent)
                    except OSError:
                        pass
                elif abs_path.exists():
                    try:
                        abs_path.unlink()
                    except OSError:
                        pass
                for ext in (".html",):
                    hp = abs_path.with_suffix(ext)
                    if hp.exists():
                        try:
                            hp.unlink()
                        except OSError:
                            pass
            self._conn.commit()
        return deleted


def _row_to_session(r: sqlite3.Row) -> SessionRow:
    return SessionRow(
        claw_session_id=str(r["claw_session_id"]),
        storage_slug=str(r["storage_slug"]),
        jsonl_relpath=str(r["jsonl_relpath"]),
        created_at=str(r["created_at"]),
        updated_at=str(r["updated_at"]),
        first_calendar_date=r["first_calendar_date"],
        last_calendar_date=r["last_calendar_date"],
        last_turn=int(r["last_turn"]),
    )


def jsonl_relpath_for_slug(storage_slug: str) -> str:
    return f"{SESSIONS_SUBDIR}/{storage_slug}/trace.jsonl"
