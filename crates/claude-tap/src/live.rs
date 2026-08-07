//! Live viewer — disk is source of truth; SSE is push-only (no RAM replay).
//! Stream chunks live on disk as JSONL `response.sse_events` / `ws_events`.
//! List/poll APIs and Live push strip chunk bodies and keep counts only;
//! the viewer Ajax-loads one turn's chunks on SSE section expand.
//! Author: kejiqing

use crate::path_util::normalize_live_prefix_path;
use crate::session_index::SessionIndex;
use crate::VERSION;
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

    pub fn broadcast(&self, mut record: Value) {
        // Disk already has full chunks; Live push must not ship them to the browser.
        strip_stream_events_keep_counts(&mut record);
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

#[derive(Debug, Deserialize)]
pub struct StreamEventsQuery {
    pub session: Option<String>,
    pub claw_session_id: Option<String>,
    pub turn: Option<i64>,
}

fn require_session(q_session: Option<&str>, q_claw: Option<&str>) -> Result<String, Response> {
    let raw = q_session.or(q_claw).unwrap_or("").trim();
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
    percent_decode(s)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
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

/// Replace bulky stream arrays with counts for list/poll/push payloads. Author: kejiqing
fn strip_stream_events_keep_counts(rec: &mut Value) {
    let Some(resp) = rec.get_mut("response").and_then(|v| v.as_object_mut()) else {
        return;
    };
    if let Some(Value::Array(arr)) = resp.remove("sse_events") {
        resp.insert("sse_event_count".into(), json!(arr.len() as u64));
    }
    if let Some(Value::Array(arr)) = resp.remove("ws_events") {
        resp.insert("ws_event_count".into(), json!(arr.len() as u64));
    }
}

fn strip_stream_events_hard(rec: &mut Value) {
    if let Some(resp) = rec.get_mut("response").and_then(|v| v.as_object_mut()) {
        resp.remove("sse_events");
        resp.remove("ws_events");
        resp.remove("sse_event_count");
        resp.remove("ws_event_count");
    }
}

pub async fn handle_index(State(state): State<LiveState>) -> Html<String> {
    // Inject LIVE_MODE + prefix so Live leaves the offline drop-zone and
    // loads sessions from disk APIs. Author: kejiqing
    let mut html = VIEWER_HTML.to_string();
    let prefix_js =
        serde_json::to_string(&state.prefix_path).unwrap_or_else(|_| "\"\"".into());
    let version_js = serde_json::to_string(VERSION).unwrap_or_else(|_| "\"\"".into());
    let live_js = format!(
        "const LIVE_MODE = true;\n\
         const LIVE_PREFIX_PATH = {prefix_js};\n\
         const __CLAUDE_TAP_VERSION__ = {version_js};\n\
         const EMBEDDED_TRACE_DATA = [];\n\
         const __TRACE_JSONL_PATH__ = \"\";\n\
         const __TRACE_HTML_PATH__ = \"\";\n"
    );
    let marker = "/* CLAUDETAP_LIVE_CONFIG */\n";
    if let Some(pos) = html.find(marker) {
        html.replace_range(pos..pos + marker.len(), &live_js);
    } else if let Some(pos) = html.find("<script>\nconst $ = s =>") {
        html.insert_str(pos + "<script>\n".len(), &live_js);
    }
    Html(html)
}

pub async fn handle_sse(
    State(state): State<LiveState>,
    Query(q): Query<SessionQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let session = require_session(q.session.as_deref(), q.claw_session_id.as_deref())?;
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
    let session = match require_session(q.session.as_deref(), q.claw_session_id.as_deref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let since = q.since_turn.unwrap_or(0).max(0);
    // Strip chunk bodies; viewer Ajax-loads on expand. Author: kejiqing
    Json(load_session_records(&state, &session, since, LoadKind::Traces)).into_response()
}

pub async fn handle_api_session_full(
    State(state): State<LiveState>,
    Query(q): Query<SessionQuery>,
) -> Response {
    let session = match require_session(q.session.as_deref(), q.claw_session_id.as_deref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let since = q.since_turn.unwrap_or(0).max(0);
    Json(load_session_records(&state, &session, since, LoadKind::FullClean)).into_response()
}

pub async fn handle_api_session_stream_events(
    State(state): State<LiveState>,
    Query(q): Query<StreamEventsQuery>,
) -> Response {
    let session = match require_session(q.session.as_deref(), q.claw_session_id.as_deref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let turn = q.turn.unwrap_or(0);
    if turn <= 0 {
        return (StatusCode::BAD_REQUEST, "turn query parameter is required").into_response();
    }
    Json(load_stream_events_for_turn(&state, &session, turn)).into_response()
}

#[derive(Clone, Copy)]
enum LoadKind {
    /// List/poll: drop chunk arrays, keep counts for badges.
    Traces,
    /// Export JSON: drop stream fields entirely.
    FullClean,
}

fn load_session_records(
    state: &LiveState,
    claw_session_id: &str,
    since_turn: i64,
    kind: LoadKind,
) -> Vec<Value> {
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
        match kind {
            LoadKind::Traces => strip_stream_events_keep_counts(&mut rec),
            LoadKind::FullClean => strip_stream_events_hard(&mut rec),
        }
        records.push(rec);
    }
    records
}

fn load_stream_events_for_turn(state: &LiveState, claw_session_id: &str, turn: i64) -> Value {
    let Some(row) = state.session_index.get_session(claw_session_id).ok().flatten() else {
        return json!({ "turn": turn, "sse_events": [], "ws_events": [] });
    };
    let path = state.output_dir.join(&row.jsonl_relpath);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({ "turn": turn, "sse_events": [], "ws_events": [] });
    };
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let t = rec.get("turn").and_then(|v| v.as_i64()).unwrap_or(0);
        if t != turn {
            continue;
        }
        let resp = rec.get("response").cloned().unwrap_or(json!({}));
        let sse = resp.get("sse_events").cloned().unwrap_or(json!([]));
        let ws = resp.get("ws_events").cloned().unwrap_or(json!([]));
        return json!({
            "turn": turn,
            "claw_session_id": claw_session_id,
            "sse_events": sse,
            "ws_events": ws,
        });
    }
    json!({ "turn": turn, "sse_events": [], "ws_events": [] })
}

pub fn live_router(state: LiveState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/", get(handle_index))
        .route("/events", get(handle_sse))
        .route("/api/sessions", get(handle_api_sessions))
        .route("/api/sessions/traces", get(handle_api_session_traces))
        .route("/api/sessions/full", get(handle_api_session_full))
        .route(
            "/api/sessions/stream-events",
            get(handle_api_session_stream_events),
        )
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
        let all = load_session_records(&live, "s1", 0, LoadKind::Traces);
        assert_eq!(all.len(), 2);
        let since = load_session_records(&live, "s1", 1, LoadKind::Traces);
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
    async fn index_injects_live_mode() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let live = LiveState::new(dir.path(), idx, "/tap-live/");
        let Html(html) = handle_index(State(live)).await;
        assert!(
            html.contains("const LIVE_MODE = true;"),
            "live index must enable LIVE_MODE (otherwise drop-zone whiteboard)"
        );
        assert!(html.contains("const LIVE_PREFIX_PATH = \"/tap-live\""));
        assert!(html.contains("const __CLAUDE_TAP_VERSION__"));
        assert!(!html.contains("/* CLAUDETAP_LIVE_CONFIG */"));
        // Raw template must not be served unchanged.
        assert_ne!(html.len(), VIEWER_HTML.len());
    }

    #[tokio::test]
    async fn broadcast_only_after_subscribe() {
        let dir = tempdir().unwrap();
        let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
        let live = LiveState::new(dir.path(), idx, "");
        let mut rx = live.subscribe("s1");
        live.broadcast(json!({
            "claw_session_id":"s1",
            "x":1,
            "response": {
                "sse_events": [{"event":"a","data":{}}],
                "body": {}
            }
        }));
        let got = rx.recv().await.unwrap();
        assert_eq!(got["x"], 1);
        assert!(got["response"].get("sse_events").is_none());
        assert_eq!(got["response"]["sse_event_count"], 1);
    }

    #[test]
    fn traces_strip_sse_events_keep_count() {
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
        let traces = load_session_records(&live, "s1", 0, LoadKind::Traces);
        assert_eq!(traces.len(), 1);
        assert!(traces[0]["response"].get("sse_events").is_none());
        assert_eq!(traces[0]["response"]["sse_event_count"], 1);
        let full = load_session_records(&live, "s1", 0, LoadKind::FullClean);
        assert!(full[0]["response"].get("sse_events").is_none());
        assert!(full[0]["response"].get("sse_event_count").is_none());
        let ev = load_stream_events_for_turn(&live, "s1", t1);
        assert_eq!(ev["sse_events"].as_array().unwrap().len(), 1);
    }
}
