//! HTTP reverse-proxy integration tests. Author: kejiqing

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use axum::Router;
use claude_tap::client_config::ClientName;
use claude_tap::proxy::{dispatch_reverse, ProxyState};
use claude_tap::session_dispatcher::SessionTraceDispatcher;
use claude_tap::session_index::SessionIndex;
use std::net::SocketAddr;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn mock_upstream() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let body = br#"{"id":"msg","content":[{"type":"text","text":"hi"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.write_all(body).await.unwrap();
    });
    (addr, handle)
}

#[tokio::test]
async fn reverse_proxy_forwards_allowed_path_and_404s_other() {
    let (up_addr, _h) = mock_upstream().await;
    let dir = tempdir().unwrap();
    let idx = Arc::new(SessionIndex::open(dir.path()).unwrap());
    let dispatcher = Arc::new(SessionTraceDispatcher::new(idx));
    let http = reqwest::Client::new();
    let state = ProxyState {
        client: ClientName::Claude,
        target_url: format!("http://{up_addr}"),
        strip_path_prefix: String::new(),
        http,
        dispatcher,
        upstream_file: None,
        gateway: None,
    };

    let app = Router::new()
        .route(
            "/{*path}",
            any(|State(s): State<ProxyState>, req| async move {
                dispatch_reverse(State(s), req).await
            }),
        )
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let blocked = client
        .get(format!("http://{proxy_addr}/admin"))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::NOT_FOUND);

    let ok = client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("claw-session-id", "it-sess")
        .json(&serde_json::json!({"model":"x","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let text = ok.text().await.unwrap();
    assert!(text.contains("hi"));
}

#[allow(dead_code)]
fn _req_builder() -> Request<Body> {
    Request::builder()
        .uri("/v1/messages")
        .body(Body::empty())
        .unwrap()
}
