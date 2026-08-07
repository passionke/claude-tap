//! Embed git tag version at compile time. Author: kejiqing
//!
//! Priority: CLAUDE_TAP_GIT_VERSION env (Docker/CI) → `git describe --tags` → CARGO_PKG_VERSION.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CLAUDE_TAP_GIT_VERSION");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");

    let from_env = std::env::var("CLAUDE_TAP_GIT_VERSION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let version = from_env
        .or_else(git_describe_version)
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()));

    let normalized = normalize_version(&version);
    println!("cargo:rustc-env=CLAUDE_TAP_GIT_VERSION={normalized}");
}

fn git_describe_version() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Tag `v0.0.14` → `0.0.14` for clap/package display. Author: kejiqing
fn normalize_version(raw: &str) -> String {
    let s = raw.trim();
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix("release-v"))
        .unwrap_or(s)
        .to_string()
}
