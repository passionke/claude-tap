//! Process orchestration — start reverse/forward + live + optional client.
//! Author: kejiqing

use crate::cli::{ProxyMode, TapArgs};
use crate::client_config::{build_client_env, client_config, ClientName};
use crate::cluster_identity::{
    claw_gateway_env_configured, gateway_cluster_id_from_env, gateway_database_url_from_env,
    gateway_proj_id_from_env, local_cluster_identity, ClusterIdentity,
};
use crate::gateway_upstream::{
    gateway_llm_poll_interval_seconds, poll_gateway_llm_upstream, GatewayLlmUpstreamStore,
};
use crate::health::healthz_response;
use crate::live::{live_router, LiveState};
use crate::proxy::{dispatch_reverse, ProxyState};
use crate::session_dispatcher::SessionTraceDispatcher;
use crate::session_index::SessionIndex;
use crate::upstream_config::{
    poll_upstream_config, strip_path_prefix_for, UpstreamConfigStore,
};
use crate::viewer_html::generate_html_viewer;
use axum::extract::State;
use axum::routing::{any, get};
use axum::Router;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

pub async fn async_main(args: TapArgs) -> anyhow::Result<i32> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let output_dir = args.output_dir.clone();
    std::fs::create_dir_all(&output_dir)?;
    let index = Arc::new(SessionIndex::open(&output_dir)?);
    let dispatcher = Arc::new(SessionTraceDispatcher::new(index.clone()));

    let gateway_mode = claw_gateway_env_configured();
    let mut identity: Option<ClusterIdentity> = None;
    let mut gateway: Option<Arc<GatewayLlmUpstreamStore>> = None;

    if gateway_mode {
        let cid = gateway_cluster_id_from_env().map_err(anyhow::Error::msg)?;
        let db = gateway_database_url_from_env().map_err(anyhow::Error::msg)?;
        let proj_id = gateway_proj_id_from_env().map_err(anyhow::Error::msg)?;
        identity = Some(local_cluster_identity(&cid, &db).map_err(anyhow::Error::msg)?);
        let store = Arc::new(GatewayLlmUpstreamStore::new(cid, db, proj_id));
        store.reload_from_db().await?;
        if !store.is_ready() {
            if let Some(pid) = proj_id {
                anyhow::bail!(
                    "claw-tap: no active LLM in PostgreSQL for cluster proj_id={pid} (gateway_llm_project_*)"
                );
            }
            anyhow::bail!("claw-tap: no active LLM in PostgreSQL for this cluster");
        }
        let poll = gateway_llm_poll_interval_seconds();
        tokio::spawn(poll_gateway_llm_upstream(store.clone(), poll));
        gateway = Some(store);
    }

    let upstream_file = if let Some(ref path) = args.upstream_config_file {
        if !gateway_mode {
            let store = Arc::new(UpstreamConfigStore::new(
                path,
                args.client.as_str(),
            ));
            let _ = store.reload_if_changed();
            tokio::spawn(poll_upstream_config(
                store.clone(),
                args.upstream_config_poll,
            ));
            Some(store)
        } else {
            None
        }
    } else {
        None
    };

    let live_state = if args.live {
        let live = LiveState::new(
            output_dir.clone(),
            index.clone(),
            &args.live_prefix_path,
        );
        let live_for_broadcast = live.clone();
        dispatcher.set_broadcast(Arc::new(move |record| {
            live_for_broadcast.broadcast(record);
        }));
        Some(live)
    } else {
        None
    };

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .pool_idle_timeout(std::time::Duration::from_secs(300))
        .build()?;

    let host = args.resolved_host().to_string();
    let proxy_mode = args.resolved_proxy_mode();
    let target = args.resolved_target().to_string();
    let strip = strip_path_prefix_for(args.client.as_str(), &target);

    let mut ca_path: Option<std::path::PathBuf> = None;
    let actual_proxy_port;

    match proxy_mode {
        ProxyMode::Forward => {
            if args.upstream_config_file.is_some() {
                tracing::warn!("--tap-upstream-config is ignored in forward proxy mode");
            }
            let ca = Arc::new(crate::certs::CertificateAuthority::ensure(None)?);
            let ca_cert = ca.ca_cert_path();
            ca_path = Some(ca_cert.clone());
            let server = Arc::new(crate::forward_proxy::ForwardProxyServer {
                host: host.clone(),
                port: args.port,
                ca,
                dispatcher: dispatcher.clone(),
                http: http.clone(),
                client: args.client,
                gateway: gateway.clone(),
            });
            actual_proxy_port = server.bind_and_serve().await?;
            tracing::info!(
                "forward proxy on http://{host}:{actual_proxy_port} (CA={})",
                ca_cert.display()
            );
        }
        ProxyMode::Reverse => {
            let state = ProxyState {
                client: args.client,
                target_url: target.clone(),
                strip_path_prefix: strip,
                http: http.clone(),
                dispatcher: dispatcher.clone(),
                upstream_file: upstream_file.clone(),
                gateway: gateway.clone(),
            };
            let identity_for_health = identity.clone();
            let gateway_for_health = gateway.clone();
            let app = Router::new()
                .route(
                    "/healthz",
                    get(move || {
                        let id = identity_for_health.clone();
                        let g = gateway_for_health.clone();
                        async move {
                            healthz_response(id.as_ref(), g.as_ref())
                        }
                    }),
                )
                .route(
                    "/{*path}",
                    any({
                        let state = state.clone();
                        move |req: axum::http::Request<axum::body::Body>| {
                            let state = state.clone();
                            async move { dispatch_reverse(State(state), req).await }
                        }
                    }),
                )
                .route(
                    "/",
                    any({
                        let state = state.clone();
                        move |req: axum::http::Request<axum::body::Body>| {
                            let state = state.clone();
                            async move { dispatch_reverse(State(state), req).await }
                        }
                    }),
                );

            let addr: SocketAddr = format!("{host}:{}", args.port).parse()?;
            let listener = tokio::net::TcpListener::bind(addr).await?;
            actual_proxy_port = listener.local_addr()?.port();
            tracing::info!("reverse proxy listening on {host}:{actual_proxy_port}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!("proxy server error: {e}");
                }
            });
        }
    }

    let mut live_port = args.live_port;
    if let Some(live) = live_state {
        let app = live_router(live);
        let addr: SocketAddr = format!("{host}:{live_port}").parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        live_port = listener.local_addr()?.port();
        tracing::info!("live viewer listening on {host}:{live_port}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("live server error: {e}");
            }
        });
    }

    if args.no_launch {
        tracing::info!(
            "proxy-only mode (port={actual_proxy_port}, live_port={live_port}). Ctrl-C to stop."
        );
        tokio::signal::ctrl_c().await?;
        cleanup_sessions(&index, args.max_traces)?;
        return Ok(0);
    }

    // Launch client
    let cfg = client_config(args.client);
    let resolved = which::which(cfg.cmd);
    let Ok(cmd_path) = resolved else {
        eprint!("{}", cfg.missing_help());
        return Ok(1);
    };
    let (env_map, cmd_args) = build_client_env(
        args.client,
        actual_proxy_port,
        proxy_mode.as_str(),
        ca_path.as_deref(),
        &args.extra_args,
    );
    let mut command = Command::new(cmd_path);
    command.args(&cmd_args).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    for (k, v) in env_map {
        command.env(k, v);
    }
    let status = command.status().await?;
    // Generate static HTML viewers for recorded sessions (Python parity). Author: kejiqing
    generate_session_htmls(&index)?;
    cleanup_sessions(&index, args.max_traces)?;
    Ok(status.code().unwrap_or(1))
}

fn generate_session_htmls(index: &SessionIndex) -> anyhow::Result<()> {
    let (rows, _) = index.list_sessions(10_000, 0)?;
    for row in rows {
        let jsonl = index.output_dir().join(&row.jsonl_relpath);
        if !jsonl.is_file() {
            continue;
        }
        let html = jsonl.with_extension("html");
        if let Err(e) = generate_html_viewer(&jsonl, &html) {
            tracing::warn!("failed to write {}: {e}", html.display());
        } else {
            tracing::info!("wrote viewer {}", html.display());
        }
    }
    Ok(())
}

fn cleanup_sessions(index: &SessionIndex, max_traces: usize) -> anyhow::Result<()> {
    let count = index.session_count()? as usize;
    if count > max_traces {
        let extra = (count - max_traces) as i64;
        index.delete_oldest_sessions(extra)?;
    }
    Ok(())
}

#[allow(dead_code)]
fn _client_name_str(c: ClientName) -> &'static str {
    c.as_str()
}
