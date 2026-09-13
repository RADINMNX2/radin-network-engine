//! radin-server binary — reference edge control API (spec 31-37).
//!
//! Stateless, provider-neutral. Endpoints:
//!   GET  /health          liveness + minimal load
//!   GET  /v1/edges        signed/config edge list (seeded with reference edges)
//!   GET  /v1/config       optimization config defaults
//!   POST /v1/benchmark    benchmark metadata ingestion (Idempotency-Key)
//!   POST /v1/telemetry    telemetry ingestion (Idempotency-Key)
//!
//! No user traffic ever transits this reference server; its only mutable
//! state is an in-memory idempotency ledger (spec 16).

use std::net::SocketAddr;
use std::sync::Arc;

use radin_server::{reference_edges, router, ServerState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut bind = String::from("127.0.0.1");
    let mut port: u16 = 8787;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind = args.next().expect("--bind requires a value"),
            "--port" => {
                port = args
                    .next()
                    .expect("--port requires a value")
                    .parse()
                    .expect("--port must be a u16 (0-65535)")
            }
            "--help" | "-h" => {
                println!(
                    "radin-server {} — reference edge control API",
                    env!("CARGO_PKG_VERSION")
                );
                println!("Usage: radin-server [--bind <ip>] [--port <u16>]");
                println!("Endpoints:");
                println!("  GET  /health");
                println!("  GET  /v1/edges");
                println!("  GET  /v1/config");
                println!("  POST /v1/benchmark   (Idempotency-Key)");
                println!("  POST /v1/telemetry   (Idempotency-Key)");
                return;
            }
            other => {
                eprintln!("error: unknown argument '{other}' (see --help)");
                std::process::exit(2);
            }
        }
    }

    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .expect("invalid --bind/--port combination");
    // Seed the reference edge catalog so /v1/edges serves a real list
    // (ServerState::default() starts with an empty registry).
    let state = Arc::new(ServerState::new_with_edges(reference_edges(now_ms())));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    tracing::info!(%addr, "radin-server listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Graceful shutdown on SIGINT (Ctrl+C) and SIGTERM (unix).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("ctrl_c handler installed");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installed")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("shutdown: SIGINT received"),
        _ = terminate => tracing::info!("shutdown: SIGTERM received"),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}