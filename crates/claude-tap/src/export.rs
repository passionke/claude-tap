//! Export JSONL traces to markdown / json / html. Author: kejiqing

use crate::viewer_html::generate_html_viewer;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn export_main(argv: &[String]) -> i32 {
    let mut trace_file: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut format: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "-o" || a == "--output" {
            i += 1;
            if i < argv.len() {
                output = Some(PathBuf::from(&argv[i]));
            }
        } else if a == "--format" {
            i += 1;
            if i < argv.len() {
                format = Some(argv[i].clone());
            }
        } else if a.starts_with('-') {
            eprintln!("unknown export flag: {a}");
            return 1;
        } else if trace_file.is_none() {
            trace_file = Some(PathBuf::from(a));
        }
        i += 1;
    }
    let Some(trace_file) = trace_file else {
        eprintln!("Usage: claude-tap export <trace.jsonl> [-o out] [--format markdown|json|html]");
        return 1;
    };
    if !trace_file.exists() {
        eprintln!("Error: trace file not found: {}", trace_file.display());
        return 1;
    }
    let records = match read_records(&trace_file) {
        Ok(r) if !r.is_empty() => r,
        Ok(_) => {
            eprintln!("Error: no valid records found in trace file");
            return 1;
        }
        Err(e) => {
            eprintln!("Error reading trace: {e}");
            return 1;
        }
    };

    let mut fmt = format.unwrap_or_default();
    if fmt.is_empty() {
        fmt = if let Some(ref o) = output {
            match o.extension().and_then(|s| s.to_str()) {
                Some("json") => "json".into(),
                Some("html") | Some("htm") => "html".into(),
                _ => "markdown".into(),
            }
        } else {
            "markdown".into()
        };
    }

    if fmt == "html" {
        let html_path = output.unwrap_or_else(|| trace_file.with_extension("html"));
        if let Err(e) = generate_html_viewer(&trace_file, &html_path) {
            eprintln!("Error: failed to generate HTML viewer: {e}");
            return 1;
        }
        println!("Exported {} turns to {}", records.len(), html_path.display());
        return 0;
    }

    let body = if fmt == "json" {
        export_json(&records)
    } else {
        export_markdown(&records)
    };

    if let Some(path) = output {
        if let Err(e) = std::fs::write(&path, &body) {
            eprintln!("Error writing {}: {e}", path.display());
            return 1;
        }
        println!("Exported {} turns to {}", records.len(), path.display());
    } else {
        print!("{body}");
    }
    0
}

fn read_records(path: &Path) -> anyhow::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    let mut records = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            records.push(v);
        }
    }
    records.sort_by_key(|r| r.get("turn").and_then(|t| t.as_i64()).unwrap_or(0));
    Ok(records)
}

fn as_obj<'a>(v: &'a Value) -> Option<&'a serde_json::Map<String, Value>> {
    v.as_object()
}

fn request_body(r: &Value) -> Value {
    r.pointer("/request/body").cloned().unwrap_or(Value::Null)
}

fn response_body(r: &Value) -> Value {
    r.pointer("/response/body").cloned().unwrap_or(Value::Null)
}

fn usage_from(r: &Value) -> Value {
    response_body(r)
        .get("usage")
        .cloned()
        .unwrap_or(Value::Object(Default::default()))
}

pub fn export_markdown(records: &[Value]) -> String {
    let mut lines = vec!["# Claude Trace Export\n".to_string()];
    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut models = std::collections::BTreeSet::new();
    for r in records {
        let u = usage_from(r);
        total_in += u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        total_out += u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        if let Some(m) = request_body(r).get("model").and_then(|v| v.as_str()) {
            if !m.is_empty() {
                models.insert(m.to_string());
            }
        }
    }
    lines.push("## Summary\n".into());
    lines.push(format!("- **Turns**: {}", records.len()));
    let model_s = if models.is_empty() {
        "unknown".into()
    } else {
        models.into_iter().collect::<Vec<_>>().join(", ")
    };
    lines.push(format!("- **Models**: {model_s}"));
    lines.push(format!("- **Input tokens**: {total_in}"));
    lines.push(format!("- **Output tokens**: {total_out}"));
    lines.push(String::new());

    for r in records {
        let turn = r.get("turn").cloned().unwrap_or(Value::String("?".into()));
        let req = request_body(r);
        let resp = response_body(r);
        let model = req.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
        let duration = r.get("duration_ms").and_then(|v| v.as_i64()).unwrap_or(0);
        lines.push(format!("---\n\n## Turn {turn}\n"));
        lines.push(format!("**Model**: `{model}` | **Duration**: {duration}ms\n"));
        if let Some(msgs) = req.get("messages").and_then(|v| v.as_array()) {
            if let Some(last) = msgs.last().and_then(|v| v.as_object()) {
                let role = last.get("role").and_then(|v| v.as_str()).unwrap_or("unknown");
                lines.push(format!("### {}\n", capitalize(role)));
                match last.get("content") {
                    Some(Value::String(s)) => lines.push(format!("{s}\n")),
                    Some(other) => lines.push(format!(
                        "```json\n{}\n```\n",
                        serde_json::to_string_pretty(other).unwrap_or_default()
                    )),
                    None => {}
                }
            }
        }
        lines.push("### Assistant\n".into());
        if let Some(content) = resp.get("content") {
            lines.push(format!(
                "```json\n{}\n```\n",
                serde_json::to_string_pretty(content).unwrap_or_default()
            ));
        } else {
            lines.push(format!(
                "```json\n{}\n```\n",
                serde_json::to_string_pretty(&resp).unwrap_or_default()
            ));
        }
    }
    lines.join("\n")
}

pub fn export_json(records: &[Value]) -> String {
    serde_json::to_string_pretty(records).unwrap_or_else(|_| "[]".into())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn export_markdown_contains_turns() {
        let recs = vec![json!({
            "turn": 1,
            "duration_ms": 10,
            "request": {"body": {"model": "m", "messages": [{"role":"user","content":"hi"}]}},
            "response": {"body": {"content": [{"type":"text","text":"yo"}], "usage": {"input_tokens":1,"output_tokens":2}}}
        })];
        let md = export_markdown(&recs);
        assert!(md.contains("Turn 1"));
        assert!(md.contains("hi"));
    }

    #[test]
    fn export_main_json_file() {
        let dir = tempdir().unwrap();
        let trace = dir.path().join("t.jsonl");
        std::fs::write(
            &trace,
            r#"{"turn":1,"request":{"body":{}},"response":{"body":{}}}
"#,
        )
        .unwrap();
        let out = dir.path().join("o.json");
        let code = export_main(&[
            trace.to_string_lossy().into(),
            "-o".into(),
            out.to_string_lossy().into(),
            "--format".into(),
            "json".into(),
        ]);
        assert_eq!(code, 0);
        assert!(out.exists());
    }

    #[allow(dead_code)]
    fn _as_obj_used(v: &Value) {
        let _ = as_obj(v);
    }
}
