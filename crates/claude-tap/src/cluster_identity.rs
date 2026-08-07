//! Cluster identity for /healthz (matches http-gateway-rs). Author: kejiqing

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const CLUSTER_ID_ENV: &str = "CLAW_CLUSTER_ID";
pub const GATEWAY_DATABASE_URL_ENV: &str = "CLAW_GATEWAY_DATABASE_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgUrlParts {
    pub scheme: String,
    pub user: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterIdentity {
    pub cluster_id: String,
    pub db_host: String,
    pub cluster_hash: String,
}

fn cluster_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]+$").expect("regex"))
}

pub fn validate_cluster_id(cluster_id: &str) -> Result<(), String> {
    let cluster_id = cluster_id.trim();
    if cluster_id.is_empty() || cluster_id.len() > 64 {
        return Err(format!("{CLUSTER_ID_ENV} is required (max 64 chars)"));
    }
    if !cluster_id_re().is_match(cluster_id) {
        return Err(format!(
            "{CLUSTER_ID_ENV} must be alphanumeric, dash, or underscore"
        ));
    }
    Ok(())
}

pub fn gateway_cluster_id_from_env() -> Result<String, String> {
    let raw = std::env::var(CLUSTER_ID_ENV).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{CLUSTER_ID_ENV} is not set in deploy .env"));
    }
    validate_cluster_id(raw)?;
    Ok(raw.to_string())
}

pub fn gateway_database_url_from_env() -> Result<String, String> {
    let raw = std::env::var(GATEWAY_DATABASE_URL_ENV).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{GATEWAY_DATABASE_URL_ENV} is not set"));
    }
    Ok(raw.to_string())
}

pub fn claw_gateway_env_configured() -> bool {
    let a = std::env::var(CLUSTER_ID_ENV).unwrap_or_default();
    let b = std::env::var(GATEWAY_DATABASE_URL_ENV).unwrap_or_default();
    !a.trim().is_empty() && !b.trim().is_empty()
}

pub fn parse_pg_url(url: &str) -> Result<PgUrlParts, String> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| "database URL must include scheme".to_string())?;
    if scheme != "postgres" && scheme != "postgresql" {
        return Err(format!("unsupported database scheme: {scheme}"));
    }
    let (auth_host, path_part) = rest
        .split_once('/')
        .ok_or_else(|| "database URL missing dbname".to_string())?;
    let dbname = path_part.split('?').next().unwrap_or("").trim();
    if dbname.is_empty() {
        return Err("database URL missing dbname".into());
    }
    let (user_part, host_port) = auth_host
        .rsplit_once('@')
        .ok_or_else(|| "database URL missing user@host".to_string())?;
    let user = user_part.split(':').next().unwrap_or("").trim();
    if user.is_empty() {
        return Err("database URL missing user".into());
    }
    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        let port: u16 = p
            .parse()
            .map_err(|_| format!("invalid port in database URL: {p}"))?;
        (h, port)
    } else {
        (host_port, 5432)
    };
    if host.trim().is_empty() {
        return Err("database URL missing host".into());
    }
    Ok(PgUrlParts {
        scheme: scheme.to_string(),
        user: user.to_string(),
        host: host.to_string(),
        port,
        dbname: dbname.to_string(),
    })
}

pub fn compute_cluster_hash(cluster_id: &str, parts: &PgUrlParts) -> String {
    let payload = format!(
        "{}|{}|{}|{}",
        cluster_id.trim(),
        parts.scheme,
        parts.user,
        parts.dbname
    );
    let digest = hex::encode(Sha256::digest(payload.as_bytes()));
    format!("sha256:{digest}")
}

pub fn local_cluster_identity(cluster_id: &str, database_url: &str) -> Result<ClusterIdentity, String> {
    let cluster_id = cluster_id.trim();
    if cluster_id.is_empty() {
        return Err("clusterId is required".into());
    }
    let parts = parse_pg_url(database_url)?;
    Ok(ClusterIdentity {
        cluster_id: cluster_id.to_string(),
        db_host: parts.host.clone(),
        cluster_hash: compute_cluster_hash(cluster_id, &parts),
    })
}

pub fn health_json_body(identity: &ClusterIdentity, ok: bool) -> Value {
    json!({
        "ok": ok,
        "clusterId": identity.cluster_id,
        "clusterHash": identity.cluster_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_hash_stable() {
        let parts = parse_pg_url("postgres://claw_gateway:p@postgres:5432/claw_gateway").unwrap();
        assert_eq!(parts.scheme, "postgres");
        assert_eq!(parts.user, "claw_gateway");
        assert_eq!(parts.host, "postgres");
        assert_eq!(parts.port, 5432);
        assert_eq!(parts.dbname, "claw_gateway");
    }

    #[test]
    fn validate_cluster_id_format() {
        assert!(validate_cluster_id("local-dev").is_ok());
        assert!(validate_cluster_id("").is_err());
        assert!(validate_cluster_id(&"a".repeat(65)).is_err());
        assert!(validate_cluster_id("bad id").is_err());
    }

    #[test]
    fn local_cluster_identity_matches_gateway_example() {
        let id = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@postgres:5432/claw_gateway",
        )
        .unwrap();
        assert_eq!(
            id.cluster_hash,
            "sha256:448807110c7f7ee11bb629f7f9e360fca3ae9117a3bcb2a3f43d97b163acac2b"
        );
    }

    #[test]
    fn same_db_different_host_port_same_hash() {
        let a = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@postgres:5432/claw_gateway",
        )
        .unwrap();
        let b = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@127.0.0.1:5433/claw_gateway",
        )
        .unwrap();
        assert_eq!(a.cluster_hash, b.cluster_hash);
    }

    #[test]
    fn postgresql_scheme_accepted() {
        let parts = parse_pg_url("postgresql://u:p@h/db").unwrap();
        assert_eq!(parts.scheme, "postgresql");
        assert_eq!(parts.port, 5432);
    }

    #[test]
    fn health_json_omits_db_host() {
        let id = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@postgres:5432/claw_gateway",
        )
        .unwrap();
        let body = health_json_body(&id, true);
        assert!(body.get("dbHost").is_none());
        assert_eq!(body["ok"], true);
        assert_eq!(body["clusterId"], "local-dev");
    }
}
