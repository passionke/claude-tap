//! TraceWriter — append JSONL records. Author: kejiqing

use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct TraceWriter {
    path: PathBuf,
    file: Option<File>,
    last_write: Instant,
}

impl TraceWriter {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            file: None,
            last_write: Instant::now(),
        })
    }

    fn ensure_open(&mut self) -> anyhow::Result<()> {
        if self.file.is_none() {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.file = Some(f);
        }
        Ok(())
    }

    pub fn append(&mut self, record: &Value) -> anyhow::Result<()> {
        self.ensure_open()?;
        let line = serde_json::to_string(record)?;
        if let Some(f) = self.file.as_mut() {
            writeln!(f, "{line}")?;
            f.flush()?;
        }
        self.last_write = Instant::now();
        Ok(())
    }

    pub fn release(&mut self) {
        self.file = None;
    }

    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    pub fn idle_seconds(&self) -> f64 {
        self.last_write.elapsed().as_secs_f64()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn append_jsonl() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let mut w = TraceWriter::new(&path).unwrap();
        w.append(&json!({"a":1})).unwrap();
        w.append(&json!({"b":2})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"a\":1"));
    }
}
