//! Forward proxy: CONNECT + TLS MITM + HTTP/SSE + WebSocket.
//! Author: kejiqing
//!
//! Parity target: Python `claude_tap/forward_proxy.py`.
//! TLS is accepted directly on the client socket (no localhost bounce).

use crate::certs::CertificateAuthority;
use crate::claw_session::{
    extract_from_map, extract_turn_from_map, strip_claw_session_header, strip_claw_turn_header,
};
use crate::gateway_model_usage::maybe_spawn_insert;
use crate::client_config::ClientName;
use crate::gateway_upstream::{apply_gateway_auth_headers, GatewayLlmUpstreamStore};
use crate::headers::{filter_headers, is_hop_by_hop};
use crate::proxy::{merge_ws_request_messages, merge_ws_response_messages};
use crate::session_dispatcher::SessionTraceDispatcher;
use crate::sse::SseReassembler;
use anyhow::Context;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, WebSocketStream};
use uuid::Uuid;

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub struct ForwardProxyServer {
    pub host: String,
    pub port: u16,
    pub ca: Arc<CertificateAuthority>,
    pub dispatcher: Arc<SessionTraceDispatcher>,
    pub http: reqwest::Client,
    pub client: ClientName,
    pub gateway: Option<Arc<GatewayLlmUpstreamStore>>,
}

impl ForwardProxyServer {
    pub async fn bind_and_serve(self: Arc<Self>) -> anyhow::Result<u16> {
        let listener = TcpListener::bind((self.host.as_str(), self.port)).await?;
        let actual = listener.local_addr()?.port();
        tracing::info!("forward proxy listening on {}:{}", self.host, actual);
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let this = this.clone();
                        tokio::spawn(async move {
                            if let Err(e) = this.handle_client(stream).await {
                                tracing::debug!("forward client error: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("accept error: {e}");
                        break;
                    }
                }
            }
        });
        Ok(actual)
    }

    async fn handle_client(&self, mut stream: TcpStream) -> anyhow::Result<()> {
        let headers_raw = read_http_head_exact(&mut stream).await?;
        let header_text = String::from_utf8_lossy(&headers_raw);
        let first = header_text.lines().next().unwrap_or("");
        let parts: Vec<&str> = first.split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(());
        }
        let method = parts[0].to_ascii_uppercase();
        if method == "CONNECT" {
            let authority = parts[1];
            let (host, port) = parse_host_port(authority);
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            self.handle_connect(host, port, stream).await
        } else {
            // Plain HTTP absolute-URL proxy (rare for HTTPS clients). Author: kejiqing
            let url = parts[1].to_string();
            let headers = parse_header_map(&headers_raw);
            let body = read_body_by_content_length(&mut stream, &headers).await?;
            self.handle_plain_proxy(&method, &url, headers, body, stream)
                .await
        }
    }

    async fn handle_connect(
        &self,
        hostname: String,
        port: u16,
        stream: TcpStream,
    ) -> anyhow::Result<()> {
        let (cert_pem, key_pem) = self.ca.issue_host(&hostname)?;
        let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .context("parse host cert")?;
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .context("parse host key")?
            .context("missing key")?;
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let tls = acceptor.accept(stream).await?;
        self.handle_tunneled_requests(&hostname, port, tls).await
    }

    async fn handle_tunneled_requests<S>(&self, hostname: &str, port: u16, mut tls: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        loop {
            let headers_raw = match read_http_head_exact(&mut tls).await {
                Ok(h) => h,
                Err(e) if is_eof_like(&e) => break,
                Err(e) => return Err(e),
            };
            if headers_raw.is_empty() {
                break;
            }
            let header_text = String::from_utf8_lossy(&headers_raw);
            let mut lines = header_text.lines();
            let request_line = lines.next().unwrap_or("").trim();
            if request_line.is_empty() {
                break;
            }
            let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
            if parts.len() < 2 {
                break;
            }
            let method = parts[0].to_string();
            let path = parts[1].to_string();
            let headers = parse_header_map(&headers_raw);
            let body = read_body_by_content_length(&mut tls, &headers).await?;

            if is_websocket_upgrade(&headers) {
                self.forward_websocket(hostname, port, &path, headers, tls)
                    .await?;
                break;
            }

            let upstream_url = format!("https://{hostname}:{port}{path}");
            self.forward_and_record(&method, &path, headers, body, &upstream_url, &mut tls)
                .await?;
        }
        Ok(())
    }

    async fn handle_plain_proxy<S>(
        &self,
        method: &str,
        url: &str,
        headers: HashMap<String, String>,
        body: Vec<u8>,
        mut stream: S,
    ) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let parsed = url::Url::parse(url).unwrap_or_else(|_| url::Url::parse("http://invalid/").unwrap());
        let mut path = parsed.path().to_string();
        if let Some(q) = parsed.query() {
            path = format!("{path}?{q}");
        }
        self.forward_and_record(method, &path, headers, body, url, &mut stream)
            .await
    }

    async fn forward_and_record<S>(
        &self,
        method: &str,
        path: &str,
        headers: HashMap<String, String>,
        body: Vec<u8>,
        upstream_url: &str,
        client: &mut S,
    ) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if let Some(g) = &self.gateway {
            if !g.is_ready() {
                let msg = b"claw-tap: no active LLM in PostgreSQL for this cluster";
                write_simple_response(client, 503, "Service Unavailable", "text/plain", msg).await?;
                return Ok(());
            }
        }

        let claw = extract_from_map(&headers);
        let turn_id = extract_turn_from_map(&headers);
        let turn = if let Some(ref cid) = claw {
            self.dispatcher.alloc_turn(cid).unwrap_or(0)
        } else {
            0
        };
        let req_id = format!("req_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let started = Instant::now();
        let log_prefix = if claw.is_some() {
            format!("[Turn {turn}]")
        } else {
            "[proxy]".into()
        };

        let req_body: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(&body).to_string())
            })
        };
        let is_streaming = req_body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let model = req_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        tracing::info!("{log_prefix} -> {method} {path} (model={model}, stream={is_streaming})");

        let mut fwd_headers = headers.clone();
        strip_claw_session_header(&mut fwd_headers);
        strip_claw_turn_header(&mut fwd_headers);
        if let Some(g) = &self.gateway {
            let (_, key) = g.target_and_key();
            apply_gateway_auth_headers(&mut fwd_headers, self.client, key.as_deref());
        }
        remove_header_ci(&mut fwd_headers, "host");
        fwd_headers.insert("Accept-Encoding".into(), "identity".into());

        let reqwest_method =
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut builder = self.http.request(reqwest_method, upstream_url);
        for (k, v) in &fwd_headers {
            if is_hop_by_hop(k) || k.eq_ignore_ascii_case("content-length") {
                continue;
            }
            builder = builder.header(k, v);
        }
        builder = builder.body(body);

        let resp = match builder.send().await {
            Ok(r) => r,
            Err(exc) => {
                tracing::error!("{log_prefix} upstream error: {exc}");
                let error_body = format!("{exc}").into_bytes();
                write_simple_response(client, 502, "Bad Gateway", "text/plain", &error_body).await?;
                return Ok(());
            }
        };

        let status = resp.status().as_u16();
        let reason = resp
            .status()
            .canonical_reason()
            .unwrap_or("OK")
            .to_string();
        let resp_headers = resp.headers().clone();
        let upstream_base = upstream_base_url(upstream_url);

        if is_streaming && status == 200 {
            self.handle_streaming(
                resp,
                client,
                &req_id,
                turn,
                turn_id.as_deref(),
                started,
                method,
                path,
                &headers,
                &req_body,
                claw.as_deref(),
                &log_prefix,
                status,
                &reason,
                &resp_headers,
                &upstream_base,
            )
            .await?;
        } else {
            self.handle_non_streaming(
                resp,
                client,
                &req_id,
                turn,
                turn_id.as_deref(),
                started,
                method,
                path,
                &headers,
                &req_body,
                claw.as_deref(),
                &log_prefix,
                status,
                &reason,
                &resp_headers,
                &upstream_base,
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_streaming<S>(
        &self,
        resp: reqwest::Response,
        client: &mut S,
        req_id: &str,
        turn: i64,
        turn_id: Option<&str>,
        started: Instant,
        method: &str,
        path: &str,
        req_headers: &HashMap<String, String>,
        req_body: &Value,
        claw: Option<&str>,
        log_prefix: &str,
        status: u16,
        reason: &str,
        resp_headers: &reqwest::header::HeaderMap,
        upstream_base: &str,
    ) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        // Status + filtered headers + chunked transfer (Python parity). Author: kejiqing
        let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
        for (k, v) in resp_headers.iter() {
            let name = k.as_str();
            if is_hop_by_hop(name) || name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            if let Ok(val) = v.to_str() {
                out.push_str(&format!("{name}: {val}\r\n"));
            }
        }
        out.push_str("Transfer-Encoding: chunked\r\n\r\n");
        client.write_all(out.as_bytes()).await?;

        let mut reassembler = SseReassembler::new();
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    write_http_chunk(client, &chunk).await?;
                    reassembler.feed_bytes(&chunk);
                }
                Err(e) => {
                    tracing::warn!("{log_prefix} stream read error: {e}");
                    break;
                }
            }
        }
        let _ = client.write_all(b"0\r\n\r\n").await;

        let duration_ms = started.elapsed().as_millis() as u64;
        let reconstructed = reassembler.reconstruct().unwrap_or(Value::Null);
        let usage = reconstructed.get("usage").cloned().unwrap_or(Value::Null);
        tracing::info!(
            "{log_prefix} <- 200 stream done ({duration_ms}ms, in={} out={} cache_read={} cache_create={})",
            usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );

        let sse_events: Vec<Value> = reassembler
            .events
            .iter()
            .map(|e| json!({"event": e.event, "data": e.data}))
            .collect();
        write_http_record(
            &self.dispatcher,
            self.gateway.clone(),
            claw,
            turn,
            turn_id,
            Some(self.client.as_str()),
            req_id,
            duration_ms,
            method,
            path,
            req_headers,
            req_body,
            status,
            resp_headers,
            reconstructed,
            Some(sse_events),
            upstream_base,
        );
        Ok(())
    }

    async fn handle_non_streaming<S>(
        &self,
        resp: reqwest::Response,
        client: &mut S,
        req_id: &str,
        turn: i64,
        turn_id: Option<&str>,
        started: Instant,
        method: &str,
        path: &str,
        req_headers: &HashMap<String, String>,
        req_body: &Value,
        claw: Option<&str>,
        log_prefix: &str,
        status: u16,
        reason: &str,
        resp_headers: &reqwest::header::HeaderMap,
        upstream_base: &str,
    ) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        let resp_bytes = resp.bytes().await.unwrap_or_default();
        let duration_ms = started.elapsed().as_millis() as u64;
        let resp_body = parse_response_body(resp_headers, &resp_bytes);
        tracing::info!(
            "{log_prefix} <- {status} ({duration_ms}ms, {} bytes)",
            resp_bytes.len()
        );

        write_http_record(
            &self.dispatcher,
            self.gateway.clone(),
            claw,
            turn,
            turn_id,
            Some(self.client.as_str()),
            req_id,
            duration_ms,
            method,
            path,
            req_headers,
            req_body,
            status,
            resp_headers,
            resp_body,
            None,
            upstream_base,
        );

        let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
        for (k, v) in resp_headers.iter() {
            let name = k.as_str();
            if is_hop_by_hop(name) || name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            if let Ok(val) = v.to_str() {
                out.push_str(&format!("{name}: {val}\r\n"));
            }
        }
        out.push_str(&format!("Content-Length: {}\r\n\r\n", resp_bytes.len()));
        client.write_all(out.as_bytes()).await?;
        client.write_all(&resp_bytes).await?;
        Ok(())
    }

    async fn forward_websocket<S>(
        &self,
        hostname: &str,
        port: u16,
        path: &str,
        headers: HashMap<String, String>,
        mut tls: S,
    ) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let claw = extract_from_map(&headers);
        let turn = if let Some(ref cid) = claw {
            self.dispatcher.alloc_turn(cid).unwrap_or(0)
        } else {
            0
        };
        let req_id = format!("req_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let started = Instant::now();
        let log_prefix = if claw.is_some() {
            format!("[Turn {turn}]")
        } else {
            "[proxy]".into()
        };
        let upstream_base_url = format!("https://{hostname}:{port}");
        let upstream_ws_url = format!("wss://{hostname}:{port}{path}");

        let mut fwd_headers = headers.clone();
        strip_claw_session_header(&mut fwd_headers);
        strip_claw_turn_header(&mut fwd_headers);
        if let Some(g) = &self.gateway {
            let (_, key) = g.target_and_key();
            apply_gateway_auth_headers(&mut fwd_headers, self.client, key.as_deref());
        }
        remove_header_ci(&mut fwd_headers, "host");
        // Drop hop-by-hop and Sec-WebSocket-* — tungstenite sets handshake headers.
        fwd_headers.retain(|k, _| {
            let lower = k.to_ascii_lowercase();
            !is_hop_by_hop(k)
                && !lower.starts_with("sec-websocket-")
                && lower != "upgrade"
                && lower != "connection"
        });

        tracing::info!("{log_prefix} -> WS UPGRADE {path} (upstream={upstream_ws_url})");

        let mut request = upstream_ws_url
            .as_str()
            .into_client_request()
            .context("build ws request")?;
        {
            let hdrs = request.headers_mut();
            for (k, v) in &fwd_headers {
                if let (Ok(name), Ok(val)) = (
                    http::HeaderName::from_bytes(k.as_bytes()),
                    http::HeaderValue::from_str(v),
                ) {
                    hdrs.insert(name, val);
                }
            }
            if let Some(proto) = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-protocol"))
                .map(|(_, v)| v.as_str())
            {
                // Pass first requested subprotocol if present.
                if let Some(first) = proto.split(',').next().map(str::trim).filter(|s| !s.is_empty())
                {
                    if let Ok(val) = http::HeaderValue::from_str(first) {
                        hdrs.insert(http::HeaderName::from_static("sec-websocket-protocol"), val);
                    }
                }
            }
        }

        let (upstream, upstream_resp) = match connect_async(request).await {
            Ok(pair) => pair,
            Err(exc) => {
                let duration_ms = started.elapsed().as_millis() as u64;
                tracing::error!("{log_prefix} upstream WS connect failed: {exc}");
                let error_body = format!("{exc}").into_bytes();
                write_simple_response(&mut tls, 502, "Bad Gateway", "text/plain", &error_body)
                    .await?;
                if let Some(cid) = claw.as_deref() {
                    let record = json!({
                        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                        "request_id": req_id,
                        "turn": turn,
                        "duration_ms": duration_ms,
                        "transport": "websocket",
                        "request": {
                            "method": "WEBSOCKET",
                            "path": path,
                            "headers": filter_headers(&headers, true),
                            "body": Value::Null,
                        },
                        "response": {
                            "status": 502,
                            "headers": {},
                            "body": Value::Null,
                            "error": format!("{exc}"),
                        },
                        "upstream_base_url": upstream_base_url,
                    });
                    let _ = self.dispatcher.write(cid, record);
                }
                return Ok(());
            }
        };

        let sec_key = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        if sec_key.is_empty() {
            write_simple_response(&mut tls, 400, "Bad Request", "text/plain", b"").await?;
            return Ok(());
        }

        let accept = ws_accept_key(sec_key);
        let mut response_lines = vec![
            "HTTP/1.1 101 Switching Protocols".to_string(),
            "Upgrade: websocket".to_string(),
            "Connection: Upgrade".to_string(),
            format!("Sec-WebSocket-Accept: {accept}"),
        ];
        if let Some(proto) = upstream_resp
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
        {
            response_lines.push(format!("Sec-WebSocket-Protocol: {proto}"));
        }
        let resp = format!("{}\r\n\r\n", response_lines.join("\r\n"));
        tls.write_all(resp.as_bytes()).await?;

        let client_ws = WebSocketStream::from_raw_socket(tls, Role::Server, None).await;
        relay_forward_websocket(
            self.dispatcher.clone(),
            client_ws,
            upstream,
            claw,
            turn,
            req_id,
            path.to_string(),
            headers,
            upstream_base_url,
            started,
            log_prefix,
        )
        .await;
        Ok(())
    }
}

async fn relay_forward_websocket<S>(
    dispatcher: Arc<SessionTraceDispatcher>,
    client_ws: WebSocketStream<S>,
    upstream: WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    claw: Option<String>,
    turn: i64,
    req_id: String,
    path: String,
    headers_in: HashMap<String, String>,
    upstream_base_url: String,
    started: Instant,
    log_prefix: String,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut up_sink, mut up_stream) = upstream.split();

    let mut client_messages: Vec<Value> = Vec::new();
    let mut server_messages: Vec<Value> = Vec::new();

    loop {
        tokio::select! {
            msg = client_stream.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                            client_messages.push(v);
                        }
                        if up_sink.send(WsMessage::Text(t)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Binary(b))) => {
                        if up_sink.send(WsMessage::Binary(b)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Ping(p))) => {
                        let _ = up_sink.send(WsMessage::Ping(p)).await;
                    }
                    Some(Ok(WsMessage::Pong(p))) => {
                        let _ = up_sink.send(WsMessage::Pong(p)).await;
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            msg = up_stream.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                            server_messages.push(v);
                        }
                        if client_sink.send(WsMessage::Text(t)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Binary(b))) => {
                        if client_sink.send(WsMessage::Binary(b)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Ping(p))) => {
                        let _ = client_sink.send(WsMessage::Ping(p)).await;
                    }
                    Some(Ok(WsMessage::Pong(p))) => {
                        let _ = client_sink.send(WsMessage::Pong(p)).await;
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let client_count = client_messages.len();
    let server_count = server_messages.len();
    if let Some(cid) = claw.as_deref() {
        let req_body = merge_ws_request_messages(&client_messages);
        let resp_body = merge_ws_response_messages(&server_messages);
        let record = json!({
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            "request_id": req_id,
            "turn": turn,
            "duration_ms": duration_ms,
            "transport": "websocket",
            "request": {
                "method": "WEBSOCKET",
                "path": path,
                "headers": filter_headers(&headers_in, true),
                "body": req_body,
                "ws_events": client_messages,
            },
            "response": {
                "status": 101,
                "headers": {},
                "body": resp_body,
                "ws_events": server_messages,
            },
            "upstream_base_url": upstream_base_url,
        });
        let _ = dispatcher.write(cid, record);
    }
    tracing::info!(
        "{log_prefix} <- WS closed ({duration_ms}ms, {client_count} client→upstream, {server_count} upstream→client)"
    );
}

fn write_http_record(
    dispatcher: &SessionTraceDispatcher,
    gateway: Option<Arc<GatewayLlmUpstreamStore>>,
    claw: Option<&str>,
    turn: i64,
    turn_id: Option<&str>,
    provider: Option<&str>,
    req_id: &str,
    duration_ms: u64,
    method: &str,
    path: &str,
    req_headers: &HashMap<String, String>,
    req_body: &Value,
    status: u16,
    resp_headers: &reqwest::header::HeaderMap,
    resp_body: Value,
    sse_events: Option<Vec<Value>>,
    upstream_base: &str,
) {
    maybe_spawn_insert(
        gateway,
        turn_id,
        provider,
        req_body,
        &resp_body,
        duration_ms as i64,
    );
    let Some(cid) = claw else {
        return;
    };
    let mut response = json!({
        "status": status,
        "headers": reqwest_headers_to_json(resp_headers),
        "body": resp_body,
    });
    if let Some(ev) = sse_events {
        response
            .as_object_mut()
            .unwrap()
            .insert("sse_events".into(), Value::Array(ev));
    }
    let record = json!({
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        "request_id": req_id,
        "turn": turn,
        "duration_ms": duration_ms,
        "request": {
            "method": method,
            "path": path,
            "headers": filter_headers(req_headers, true),
            "body": req_body,
        },
        "response": response,
        "upstream_base_url": upstream_base,
    });
    if let Err(e) = dispatcher.write(cid, record) {
        tracing::warn!("trace write failed: {e}");
    }
}

fn reqwest_headers_to_json(h: &reqwest::header::HeaderMap) -> Value {
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

fn parse_response_body(headers: &reqwest::header::HeaderMap, bytes: &[u8]) -> Value {
    let ce = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let data = decompress_body(&ce, bytes).unwrap_or_else(|| bytes.to_vec());
    serde_json::from_slice(&data)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&data).to_string()))
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

/// Read HTTP headers exactly up to `\r\n\r\n` without consuming body/TLS bytes beyond.
/// Author: kejiqing
async fn read_http_head_exact<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(buf);
            }
            anyhow::bail!("unexpected EOF while reading HTTP headers");
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && buf[buf.len() - 4..] == *b"\r\n\r\n" {
            break;
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("request headers too large");
        }
    }
    Ok(buf)
}

async fn read_body_by_content_length<R: AsyncRead + Unpin>(
    reader: &mut R,
    headers: &HashMap<String, String>,
) -> anyhow::Result<Vec<u8>> {
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length == 0 {
        return Ok(Vec::new());
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

fn parse_header_map(headers_raw: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(headers_raw);
    let mut headers = HashMap::new();
    for line in text.lines().skip(1) {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    headers
}

pub fn parse_host_port(authority: &str) -> (String, u16) {
    if let Some((h, p)) = authority.rsplit_once(':') {
        // Avoid treating IPv6 without brackets incorrectly when no port.
        if !h.contains(']') && p.parse::<u16>().is_ok() {
            if let Ok(port) = p.parse() {
                return (h.to_string(), port);
            }
        }
    }
    (authority.to_string(), 443)
}

pub fn is_websocket_upgrade(headers: &HashMap<String, String>) -> bool {
    let upgrade = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("upgrade"))
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default();
    if upgrade != "websocket" {
        return false;
    }
    let connection = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("connection"))
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default();
    connection.contains("upgrade")
}

pub fn ws_accept_key(sec_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(sec_key.as_bytes());
    hasher.update(WS_GUID);
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

fn remove_header_ci(headers: &mut HashMap<String, String>, name: &str) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(name));
}

fn upstream_base_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .map(|u| {
            let host = u.host_str().unwrap_or("");
            let port = u.port_or_known_default().unwrap_or(443);
            format!("{}://{host}:{port}", u.scheme())
        })
        .unwrap_or_else(|| url.to_string())
}

async fn write_simple_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\n\r\n",
        body.len()
    );
    w.write_all(header.as_bytes()).await?;
    w.write_all(body).await?;
    Ok(())
}

async fn write_http_chunk<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> anyhow::Result<()> {
    w.write_all(format!("{:x}\r\n", data.len()).as_bytes()).await?;
    w.write_all(data).await?;
    w.write_all(b"\r\n").await?;
    Ok(())
}

fn is_eof_like(err: &anyhow::Error) -> bool {
    if let Some(io) = err.downcast_ref::<std::io::Error>() {
        return matches!(
            io.kind(),
            ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
        );
    }
    let msg = format!("{err:#}");
    msg.contains("unexpected EOF") || msg.contains("early eof")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_accept_rfc_example() {
        // RFC6455 example
        let accept = ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn parse_authority() {
        assert_eq!(
            parse_host_port("example.com:443"),
            ("example.com".into(), 443)
        );
        assert_eq!(
            parse_host_port("example.com"),
            ("example.com".into(), 443)
        );
        assert_eq!(parse_host_port("api.anthropic.com:8443"), ("api.anthropic.com".into(), 8443));
    }

    #[test]
    fn websocket_upgrade_detection() {
        let mut ok = HashMap::new();
        ok.insert("Upgrade".into(), "websocket".into());
        ok.insert("Connection".into(), "Upgrade".into());
        assert!(is_websocket_upgrade(&ok));

        let mut mixed = HashMap::new();
        mixed.insert("upgrade".into(), "WebSocket".into());
        mixed.insert("connection".into(), "keep-alive, Upgrade".into());
        assert!(is_websocket_upgrade(&mixed));

        let mut no = HashMap::new();
        no.insert("Upgrade".into(), "websocket".into());
        no.insert("Connection".into(), "keep-alive".into());
        assert!(!is_websocket_upgrade(&no));
    }

    #[test]
    fn parse_headers_from_raw() {
        let raw = b"GET /v1/messages HTTP/1.1\r\nHost: api.anthropic.com\r\nContent-Length: 2\r\n\r\n";
        let h = parse_header_map(raw);
        assert_eq!(h.get("Host").map(String::as_str), Some("api.anthropic.com"));
        assert_eq!(h.get("Content-Length").map(String::as_str), Some("2"));
    }
}
