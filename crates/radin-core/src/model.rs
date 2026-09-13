//! Core domain model: transports, edges, route candidates, measurements.
//!
//! All measurement-carrying types expose a `source: MeasuredSource` flag so
//! the system can never confuse synthetic chaos-harness data with real
//! telemetry (spec 39).

use serde::{Deserialize, Serialize};

use crate::TimestampMs;

/// Origin of a measurement. `Synthetic` values are TEST ONLY and must be
/// dropped at any production telemetry boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasuredSource {
    /// Produced by real probes / observations on device.
    Real,
    /// Produced by the chaos harness or synthetic drives. Never accepted by
    /// production sinks.
    Synthetic,
}

/// Network types the engine distinguishes (spec 25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkType {
    Wifi,
    /// 5G NR / NR-NSA.
    FiveG,
    FourG,
    ThreeG,
    Ethernet,
    /// Bluetooth PAN, hotspot, wired-but-unknown, etc.
    Other,
    Unknown,
}

impl NetworkType {
    /// Default keepalive base-interval hints per network, in ms.
    /// These are *starting points for benchmarking*, never hard guarantees
    /// (spec 25: "Do not hard-code assumptions. Benchmark and adapt.").
    pub fn default_keepalive_base_ms(self) -> u64 {
        match self {
            NetworkType::Wifi => 25_000,
            NetworkType::FiveG | NetworkType::FourG => 15_000,
            NetworkType::ThreeG => 10_000,
            NetworkType::Ethernet => 30_000,
            NetworkType::Other | NetworkType::Unknown => 20_000,
        }
    }
}

/// Transport abstraction (spec 2, 6). Does NOT assume QUIC is always best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Quic,
    Udp,
    TcpTls,
    Http2,
    WebSocketTls,
    /// Raw best-effort routing with no optimizer edge. Used for fail-safe
    /// (spec 35).
    Direct,
}

impl TransportKind {
    /// Default fallback order (spec 6). The engine may re-order dynamically
    /// based on measured capability scores.
    pub const FALLBACK_ORDER: [TransportKind; 6] = [
        TransportKind::Quic,
        TransportKind::Udp,
        TransportKind::TcpTls,
        TransportKind::Http2,
        TransportKind::WebSocketTls,
        TransportKind::Direct,
    ];

    /// True when this kind can terminate on an optimizer edge.
    pub fn uses_edge(self) -> bool {
        !matches!(self, TransportKind::Direct)
    }

    /// True when the transport is not end-to-end reliable (spec 4).
    pub fn is_best_effort(self) -> bool {
        matches!(self, TransportKind::Udp | TransportKind::Quic)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Quic => "quic",
            TransportKind::Udp => "udp",
            TransportKind::TcpTls => "tcp_tls",
            TransportKind::Http2 => "http2",
            TransportKind::WebSocketTls => "websocket_tls",
            TransportKind::Direct => "direct",
        }
    }
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lightweight connective-tissue measurement, carried by every model type.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub availability: f64,
    pub source: MeasuredSource,
}

/// A scored route candidate (spec 10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteCandidate {
    pub id: String,
    pub endpoint: String,
    pub transport: TransportKind,
    pub region: String,
    /// Current best-known one-way/RTT latency in ms.
    pub latency_ms: f64,
    /// Standard deviation of latency samples in ms.
    pub jitter_ms: f64,
    /// Ratio in [0, 1]. 0.0 = perfect, 0.1 = 10% loss.
    pub packet_loss_ratio: f64,
    /// [0, 1] stability of the route across the observation window.
    pub stability: f64,
    /// Handshake latency in ms.
    pub handshake_latency_ms: f64,
    /// Reconnects per hour (lower is better).
    pub reconnect_rate: f64,
    /// Computed by `scoring`; populated by `score_all`.
    pub score: Option<f64>,
    pub source: MeasuredSource,
}

impl RouteCandidate {
    pub fn metrics(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            latency_ms: self.latency_ms,
            jitter_ms: self.jitter_ms,
            packet_loss_ratio: self.packet_loss_ratio,
            stability: self.stability,
            handshake_latency_ms: self.handshake_latency_ms,
            reconnect_rate: self.reconnect_rate,
        }
    }
}

/// A compact snapshot of the metrics that matter for decisions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub stability: f64,
    pub handshake_latency_ms: f64,
    pub reconnect_rate: f64,
}

/// Server-issued edge candidate (spec 33).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeInfo {
    pub id: String,
    pub region: String,
    pub address: String,
    pub supported_transports: Vec<TransportKind>,
    /// Priority: lower number = preferred. Optional.
    pub priority: Option<i32>,
    /// Epoch ms after which this entry must be discarded.
    pub expires_at: TimestampMs,
    /// Optional signature over the canonical JSON of this entry.
    pub signature_b64: Option<String>,
}

/// Edge runtime state (spec 8): each edge keeps its own rolling metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeHealth {
    pub edge_id: String,
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub availability: f64,
    pub handshake_latency_ms: f64,
    pub reconnect_rate: f64,
    /// Historical stability index in [0, 1].
    pub historical_stability: f64,
    /// Derived Edge Health Score in [0, 100] (100 = best).
    pub score: f64,
    pub updated_at: TimestampMs,
    pub source: MeasuredSource,
}

/// Battery states for keepalive adaptation (spec 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryState {
    Charging,
    High,
    Medium,
    Low,
    Critical,
}

/// VPN states observed by the engine (spec 7, 22, 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnState {
    Connected,
    Connecting,
    Disconnected,
    Error,
}

/// Policy that governs the data plane when no optimizer edge is reachable
/// (spec 35).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataPlanePolicy {
    /// Fail safe: allow direct connection when every edge fails.
    AllowDirectFallback,
    /// Strict: block the tunnel rather than leak traffic off-edge.
    StrictVpnOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_type_has_benchmarked_keepalive_hints() {
        assert!(
            NetworkType::Wifi.default_keepalive_base_ms()
                > NetworkType::FourG.default_keepalive_base_ms()
        );
    }

    #[test]
    fn fallback_order_ends_in_direct() {
        assert_eq!(
            *TransportKind::FALLBACK_ORDER.last().unwrap(),
            TransportKind::Direct
        );
        assert!(!TransportKind::Direct.uses_edge());
    }

    #[test]
    fn route_candidate_exposes_snapshot() {
        let c = RouteCandidate {
            id: "r1".into(),
            endpoint: "edge-a.example".into(),
            transport: TransportKind::Quic,
            region: "ap-sg".into(),
            latency_ms: 65.0,
            jitter_ms: 30.0,
            packet_loss_ratio: 0.02,
            stability: 0.7,
            handshake_latency_ms: 120.0,
            reconnect_rate: 0.1,
            score: None,
            source: MeasuredSource::Real,
        };
        let m = c.metrics();
        assert_eq!(m.latency_ms, 65.0);
    }
}
