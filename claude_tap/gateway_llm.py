"""Load active gateway LLM from PostgreSQL (gateway_llm_cluster_* tables). Author: kejiqing"""

from __future__ import annotations

import hashlib
import json
import logging
from dataclasses import dataclass
from typing import Any

log = logging.getLogger("claude-tap")


class GatewayLlmConfigError(RuntimeError):
    """Active LLM missing or invalid in PostgreSQL (no .env / CLI fallback)."""


@dataclass(frozen=True)
class ActiveLlmRuntime:
    model_id: str
    model_rev: str
    base_model_url: str
    model_name: str
    api_key: str


def llm_api_key_slot(model_id: str, model_rev: str) -> str:
    return f"{model_id}@{model_rev}"


def normalize_upstream_base_url(raw: str) -> str | None:
    s = raw.strip().rstrip("/")
    if not s or not (s.startswith("http://") or s.startswith("https://")):
        return None
    return s


def normalize_model_name(raw: str) -> str | None:
    s = raw.strip()
    if not s or len(s) > 256:
        return None
    return s


def normalize_model_name_for_upstream(raw: str, upstream_base_url: str) -> str | None:
    model = normalize_model_name(raw)
    if model is None:
        return None
    host = upstream_base_url.lower()
    if "xiaomimimo" not in host:
        return model
    bare = model.removeprefix("openai/")
    key = bare.lower().replace("_", "-")
    mapped = {
        "mimo-v2.5-pro": "mimo-v2.5-pro",
        "mimo-v2.5": "mimo-v2.5-pro",
        "mimo-v2.5-flash": "mimo-v2.5-flash",
        "mimo-v2-pro": "mimo-v2-pro",
    }.get(key, bare)
    return mapped


def decrypt_llm_api_key(cluster_id: str, stored: str) -> str | None:
    """Decrypt ``api_key_ciphertext`` (matches gateway ``encrypt_llm_api_key``). Author: kejiqing"""
    stored = stored.strip()
    if not stored:
        return None
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except ImportError:
        log.warning("cryptography not available; cannot decrypt cluster LLM api key")
        return None
    try:
        raw = bytes.fromhex(stored)
        if len(raw) <= 12:
            return None
        key = hashlib.sha256(cluster_id.strip().encode("utf-8")).digest()
        plaintext = AESGCM(key).decrypt(raw[:12], raw[12:], None)
        return plaintext.decode("utf-8").strip() or None
    except Exception as exc:
        log.warning("Failed to decrypt LLM api key for cluster %s: %s", cluster_id, exc)
        return None


def _llm_api_key_for(api_keys: dict[str, str], model_id: str, model_rev: str) -> str | None:
    slot = llm_api_key_slot(model_id, model_rev)
    for key in (slot, model_id):
        val = api_keys.get(key, "").strip()
        if val:
            return val
    return None


def _runtime_from_revision(
    *,
    model_id: str,
    model_rev: str,
    base_model_url: str,
    model_name: str,
    api_key: str = "",
) -> ActiveLlmRuntime | None:
    upstream = normalize_upstream_base_url(base_model_url)
    norm_model = normalize_model_name_for_upstream(model_name, upstream or "")
    if not upstream or not norm_model:
        return None
    return ActiveLlmRuntime(
        model_id=model_id,
        model_rev=model_rev,
        base_model_url=upstream,
        model_name=norm_model,
        api_key=api_key,
    )


def load_active_llm_runtime_sync(conn: Any, cluster_id: str) -> ActiveLlmRuntime | None:
    """Load active LLM for ``CLAW_CLUSTER_ID`` from cluster tables (http-gateway-rs source of truth)."""
    cluster_id = cluster_id.strip()
    if not cluster_id:
        return None

    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT active_model_id, active_model_rev
              FROM gateway_llm_cluster_state
             WHERE cluster_id = %s
            """,
            (cluster_id,),
        )
        state = cur.fetchone()
    if state is None:
        return _load_active_llm_runtime_legacy(conn)
    active_id, active_rev = (state[0] or "").strip(), (state[1] or "").strip()
    if not active_id or not active_rev:
        return _load_active_llm_runtime_legacy(conn)

    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT base_model_url, model_name
              FROM gateway_llm_cluster_revision
             WHERE cluster_id = %s AND model_id = %s AND model_rev = %s
            """,
            (cluster_id, active_id, active_rev),
        )
        rev_row = cur.fetchone()
    if rev_row is None:
        log.warning(
            "Missing gateway_llm_cluster_revision for cluster=%s model=%s rev=%s",
            cluster_id,
            active_id,
            active_rev,
        )
        return None

    api_key = ""
    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT api_key_ciphertext, base_model_url, model_name
              FROM gateway_llm_cluster_model
             WHERE cluster_id = %s AND model_id = %s
            """,
            (cluster_id, active_id),
        )
        model_row = cur.fetchone()
    if model_row is not None:
        api_key = decrypt_llm_api_key(cluster_id, model_row[0] or "") or ""
        base_url = rev_row[0] or model_row[1] or ""
        model_name = rev_row[1] or model_row[2] or ""
    else:
        base_url, model_name = rev_row[0], rev_row[1]

    return _runtime_from_revision(
        model_id=active_id,
        model_rev=active_rev,
        base_model_url=base_url or "",
        model_name=model_name or "",
        api_key=api_key,
    )


def _load_active_llm_runtime_legacy(conn: Any) -> ActiveLlmRuntime | None:
    """Pre-cluster schema fallback (singleton ``gateway_global_settings``). Author: kejiqing"""
    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT llm_models_json, llm_model_api_keys_json, active_llm_model_id,
                   active_llm_model_rev
              FROM gateway_global_settings
             WHERE singleton_id = 1
            """
        )
        row = cur.fetchone()
    if row is None:
        return None

    models_v, keys_v, active_id, active_rev = row
    active_id = (active_id or "").strip()
    active_rev = (active_rev or "").strip()
    if not active_id or not active_rev:
        return None

    if isinstance(models_v, str):
        models_v = json.loads(models_v)
    if isinstance(keys_v, str):
        keys_v = json.loads(keys_v)
    api_keys: dict[str, str] = keys_v if isinstance(keys_v, dict) else {}

    with conn.cursor() as cur:
        cur.execute(
            """
            SELECT base_model_url, model_name
              FROM gateway_llm_model_revision
             WHERE model_id = %s AND model_rev = %s
            """,
            (active_id, active_rev),
        )
        rev_row = cur.fetchone()
    if rev_row is None:
        return None

    api_key = _llm_api_key_for(api_keys, active_id, active_rev) or ""
    return _runtime_from_revision(
        model_id=active_id,
        model_rev=active_rev,
        base_model_url=rev_row[0] or "",
        model_name=rev_row[1] or "",
        api_key=api_key,
    )


def fetch_active_llm_runtime(database_url: str, cluster_id: str) -> ActiveLlmRuntime | None:
    try:
        import psycopg
    except ImportError as exc:
        log.error("psycopg not installed; cannot load gateway LLM from PostgreSQL: %s", exc)
        return None

    try:
        with psycopg.connect(database_url) as conn:
            return load_active_llm_runtime_sync(conn, cluster_id)
    except Exception as exc:
        log.error("Failed to load active LLM from PostgreSQL: %s", exc)
        return None


def fetch_active_upstream_target(database_url: str, cluster_id: str) -> str | None:
    runtime = fetch_active_llm_runtime(database_url, cluster_id)
    return runtime.base_model_url if runtime else None
