"""Tests for clawTap cluster identity (byte-compatible with http-gateway-rs). Author: kejiqing"""

from claude_tap.cluster_identity import (
    compute_cluster_hash,
    health_json_body,
    local_cluster_identity,
    parse_pg_url,
    validate_cluster_id,
)


def test_parse_and_hash_stable():
    url = "postgres://claw_gateway:secret@postgres:5432/claw_gateway"
    parts = parse_pg_url(url)
    assert parts.host == "postgres"
    assert parts.port == 5432
    assert parts.dbname == "claw_gateway"
    assert parts.scheme == "postgres"
    assert parts.user == "claw_gateway"
    h1 = compute_cluster_hash("prod-01", parts)
    h2 = compute_cluster_hash("prod-01", parts)
    assert h1 == h2
    assert h1.startswith("sha256:")


def test_validate_cluster_id_format():
    validate_cluster_id("prod-claw-01")
    try:
        validate_cluster_id("")
        assert False, "expected error"
    except ValueError:
        pass
    try:
        validate_cluster_id("bad id")
        assert False, "expected error"
    except ValueError:
        pass


def test_local_cluster_identity_matches_gateway_example():
    identity = local_cluster_identity("prod-01", "postgres://claw_gateway:secret@postgres:5432/claw_gateway")
    assert identity.cluster_id == "prod-01"
    assert identity.db_host == "postgres"
    body = health_json_body(identity)
    assert body["ok"] is True
    assert body["clusterId"] == "prod-01"
    assert "dbHost" not in body
    assert body["clusterHash"] == identity.cluster_hash


def test_same_db_different_host_port_same_hash():
    """Host 127.0.0.1:5433 vs postgres:5432 — same clusterHash (gateway compose + host tap)."""
    cid = "local-dev"
    h1 = compute_cluster_hash(cid, parse_pg_url("postgres://claw_gateway:p@postgres:5432/claw_gateway"))
    h2 = compute_cluster_hash(cid, parse_pg_url("postgres://claw_gateway:p@127.0.0.1:5433/claw_gateway"))
    assert h1 == h2
    assert h1 == "sha256:448807110c7f7ee11bb629f7f9e360fca3ae9117a3bcb2a3f43d97b163acac2b"


def test_postgresql_scheme_accepted():
    parts = parse_pg_url("postgresql://u:p@db.example.com/mydb")
    assert parts.scheme == "postgresql"
    assert parts.port == 5432
