//! Path allowlist for reverse proxy. Author: kejiqing

pub const ALLOWED_PATH_PREFIXES: &[&str] = &[
    "/v1/messages",
    "/v1/complete",
    "/v1/responses",
    "/v1/chat/completions",
    "/v1/completions",
    "/v1/models",
    "/v1/embeddings",
    "/responses",
    "/chat/completions",
    "/completions",
    "/models",
    "/embeddings",
];

/// Check whether the request path matches a known API endpoint.
pub fn is_allowed_path(path: &str) -> bool {
    let clean = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    ALLOWED_PATH_PREFIXES
        .iter()
        .any(|prefix| clean == *prefix || clean.starts_with(&format!("{prefix}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_paths() {
        for p in [
            "/v1/messages",
            "/v1/messages?stream=true",
            "/v1/complete",
            "/v1/responses",
            "/v1/chat/completions",
            "/v1/completions",
            "/v1/models",
            "/v1/models/claude-3",
            "/v1/embeddings",
            "/responses",
            "/chat/completions",
            "/completions",
            "/models",
            "/embeddings",
        ] {
            assert!(is_allowed_path(p), "expected allowed: {p}");
        }
    }

    #[test]
    fn blocked_paths() {
        for p in [
            "/etc/passwd",
            "/swagger/",
            "/swagger-ui.html",
            "/login.html",
            "/metrics",
            "/nacos/",
            "/nexus/",
            "/zabbix",
            "/vnc.html",
            "/",
            "/admin",
            "/wp-admin",
            "/.env",
            "/actuator/health",
            "/api/v1/hack",
        ] {
            assert!(!is_allowed_path(p), "expected blocked: {p}");
        }
    }
}
