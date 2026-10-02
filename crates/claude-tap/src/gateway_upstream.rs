//! Gateway upstream store + auth header rewrite. Author: kejiqing

use crate::client_config::ClientName;
use crate::gateway_llm::{
    decrypt_llm_api_key, llm_api_key_for, parse_llm_api_keys_json, runtime_from_revision,
    GatewayLlmRuntime,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_POLL_SECS: f64 = 30.0;
pub const POLL_INTERVAL_ENV: &str = "CLAW_GATEWAY_LLM_CONFIG_POLL_INTERVAL_SECS";

pub fn gateway_llm_poll_interval_seconds() -> f64 {
    let raw = std::env::var(POLL_INTERVAL_ENV).unwrap_or_default();
    let v: f64 = raw.trim().parse().unwrap_or(DEFAULT_POLL_SECS);
    if v > 0.0 {
        v
    } else {
        DEFAULT_POLL_SECS
    }
}

/// Apply DB-managed API key to outbound headers.
pub fn apply_gateway_auth_headers(
    headers: &mut HashMap<String, String>,
    client: ClientName,
    api_key: Option<&str>,
) {
    let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) else {
        return;
    };
    headers.retain(|k, _| {
        let l = k.to_ascii_lowercase();
        l != "x-api-key" && l != "authorization"
    });
    if client == ClientName::Claude {
        headers.insert("x-api-key".into(), key.to_string());
    } else {
        headers.insert("Authorization".into(), format!("Bearer {key}"));
    }
}

pub struct GatewayLlmUpstreamStore {
    cluster_id: String,
    database_url: String,
    runtime: RwLock<GatewayLlmRuntime>,
}

impl GatewayLlmUpstreamStore {
    pub fn new(cluster_id: String, database_url: String) -> Self {
        Self {
            cluster_id,
            database_url,
            runtime: RwLock::new(GatewayLlmRuntime::default()),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.runtime.read().is_ready()
    }

    pub fn snapshot(&self) -> GatewayLlmRuntime {
        self.runtime.read().clone()
    }

    pub fn set_runtime_for_test(&self, rt: GatewayLlmRuntime) {
        *self.runtime.write() = rt;
    }

    /// Load active model from PG. On miss after initial load, keeps previous runtime.
    pub async fn reload_from_db(&self) -> anyhow::Result<bool> {
        match load_active_runtime(&self.database_url, &self.cluster_id).await {
            Ok(Some(rt)) => {
                *self.runtime.write() = rt;
                Ok(true)
            }
            Ok(None) => {
                // keep previous
                Ok(self.is_ready())
            }
            Err(e) => {
                if self.is_ready() {
                    tracing::warn!("gateway LLM reload failed (keeping previous): {e}");
                    Ok(true)
                } else {
                    Err(e)
                }
            }
        }
    }

    pub fn target_and_key(&self) -> (String, Option<String>) {
        let rt = self.runtime.read();
        (rt.base_url.clone(), rt.api_key.clone())
    }

    /// Record one LLM call into `gateway_model_usage` (`source=tap`). Author: kejiqing
    ///
    /// §6.1: only called when the request carried a non-empty `claw-turn-id`; a missing turn
    /// is a no-op. Opens a fresh PG connection per call (matches the reload path); failures are
    /// returned to the caller which only warns — they must never block the proxy response.
    pub async fn insert_model_usage(
        &self,
        row: &crate::model_usage::ModelUsageRow,
    ) -> anyhow::Result<()> {
        if row.turn_id.is_empty() {
            return Ok(());
        }
        let (client, connection) =
            tokio_postgres::connect(&self.database_url, tokio_postgres::NoTls).await?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("postgres connection error: {e}");
            }
        });
        let turn_id = row.turn_id.as_str();
        let provider = row.provider;
        let model = row.model.as_str();
        let base_url = row.base_url.as_str();
        let input: i32 = i32::try_from(row.usage.input).unwrap_or(i32::MAX);
        let output: i32 = i32::try_from(row.usage.output).unwrap_or(i32::MAX);
        let cache_create: i32 = i32::try_from(row.usage.cache_create).unwrap_or(i32::MAX);
        let cache_read: i32 = i32::try_from(row.usage.cache_read).unwrap_or(i32::MAX);
        let latency: i64 = i64::try_from(row.latency_ms).unwrap_or(i64::MAX);
        client
            .execute(
                r#"INSERT INTO gateway_model_usage
                     (turn_id, provider, model, base_url, input_tokens, output_tokens,
                      cache_creation_input_tokens, cache_read_input_tokens, latency_ms, source)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'tap')"#,
                &[
                    &turn_id,
                    &provider,
                    &model,
                    &base_url,
                    &input,
                    &output,
                    &cache_create,
                    &cache_read,
                    &latency,
                ],
            )
            .await?;
        Ok(())
    }
}

async fn load_active_runtime(
    database_url: &str,
    cluster_id: &str,
) -> anyhow::Result<Option<GatewayLlmRuntime>> {
    let (client, connection) = tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection error: {e}");
        }
    });

    let cluster_id = cluster_id.trim();
    if cluster_id.is_empty() {
        return Ok(None);
    }

    // Cluster state first; missing/empty → legacy singleton (Python parity). Author: kejiqing
    let state = client
        .query_opt(
            r#"
            SELECT active_model_id, active_model_rev
              FROM gateway_llm_cluster_state
             WHERE cluster_id = $1
            "#,
            &[&cluster_id],
        )
        .await?;

    let (active_id, active_rev) = match state {
        Some(row) => {
            let id: String = row.get::<_, Option<String>>(0).unwrap_or_default();
            let rev: String = row.get::<_, Option<String>>(1).unwrap_or_default();
            (id.trim().to_string(), rev.trim().to_string())
        }
        None => {
            return Ok(load_active_llm_runtime_legacy(&client).await?);
        }
    };

    if active_id.is_empty() || active_rev.is_empty() {
        return Ok(load_active_llm_runtime_legacy(&client).await?);
    }

    let rev_row = client
        .query_opt(
            r#"
            SELECT base_model_url, model_name
              FROM gateway_llm_cluster_revision
             WHERE cluster_id = $1 AND model_id = $2 AND model_rev = $3
            "#,
            &[&cluster_id, &active_id, &active_rev],
        )
        .await?;

    let Some(rev_row) = rev_row else {
        tracing::warn!(
            "Missing gateway_llm_cluster_revision for cluster={cluster_id} model={active_id} rev={active_rev}"
        );
        return Ok(None);
    };

    let rev_base: String = rev_row.get::<_, Option<String>>(0).unwrap_or_default();
    let rev_name: String = rev_row.get::<_, Option<String>>(1).unwrap_or_default();

    let model_row = client
        .query_opt(
            r#"
            SELECT api_key_ciphertext, base_model_url, model_name
              FROM gateway_llm_cluster_model
             WHERE cluster_id = $1 AND model_id = $2
            "#,
            &[&cluster_id, &active_id],
        )
        .await?;

    let (api_key, base_url, model_name) = if let Some(m) = model_row {
        let ciphertext: Option<String> = m.get(0);
        let api_key = ciphertext
            .as_deref()
            .and_then(|c| decrypt_llm_api_key(cluster_id, c))
            .unwrap_or_default();
        let model_base: String = m.get::<_, Option<String>>(1).unwrap_or_default();
        let model_name_fb: String = m.get::<_, Option<String>>(2).unwrap_or_default();
        let base_url = if !rev_base.is_empty() {
            rev_base
        } else {
            model_base
        };
        let model_name = if !rev_name.is_empty() {
            rev_name
        } else {
            model_name_fb
        };
        (api_key, base_url, model_name)
    } else {
        (String::new(), rev_base, rev_name)
    };

    Ok(runtime_from_revision(&base_url, &model_name, &api_key))
}

/// Pre-cluster schema fallback (singleton `gateway_global_settings`).
/// Python `_load_active_llm_runtime_legacy` parity. Author: kejiqing
async fn load_active_llm_runtime_legacy(
    client: &tokio_postgres::Client,
) -> anyhow::Result<Option<GatewayLlmRuntime>> {
    let row = client
        .query_opt(
            r#"
            SELECT llm_models_json::text, llm_model_api_keys_json::text, active_llm_model_id,
                   active_llm_model_rev
              FROM gateway_global_settings
             WHERE singleton_id = 1
            "#,
            &[],
        )
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let _models_v = cell_as_json(&row, 0);
    let keys_v = cell_as_json(&row, 1);
    let active_id: String = row
        .try_get::<_, Option<String>>(2)
        .ok()
        .flatten()
        .unwrap_or_default();
    let active_rev: String = row
        .try_get::<_, Option<String>>(3)
        .ok()
        .flatten()
        .unwrap_or_default();
    let active_id = active_id.trim().to_string();
    let active_rev = active_rev.trim().to_string();
    if active_id.is_empty() || active_rev.is_empty() {
        return Ok(None);
    }

    let _ = _models_v;
    let api_keys = parse_llm_api_keys_json(keys_v.as_ref().unwrap_or(&serde_json::Value::Null));

    let rev_row = client
        .query_opt(
            r#"
            SELECT base_model_url, model_name
              FROM gateway_llm_model_revision
             WHERE model_id = $1 AND model_rev = $2
            "#,
            &[&active_id, &active_rev],
        )
        .await?;

    let Some(rev_row) = rev_row else {
        return Ok(None);
    };

    let base_url: String = rev_row.get::<_, Option<String>>(0).unwrap_or_default();
    let model_name: String = rev_row.get::<_, Option<String>>(1).unwrap_or_default();
    let api_key = llm_api_key_for(&api_keys, &active_id, &active_rev).unwrap_or_default();

    Ok(runtime_from_revision(&base_url, &model_name, &api_key))
}

/// Read a JSON cell returned as text (via `::text` cast). Author: kejiqing
fn cell_as_json(row: &tokio_postgres::Row, idx: usize) -> Option<serde_json::Value> {
    if let Ok(Some(s)) = row.try_get::<_, Option<String>>(idx) {
        return serde_json::from_str(&s)
            .ok()
            .or(Some(serde_json::Value::String(s)));
    }
    None
}

pub async fn poll_gateway_llm_upstream(store: Arc<GatewayLlmUpstreamStore>, interval_secs: f64) {
    let interval = Duration::from_secs_f64(interval_secs.max(0.2));
    loop {
        tokio::time::sleep(interval).await;
        if let Err(e) = store.reload_from_db().await {
            tracing::warn!("gateway poll error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_llm::{llm_api_key_for, llm_api_key_slot};

    #[test]
    fn auth_codex_replaces_bearer() {
        let mut h = HashMap::from([("Authorization".into(), "Bearer client-key".into())]);
        apply_gateway_auth_headers(&mut h, ClientName::Codex, Some("db-managed-key"));
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some("Bearer db-managed-key")
        );
        assert!(!h.keys().any(|k| k.eq_ignore_ascii_case("x-api-key")));
    }

    #[test]
    fn auth_claude_uses_x_api_key() {
        let mut h = HashMap::from([("x-api-key".into(), "client".into())]);
        apply_gateway_auth_headers(&mut h, ClientName::Claude, Some("db-key"));
        assert_eq!(h.get("x-api-key").map(String::as_str), Some("db-key"));
        assert!(!h.keys().any(|k| k.eq_ignore_ascii_case("authorization")));
    }

    #[test]
    fn empty_key_keeps_client() {
        let mut h = HashMap::from([("Authorization".into(), "Bearer client".into())]);
        apply_gateway_auth_headers(&mut h, ClientName::Codex, Some(""));
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some("Bearer client")
        );
    }

    #[test]
    fn both_headers_codex_only_authorization() {
        let mut h = HashMap::from([
            ("Authorization".into(), "Bearer c".into()),
            ("x-api-key".into(), "c2".into()),
        ]);
        apply_gateway_auth_headers(&mut h, ClientName::Codex, Some("db-key"));
        assert_eq!(h.len(), 1);
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some("Bearer db-key")
        );
    }

    #[test]
    fn store_ready_after_set() {
        let s = GatewayLlmUpstreamStore::new("c".into(), "postgres://u:p@h/db".into());
        assert!(!s.is_ready());
        s.set_runtime_for_test(GatewayLlmRuntime {
            base_url: "https://api.example.com".into(),
            model_name: "m".into(),
            api_key: Some("k".into()),
        });
        assert!(s.is_ready());
    }

    #[test]
    fn legacy_helpers_slot_and_lookup() {
        // Re-export coverage from gateway_upstream tests (no real PG). Author: kejiqing
        assert_eq!(llm_api_key_slot("mid", "rev"), "mid@rev");
        let mut keys = HashMap::new();
        keys.insert("mid@rev".into(), "from-slot".into());
        assert_eq!(
            llm_api_key_for(&keys, "mid", "rev").as_deref(),
            Some("from-slot")
        );
    }

    #[tokio::test]
    async fn insert_model_usage_requires_turn_id() {
        // Empty turn_id must short-circuit before opening any PG connection. Author: kejiqing
        let s = GatewayLlmUpstreamStore::new("c".into(), "postgres://u:p@invalid/db".into());
        let row = crate::model_usage::build_model_usage_row(
            "   ",
            "m",
            "https://x",
            "https://x/v1/messages",
            &serde_json::Value::Null,
            0,
        );
        assert!(s.insert_model_usage(&row).await.is_ok());
    }
}
