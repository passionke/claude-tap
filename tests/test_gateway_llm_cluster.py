"""Tests for per-cluster / per-project PostgreSQL LLM loading. Author: kejiqing"""

from __future__ import annotations

from typing import Any

from claude_tap.gateway_llm import (
    _runtime_from_revision,
    load_active_llm_runtime_sync,
    normalize_upstream_base_url,
)
from claude_tap.gateway_upstream import GatewayLlmUpstreamStore


def test_runtime_from_revision_maxiot_url():
    rt = _runtime_from_revision(
        model_id="llm-1",
        model_rev="2026-05-29_16-17-39",
        base_model_url="https://llm-gw-sh.maxiot-inc.com:5443/v1",
        model_name="qwen3.7-max",
    )
    assert rt is not None
    assert rt.base_model_url == "https://llm-gw-sh.maxiot-inc.com:5443/v1"
    assert rt.model_name == "qwen3.7-max"


def test_normalize_upstream_strips_trailing_slash():
    assert normalize_upstream_base_url("https://api.deepseek.com/") == "https://api.deepseek.com"


class _FakeCursor:
    def __init__(self, rows: list[Any]) -> None:
        self._rows = list(rows)
        self._idx = 0
        self.last_sql = ""
        self.last_params: tuple[Any, ...] | None = None

    def execute(self, sql: str, params: tuple[Any, ...] | None = None) -> None:
        self.last_sql = " ".join(sql.split())
        self.last_params = params

    def fetchone(self) -> Any:
        if self._idx >= len(self._rows):
            return None
        row = self._rows[self._idx]
        self._idx += 1
        return row

    def __enter__(self) -> _FakeCursor:
        return self

    def __exit__(self, *args: object) -> None:
        return None


class _FakeConn:
    """Returns a new cursor per ``with conn.cursor()`` with the next scripted row list."""

    def __init__(self, scripts: list[list[Any]]) -> None:
        self._scripts = list(scripts)
        self.cursors: list[_FakeCursor] = []

    def cursor(self) -> _FakeCursor:
        rows = self._scripts.pop(0) if self._scripts else []
        cur = _FakeCursor(rows)
        self.cursors.append(cur)
        return cur


def _encrypt_for_test(cluster_id: str, plaintext: str) -> str:
    import hashlib

    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    key = hashlib.sha256(cluster_id.encode("utf-8")).digest()
    nonce = b"\x00" * 12
    ct = AESGCM(key).encrypt(nonce, plaintext.encode("utf-8"), None)
    return (nonce + ct).hex()


def test_load_active_project_llm_runtime_reads_project_tables():
    cluster = "ailab-dev"
    ciphertext = _encrypt_for_test(cluster, "sk-project-deepseek-v4")
    conn = _FakeConn(
        [
            [("llm-proj-1", "2026-08-25_03-37-46")],  # state
            [("https://llm-gw.maxiot-inc.com/v1", "deepseek-v4-pro")],  # revision
            [
                (
                    ciphertext,
                    "https://llm-gw.maxiot-inc.com/v1",
                    "deepseek-v4-pro",
                )
            ],  # model
        ]
    )
    rt = load_active_llm_runtime_sync(conn, cluster, proj_id=297)
    assert rt is not None
    assert rt.model_name == "deepseek-v4-pro"
    assert rt.base_model_url == "https://llm-gw.maxiot-inc.com/v1"
    assert rt.api_key == "sk-project-deepseek-v4"
    assert "gateway_llm_project_state" in conn.cursors[0].last_sql
    assert conn.cursors[0].last_params == (cluster, 297)
    assert "gateway_llm_project_revision" in conn.cursors[1].last_sql
    assert conn.cursors[1].last_params == (cluster, 297, "llm-proj-1", "2026-08-25_03-37-46")
    assert "gateway_llm_project_model" in conn.cursors[2].last_sql
    assert conn.cursors[2].last_params == (cluster, 297, "llm-proj-1")
    assert all("gateway_llm_cluster_" not in c.last_sql for c in conn.cursors)


def test_load_active_without_proj_id_reads_cluster_tables():
    cluster = "ailab-dev"
    ciphertext = _encrypt_for_test(cluster, "sk-global-qwen")
    conn = _FakeConn(
        [
            [("llm-global", "rev-1")],
            [("https://llm-gw.maxiot-inc.com/v1", "qwen3.7-max")],
            [(ciphertext, "https://llm-gw.maxiot-inc.com/v1", "qwen3.7-max")],
        ]
    )
    rt = load_active_llm_runtime_sync(conn, cluster, proj_id=None)
    assert rt is not None
    assert rt.model_name == "qwen3.7-max"
    assert rt.api_key == "sk-global-qwen"
    assert "gateway_llm_cluster_state" in conn.cursors[0].last_sql
    assert all("gateway_llm_project_" not in c.last_sql for c in conn.cursors)


def test_load_active_proj_id_zero_falls_back_to_cluster():
    cluster = "ailab-dev"
    ciphertext = _encrypt_for_test(cluster, "sk-global")
    conn = _FakeConn(
        [
            [("llm-global", "rev-1")],
            [("https://example.com/v1", "qwen")],
            [(ciphertext, "https://example.com/v1", "qwen")],
        ]
    )
    rt = load_active_llm_runtime_sync(conn, cluster, proj_id=0)
    assert rt is not None
    assert "gateway_llm_cluster_state" in conn.cursors[0].last_sql


def test_missing_project_state_does_not_fall_back_to_cluster():
    """With proj_id set, missing project row must not silently use global key. Author: kejiqing"""
    conn = _FakeConn([[]])  # state miss
    rt = load_active_llm_runtime_sync(conn, "ailab-dev", proj_id=297)
    assert rt is None
    assert len(conn.cursors) == 1
    assert "gateway_llm_project_state" in conn.cursors[0].last_sql


def test_upstream_store_scope_uses_proj_id():
    store = GatewayLlmUpstreamStore(
        client="codex",
        database_url="postgres://u:p@h/db",
        cluster_id="ailab-dev",
        proj_id=297,
    )
    assert store.proj_id == 297
    assert "proj_id=297" in store._scope_label()
    assert "gateway_llm_project_state" in store._missing_tables_hint()

    store0 = GatewayLlmUpstreamStore(
        client="codex",
        database_url="postgres://u:p@h/db",
        cluster_id="ailab-dev",
        proj_id=0,
    )
    assert store0.proj_id is None
    assert "gateway_llm_cluster_state" in store0._missing_tables_hint()
