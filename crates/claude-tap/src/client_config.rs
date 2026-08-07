//! Per-client launch / reverse-URL configuration. Author: kejiqing

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientName {
    Claude,
    Codex,
    Opencode,
    Cursor,
}

impl ClientName {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientName::Claude => "claude",
            ClientName::Codex => "codex",
            ClientName::Opencode => "opencode",
            ClientName::Cursor => "cursor",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClientConfig {
    pub cmd: &'static str,
    pub label: &'static str,
    pub install_url: &'static str,
    pub base_url_env: &'static str,
    pub base_url_suffix: &'static str,
    pub default_target: &'static str,
    pub nesting_env_keys: &'static [&'static str],
    pub default_proxy_mode: &'static str,
}

pub fn client_config(name: ClientName) -> ClientConfig {
    match name {
        ClientName::Claude => ClientConfig {
            cmd: "claude",
            label: "Claude Code",
            install_url: "https://docs.anthropic.com/en/docs/claude-code",
            base_url_env: "ANTHROPIC_BASE_URL",
            base_url_suffix: "",
            default_target: "https://api.anthropic.com",
            nesting_env_keys: &["CLAUDECODE", "CLAUDE_CODE_SSE_PORT"],
            default_proxy_mode: "reverse",
        },
        ClientName::Codex => ClientConfig {
            cmd: "codex",
            label: "Codex CLI",
            install_url: "https://github.com/openai/codex",
            base_url_env: "OPENAI_BASE_URL",
            base_url_suffix: "/v1",
            default_target: "https://api.openai.com",
            nesting_env_keys: &[],
            default_proxy_mode: "reverse",
        },
        ClientName::Opencode => ClientConfig {
            cmd: "opencode",
            label: "OpenCode",
            install_url: "https://opencode.ai/docs/",
            base_url_env: "ANTHROPIC_BASE_URL",
            base_url_suffix: "",
            default_target: "https://api.anthropic.com",
            nesting_env_keys: &[],
            default_proxy_mode: "forward",
        },
        ClientName::Cursor => ClientConfig {
            cmd: "cursor-agent",
            label: "Cursor CLI",
            install_url: "https://cursor.com/cli",
            base_url_env: "CURSOR_BASE_URL",
            base_url_suffix: "",
            default_target: "https://api2.cursor.sh",
            nesting_env_keys: &[],
            default_proxy_mode: "forward",
        },
    }
}

impl ClientConfig {
    pub fn reverse_base_url(&self, port: u16) -> String {
        format!("http://127.0.0.1:{}{}", port, self.base_url_suffix)
    }

    pub fn missing_help(&self) -> String {
        format!(
            "\nError: '{}' command not found in PATH.\nPlease install {} first: {}\n",
            self.cmd, self.label, self.install_url
        )
    }
}

/// Build process env map for launching a client. Author: kejiqing
pub fn build_client_env(
    client: ClientName,
    port: u16,
    proxy_mode: &str,
    ca_cert_path: Option<&std::path::Path>,
    extra_args: &[String],
) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let cfg = client_config(client);
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let mut cmd_args = extra_args.to_vec();

    for k in cfg.nesting_env_keys {
        env.remove(*k);
    }

    if proxy_mode == "forward" {
        let proxy_url = format!("http://127.0.0.1:{port}");
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            env.insert(key.to_string(), proxy_url.clone());
        }
        extend_no_proxy(&mut env, &["localhost", "127.0.0.1", "::1"]);
        if let Some(ca) = ca_cert_path {
            let ca_s = ca.to_string_lossy().to_string();
            env.insert("NODE_EXTRA_CA_CERTS".into(), ca_s.clone());
            env.insert("SSL_CERT_FILE".into(), ca_s.clone());
            env.insert("CODEX_CA_CERTIFICATE".into(), ca_s);
        }
        if client == ClientName::Claude {
            let has_settings = cmd_args
                .iter()
                .any(|a| a == "--settings" || a.starts_with("--settings="));
            if !has_settings {
                let mut settings = serde_json::json!({
                    "env": {
                        "HTTP_PROXY": proxy_url,
                        "HTTPS_PROXY": proxy_url,
                        "ALL_PROXY": proxy_url,
                        "http_proxy": proxy_url,
                        "https_proxy": proxy_url,
                        "all_proxy": proxy_url,
                    }
                });
                if let Some(ca) = ca_cert_path {
                    settings["env"]["NODE_EXTRA_CA_CERTS"] =
                        serde_json::Value::String(ca.to_string_lossy().to_string());
                }
                let payload = serde_json::to_string(&settings).unwrap_or_default();
                cmd_args.insert(0, payload);
                cmd_args.insert(0, "--settings".into());
            }
        }
    } else {
        let base_url = cfg.reverse_base_url(port);
        env.insert(cfg.base_url_env.to_string(), base_url);
        env.insert("NO_PROXY".into(), "127.0.0.1".into());
    }

    (env, cmd_args)
}

fn extend_no_proxy(env: &mut std::collections::HashMap<String, String>, hosts: &[&str]) {
    let mut parts: Vec<String> = env
        .get("NO_PROXY")
        .or_else(|| env.get("no_proxy"))
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    for h in hosts {
        if !parts.iter().any(|p| p.eq_ignore_ascii_case(h)) {
            parts.push((*h).to_string());
        }
    }
    let joined = parts.join(",");
    env.insert("NO_PROXY".into(), joined.clone());
    env.insert("no_proxy".into(), joined);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_env_sets_base_url() {
        let (env, _) = build_client_env(ClientName::Codex, 8080, "reverse", None, &[]);
        assert_eq!(
            env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:8080/v1")
        );
    }

    #[test]
    fn forward_env_sets_proxy_and_ca() {
        let ca = std::path::Path::new("/tmp/ca.pem");
        let (env, _) = build_client_env(ClientName::Codex, 9090, "forward", Some(ca), &[]);
        assert_eq!(
            env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://127.0.0.1:9090")
        );
        assert_eq!(
            env.get("SSL_CERT_FILE").map(String::as_str),
            Some("/tmp/ca.pem")
        );
        assert_eq!(
            env.get("CODEX_CA_CERTIFICATE").map(String::as_str),
            Some("/tmp/ca.pem")
        );
    }

    #[test]
    fn clients_registered() {
        assert_eq!(client_config(ClientName::Claude).cmd, "claude");
        assert_eq!(client_config(ClientName::Cursor).cmd, "cursor-agent");
        assert_eq!(client_config(ClientName::Opencode).default_proxy_mode, "forward");
    }
}
