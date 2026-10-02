//! `gateway_model_usage` row building + usage normalization (§6.1 write-side). Author: kejiqing
//!
//! claw-tap records one row per LLM call into `gateway_model_usage` (`source=tap`) when the
//! request carried a non-empty `claw-turn-id`. `provider` is derived from the request URL path
//! (not the CLI client name); usage is normalized to Anthropic-style `input/output/cache_*`
//! columns following §6.1 rule 5.

use serde_json::Value;

/// Normalized usage in Anthropic column semantics (`input` excludes cached read). Author: kejiqing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NormalizedUsage {
    pub input: u32,
    pub output: u32,
    pub cache_create: u32,
    pub cache_read: u32,
}

fn u32_or(v: Option<&Value>) -> u32 {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Normalize upstream usage JSON to Anthropic-style columns.
///
/// Anthropic usage (`input_tokens`/`output_tokens`/`cache_*_input_tokens`) is taken directly.
/// OpenAI usage (`prompt_tokens`/`completion_tokens`/`prompt_tokens_details.cached_tokens`) is
/// split so `input = prompt_tokens - cached`, `cache_read = cached` (§6.1 rule 5). The
/// Anthropic field names win when present, so an OpenAI response that was already dual-named
/// by [`crate::sse::SseReassembler`] (`prompt_tokens -> input_tokens` but no `cache_read_input_tokens`)
/// still takes the OpenAI split path instead of double-counting the cached tokens.
pub fn normalize_usage(usage: &Value) -> NormalizedUsage {
    let Some(obj) = usage.as_object() else {
        return NormalizedUsage::default();
    };
    // Anthropic style when it carries Anthropic cache fields, or when it carries no OpenAI
    // `prompt_tokens`/`completion_tokens` at all (e.g. a partial `{output_tokens:7}` body).
    let has_anthropic_cache = obj.contains_key("cache_read_input_tokens")
        || obj.contains_key("cache_creation_input_tokens");
    let has_openai_fields = obj.contains_key("prompt_tokens") || obj.contains_key("completion_tokens");
    if has_anthropic_cache || !has_openai_fields {
        return NormalizedUsage {
            input: u32_or(obj.get("input_tokens")),
            output: u32_or(obj.get("output_tokens")),
            cache_create: u32_or(obj.get("cache_creation_input_tokens")),
            cache_read: u32_or(obj.get("cache_read_input_tokens")),
        };
    }
    // OpenAI style: `prompt_tokens` includes cached tokens.
    let prompt = u32_or(obj.get("prompt_tokens"));
    let completion = u32_or(obj.get("completion_tokens"));
    let cached = u32_or(
        obj.get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens")),
    );
    NormalizedUsage {
        input: prompt.saturating_sub(cached),
        output: completion,
        cache_create: 0,
        cache_read: cached,
    }
}

/// Provider for the `gateway_model_usage.provider` column (`anthropic` | `openai`).
/// Derived from the request URL path, not the CLI client name. Author: kejiqing
#[must_use]
pub fn provider_from_url(url: &str) -> &'static str {
    let path = normalize_url_path(url);
    if path_ends_with(&path, "/messages") {
        return "anthropic";
    }
    if path_ends_with(&path, "/responses") || path_ends_with(&path, "/chat/completions") {
        return "openai";
    }
    "openai"
}

fn normalize_url_path(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    let after_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .or_else(|| s.strip_prefix("HTTPS://"))
        .or_else(|| s.strip_prefix("HTTP://"))
        .unwrap_or(s);
    let path_and_more = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => return String::new(),
    };
    let path_only = path_and_more
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_more);
    let lower = path_only.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_slash = false;
    for ch in lower.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
            out.push('/');
        } else {
            prev_slash = false;
            out.push(ch);
        }
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

fn path_ends_with(path: &str, suffix: &str) -> bool {
    path == suffix || path.ends_with(suffix)
}

/// One row to insert into `gateway_model_usage`. `source` is fixed to `tap`. Author: kejiqing
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUsageRow {
    pub turn_id: String,
    pub model: String,
    pub base_url: String,
    pub provider: &'static str,
    pub usage: NormalizedUsage,
    pub latency_ms: u64,
}

/// Build a `gateway_model_usage` row from a single LLM call. Author: kejiqing
#[must_use]
pub fn build_model_usage_row(
    turn_id: &str,
    model: &str,
    base_url: &str,
    request_url: &str,
    usage: &Value,
    latency_ms: u64,
) -> ModelUsageRow {
    ModelUsageRow {
        turn_id: turn_id.trim().to_string(),
        model: model.trim().to_string(),
        base_url: base_url.trim().to_string(),
        provider: provider_from_url(request_url),
        usage: normalize_usage(usage),
        latency_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn n(input: u32, output: u32, cache_create: u32, cache_read: u32) -> NormalizedUsage {
        NormalizedUsage {
            input,
            output,
            cache_create,
            cache_read,
        }
    }

    #[test]
    fn normalize_anthropic_direct() {
        let u = json!({"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":2,"cache_read_input_tokens":3});
        assert_eq!(normalize_usage(&u), n(10, 5, 2, 3));
    }

    #[test]
    fn normalize_openai_split_cached() {
        let u = json!({"prompt_tokens":100,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":40}});
        assert_eq!(normalize_usage(&u), n(60, 30, 0, 40));
    }

    #[test]
    fn normalize_openai_no_cached() {
        let u = json!({"prompt_tokens":100,"completion_tokens":30});
        assert_eq!(normalize_usage(&u), n(100, 30, 0, 0));
    }

    #[test]
    fn normalize_openai_cached_gt_prompt_saturates() {
        let u = json!({"prompt_tokens":100,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":120}});
        assert_eq!(normalize_usage(&u), n(0, 30, 0, 120));
    }

    #[test]
    fn normalize_empty_is_zero() {
        assert_eq!(normalize_usage(&Value::Null), NormalizedUsage::default());
        assert_eq!(normalize_usage(&json!({})), NormalizedUsage::default());
    }

    #[test]
    fn normalize_non_object_is_zero() {
        assert_eq!(normalize_usage(&json!([])), NormalizedUsage::default());
        assert_eq!(normalize_usage(&json!("x")), NormalizedUsage::default());
    }

    #[test]
    fn normalize_missing_fields_zero() {
        let u = json!({"output_tokens":7});
        assert_eq!(normalize_usage(&u), n(0, 7, 0, 0));
    }

    #[test]
    fn normalize_anthropic_wins_when_both_cache_fields_present() {
        // Anthropic cache field names present → direct read wins, cached from cache_read_input_tokens.
        let u = json!({"input_tokens":10,"output_tokens":5,"prompt_tokens":999,"completion_tokens":999,
                       "cache_creation_input_tokens":2,"cache_read_input_tokens":3});
        assert_eq!(normalize_usage(&u), n(10, 5, 2, 3));
    }

    #[test]
    fn normalize_dual_named_openai_still_splits() {
        // SSE reassembler copies prompt_tokens -> input_tokens but keeps no cache_read_input_tokens.
        // Must NOT treat as Anthropic (would double-count cached as input).
        let u = json!({"input_tokens":100,"output_tokens":30,"prompt_tokens":100,"completion_tokens":30,
                       "prompt_tokens_details":{"cached_tokens":40}});
        assert_eq!(normalize_usage(&u), n(60, 30, 0, 40));
    }

    #[test]
    fn provider_messages_is_anthropic() {
        assert_eq!(
            provider_from_url("https://api.anthropic.com/v1/messages"),
            "anthropic"
        );
    }

    #[test]
    fn provider_responses_is_openai() {
        assert_eq!(
            provider_from_url("https://api.openai.com/v1/responses"),
            "openai"
        );
    }

    #[test]
    fn provider_chat_completions_is_openai() {
        assert_eq!(
            provider_from_url("https://x.example/v1/chat/completions"),
            "openai"
        );
    }

    #[test]
    fn provider_case_insensitive() {
        assert_eq!(
            provider_from_url("https://api.anthropic.com/v1/Messages"),
            "anthropic"
        );
    }

    #[test]
    fn provider_query_and_slash() {
        assert_eq!(
            provider_from_url("https://api.anthropic.com/v1/messages?foo=1"),
            "anthropic"
        );
        assert_eq!(
            provider_from_url("https://api.anthropic.com/v1/messages/"),
            "anthropic"
        );
    }

    #[test]
    fn provider_default_openai() {
        assert_eq!(provider_from_url(""), "openai");
        assert_eq!(provider_from_url("not-a-url"), "openai");
        assert_eq!(provider_from_url("https://host.only"), "openai");
    }

    #[test]
    fn row_source_and_field_mapping() {
        let usage = json!({"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":2,"cache_read_input_tokens":3});
        let row = build_model_usage_row(
            "  T_abc  ",
            "  my-model  ",
            " https://api.anthropic.com ",
            "https://api.anthropic.com/v1/messages",
            &usage,
            123,
        );
        assert_eq!(row.turn_id, "T_abc");
        assert_eq!(row.model, "my-model");
        assert_eq!(row.base_url, "https://api.anthropic.com");
        assert_eq!(row.provider, "anthropic");
        assert_eq!(row.usage, n(10, 5, 2, 3));
        assert_eq!(row.latency_ms, 123);
    }

    #[test]
    fn row_latency_ms_nonneg() {
        let row = build_model_usage_row("T_1", "m", "b", "https://x/v1/messages", &Value::Null, 0);
        assert_eq!(row.latency_ms, 0);
    }
}
