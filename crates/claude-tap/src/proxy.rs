//! Reverse HTTP/SSE/WS proxy. Author: kejiqing

use crate::allowlist::is_allowed_path;
use crate::claw_session::{
    extract_from_map, extract_turn_from_map, strip_claw_session_header, strip_claw_turn_header,
};
use crate::client_config::ClientName;
use crate::gateway_upstream::{apply_gateway_auth_headers, GatewayLlmUpstreamStore};
use crate::headers::{filter_headers, is_hop_by_hop};
use crate::session_dispatcher::SessionTraceDispatcher;
use crate::sse::SseReassembler;
use crate::upstream_config::{resolve_upstream, UpstreamConfigStore};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone)]
pub struct ProxyState {
    pub client: ClientName,
    pub target_url: String,
    pub strip_path_prefix: String,
    pub http: reqwest::Client,
    pub dispatcher: Arc<SessionTraceDispatcher>,
    pub upstream_file: Option<Arc<UpstreamConfigStore>>,
    pub gateway: Option<Arc<GatewayLlmUpstreamStore>>,
}

/// Catch-all reverse dispatch: WebSocket Upgrade → WS proxy; else HTTP/SSE proxy.
/// Works for any allowlisted path (not only `/v1/responses`). Author: kejiqing
pub async fn dispatch_reverse(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Response {
    let is_ws = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_ws {
        let (mut parts, body) = req.into_parts();
        match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
            Ok(ws) => {
                let req = Request::from_parts(parts, body);
                return ws_proxy_handler(ws, State(state), req).await;
            }
            Err(rejection) => return rejection.into_response(),
        }
    }

    proxy_handler(State(state), req).await
}

pub async fn proxy_handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| path.clone());

    if path == "/healthz" {
        // Handled by separate route normally; fallback
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    if !is_allowed_path(&path_and_query) && !is_allowed_path(&path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    if let Some(g) = &state.gateway {
        if !g.is_ready() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "claw-tap: no active LLM in PostgreSQL for this cluster",
            )
                .into_response();
        }
    }

    let method = req.method().clone();
    let headers_in = headers_to_map(req.headers());
    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad body").into_response(),
    };

    let claw = extract_from_map(&headers_in);
    let claw_turn = extract_turn_from_map(&headers_in);
    let mut fwd_headers = headers_in.clone();
    strip_claw_session_header(&mut fwd_headers);
    strip_claw_turn_header(&mut fwd_headers);

    let (target, strip) = resolve_upstream(
        &state.target_url,
        &state.strip_path_prefix,
        state.upstream_file.as_deref(),
        state.gateway.as_deref(),
    );

    let mut fwd_path = path_and_query.clone();
    if !strip.is_empty() && fwd_path.starts_with(&strip) {
        fwd_path = fwd_path[strip.len()..].to_string();
        if fwd_path.is_empty() {
            fwd_path = "/".into();
        }
    }
    let upstream_url = format!(
        "{}/{}",
        target.trim_end_matches('/'),
        fwd_path.trim_start_matches('/')
    );

    if let Some(g) = &state.gateway {
        let (_, key) = g.target_and_key();
        apply_gateway_auth_headers(&mut fwd_headers, state.client, key.as_deref());
    }
    fwd_headers.remove("host");
    fwd_headers.remove("Host");

    // Drop content-encoding related length if compressed
    let ce = fwd_headers
        .get("content-encoding")
        .or_else(|| fwd_headers.get("Content-Encoding"))
        .cloned()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body_for_upstream = if matches!(ce.as_str(), "zstd" | "gzip" | "deflate" | "br") {
        fwd_headers.remove("content-encoding");
        fwd_headers.remove("Content-Encoding");
        fwd_headers.remove("content-length");
        fwd_headers.remove("Content-Length");
        decompress_body(&ce, &body_bytes).unwrap_or_else(|| body_bytes.to_vec())
    } else {
        body_bytes.to_vec()
    };

    let body_json: Value = serde_json::from_slice(&body_for_upstream).unwrap_or_else(|_| {
        Value::String(String::from_utf8_lossy(&body_for_upstream).to_string())
    });
    let stream = body_json
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let turn = if let Some(ref cid) = claw {
        state.dispatcher.alloc_turn(cid).unwrap_or(0)
    } else {
        0
    };

    fwd_headers.insert("Accept-Encoding".into(), "identity".into());

    let mut builder = state.http.request(method_to_reqwest(&method), &upstream_url);
    for (k, v) in &fwd_headers {
        if is_hop_by_hop(k) {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder = builder.body(body_for_upstream.clone());

    let started = Instant::now();
    let req_id = format!("req_{}", &Uuid::new_v4().simple().to_string()[..12]);

    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("upstream error: {e}");
            return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response();
        }
    };

    let status = resp.status();
    let resp_headers = resp.headers().clone();

    if stream && status.as_u16() == 200 {
        return handle_streaming(
            state,
            claw,
            claw_turn,
            turn,
            req_id,
            method,
            path_and_query,
            headers_in,
            body_json,
            target,
            started,
            status,
            resp_headers,
            resp,
        )
        .await;
    }

    let resp_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("read error: {e}")).into_response();
        }
    };

    let parsed_body = parse_response_body(&resp_headers, &resp_bytes);
    spawn_model_usage_insert(
        &state,
        claw_turn.as_deref(),
        &body_json,
        &parsed_body,
        &target,
        &upstream_url,
        started,
    );
    maybe_write_record(
        &state,
        claw.as_deref(),
        turn,
        &req_id,
        &method,
        &path_and_query,
        &headers_in,
        &body_json,
        status.as_u16(),
        &resp_headers,
        parsed_body,
        None,
        &target,
        started,
    );

    let mut response = Response::builder().status(status.as_u16());
    for (k, v) in resp_headers.iter() {
        if is_hop_by_hop(k.as_str()) {
            continue;
        }
        response = response.header(k, v);
    }
    response
        .body(Body::from(resp_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn handle_streaming(
    state: ProxyState,
    claw: Option<String>,
    claw_turn: Option<String>,
    turn: i64,
    req_id: String,
    method: Method,
    path: String,
    headers_in: HashMap<String, String>,
    body_json: Value,
    target: String,
    started: Instant,
    status: reqwest::StatusCode,
    resp_headers: HeaderMap,
    resp: reqwest::Response,
) -> Response {
    // True streaming (Python `_handle_streaming` parity): forward each chunk to the
    // client as it arrives while feeding SSEReassembler; write JSONL only after EOF.
    // Chunks live on disk via the completed record (sse_events), not in Live RAM.
    // Author: kejiqing
    let upstream = resp.bytes_stream();
    let body = streaming_proxy_body(
        upstream,
        state,
        claw,
        claw_turn,
        turn,
        req_id,
        method,
        path,
        headers_in,
        body_json,
        target,
        started,
        status.as_u16(),
        resp_headers.clone(),
    );

    let mut response = Response::builder().status(status.as_u16());
    for (k, v) in resp_headers.iter() {
        if is_hop_by_hop(k.as_str()) {
            continue;
        }
        // Streaming response: do not forward Content-Length (body length unknown).
        if k.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        response = response.header(k, v);
    }
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Tee upstream byte chunks to the client body stream and SSEReassembler.
/// On EOF, persist reconstructed body + `sse_events` to JSONL (disk is source of truth).
/// Author: kejiqing
pub fn streaming_proxy_body<S, E>(
    upstream: S,
    state: ProxyState,
    claw: Option<String>,
    claw_turn: Option<String>,
    turn: i64,
    req_id: String,
    method: Method,
    path: String,
    headers_in: HashMap<String, String>,
    body_json: Value,
    target: String,
    started: Instant,
    status: u16,
    resp_headers: HeaderMap,
) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let byte_stream = tee_upstream_chunks(upstream, move |reassembler| {
        let reconstructed = reassembler.reconstruct().unwrap_or(Value::Null);
        let sse_events: Vec<Value> = reassembler
            .events
            .iter()
            .map(|e| json!({"event": e.event, "data": e.data}))
            .collect();
        let upstream_url = format!(
            "{}/{}",
            target.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        spawn_model_usage_insert(
            &state,
            claw_turn.as_deref(),
            &body_json,
            &reconstructed,
            &target,
            &upstream_url,
            started,
        );
        // sse_events are written into JSONL on disk here — Live has no chunk deque.
        maybe_write_record(
            &state,
            claw.as_deref(),
            turn,
            &req_id,
            &method,
            &path,
            &headers_in,
            &body_json,
            status,
            &resp_headers,
            reconstructed,
            Some(sse_events),
            &target,
            started,
        );
    });
    Body::from_stream(byte_stream)
}

/// Forward each upstream chunk immediately; invoke `on_complete` after the stream ends.
/// Used so clients see bytes as they arrive (not after full buffer). Author: kejiqing
pub fn tee_upstream_chunks<S, E, F>(
    upstream: S,
    on_complete: F,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    F: FnOnce(SseReassembler) + Send + 'static,
{
    async_stream::stream! {
        let mut reassembler = SseReassembler::new();
        futures_util::pin_mut!(upstream);
        let mut on_complete = Some(on_complete);
        while let Some(item) = upstream.next().await {
            match item {
                Ok(chunk) => {
                    reassembler.feed_bytes(&chunk);
                    yield Ok(chunk);
                }
                Err(e) => {
                    tracing::warn!("stream read error: {e}");
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
            }
        }
        if let Some(cb) = on_complete.take() {
            cb(reassembler);
        }
    }
}

/// Fire-and-forget `gateway_model_usage` INSERT (never blocks the proxy response). Author: kejiqing
///
/// Only runs when the request carried a non-empty `claw-turn-id` and a gateway store is present.
/// Failures are logged as warnings per §6.1 rule 4 (INSERT must not break proxying).
fn spawn_model_usage_insert(
    state: &ProxyState,
    claw_turn: Option<&str>,
    req_body: &Value,
    resp_body: &Value,
    base_url: &str,
    upstream_url: &str,
    started: Instant,
) {
    let Some(turn_id) = claw_turn.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(gateway) = state.gateway.clone() else {
        return;
    };
    let model = req_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let usage = resp_body.get("usage").cloned().unwrap_or(Value::Null);
    let turn_id = turn_id.to_string();
    let base_url = base_url.to_string();
    let upstream_url = upstream_url.to_string();
    let latency_ms = started.elapsed().as_millis() as u64;
    tokio::spawn(async move {
        let row = crate::model_usage::build_model_usage_row(
            &turn_id,
            &model,
            &base_url,
            &upstream_url,
            &usage,
            latency_ms,
        );
        if let Err(e) = gateway.insert_model_usage(&row).await {
            tracing::warn!("gateway_model_usage insert failed: {e}");
        }
    });
}

fn maybe_write_record(
    state: &ProxyState,
    claw: Option<&str>,
    turn: i64,
    req_id: &str,
    method: &Method,
    path: &str,
    headers_in: &HashMap<String, String>,
    body_json: &Value,
    status: u16,
    resp_headers: &HeaderMap,
    resp_body: Value,
    sse_events: Option<Vec<Value>>,
    target: &str,
    started: Instant,
) {
    let Some(cid) = claw else {
        return;
    };
    let mut response = json!({
        "status": status,
        "headers": header_map_to_json(resp_headers),
        "body": resp_body,
    });
    if let Some(ev) = sse_events {
        // Persist SSE chunks on disk (JSONL). Live broadcast may send this full record
        // once, but must not retain a deque of chunks in RAM. Author: kejiqing
        response
            .as_object_mut()
            .unwrap()
            .insert("sse_events".into(), Value::Array(ev));
    }
    let record = json!({
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        "request_id": req_id,
        "turn": turn,
        "duration_ms": started.elapsed().as_millis() as u64,
        "request": {
            "method": method.as_str(),
            "path": path,
            "headers": filter_headers(headers_in, true),
            "body": body_json,
        },
        "response": response,
        "upstream_base_url": target,
    });
    if let Err(e) = state.dispatcher.write(cid, record) {
        tracing::warn!("trace write failed: {e}");
    }
}

/// WebSocket reverse proxy (axum Upgrade extractor).
pub async fn ws_proxy_handler(
    ws: WebSocketUpgrade,
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Response {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    if !is_allowed_path(&path_and_query) && !is_allowed_path(req.uri().path()) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let headers_in = headers_to_map(req.headers());
    let claw = extract_from_map(&headers_in);
    let (target, strip) = resolve_upstream(
        &state.target_url,
        &state.strip_path_prefix,
        state.upstream_file.as_deref(),
        state.gateway.as_deref(),
    );
    let mut fwd_path = path_and_query.clone();
    if !strip.is_empty() && fwd_path.starts_with(&strip) {
        fwd_path = fwd_path[strip.len()..].to_string();
        if fwd_path.is_empty() {
            fwd_path = "/".into();
        }
    }
    let host_part = target
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let scheme = if target.starts_with("https") { "wss" } else { "ws" };
    let ws_url = format!(
        "{scheme}://{}/{}",
        host_part.trim_end_matches('/'),
        fwd_path.trim_start_matches('/')
    );

    ws.on_upgrade(move |socket| async move {
        relay_websocket(state, socket, ws_url, claw, path_and_query, headers_in).await;
    })
}

async fn relay_websocket(
    state: ProxyState,
    client_ws: WebSocket,
    ws_url: String,
    claw: Option<String>,
    path: String,
    headers_in: HashMap<String, String>,
) {
    let started = Instant::now();
    let req_id = format!("req_{}", &Uuid::new_v4().simple().to_string()[..12]);
    let turn = if let Some(ref cid) = claw {
        state.dispatcher.alloc_turn(cid).unwrap_or(0)
    } else {
        0
    };

    let (upstream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("ws upstream connect failed: {e}");
            if let Some(cid) = claw.as_deref() {
                let record = json!({
                    "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                    "request_id": req_id,
                    "turn": turn,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "request": {"method":"GET","path": path, "headers": filter_headers(&headers_in, true), "body": null},
                    "response": {"status": 502, "headers": {}, "body": {"error": format!("{e}")}},
                });
                let _ = state.dispatcher.write(cid, record);
            }
            return;
        }
    };

    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut up_sink, mut up_stream) = upstream.split();

    let mut client_messages: Vec<Value> = Vec::new();
    let mut server_messages: Vec<Value> = Vec::new();

    loop {
        tokio::select! {
            msg = client_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            client_messages.push(v);
                        }
                        if up_sink.send(tokio_tungstenite::tungstenite::Message::Text(t.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if up_sink.send(tokio_tungstenite::tungstenite::Message::Binary(b.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            msg = up_stream.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            server_messages.push(v);
                        }
                        if client_sink.send(Message::Text(t.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b))) => {
                        if client_sink.send(Message::Binary(b.to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    if let Some(cid) = claw.as_deref() {
        let req_body = merge_ws_request_messages(&client_messages);
        let resp_body = merge_ws_response_messages(&server_messages);
        let record = json!({
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            "request_id": req_id,
            "turn": turn,
            "duration_ms": started.elapsed().as_millis() as u64,
            "request": {
                "method": "GET",
                "path": path,
                "headers": filter_headers(&headers_in, true),
                "body": req_body,
            },
            "response": {
                "status": 101,
                "headers": {},
                "body": resp_body,
                "ws_events": server_messages,
            },
        });
        let _ = state.dispatcher.write(cid, record);
    }
}

fn merge_json_lists(a: &mut Vec<Value>, b: &[Value]) {
    for item in b {
        let key = serde_json::to_string(item).unwrap_or_default();
        if !a.iter().any(|x| serde_json::to_string(x).unwrap_or_default() == key) {
            a.push(item.clone());
        }
    }
}

pub fn merge_ws_request_messages(msgs: &[Value]) -> Value {
    let mut out = json!({});
    for m in msgs {
        let Some(obj) = m.as_object() else {
            continue;
        };
        let dest = out.as_object_mut().unwrap();
        for (k, v) in obj {
            if k == "input" || k == "tools" {
                if let Some(arr) = v.as_array() {
                    let entry = dest.entry(k.clone()).or_insert_with(|| json!([]));
                    if let Some(existing) = entry.as_array_mut() {
                        merge_json_lists(existing, arr);
                    }
                }
            } else if !v.is_null()
                && !(v.is_string() && v.as_str() == Some(""))
                && !(v.is_array() && v.as_array().map(|a| a.is_empty()).unwrap_or(false))
            {
                dest.insert(k.clone(), v.clone());
            } else {
                dest.entry(k.clone()).or_insert(v.clone());
            }
        }
    }
    out
}

pub fn merge_ws_response_messages(msgs: &[Value]) -> Value {
    let mut merged = json!({});
    let mut items: Vec<(u64, Value)> = Vec::new();
    for m in msgs {
        let event = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(
            event,
            "response.created" | "response.in_progress" | "response.completed" | "response.done"
        ) {
            if let Some(resp) = m.get("response") {
                if let (Some(mo), Some(ro)) = (merged.as_object_mut(), resp.as_object()) {
                    for (k, v) in ro {
                        mo.insert(k.clone(), v.clone());
                    }
                }
            }
        } else if event == "response.output_item.done" {
            if let Some(item) = m.get("item") {
                let idx = m.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                items.push((idx, item.clone()));
            }
        }
    }
    items.sort_by_key(|(i, _)| *i);
    let output_empty = merged
        .get("output")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);
    if output_empty && !items.is_empty() {
        let arr: Vec<Value> = items.into_iter().map(|(_, v)| v).collect();
        if let Some(mo) = merged.as_object_mut() {
            mo.insert("output".into(), Value::Array(arr));
        }
    }
    merged
}

fn headers_to_map(h: &HeaderMap) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for (k, v) in h.iter() {
        if let Ok(s) = v.to_str() {
            m.insert(k.as_str().to_string(), s.to_string());
        }
    }
    m
}

fn header_map_to_json(h: &HeaderMap) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in h.iter() {
        if is_hop_by_hop(k.as_str()) {
            continue;
        }
        if let Ok(s) = v.to_str() {
            m.insert(k.as_str().to_string(), json!(s));
        }
    }
    Value::Object(m)
}

fn method_to_reqwest(m: &Method) -> reqwest::Method {
    reqwest::Method::from_bytes(m.as_str().as_bytes()).unwrap_or(reqwest::Method::POST)
}

fn decompress_body(ce: &str, data: &[u8]) -> Option<Vec<u8>> {
    match ce {
        "gzip" => {
            use flate2::read::GzDecoder;
            use std::io::Read;
            let mut d = GzDecoder::new(data);
            let mut out = Vec::new();
            d.read_to_end(&mut out).ok()?;
            Some(out)
        }
        "deflate" => {
            use flate2::read::ZlibDecoder;
            use std::io::Read;
            let mut d = ZlibDecoder::new(data);
            let mut out = Vec::new();
            d.read_to_end(&mut out).ok()?;
            Some(out)
        }
        "zstd" => zstd::decode_all(data).ok(),
        _ => None,
    }
}

fn parse_response_body(headers: &HeaderMap, bytes: &[u8]) -> Value {
    let ce = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let data = decompress_body(&ce, bytes).unwrap_or_else(|| bytes.to_vec());
    serde_json::from_slice(&data)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&data).to_string()))
}

// Silence unused import warnings for HeaderName/HeaderValue if not used
#[allow(dead_code)]
fn _unused() {
    let _ = HeaderName::from_static("x");
    let _ = HeaderValue::from_static("y");
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::convert::Infallible;
    use tokio::sync::oneshot;

    #[test]
    fn merge_ws_request_lists() {
        let msgs = vec![
            json!({"input":[{"id":"a"}],"model":"m"}),
            json!({"input":[{"id":"b"}],"model":"m2"}),
        ];
        let merged = merge_ws_request_messages(&msgs);
        assert_eq!(merged["model"], "m2");
        assert_eq!(merged["input"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn merge_ws_response_output_items() {
        let msgs = vec![
            json!({"type":"response.created","response":{"id":"r1","output":[]}}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"id":"i1"}}),
        ];
        let merged = merge_ws_response_messages(&msgs);
        assert_eq!(merged["id"], "r1");
        assert_eq!(merged["output"][0]["id"], "i1");
    }

    #[tokio::test]
    async fn streaming_tee_yields_chunks_before_complete() {
        // Prove stream body forwards chunks as they arrive (not after full buffer).
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let upstream = stream::unfold(
            (0u8, Some(gate_rx)),
            |(n, gate)| async move {
                match n {
                    0 => Some((Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk-a\n")), (1, gate))),
                    1 => {
                        if let Some(rx) = gate {
                            let _ = rx.await;
                        }
                        Some((Ok(Bytes::from_static(b"chunk-b\n")), (2, None)))
                    }
                    _ => None,
                }
            },
        );

        let (done_tx, mut done_rx) = oneshot::channel::<usize>();
        let mut out = Box::pin(tee_upstream_chunks(upstream, move |re| {
            let _ = done_tx.send(re.events.len());
        }));

        let first = out.next().await.unwrap().unwrap();
        assert_eq!(&first[..], b"chunk-a\n");
        // Second chunk is gated — complete callback must not have run yet.
        assert!(done_rx.try_recv().is_err());
        let _ = gate_tx.send(());
        let second = out.next().await.unwrap().unwrap();
        assert_eq!(&second[..], b"chunk-b\n");
        assert!(out.next().await.is_none());
        let _ = done_rx.await.unwrap();
    }

    #[tokio::test]
    async fn streaming_proxy_body_is_stream_not_full_buffer() {
        let upstream = stream::iter(vec![
            Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"a\":1}\n\n")),
            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
        ]);
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let body = Body::from_stream(tee_upstream_chunks(upstream, move |_| {
            let _ = done_tx.send(());
        }));
        // Collect via axum body helper — proves Body::from_stream path works end-to-end.
        let collected = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert!(collected.windows(7).any(|w| w == b"data: {"));
        done_rx.await.unwrap();
    }
}
