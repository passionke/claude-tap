//! claw-session-id helpers. Author: kejiqing

use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const CLAW_SESSION_HEADER: &str = "claw-session-id";
pub const CLAW_TURN_HEADER: &str = "claw-turn-id";
const MAX_SLUG_LEN: usize = 48;

fn sanitize_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^a-zA-Z0-9._-]+").expect("regex"))
}

pub fn extract_claw_session_id(headers: &[(impl AsRef<str>, impl AsRef<str>)]) -> Option<String> {
    for (k, v) in headers {
        if k.as_ref().eq_ignore_ascii_case(CLAW_SESSION_HEADER) {
            let s = v.as_ref().trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn extract_from_map(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(CLAW_SESSION_HEADER) {
            let s = v.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn strip_claw_session_header(headers: &mut std::collections::HashMap<String, String>) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(CLAW_SESSION_HEADER));
}

/// Extract `claw-turn-id` from a generic slice of (key, value) pairs. Author: kejiqing
pub fn extract_claw_turn_id(headers: &[(impl AsRef<str>, impl AsRef<str>)]) -> Option<String> {
    for (k, v) in headers {
        if k.as_ref().eq_ignore_ascii_case(CLAW_TURN_HEADER) {
            let s = v.as_ref().trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Extract `claw-turn-id` from a header map (case-insensitive). Author: kejiqing
pub fn extract_turn_from_map(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(CLAW_TURN_HEADER) {
            let s = v.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Remove `claw-turn-id` (any casing) from a mutable header map before forwarding upstream.
pub fn strip_claw_turn_header(headers: &mut std::collections::HashMap<String, String>) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(CLAW_TURN_HEADER));
}

pub fn sanitize_filename_suffix(raw: &str) -> String {
    let compact = sanitize_re().replace_all(raw, "_");
    let trimmed = compact.trim_matches(|c| c == '.' || c == '_' || c == '-');
    let mut s = if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.to_string()
    };
    if s.len() > MAX_SLUG_LEN {
        let hash = hex::encode(Sha256::digest(raw.as_bytes()));
        let hash12 = &hash[..12];
        let head_len = (MAX_SLUG_LEN - 13).max(8);
        let head: String = s.chars().take(head_len).collect();
        s = format!("{head}_{hash12}");
    }
    s
}

/// POSIX-style relative path (forward slashes). Author: kejiqing
pub fn rel_posix(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_filename_suffix("abc-123"), "abc-123");
        assert_eq!(sanitize_filename_suffix("a/b c"), "a_b_c");
        assert_eq!(sanitize_filename_suffix("@@@"), "session");
    }

    #[test]
    fn sanitize_long_hashes() {
        let long = "a".repeat(80);
        let s = sanitize_filename_suffix(&long);
        assert!(s.len() <= MAX_SLUG_LEN);
        assert!(s.contains('_'));
    }

    #[test]
    fn extract_and_strip() {
        let headers = vec![("Claw-Session-Id", " sess-1 "), ("X-Other", "1")];
        assert_eq!(
            extract_claw_session_id(&headers).as_deref(),
            Some("sess-1")
        );
        let mut map = std::collections::HashMap::from([
            ("claw-session-id".into(), "x".into()),
            ("Authorization".into(), "y".into()),
        ]);
        strip_claw_session_header(&mut map);
        assert!(!map.contains_key("claw-session-id"));
        assert!(map.contains_key("Authorization"));
    }

    #[test]
    fn rel_posix_forward_slashes() {
        assert_eq!(
            rel_posix(std::path::Path::new(r"a\b\c")),
            "a/b/c"
        );
    }

    #[test]
    fn extract_claw_turn_id_basic() {
        let headers = vec![("claw-turn-id", "T_abc")];
        assert_eq!(extract_claw_turn_id(&headers).as_deref(), Some("T_abc"));
    }

    #[test]
    fn extract_claw_turn_id_case_insensitive() {
        let headers = vec![("CLAW-TURN-ID", "T_abc")];
        assert_eq!(extract_claw_turn_id(&headers).as_deref(), Some("T_abc"));
    }

    #[test]
    fn extract_claw_turn_id_trims() {
        let headers = vec![("claw-turn-id", "  T_abc  ")];
        assert_eq!(extract_claw_turn_id(&headers).as_deref(), Some("T_abc"));
    }

    #[test]
    fn extract_claw_turn_id_blank_is_none() {
        let headers = vec![("claw-turn-id", "   ")];
        assert!(extract_claw_turn_id(&headers).is_none());
    }

    #[test]
    fn extract_claw_turn_id_missing_is_none() {
        let headers = vec![("X-Other", "1")];
        assert!(extract_claw_turn_id(&headers).is_none());
    }

    #[test]
    fn extract_turn_from_map_basic() {
        let map = std::collections::HashMap::from([("claw-turn-id".into(), "T_abc".into())]);
        assert_eq!(extract_turn_from_map(&map).as_deref(), Some("T_abc"));
    }

    #[test]
    fn strip_claw_turn_header_removes_any_case() {
        let mut map = std::collections::HashMap::from([
            ("Claw-Turn-Id".into(), "T_abc".into()),
            ("Authorization".into(), "y".into()),
        ]);
        strip_claw_turn_header(&mut map);
        assert!(!map.keys().any(|k| k.eq_ignore_ascii_case("claw-turn-id")));
        assert!(map.contains_key("Authorization"));
    }

    #[test]
    fn strip_claw_turn_header_noop_when_absent() {
        let mut map = std::collections::HashMap::from([("Authorization".into(), "y".into())]);
        strip_claw_turn_header(&mut map);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("Authorization"));
    }
}
