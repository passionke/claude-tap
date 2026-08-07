//! SessionTraceDispatcher — per-session writers + Live broadcast hook. Author: kejiqing

use crate::claw_session::sanitize_filename_suffix;
use crate::session_index::{jsonl_relpath_for_slug, SessionIndex};
use crate::trace::TraceWriter;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

pub const DEFAULT_WRITER_IDLE_SECONDS: f64 = 3600.0;

pub type BroadcastFn = Arc<dyn Fn(Value) + Send + Sync>;

pub struct SessionTraceDispatcher {
    index: Arc<SessionIndex>,
    writers: Mutex<HashMap<String, TraceWriter>>,
    slug_by_raw: Mutex<HashMap<String, String>>,
    turn_state: Mutex<HashMap<String, i64>>,
    broadcast: Mutex<Option<BroadcastFn>>,
    idle_seconds: f64,
}

impl SessionTraceDispatcher {
    pub fn new(index: Arc<SessionIndex>) -> Self {
        Self {
            index,
            writers: Mutex::new(HashMap::new()),
            slug_by_raw: Mutex::new(HashMap::new()),
            turn_state: Mutex::new(HashMap::new()),
            broadcast: Mutex::new(None),
            idle_seconds: DEFAULT_WRITER_IDLE_SECONDS,
        }
    }

    pub fn set_broadcast(&self, f: BroadcastFn) {
        *self.broadcast.lock() = Some(f);
    }

    pub fn index(&self) -> &Arc<SessionIndex> {
        &self.index
    }

    fn resolve_slug(&self, claw_session_id: &str) -> String {
        let mut map = self.slug_by_raw.lock();
        if let Some(s) = map.get(claw_session_id) {
            return s.clone();
        }
        let base = sanitize_filename_suffix(claw_session_id);
        // Collision: same slug different raw → append hash
        let slug = if map.values().any(|v| v == &base)
            && !map
                .iter()
                .any(|(k, v)| k == claw_session_id && v == &base)
        {
            let h = hex::encode(Sha256::digest(claw_session_id.as_bytes()));
            format!("{base}_{}", &h[..8])
        } else {
            base
        };
        map.insert(claw_session_id.to_string(), slug.clone());
        slug
    }

    pub fn alloc_turn(&self, claw_session_id: &str) -> anyhow::Result<i64> {
        let slug = self.resolve_slug(claw_session_id);
        let rel = jsonl_relpath_for_slug(&slug);
        self.index
            .upsert_session_row(claw_session_id, &slug, &rel)?;
        let mut turns = self.turn_state.lock();
        let entry = turns.entry(claw_session_id.to_string()).or_insert_with(|| {
            self.index.get_last_turn(claw_session_id).unwrap_or(0)
        });
        *entry += 1;
        let turn = *entry;
        self.index.record_write(claw_session_id, turn)?;
        Ok(turn)
    }

    pub fn write(&self, claw_session_id: &str, mut record: Value) -> anyhow::Result<()> {
        if let Some(obj) = record.as_object_mut() {
            obj.insert("claw_session_id".into(), json!(claw_session_id));
        }
        let slug = self.resolve_slug(claw_session_id);
        let rel = jsonl_relpath_for_slug(&slug);
        self.index
            .upsert_session_row(claw_session_id, &slug, &rel)?;
        {
            let mut writers = self.writers.lock();
            let path = self.index.output_dir().join(&rel);
            let writer = writers
                .entry(claw_session_id.to_string())
                .or_insert_with(|| TraceWriter::new(&path).expect("create writer"));
            writer.append(&record)?;
        }
        let turn = record
            .get("turn")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        self.index.record_write(claw_session_id, turn)?;
        if let Some(f) = self.broadcast.lock().as_ref() {
            f(record);
        }
        self.reclaim_idle_writers();
        Ok(())
    }

    pub fn reclaim_idle_writers(&self) {
        let mut writers = self.writers.lock();
        for w in writers.values_mut() {
            if w.is_open() && w.idle_seconds() >= self.idle_seconds {
                w.release();
            }
        }
    }

    pub fn writer_count(&self) -> usize {
        self.writers.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn alloc_turn_and_write() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let d = SessionTraceDispatcher::new(idx);
        let t1 = d.alloc_turn("sess-a").unwrap();
        let t2 = d.alloc_turn("sess-a").unwrap();
        assert_eq!(t1, 1);
        assert_eq!(t2, 2);
        d.write("sess-a", json!({"turn": t2, "x": 1})).unwrap();
        let path = dir.path().join("sessions").join(
            sanitize_filename_suffix("sess-a"),
        ).join("trace.jsonl");
        assert!(path.exists());
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("claw_session_id"));
    }
}
