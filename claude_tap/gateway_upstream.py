"""Poll PostgreSQL for active gateway LLM upstream (claw-tap mode; DB only). Author: kejiqing"""

from __future__ import annotations

import asyncio
import logging
import os

from claude_tap.gateway_llm import ActiveLlmRuntime, GatewayLlmConfigError, fetch_active_llm_runtime
from claude_tap.upstream_config import UpstreamSnapshot, strip_path_prefix_for

log = logging.getLogger("claude-tap")

DEFAULT_POLL_SECS = 30.0

_AUTH_HEADER_NAMES = frozenset({"x-api-key", "authorization"})


def apply_gateway_auth_headers(headers: dict[str, str], *, client: str, api_key: str) -> None:
    """Replace client auth headers with the gateway-managed API key from PostgreSQL.

    In claw gateway mode the LLM key is stored in DB; client-supplied keys must not
    override it when forwarding to the upstream LLM.
    Author: kejiqing
    """
    key = api_key.strip()
    if not key:
        return
    for name in list(headers):
        if name.lower() in _AUTH_HEADER_NAMES:
            del headers[name]
    if client == "claude":
        headers["x-api-key"] = key
    else:
        headers["Authorization"] = f"Bearer {key}"


def gateway_llm_poll_interval_seconds() -> float:
    raw = os.environ.get("CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS", "").strip()
    if raw:
        try:
            secs = float(raw)
            if secs > 0:
                return secs
        except ValueError:
            pass
    return DEFAULT_POLL_SECS


class GatewayLlmUpstreamStore:
    """Upstream from ``gateway_llm_cluster_*`` only — no ``--tap-target`` / ``.env`` fallback."""

    def __init__(self, *, client: str, database_url: str, cluster_id: str) -> None:
        self.client = client
        self.database_url = database_url
        self.cluster_id = cluster_id.strip()
        self._runtime: ActiveLlmRuntime | None = None
        self._snapshot: UpstreamSnapshot | None = None

    @property
    def runtime(self) -> ActiveLlmRuntime | None:
        return self._runtime

    def is_ready(self) -> bool:
        return self._runtime is not None and self._snapshot is not None

    def snapshot(self) -> UpstreamSnapshot:
        if self._snapshot is None:
            raise GatewayLlmConfigError(
                f"No active LLM loaded for cluster {self.cluster_id!r}; "
                "tap will not proxy until PostgreSQL has an applied model."
            )
        return self._snapshot

    def load_initial(self) -> ActiveLlmRuntime:
        runtime = fetch_active_llm_runtime(self.database_url, self.cluster_id)
        if runtime is None:
            raise GatewayLlmConfigError(
                f"No active LLM for cluster {self.cluster_id!r} in PostgreSQL "
                "(tables gateway_llm_cluster_state / gateway_llm_cluster_revision). "
                "Apply a model in gateway Admin. "
                "Tap ignores --tap-target, OPENAI_BASE_URL, and UPSTREAM_OPENAI_BASE_URL in this mode."
            )
        self._apply_runtime(runtime)
        log.info(
            "Upstream from PostgreSQL: %s (model=%s %s)",
            runtime.base_model_url,
            runtime.model_id,
            runtime.model_name,
        )
        return runtime

    def reload_from_db(self) -> bool:
        runtime = fetch_active_llm_runtime(self.database_url, self.cluster_id)
        if runtime is None:
            if self._runtime is None:
                log.error("PostgreSQL active LLM still missing for cluster %s", self.cluster_id)
            else:
                log.warning(
                    "PostgreSQL active LLM unavailable for cluster %s; keeping %s",
                    self.cluster_id,
                    self._runtime.base_model_url,
                )
            return False
        previous = self._runtime.base_model_url if self._runtime else ""
        self._apply_runtime(runtime)
        if runtime.base_model_url != previous:
            log.info(
                "Upstream from PostgreSQL -> %s (model=%s)",
                runtime.base_model_url,
                runtime.model_name,
            )
            return True
        return False

    def _apply_runtime(self, runtime: ActiveLlmRuntime) -> None:
        self._runtime = runtime
        self._snapshot = UpstreamSnapshot(
            target=runtime.base_model_url,
            strip_path_prefix=strip_path_prefix_for(self.client, runtime.base_model_url),
        )


async def poll_gateway_llm_upstream(store: GatewayLlmUpstreamStore, interval_seconds: float) -> None:
    interval = max(0.2, interval_seconds)
    while True:
        await asyncio.sleep(interval)
        await asyncio.to_thread(store.reload_from_db)
