//! CLI argument parsing (clap). Author: kejiqing

use crate::client_config::{client_config, ClientName};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProxyMode {
    Reverse,
    Forward,
}

impl ProxyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyMode::Reverse => "reverse",
            ProxyMode::Forward => "forward",
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "claude-tap",
    version = VERSION,
    about = "Trace AI CLI API requests via local reverse and forward proxies",
    disable_help_subcommand = true
)]
pub struct TapArgs {
    /// Proxy port (default: auto / 0)
    #[arg(long = "tap-port", default_value_t = 0)]
    pub port: u16,

    /// Bind address (default: 127.0.0.1, or 0.0.0.0 with --tap-no-launch)
    #[arg(long = "tap-host")]
    pub host: Option<String>,

    /// Client to launch / emulate
    #[arg(long = "tap-client", value_enum, default_value_t = ClientName::Claude)]
    pub client: ClientName,

    /// Upstream target base URL
    #[arg(long = "tap-target")]
    pub target: Option<String>,

    /// Proxy mode (defaults from client)
    #[arg(long = "tap-proxy-mode", value_enum)]
    pub proxy_mode: Option<ProxyMode>,

    /// Only start the proxy, don't launch client
    #[arg(long = "tap-no-launch", default_value_t = false)]
    pub no_launch: bool,

    /// Upstream JSON config file path
    #[arg(long = "tap-upstream-config")]
    pub upstream_config_file: Option<PathBuf>,

    /// Upstream config poll interval seconds
    #[arg(long = "tap-upstream-config-poll", default_value_t = 2.0)]
    pub upstream_config_poll: f64,

    /// Do not open browser for viewer
    #[arg(long = "tap-no-open", default_value_t = false)]
    pub no_open: bool,

    /// Enable live viewer server
    #[arg(long = "tap-live", default_value_t = false)]
    pub live: bool,

    /// Live viewer port (0 = auto)
    #[arg(long = "tap-live-port", default_value_t = 0)]
    pub live_port: u16,

    /// External path prefix for live viewer (e.g. e2b subpath)
    #[arg(long = "tap-live-prefix-path", default_value = "")]
    pub live_prefix_path: String,

    /// Trace output directory
    #[arg(long = "tap-output-dir", default_value = "./.traces")]
    pub output_dir: PathBuf,

    /// Max sessions to retain on disk (cleanup on exit)
    #[arg(long = "tap-max-traces", default_value_t = 50)]
    pub max_traces: usize,

    /// Deprecated: Live RAM session LRU removed; flag accepted for CLI compatibility
    #[arg(long = "tap-max-sessions", default_value_t = 1000)]
    pub max_sessions: usize,

    /// Disable update check (accepted for Docker compatibility)
    #[arg(long = "tap-no-update-check", default_value_t = false)]
    pub no_update_check: bool,

    /// Disable auto update (accepted for Docker compatibility)
    #[arg(long = "tap-no-auto-update", default_value_t = false)]
    pub no_auto_update: bool,

    /// Extra args forwarded to the launched client (after `--`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra_args: Vec<String>,
}

impl TapArgs {
    /// Resolve host / target / proxy_mode defaults like Python cli.parse_args.
    pub fn finalize(mut self) -> Self {
        if self.host.is_none() {
            self.host = Some(if self.no_launch {
                "0.0.0.0".to_string()
            } else {
                "127.0.0.1".to_string()
            });
        }
        if self.target.is_none() {
            self.target = Some(client_config(self.client).default_target.to_string());
        }
        if self.proxy_mode.is_none() {
            self.proxy_mode = Some(match client_config(self.client).default_proxy_mode {
                "forward" => ProxyMode::Forward,
                _ => ProxyMode::Reverse,
            });
        }
        self
    }

    pub fn resolved_host(&self) -> &str {
        self.host.as_deref().unwrap_or("127.0.0.1")
    }

    pub fn resolved_target(&self) -> &str {
        self.target.as_deref().unwrap_or("https://api.anthropic.com")
    }

    pub fn resolved_proxy_mode(&self) -> ProxyMode {
        self.proxy_mode.unwrap_or(ProxyMode::Reverse)
    }
}

/// Parse argv; strips a leading `dashboard` subcommand if present (compat).
pub fn parse_tap_args<I, S>(args: I) -> TapArgs
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    let mut raw: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    // Drop program name if present for FromArgMatches; clap::Parser::parse_from keeps it.
    if raw.len() > 1 && raw[1].to_string_lossy() == "dashboard" {
        raw.remove(1);
        // dashboard implies live
        let mut parsed = TapArgs::parse_from(raw);
        parsed.live = true;
        return parsed.finalize();
    }
    TapArgs::parse_from(raw).finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_claude_reverse() {
        let a = parse_tap_args(["claude-tap"]);
        assert_eq!(a.client, ClientName::Claude);
        assert_eq!(a.resolved_proxy_mode(), ProxyMode::Reverse);
        assert_eq!(a.resolved_target(), "https://api.anthropic.com");
        assert_eq!(a.resolved_host(), "127.0.0.1");
        assert!(!a.no_launch);
    }

    #[test]
    fn parse_no_launch_binds_all() {
        let a = parse_tap_args(["claude-tap", "--tap-no-launch"]);
        assert!(a.no_launch);
        assert_eq!(a.resolved_host(), "0.0.0.0");
    }

    #[test]
    fn parse_opencode_defaults_forward() {
        let a = parse_tap_args(["claude-tap", "--tap-client", "opencode"]);
        assert_eq!(a.resolved_proxy_mode(), ProxyMode::Forward);
    }

    #[test]
    fn parse_cursor_defaults_forward() {
        let a = parse_tap_args(["claude-tap", "--tap-client", "cursor"]);
        assert_eq!(a.resolved_proxy_mode(), ProxyMode::Forward);
    }

    #[test]
    fn parse_opencode_explicit_reverse() {
        let a = parse_tap_args([
            "claude-tap",
            "--tap-client",
            "opencode",
            "--tap-proxy-mode",
            "reverse",
        ]);
        assert_eq!(a.resolved_proxy_mode(), ProxyMode::Reverse);
    }

    #[test]
    fn parse_codex_default_target() {
        let a = parse_tap_args(["claude-tap", "--tap-client", "codex"]);
        assert_eq!(a.resolved_target(), "https://api.openai.com");
        assert_eq!(a.resolved_proxy_mode(), ProxyMode::Reverse);
    }

    #[test]
    fn parse_live_and_output_dir() {
        let a = parse_tap_args([
            "claude-tap",
            "--tap-live",
            "--tap-live-port",
            "3000",
            "--tap-output-dir",
            "/data/traces",
            "--tap-no-update-check",
            "--tap-no-auto-update",
        ]);
        assert!(a.live);
        assert_eq!(a.live_port, 3000);
        assert_eq!(a.output_dir, PathBuf::from("/data/traces"));
        assert!(a.no_update_check);
        assert!(a.no_auto_update);
    }

    #[test]
    fn parse_extra_args_after_double_dash() {
        let a = parse_tap_args(["claude-tap", "--", "--model", "x"]);
        assert_eq!(a.extra_args, vec!["--model".to_string(), "x".to_string()]);
    }
}
