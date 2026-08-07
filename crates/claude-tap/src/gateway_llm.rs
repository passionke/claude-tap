//! Gateway LLM crypto + Xiaomi model map + legacy key helpers. Author: kejiqing

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Decrypt hex(nonce||ciphertext+tag) with key = SHA256(cluster_id).
pub fn decrypt_llm_api_key(cluster_id: &str, stored_hex: &str) -> Option<String> {
    let key = Sha256::digest(cluster_id.trim().as_bytes());
    let raw = hex::decode(stored_hex.trim()).ok()?;
    if raw.len() <= 12 {
        return None;
    }
    let (nonce_bytes, ciphertext) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plain = cipher.decrypt(nonce, ciphertext).ok()?;
    let s = String::from_utf8(plain).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Encrypt for tests / round-trip. Author: kejiqing
pub fn encrypt_llm_api_key(cluster_id: &str, plaintext: &str, nonce12: &[u8; 12]) -> String {
    let key = Sha256::digest(cluster_id.trim().as_bytes());
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key");
    let nonce = Nonce::from_slice(nonce12);
    let ct = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("encrypt");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(nonce12);
    out.extend_from_slice(&ct);
    hex::encode(out)
}

/// Slot key for legacy `llm_model_api_keys_json` (`{model_id}@{model_rev}`). Author: kejiqing
pub fn llm_api_key_slot(model_id: &str, model_rev: &str) -> String {
    format!("{model_id}@{model_rev}")
}

/// Lookup API key: slot `{model_id}@{model_rev}`, then `model_id`, then empty. Author: kejiqing
pub fn llm_api_key_for(api_keys: &HashMap<String, String>, model_id: &str, model_rev: &str) -> Option<String> {
    let slot = llm_api_key_slot(model_id, model_rev);
    for key in [slot.as_str(), model_id] {
        if let Some(val) = api_keys.get(key) {
            let t = val.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

pub fn normalize_upstream_base_url(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('/').to_string();
    if s.is_empty() || !(s.starts_with("http://") || s.starts_with("https://")) {
        return None;
    }
    Some(s)
}

pub fn normalize_model_name(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 256 {
        None
    } else {
        Some(s.to_string())
    }
}

pub fn normalize_model_name_for_upstream(raw: &str, upstream_base_url: &str) -> Option<String> {
    let model = normalize_model_name(raw)?;
    Some(map_xiaomi_mimo_model(upstream_base_url, &model))
}

pub fn map_xiaomi_mimo_model(host: &str, model_name: &str) -> String {
    if !host.to_ascii_lowercase().contains("xiaomimimo") {
        return model_name.to_string();
    }
    let mut bare = model_name.trim().to_string();
    if let Some(rest) = bare.strip_prefix("openai/") {
        bare = rest.to_string();
    }
    let lower = bare.to_ascii_lowercase().replace('_', "-");
    match lower.as_str() {
        "mimo-v2.5-pro" | "mimo-v2.5" | "mimo-v2-pro" => "mimo-v2.5-pro".into(),
        "mimo-v2.5-flash" => "mimo-v2.5-flash".into(),
        _ => bare,
    }
}

#[derive(Debug, Clone, Default)]
pub struct GatewayLlmRuntime {
    pub base_url: String,
    pub model_name: String,
    pub api_key: Option<String>,
}

impl GatewayLlmRuntime {
    pub fn is_ready(&self) -> bool {
        !self.base_url.trim().is_empty()
    }
}

/// Build runtime from revision fields (Python `_runtime_from_revision`). Author: kejiqing
pub fn runtime_from_revision(
    base_model_url: &str,
    model_name: &str,
    api_key: &str,
) -> Option<GatewayLlmRuntime> {
    let upstream = normalize_upstream_base_url(base_model_url)?;
    let norm_model = normalize_model_name_for_upstream(model_name, &upstream)?;
    let api_key = {
        let t = api_key.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    Some(GatewayLlmRuntime {
        base_url: upstream,
        model_name: norm_model,
        api_key,
    })
}

/// Parse legacy `llm_model_api_keys_json` value (object or JSON string). Author: kejiqing
pub fn parse_llm_api_keys_json(keys_v: &serde_json::Value) -> HashMap<String, String> {
    let obj = if let Some(s) = keys_v.as_str() {
        serde_json::from_str::<serde_json::Value>(s).ok()
    } else {
        Some(keys_v.clone())
    };
    match obj.and_then(|v| v.as_object().cloned()) {
        Some(map) => map
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        None => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aes_gcm_roundtrip() {
        let nonce = [1u8; 12];
        let hex = encrypt_llm_api_key("local-dev", "secret-key", &nonce);
        let plain = decrypt_llm_api_key("local-dev", &hex).unwrap();
        assert_eq!(plain, "secret-key");
        assert!(decrypt_llm_api_key("other", &hex).is_none());
    }

    #[test]
    fn short_ciphertext_none() {
        assert!(decrypt_llm_api_key("c", "abcd").is_none());
    }

    #[test]
    fn xiaomi_map() {
        assert_eq!(
            map_xiaomi_mimo_model("api.xiaomimimo.com", "openai/mimo_v2.5"),
            "mimo-v2.5-pro"
        );
        assert_eq!(
            map_xiaomi_mimo_model("api.openai.com", "gpt-4"),
            "gpt-4"
        );
    }

    #[test]
    fn llm_api_key_slot_format() {
        assert_eq!(llm_api_key_slot("m1", "r2"), "m1@r2");
    }

    #[test]
    fn legacy_key_lookup_prefers_slot_then_model_id() {
        let mut keys = HashMap::new();
        keys.insert("m1@r1".into(), "slot-key".into());
        keys.insert("m1".into(), "id-key".into());
        assert_eq!(
            llm_api_key_for(&keys, "m1", "r1").as_deref(),
            Some("slot-key")
        );

        let mut keys2 = HashMap::new();
        keys2.insert("m1".into(), "id-only".into());
        assert_eq!(
            llm_api_key_for(&keys2, "m1", "r9").as_deref(),
            Some("id-only")
        );

        let empty = HashMap::new();
        assert!(llm_api_key_for(&empty, "m1", "r1").is_none());
    }

    #[test]
    fn legacy_key_lookup_skips_blank() {
        let mut keys = HashMap::new();
        keys.insert("m1@r1".into(), "  ".into());
        keys.insert("m1".into(), "fallback".into());
        assert_eq!(
            llm_api_key_for(&keys, "m1", "r1").as_deref(),
            Some("fallback")
        );
    }

    #[test]
    fn parse_api_keys_from_string_or_object() {
        let as_obj = json!({"a@b": "k1", "a": "k2"});
        let map = parse_llm_api_keys_json(&as_obj);
        assert_eq!(map.get("a@b").map(String::as_str), Some("k1"));

        let as_str = json!("{\"x@y\":\"z\"}");
        let map2 = parse_llm_api_keys_json(&as_str);
        assert_eq!(map2.get("x@y").map(String::as_str), Some("z"));
    }

    #[test]
    fn runtime_from_revision_rejects_bad_url() {
        assert!(runtime_from_revision("not-a-url", "m", "k").is_none());
        let rt = runtime_from_revision("https://api.example.com/", "gpt", "k").unwrap();
        assert_eq!(rt.base_url, "https://api.example.com");
        assert_eq!(rt.api_key.as_deref(), Some("k"));
    }
}
