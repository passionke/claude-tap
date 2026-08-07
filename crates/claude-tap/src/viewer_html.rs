//! Embed JSONL into viewer.html (Python `_generate_html_viewer` parity). Author: kejiqing

use std::path::Path;

const VIEWER_TEMPLATE: &str = include_str!("../../../assets/viewer.html");
const LAZY_THRESHOLD: usize = 200;

pub fn generate_html_viewer(trace_path: &Path, html_path: &Path) -> anyhow::Result<()> {
    let text = if trace_path.exists() {
        std::fs::read_to_string(trace_path)?
    } else {
        String::new()
    };
    let mut records: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Escape </ so embedded JSON cannot break script tags (Python parity).
        records.push(line.replace("</", "<\\/"));
    }

    let jsonl_path_js = serde_json::to_string(&trace_path.canonicalize().unwrap_or_else(|_| trace_path.to_path_buf()))?;
    let html_path_js = serde_json::to_string(html_path)?;
    let version_js = serde_json::to_string(crate::VERSION)?;

    let inject = if records.len() > LAZY_THRESHOLD {
        // Lazy: embed as text/plain lines for progressive parse (simplified parity).
        let joined = records.join("\n");
        format!(
            r#"<script type="text/plain" id="trace-data-lazy">{joined}</script>
<script>
window.__CLAUDE_TAP_EMBEDDED__ = {{ lazy: true, jsonlPath: {jsonl_path_js}, htmlPath: {html_path_js}, version: {version_js} }};
</script>"#
        )
    } else {
        let arr = format!("[{}]", records.join(",\n"));
        format!(
            r#"<script>
window.__CLAUDE_TAP_EMBEDDED__ = {{ lazy: false, records: {arr}, jsonlPath: {jsonl_path_js}, htmlPath: {html_path_js}, version: {version_js} }};
</script>"#
        )
    };

    let mut html = VIEWER_TEMPLATE.to_string();
    let needle_marked = "<script>\n/* CLAUDETAP_LIVE_CONFIG */\nconst $ = s =>";
    let needle_legacy = "<script>\nconst $ = s =>";
    if let Some(pos) = html.find(needle_marked) {
        html.insert_str(pos, &inject);
    } else if let Some(pos) = html.find(needle_legacy) {
        html.insert_str(pos, &inject);
    } else if let Some(pos) = html.find("</head>") {
        html.insert_str(pos, &inject);
    } else {
        html.push_str(&inject);
    }

    if let Some(parent) = html_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(html_path, html)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generate_writes_html() {
        let dir = tempdir().unwrap();
        let trace = dir.path().join("t.jsonl");
        std::fs::write(&trace, r#"{"turn":1,"request":{},"response":{}}"#).unwrap();
        let html = dir.path().join("t.html");
        generate_html_viewer(&trace, &html).unwrap();
        let s = std::fs::read_to_string(&html).unwrap();
        assert!(s.contains("__CLAUDE_TAP_EMBEDDED__"));
        assert!(s.contains("turn"));
    }
}
