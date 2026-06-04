"""PostgreSQL cluster identity for clawTap /healthz (matches http-gateway-rs). Author: kejiqing"""

from __future__ import annotations

import hashlib
import os
import re
from dataclasses import dataclass

CLUSTER_ID_ENV = "CLAW_CLUSTER_ID"
GATEWAY_DATABASE_URL_ENV = "CLAW_GATEWAY_DATABASE_URL"

_CLUSTER_ID_RE = re.compile(r"^[A-Za-z0-9_-]+$")


@dataclass(frozen=True)
class PgUrlParts:
    scheme: str
    user: str
    host: str
    port: int
    dbname: str


@dataclass(frozen=True)
class ClusterIdentity:
    cluster_id: str
    db_host: str
    cluster_hash: str


def validate_cluster_id(cluster_id: str) -> None:
    cluster_id = cluster_id.strip()
    if not cluster_id or len(cluster_id) > 64:
        raise ValueError(f"{CLUSTER_ID_ENV} is required (max 64 chars)")
    if not _CLUSTER_ID_RE.fullmatch(cluster_id):
        raise ValueError(f"{CLUSTER_ID_ENV} must be alphanumeric, dash, or underscore")


def gateway_cluster_id_from_env() -> str:
    raw = os.environ.get(CLUSTER_ID_ENV, "").strip()
    if not raw:
        raise ValueError(f"{CLUSTER_ID_ENV} is not set in deploy .env")
    validate_cluster_id(raw)
    return raw


def gateway_database_url_from_env() -> str:
    raw = os.environ.get(GATEWAY_DATABASE_URL_ENV, "").strip()
    if not raw:
        raise ValueError(f"{GATEWAY_DATABASE_URL_ENV} is not set")
    return raw


def claw_gateway_env_configured() -> bool:
    return bool(os.environ.get(CLUSTER_ID_ENV, "").strip() and os.environ.get(GATEWAY_DATABASE_URL_ENV, "").strip())


def parse_pg_url(url: str) -> PgUrlParts:
    """Parse postgres URL (password ignored). Matches gateway ``parse_pg_url``."""
    trimmed = url.strip()
    if "://" not in trimmed:
        raise ValueError("database URL must include scheme")
    scheme, rest = trimmed.split("://", 1)
    if scheme not in ("postgres", "postgresql"):
        raise ValueError(f"unsupported database scheme: {scheme}")

    auth_host, _, path_part = rest.partition("/")
    if not path_part:
        raise ValueError("database URL missing dbname")
    dbname = path_part.split("?", 1)[0].strip()
    if not dbname:
        raise ValueError("database URL missing dbname")

    if "@" not in auth_host:
        raise ValueError("database URL missing user@host")
    user_part, host_port = auth_host.rsplit("@", 1)
    user = user_part.split(":", 1)[0].strip()
    if not user:
        raise ValueError("database URL missing user")

    if ":" in host_port:
        host, port_s = host_port.rsplit(":", 1)
        try:
            port = int(port_s)
        except ValueError as exc:
            raise ValueError(f"invalid port in database URL: {port_s}") from exc
    else:
        host, port = host_port, 5432

    if not host.strip():
        raise ValueError("database URL missing host")

    return PgUrlParts(
        scheme=scheme,
        user=user,
        host=host,
        port=port,
        dbname=dbname,
    )


def compute_cluster_hash(cluster_id: str, parts: PgUrlParts) -> str:
    """clusterId + scheme + user + dbname only (no host/port). Matches http-gateway-rs."""
    payload = f"{cluster_id.strip()}|{parts.scheme}|{parts.user}|{parts.dbname}"
    digest = hashlib.sha256(payload.encode("utf-8")).hexdigest()
    return f"sha256:{digest}"


def local_cluster_identity(cluster_id: str, database_url: str) -> ClusterIdentity:
    cluster_id = cluster_id.strip()
    if not cluster_id:
        raise ValueError("clusterId is required")
    parts = parse_pg_url(database_url)
    return ClusterIdentity(
        cluster_id=cluster_id,
        db_host=parts.host,
        cluster_hash=compute_cluster_hash(cluster_id, parts),
    )


def health_json_body(identity: ClusterIdentity, *, ok: bool = True) -> dict[str, object]:
    """Public /healthz — omits dbHost (PG host not exposed). Author: kejiqing"""
    return {
        "ok": ok,
        "clusterId": identity.cluster_id,
        "clusterHash": identity.cluster_hash,
    }
