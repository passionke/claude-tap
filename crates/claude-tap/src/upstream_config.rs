//! Upstream config file store. Author: kejiqing

use parking_lot::RwLock;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct UpstreamSnapshot {
    pub target: String,
    pub strip_path_prefix: String,
}

pub fn strip_path_prefix_for(client: &str, target: &str) -> String {
    if client == "codex" && !target.contains("api.openai.com") {
        "/v1".into()
    } else {
        String::new()
    }
}

pub struct UpstreamConfigStore {
    path: PathBuf,
    client: String,
    snapshot: RwLock<UpstreamSnapshot>,
    mtime_ns: RwLock<Option<u128>>,
}

impl UpstreamConfigStore {
    pub fn new(path: impl AsRef<Path>, client: &str) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            client: client.to_string(),
            snapshot: RwLock::new(UpstreamSnapshot::default()),
            mtime_ns: RwLock::new(None),
        }
    }

    pub fn snapshot(&self) -> UpstreamSnapshot {
        self.snapshot.read().clone()
    }

    pub fn reload_if_changed(&self) -> anyhow::Result<bool> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        let prev = *self.mtime_ns.read();
        if mtime == prev {
            return Ok(false);
        }
        *self.mtime_ns.write() = mtime;
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(_) => return Ok(false),
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Ok(false), // invalid: do not overwrite existing
        };
        let target = v
            .get("target")
            .or_else(|| v.get("target_url"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        if target.is_empty() {
            return Ok(false);
        }
        let strip = strip_path_prefix_for(&self.client, &target);
        *self.snapshot.write() = UpstreamSnapshot {
            target,
            strip_path_prefix: strip,
        };
        Ok(true)
    }
}

pub async fn poll_upstream_config(store: Arc<UpstreamConfigStore>, interval_secs: f64) {
    let interval = Duration::from_secs_f64(interval_secs.max(0.2));
    loop {
        tokio::time::sleep(interval).await;
        let _ = store.reload_if_changed();
    }
}

pub fn resolve_upstream(
    target_url: &str,
    strip_path_prefix: &str,
    file_store: Option<&UpstreamConfigStore>,
    gateway: Option<&crate::gateway_upstream::GatewayLlmUpstreamStore>,
) -> (String, String) {
    if let Some(g) = gateway {
        let (t, _) = g.target_and_key();
        return (t, String::new());
    }
    if let Some(s) = file_store {
        let snap = s.snapshot();
        if !snap.target.is_empty() {
            return (snap.target, snap.strip_path_prefix);
        }
    }
    (
        target_url.trim_end_matches('/').to_string(),
        strip_path_prefix.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn strip_prefix_codex_non_openai() {
        assert_eq!(
            strip_path_prefix_for("codex", "https://chatgpt.com/backend-api/codex"),
            "/v1"
        );
        assert_eq!(
            strip_path_prefix_for("codex", "https://api.openai.com"),
            ""
        );
        assert_eq!(strip_path_prefix_for("claude", "https://x"), "");
    }

    #[test]
    fn reload_valid_json() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"target":"https://api.example.com/"}}"#).unwrap();
        let store = UpstreamConfigStore::new(f.path(), "claude");
        assert!(store.reload_if_changed().unwrap());
        assert_eq!(store.snapshot().target, "https://api.example.com");
    }
}
