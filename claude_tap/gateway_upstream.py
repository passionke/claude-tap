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
    """Upstream from PG: project tables when ``proj_id`` set, else ``gateway_llm_cluster_*``.

    Author: kejiqing
    """

    def __init__(
        self,
        *,
        client: str,
        database_url: str,
        cluster_id: str,
        proj_id: int | None = None,
    ) -> None:
        self.client = client
        self.database_url = database_url
        self.cluster_id = cluster_id.strip()
        self.proj_id = proj_id if proj_id is not None and proj_id >= 1 else None
        self._runtime: ActiveLlmRuntime | None = None
        self._snapshot: UpstreamSnapshot | None = None

    @property
    def runtime(self) -> ActiveLlmRuntime | None:
        return self._runtime

    def is_ready(self) -> bool:
        return self._runtime is not None and self._snapshot is not None

    def _scope_label(self) -> str:
        if self.proj_id is not None:
            return f"cluster {self.cluster_id!r} proj_id={self.proj_id}"
        return f"cluster {self.cluster_id!r}"

    def _missing_tables_hint(self) -> str:
        if self.proj_id is not None:
            return (
                "tables gateway_llm_project_state / gateway_llm_project_revision "
                f"(CLAW_PROJ_ID={self.proj_id})"
            )
        return "tables gateway_llm_cluster_state / gateway_llm_cluster_revision"

    def snapshot(self) -> UpstreamSnapshot:
        if self._snapshot is None:
            raise GatewayLlmConfigError(
                f"No active LLM loaded for {self._scope_label()}; "
                "tap will not proxy until PostgreSQL has an applied model."
            )
        return self._snapshot

    def _fetch(self) -> ActiveLlmRuntime | None:
        return fetch_active_llm_runtime(
            self.database_url, self.cluster_id, proj_id=self.proj_id
        )

    def load_initial(self) -> ActiveLlmRuntime:
        runtime = self._fetch()
        if runtime is None:
            raise GatewayLlmConfigError(
                f"No active LLM for {self._scope_label()} in PostgreSQL "
                f"({self._missing_tables_hint()}). "
                "Apply a model in gateway Admin. "
                "Tap ignores --tap-target, OPENAI_BASE_URL, and UPSTREAM_OPENAI_BASE_URL in this mode."
            )
        self._apply_runtime(runtime)
        log.info(
            "Upstream from PostgreSQL (%s): %s (model=%s %s)",
            self._scope_label(),
            runtime.base_model_url,
            runtime.model_id,
            runtime.model_name,
        )
        return runtime

    def reload_from_db(self) -> bool:
        runtime = self._fetch()
        if runtime is None:
            if self._runtime is None:
                log.error("PostgreSQL active LLM still missing for %s", self._scope_label())
            else:
                log.warning(
                    "PostgreSQL active LLM unavailable for %s; keeping %s",
                    self._scope_label(),
                    self._runtime.base_model_url,
                )
            return False
        previous = self._runtime.base_model_url if self._runtime else ""
        prev_key = self._runtime.api_key if self._runtime else ""
        prev_model = self._runtime.model_name if self._runtime else ""
        self._apply_runtime(runtime)
        changed = (
            runtime.base_model_url != previous
            or runtime.api_key != prev_key
            or runtime.model_name != prev_model
        )
        if changed:
            log.info(
                "Upstream from PostgreSQL (%s) -> %s (model=%s)",
                self._scope_label(),
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
