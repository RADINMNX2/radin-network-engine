//! Server state: the edge catalog for the reference control API.
//!
//! Stateless by design: everything here is a read-model that deployments
//! feed from their own (provider-neutral) fleet configuration. The server
//! never holds user traffic.

use std::sync::{Arc, RwLock};

use radin_core::model::{EdgeInfo, TransportKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeHealthStatus {
    pub id: String,
    pub status: String,
    pub region: String,
    pub version: String,
    pub load: f32,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub edges: Arc<RwLock<Vec<EdgeInfo>>>,
    pub health: Arc<RwLock<Vec<EdgeHealthStatus>>>,
    pub protocol_version: u32,
    /// In-memory idempotency ledger (spec 16). A real deployment would back
    /// this with a KV store; the behavior contract is identical.
    pub idempotency: Arc<RwLock<std::collections::HashMap<String, String>>>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            edges: Arc::new(RwLock::new(vec![])),
            health: Arc::new(RwLock::new(vec![])),
            protocol_version: 1,
            idempotency: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }
}

impl ServerState {
    pub fn new_with_edges(edges: Vec<EdgeInfo>) -> Self {
        let state = Self::default();
        *state.edges.write().unwrap() = edges;
        state
    }

    pub fn edge_list(&self) -> Vec<EdgeInfo> {
        self.edges.read().unwrap().clone()
    }

    pub fn primary_transports(&self) -> Vec<TransportKind> {
        vec![
            TransportKind::Quic,
            TransportKind::Udp,
            TransportKind::TcpTls,
            TransportKind::WebSocketTls,
        ]
    }

    /// Health endpoint payload (spec 32) — deliberately minimal, no infra
    /// details (no IPs of internal load balancers, no capacity cash).
    pub fn health_summary(&self, region: &str) -> Option<EdgeHealthStatus> {
        self.health
            .read()
            .unwrap()
            .iter()
            .find(|h| h.region == region)
            .cloned()
    }
}

/// The canonical set of reference edges (spec 31: provider-neutral names).
pub fn reference_edges(now: u64) -> Vec<EdgeInfo> {
    vec![
        edge("edge-asia-1", "asia", "edge-asia-1.radin.example:443", now + 86_400_000),
        edge("edge-mideast-1", "middle-east", "edge-mideast-1.radin.example:443", now + 86_400_000),
        edge("edge-eu-1", "europe", "edge-eu-1.radin.example:443", now + 86_400_000),
        edge("edge-na-1", "north-america", "edge-na-1.radin.example:443", now + 86_400_000),
    ]
}

fn edge(id: &str, region: &str, address: &str, expires: u64) -> EdgeInfo {
    EdgeInfo {
        id: id.into(),
        region: region.into(),
        address: address.into(),
        supported_transports: vec![
            TransportKind::Quic,
            TransportKind::Udp,
            TransportKind::TcpTls,
            TransportKind::WebSocketTls,
        ],
        priority: None,
        expires_at: expires,
        signature_b64: None,
    }
}