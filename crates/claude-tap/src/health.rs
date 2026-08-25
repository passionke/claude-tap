//! /healthz handler helpers. Author: kejiqing

use crate::cluster_identity::{health_json_body, ClusterIdentity};
use crate::gateway_upstream::GatewayLlmUpstreamStore;
use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;
use std::sync::Arc;

pub fn healthz_response(
    identity: Option<&ClusterIdentity>,
    store: Option<&Arc<GatewayLlmUpstreamStore>>,
) -> (StatusCode, Json<Value>) {
    let Some(identity) = identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": "gateway identity not configured"})),
        );
    };
    let ok = store.map(|s| s.is_ready()).unwrap_or(true);
    // Always HTTP 200 with ok flag (matches Python health.py)
    (StatusCode::OK, Json(health_json_body(identity, ok)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster_identity::local_cluster_identity;
    use crate::gateway_llm::GatewayLlmRuntime;

    #[test]
    fn healthz_ok_false_still_200() {
        let id = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@postgres:5432/claw_gateway",
        )
        .unwrap();
        let store = Arc::new(GatewayLlmUpstreamStore::new(
            "local-dev".into(),
            "postgres://claw_gateway:p@postgres:5432/claw_gateway".into(),
            None,
        ));
        let (status, Json(body)) = healthz_response(Some(&id), Some(&store));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], false);
        assert!(body.get("dbHost").is_none());
    }

    #[test]
    fn healthz_ok_true() {
        let id = local_cluster_identity(
            "local-dev",
            "postgres://claw_gateway:p@postgres:5432/claw_gateway",
        )
        .unwrap();
        let store = Arc::new(GatewayLlmUpstreamStore::new(
            "local-dev".into(),
            "postgres://claw_gateway:p@postgres:5432/claw_gateway".into(),
            None,
        ));
        store.set_runtime_for_test(GatewayLlmRuntime {
            base_url: "https://x".into(),
            ..Default::default()
        });
        let (status, Json(body)) = healthz_response(Some(&id), Some(&store));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
    }
}
