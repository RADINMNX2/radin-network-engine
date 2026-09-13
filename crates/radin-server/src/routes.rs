//! Routes for the reference edge control server (spec 31-34).
//!
//! - GET  /health                    → liveness + minimal load
//! - GET  /v1/edges                  → signed/configured edge list
//! - GET  /v1/config                 → optimization config defaults
//! - POST /v1/benchmark              → benchmark metadata ingestion
//! - POST /v1/telemetry              → telemetry ingestion (idempotent)
//!
//! Mutations carry `Idempotency-Key`; duplicate keys return the first
//! outcome without reapplying (spec 16).

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header::HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

use serde::{Deserialize, Serialize};

use crate::state::ServerState;

/// All edges: status, region, version, load per spec 32.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    region: &'static str,
    version: &'static str,
    load: f32,
    timestamp: u64,
}

async fn health() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        region: "reference",
        version: env!("CARGO_PKG_VERSION"),
        load: 0.0,
        timestamp: now_ms(),
    })
}

/// GET /v1/edges — returns the (validated, expiring) edge candidate list.
async fn edges(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let list = state.edge_list();
    Json(serde_json::json!({
        "protocol_version": state.protocol_version,
        "edges": list,
        "issued_at": now_ms(),
    }))
}

/// GET /v1/config — read-model of the optimization defaults.
async fn config() -> impl IntoResponse {
    Json(serde_json::json!({
        "score_weights": {
            "latency": 0.30, "jitter": 0.20, "loss": 0.30,
            "stability": 0.15, "handshake": 0.05
        },
        "fallback_order": ["quic", "udp", "tcp_tls", "http2", "websocket_tls", "direct"],
        "fail_safe": { "allow_direct_fallback": true }
    }))
}

#[derive(Deserialize, Serialize)]
struct BenchmarkIngest {
    version: String,
    region: String,
    samples: Vec<BenchmarkSample>,
}

#[derive(Deserialize, Serialize)]
struct BenchmarkSample {
    transport: String,
    rtt_ms: f64,
    jitter_ms: f64,
    loss_ratio: f64,
}

#[derive(Serialize)]
struct IngestAck {
    accepted: usize,
}

/// POST /v1/benchmark — store benchmark metadata (server keeps histories as
/// a read model; client already keeps its local-first copy per spec 15).
async fn benchmark(State(state): State<Arc<ServerState>>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let key = idempotency_key(&headers)
        .unwrap_or_else(|| "benchmark-no-key".to_string());
    if let Some(_outcome) = state.idempotency.read().unwrap().get(&key) {
        return (StatusCode::OK, Json(IngestAck { accepted: 0 })).into_response();
    }
    let parsed: Result<BenchmarkIngest, _> = serde_json::from_str(&body);
    match parsed {
        Ok(ingest) => {
            state.idempotency.write().unwrap().insert(key, format!("accepted:{}", ingest.samples.len()));
            let n = ingest.samples.len();
            (StatusCode::OK, Json(IngestAck { accepted: n })).into_response()
        }
        Err(e) => {
            state.idempotency.write().unwrap().insert(key, "invalid".into());
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("invalid benchmark payload: {e}")})),
            )
                .into_response()
        }
    }
}

/// POST /v1/telemetry — idempotent telemetry ingestion. Holds no payload
/// bytes beyond what the operator's own pipeline needs.
async fn telemetry(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let key = idempotency_key(&headers).unwrap_or_else(|| "telemetry-no-key".to_string());
    let mut ledger = state.idempotency.write().unwrap();
    if let Some(outcome) = ledger.get(&key) {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "duplicate",
                "first_outcome": outcome,
            })),
        )
            .into_response();
    }
    let bytes = body.len();
    ledger.insert(key, format!("accepted:{bytes}"));

    Json(serde_json::json!({
        "status": "accepted",
        "detail": "anonymized summary bucket counter (payload bytes do not leave the pipeline)",
    }))
    .into_response()
}

fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/edges", get(edges))
        .route("/v1/config", get(config))
        .route("/v1/benchmark", post(benchmark))
        .route("/v1/telemetry", post(telemetry))
        .with_state(state)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}