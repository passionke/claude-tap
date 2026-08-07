//! Live viewer — disk is source of truth; SSE is push-only (no RAM replay).
//! SSE/stream chunks live on disk as JSONL `response.sse_events` (written by proxy
//! record path); Live may broadcast each completed record once but never retains a
//! deque of chunks in RAM. History is loaded on demand via `/api/sessions/traces`.
//! Author: kejiqing

use crate::path_util::normalize_live_prefix_path;
use crate::session_index::SessionIndex;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use async_stream::stream;
use futures_util::stream::Stream;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;

const VIEWER_HTML: &str = include_str!("../../../assets/viewer.html");

#[derive(Clone)]
pub struct LiveState {
    pub output_dir: std::path::PathBuf,
    pub session_index: Arc<SessionIndex>,
    pub prefix_path: String,
    /// Per-session broadcast of new records (no history buffer).
    tx: Arc<Mutex<HashMap<String, broadcast::Sender<Value>>>>,
}

impl LiveState {
    pub fn new(
        output_dir: impl Into<std::path::PathBuf>,
        session_index: Arc<SessionIndex>,
        prefix_path: &str,
    ) -> Self {
        Self {
            output_dir: output_dir.into(),
            session_index,
            prefix_path: normalize_live_prefix_path(prefix_path),
            tx: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Invariant: no per-session RAM record buffers.
    pub fn has_session_ram_buffer(&self) -> bool {
        false
    }

    pub fn broadcast(&self, record: Value) {
        let Some(sid) = record
            .get("claw_session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return;
        };
        let mut map = self.tx.lock();
        let sender = map.entry(sid).or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            tx
        });
        let _ = sender.send(record);
    }

    fn subscribe(&self, session: &str) -> broadcast::Receiver<Value> {
        let mut map = self.tx.lock();
        let sender = map.entry(session.to_string()).or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            tx
        });
        sender.subscribe()
    }

    pub fn active_sse_channel_count(&self) -> usize {
        self.tx.lock().len()
    }
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session: Option<String>,
    pub claw_session_id: Option<String>,
    pub since_turn: Option<i64>,
}

fn require_session(q: &SessionQuery) -> Result<String, Response> {
    let raw = q
        .session
        .as_deref()
        .or(q.claw_session_id.as_deref())
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "session or claw_session_id query parameter is required",
        )
            .into_response());
    }
    Ok(urlencoding_decode(raw))
}

fn urlencoding_decode(s: &str) -> String {
    // minimal: percent-decode via simple replace of common cases; use form_urlencoded
    percent_decode(s)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                from_hex(bytes[i + 1]),
                from_hex(bytes[i + 2]),
            ) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub async fn handle_index(State(state): State<LiveState>) -> Html<String> {
    let mut html = VIEWER_HTML.to_string();
    if !state.prefix_path.is_empty() {
        let inject = format!(
            "<script>window.__CLAUDE_TAP_LIVE_PREFIX__={};</script>",
            serde_json::to_string(&state.prefix_path).unwrap_or_else(|_| "\"\"".into())
        );
        if let Some(pos) = html.find("</head>") {
            html.insert_str(pos, &inject);
        }
    }
    // Hint for file-first UX (viewer may already support load-from-API)
    Html(html)
}

pub async fn handle_sse(
    State(state): State<LiveState>,
    Query(q): Query<SessionQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let session = require_session(&q)?;
    let mut rx = state.subscribe(&session);
    // No RAM replay on connect — only future records.
    let event_stream = stream! {
        loop {
            match rx.recv().await {
                Ok(record) => {
                    let data = serde_json::to_string(&record).unwrap_or_default();
                    yield Ok(Event::default().data(data));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()))
}

pub async fn handle_api_sessions(
    State(state): State<LiveState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let offset: i64 = q
        .get("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
        .max(0);
    match state.session_index.list_sessions(limit, offset) {
        Ok((rows, total)) => {
            let sessions: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "claw_session_id": r.claw_session_id,
                        "storage_slug": r.storage_slug,
                        "jsonl_relpath": r.jsonl_relpath,
                        "created_at": r.created_at,
                        "updated_at": r.updated_at,
                        "first_calendar_date": r.first_calendar_date,
                        "last_calendar_date": r.last_calendar_date,
                        "last_turn": r.last_turn,
                    })
                })
                .collect();
            Json(json!({"sessions": sessions, "total": total, "limit": limit, "offset": offset}))
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn handle_api_session_traces(
    State(state): State<LiveState>,
    Query(q): Query<SessionQuery>,
) -> Response {
    let session = match require_session(&q) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let since = q.since_turn.unwrap_or(0).max(0);
    Json(load_session_records(&state, &session, since, false)).into_response()
}

pub async fn handle_api_session_full(
    State(state): State<LiveState>,
    Query(q): Query<SessionQuery>,
) -> Response {
    let session = match require_session(&q) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let since = q.since_turn.unwrap_or(0).max(0);
    Json(load_session_records(&state, &session, since, true)).into_response()
}

fn load_session_records(state: &LiveState, claw_session_id: &str, since_turn: i64, strip_stream: bool) -> Vec<Value> {
    // Disk JSONL includes sse_events (unless strip_stream for /full). Author: kejiqing
    let Some(row) = state.session_index.get_session(claw_session_id).ok().flatten() else {
        return vec![];
    };
    let path = state.output_dir.join(&row.jsonl_relpath);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    let mut records = Vec::new();
    for line in text.lines() {
        let Ok(mut rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let turn = rec.get("turn").and_then(|v| v.as_i64()).unwrap_or(0);
        if turn <= since_turn {
            continue;
        }
        if strip_stream {
            if let Some(resp) = rec.get_mut("response").and_then(|v| v.as_object_mut()) {
                resp.remove("sse_events");
                resp.remove("ws_events");
            }
        }
        records.push(rec);
    }
    records
}

pub fn live_router(state: LiveState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/", get(handle_index))
        .route("/events", get(handle_sse))
        .route("/api/sessions", get(handle_api_sessions))
        .route("/api/sessions/traces", get(handle_api_session_traces))
        .route("/api/sessions/full", get(handle_api_session_full))
        .with_state(state)
}

/// Ensure content-type for SSE is set (axum Sse handles it).
#[allow(dead_code)]
fn sse_headers() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-cache")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_dispatcher::SessionTraceDispatcher;
    use tempfile::tempdir;

    #[test]
    fn no_ram_buffer_invariant() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let live = LiveState::new(dir.path(), idx, "");
        assert!(!live.has_session_ram_buffer());
    }

    #[test]
    fn load_from_disk_since_turn() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let disp = SessionTraceDispatcher::new(idx.clone());
        let t1 = disp.alloc_turn("s1").unwrap();
        disp.write("s1", json!({"turn": t1, "n": 1})).unwrap();
        let t2 = disp.alloc_turn("s1").unwrap();
        disp.write("s1", json!({"turn": t2, "n": 2})).unwrap();
        let live = LiveState::new(dir.path(), idx, "");
        let all = load_session_records(&live, "s1", 0, false);
        assert_eq!(all.len(), 2);
        let since = load_session_records(&live, "s1", 1, false);
        assert_eq!(since.len(), 1);
        assert_eq!(since[0]["n"], 2);
    }

    #[test]
    fn normalize_prefix_on_state() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let live = LiveState::new(dir.path(), idx, "foo/");
        assert_eq!(live.prefix_path, "/foo");
    }

    #[tokio::test]
    async fn broadcast_only_after_subscribe() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let live = LiveState::new(dir.path(), idx, "");
        let mut rx = live.subscribe("s1");
        live.broadcast(json!({"claw_session_id":"s1","x":1}));
        let got = rx.recv().await.unwrap();
        assert_eq!(got["x"], 1);
    }

    #[test]
    fn disk_traces_keep_sse_events() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let disp = SessionTraceDispatcher::new(idx.clone());
        let t1 = disp.alloc_turn("s1").unwrap();
        disp.write(
            "s1",
            json!({
                "turn": t1,
                "response": {
                    "status": 200,
                    "body": {"id":"m"},
                    "sse_events": [{"event":"message_delta","data":{"t":"hi"}}]
                }
            }),
        )
        .unwrap();
        let live = LiveState::new(dir.path(), idx, "");
        let traces = load_session_records(&live, "s1", 0, false);
        assert_eq!(traces.len(), 1);
        assert!(traces[0]["response"]["sse_events"].is_array());
        let full = load_session_records(&live, "s1", 0, true);
        assert!(full[0]["response"].get("sse_events").is_none());
    }
}
