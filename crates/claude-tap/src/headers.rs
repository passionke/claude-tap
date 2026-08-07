//! Header filtering / redaction. Author: kejiqing

use std::collections::{HashMap, HashSet};

fn hop_by_hop() -> HashSet<&'static str> {
    [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailers",
        "transfer-encoding",
        "upgrade",
    ]
    .into_iter()
    .collect()
}

/// Filter hop-by-hop headers and optionally redact sensitive values.
pub fn filter_headers(headers: &HashMap<String, String>, redact_keys: bool) -> HashMap<String, String> {
    let hop = hop_by_hop();
    let mut out = HashMap::new();
    for (k, v) in headers {
        if hop.contains(k.to_ascii_lowercase().as_str()) {
            continue;
        }
        let lower = k.to_ascii_lowercase();
        if redact_keys && (lower == "x-api-key" || lower == "authorization") {
            let redacted = if v.len() > 12 {
                format!("{}...", &v[..12])
            } else {
                "***".to_string()
            };
            out.insert(k.clone(), redacted);
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

pub fn is_hop_by_hop(name: &str) -> bool {
    hop_by_hop().contains(name.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_hop_by_hop() {
        let mut h = HashMap::new();
        h.insert("Connection".into(), "keep-alive".into());
        h.insert("X-Custom".into(), "1".into());
        let out = filter_headers(&h, false);
        assert!(!out.contains_key("Connection"));
        assert_eq!(out.get("X-Custom").map(String::as_str), Some("1"));
    }

    #[test]
    fn redacts_auth() {
        let mut h = HashMap::new();
        h.insert("Authorization".into(), "Bearer secret-key-value".into());
        h.insert("x-api-key".into(), "short".into());
        let out = filter_headers(&h, true);
        assert_eq!(
            out.get("Authorization").map(String::as_str),
            Some("Bearer secre...")
        );
        assert_eq!(out.get("x-api-key").map(String::as_str), Some("***"));
    }
}
