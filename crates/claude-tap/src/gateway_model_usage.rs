//! Record per-LLM-call token usage into gateway PG when `claw-turn-id` is present.
//! Missing turn header → no INSERT (proxy still succeeds). INSERT failure → warn only.
//! Author: kejiqing

use crate::gateway_upstream::GatewayLlmUpstreamStore;
use serde_json::Value;
use std::sync::Arc;

/// Normalized Anthropic-style counters for `gateway_model_usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NormalizedUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
}

/// Extract usage object from a (possibly nested) response body.
pub fn usage_object_from_body(body: &Value) -> Option<&Value> {
    if let Some(u) = body.get("usage") {
        if u.is_object() {
            return Some(u);
        }
    }
    // Responses API: sometimes nested under `response`
    if let Some(u) = body.pointer("/response/usage") {
        if u.is_object() {
            return Some(u);
        }
    }
    None
}

/// Map OpenAI or Anthropic usage JSON → DB columns.
///
/// OpenAI: `prompt_tokens` includes cached tokens; we split so Gateway can
/// recompute `prompt = input + cache_creation + cache_read` without double-counting.
pub fn normalize_usage(usage: &Value) -> Option<NormalizedUsage> {
    if !usage.is_object() {
        return None;
    }
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_i64)
        .or_else(|| usage.get("cache_read_input_tokens").and_then(Value::as_i64))
        .unwrap_or(0)
        .max(0) as i32;
    let cache_create = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as i32;

    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as i32;

    let input = if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_i64) {
        // OpenAI: prompt_tokens = non-cached + cached (+ sometimes creation billed separately).
        (prompt.max(0) as i32).saturating_sub(cached)
    } else if let Some(inp) = usage.get("input_tokens").and_then(Value::as_i64) {
        inp.max(0) as i32
    } else {
        return None;
    };

    Some(NormalizedUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cache_create,
        cache_read_input_tokens: cached,
    })
}

pub fn model_from_bodies(req_body: &Value, resp_body: &Value) -> Option<String> {
    resp_body
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| resp_body.pointer("/response/model").and_then(Value::as_str))
        .or_else(|| req_body.get("model").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Spawn non-blocking INSERT when turn_id is present. Never fails the proxy path.
pub fn maybe_spawn_insert(
    gateway: Option<Arc<GatewayLlmUpstreamStore>>,
    turn_id: Option<&str>,
    provider: Option<&str>,
    req_body: &Value,
    resp_body: &Value,
    latency_ms: i64,
) {
    let Some(turn_id) = turn_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(usage_val) = usage_object_from_body(resp_body) else {
        tracing::debug!("claw-turn-id present but response has no usage; skip gateway_model_usage");
        return;
    };
    let Some(norm) = normalize_usage(usage_val) else {
        tracing::debug!("claw-turn-id present but usage unparsable; skip gateway_model_usage");
        return;
    };
    let Some(model) = model_from_bodies(req_body, resp_body) else {
        tracing::warn!("claw-turn-id present but model missing; skip gateway_model_usage");
        return;
    };
    let Some(store) = gateway else {
        tracing::debug!("claw-turn-id present but no gateway PG store; skip gateway_model_usage");
        return;
    };
    let turn_id = turn_id.to_string();
    let provider = provider.map(str::to_string);
    tokio::spawn(async move {
        if let Err(e) = store
            .insert_model_usage(
                &turn_id,
                provider.as_deref(),
                &model,
                norm,
                Some(latency_ms),
                "tap",
            )
            .await
        {
            tracing::warn!("gateway_model_usage insert failed (turn={turn_id}): {e}");
        }
    });
}

impl GatewayLlmUpstreamStore {
    /// INSERT one row into `gateway_model_usage`. Author: kejiqing
    pub async fn insert_model_usage(
        &self,
        turn_id: &str,
        provider: Option<&str>,
        model: &str,
        usage: NormalizedUsage,
        latency_ms: Option<i64>,
        source: &str,
    ) -> anyhow::Result<()> {
        let (client, connection) =
            tokio_postgres::connect(self.database_url(), tokio_postgres::NoTls).await?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("postgres connection error: {e}");
            }
        });
        client
            .execute(
                r#"
                INSERT INTO gateway_model_usage (
                    turn_id, provider, model,
                    input_tokens, output_tokens,
                    cache_creation_input_tokens, cache_read_input_tokens,
                    latency_ms, source
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
                &[
                    &turn_id,
                    &provider,
                    &model,
                    &usage.input_tokens,
                    &usage.output_tokens,
                    &usage.cache_creation_input_tokens,
                    &usage.cache_read_input_tokens,
                    &latency_ms,
                    &source,
                ],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_openai_splits_cached() {
        let u = json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "prompt_tokens_details": { "cached_tokens": 40 }
        });
        let n = normalize_usage(&u).unwrap();
        assert_eq!(n.input_tokens, 60);
        assert_eq!(n.output_tokens, 20);
        assert_eq!(n.cache_read_input_tokens, 40);
        assert_eq!(n.cache_creation_input_tokens, 0);
    }

    #[test]
    fn normalize_anthropic() {
        let u = json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 3,
            "cache_creation_input_tokens": 2
        });
        let n = normalize_usage(&u).unwrap();
        assert_eq!(n.input_tokens, 10);
        assert_eq!(n.output_tokens, 5);
        assert_eq!(n.cache_read_input_tokens, 3);
        assert_eq!(n.cache_creation_input_tokens, 2);
    }
}
