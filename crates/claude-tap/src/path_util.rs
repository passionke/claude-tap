//! Path helpers. Author: kejiqing

/// Normalize external prefix path for live viewer JS.
pub fn normalize_live_prefix_path(prefix_path: &str) -> String {
    let raw = prefix_path.trim();
    if raw.is_empty() {
        return String::new();
    }
    let trimmed = raw.trim_end_matches('/');
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_prefix() {
        assert_eq!(normalize_live_prefix_path(""), "");
        assert_eq!(normalize_live_prefix_path("/foo/"), "/foo");
        assert_eq!(normalize_live_prefix_path("bar"), "/bar");
        assert_eq!(normalize_live_prefix_path("/x"), "/x");
    }
}
