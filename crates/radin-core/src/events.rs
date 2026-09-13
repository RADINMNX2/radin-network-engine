//! Structured observability events (spec 29).
//!
//! Every event carries: timestamp, network, route, transport, metrics,
//! reason. Event names follow a fixed vocabulary so dashboards and the
//! diagnostic report stay stable.

use serde::{Deserialize, Serialize};

use crate::model::{MetricsSnapshot, NetworkType, TransportKind};
use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    NetworkChanged,
    EdgeSelected,
    EdgeDegraded,
    EdgeFailed,
    EdgeRecovered,
    TransportChanged,
    VpnConnected,
    VpnDisconnected,
    RouteChanged,
    DnsFailure,
    HandshakeFailure,
    PacketLossSpike,
    JitterSpike,
    LatencySpike,
    CircuitOpen,
    CircuitClosed,
    SessionResumed,
    NetworkMigration,
    TransportFallback,
    SyncCompleted,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::NetworkChanged => "NETWORK_CHANGED",
            EventKind::EdgeSelected => "EDGE_SELECTED",
            EventKind::EdgeDegraded => "EDGE_DEGRADED",
            EventKind::EdgeFailed => "EDGE_FAILED",
            EventKind::EdgeRecovered => "EDGE_RECOVERED",
            EventKind::TransportChanged => "TRANSPORT_CHANGED",
            EventKind::VpnConnected => "VPN_CONNECTED",
            EventKind::VpnDisconnected => "VPN_DISCONNECTED",
            EventKind::RouteChanged => "ROUTE_CHANGED",
            EventKind::DnsFailure => "DNS_FAILURE",
            EventKind::HandshakeFailure => "HANDSHAKE_FAILURE",
            EventKind::PacketLossSpike => "PACKET_LOSS_SPIKE",
            EventKind::JitterSpike => "JITTER_SPIKE",
            EventKind::LatencySpike => "LATENCY_SPIKE",
            EventKind::CircuitOpen => "CIRCUIT_OPEN",
            EventKind::CircuitClosed => "CIRCUIT_CLOSED",
            EventKind::SessionResumed => "SESSION_RESUMED",
            EventKind::NetworkMigration => "NETWORK_MIGRATION",
            EventKind::TransportFallback => "TRANSPORT_FALLBACK",
            EventKind::SyncCompleted => "SYNC_COMPLETED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub timestamp: TimestampMs,
    pub network: NetworkType,
    pub route: Option<String>,
    pub transport: Option<TransportKind>,
    pub metrics: Option<MetricsSnapshot>,
    pub reason: String,
    /// Optional severity hint for dashboards.
    #[serde(default)]
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Info,
    Warning,
    Critical,
}

impl Event {
    pub fn new(kind: EventKind, now: TimestampMs) -> Self {
        Self {
            kind,
            timestamp: now,
            network: NetworkType::Unknown,
            route: None,
            transport: None,
            metrics: None,
            reason: String::new(),
            severity: Severity::Info,
        }
    }

    pub fn with_network(mut self, n: NetworkType) -> Self {
        self.network = n;
        self
    }
    pub fn with_route(mut self, r: impl Into<String>) -> Self {
        self.route = Some(r.into());
        self
    }
    pub fn with_transport(mut self, t: TransportKind) -> Self {
        self.transport = Some(t);
        self
    }
    pub fn with_metrics(mut self, m: MetricsSnapshot) -> Self {
        self.metrics = Some(m);
        self
    }
    pub fn with_reason(mut self, r: impl Into<String>) -> Self {
        self.reason = r.into();
        self
    }
    pub fn with_severity(mut self, s: Severity) -> Self {
        self.severity = s;
        self
    }
}

/// Severity defaults by kind (so callers don't have to pick).
pub fn default_severity(kind: EventKind) -> Severity {
    match kind {
        EventKind::CircuitOpen
        | EventKind::EdgeFailed
        | EventKind::DnsFailure
        | EventKind::HandshakeFailure
        | EventKind::PacketLossSpike
        | EventKind::JitterSpike
        | EventKind::LatencySpike => Severity::Warning,
        EventKind::EdgeDegraded | EventKind::TransportFallback => Severity::Warning,
        EventKind::VpnDisconnected => Severity::Warning,
        _ => Severity::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_carries_full_context() {
        let e = Event::new(EventKind::EdgeSelected, 1234)
            .with_network(NetworkType::FiveG)
            .with_route("Edge-SG-01")
            .with_transport(TransportKind::Quic)
            .with_reason("best score")
            .with_severity(Severity::Info);
        assert_eq!(e.kind.as_str(), "EDGE_SELECTED");
        assert_eq!(e.network, NetworkType::FiveG);
        assert_eq!(e.route.as_deref(), Some("Edge-SG-01"));
    }

    #[test]
    fn fixed_vocabulary_is_stable() {
        assert_eq!(EventKind::CircuitOpen.as_str(), "CIRCUIT_OPEN");
        assert_eq!(EventKind::RouteChanged.as_str(), "ROUTE_CHANGED");
        assert_eq!(EventKind::VpnConnected.as_str(), "VPN_CONNECTED");
    }

    #[test]
    fn vpn_state_roundtrips() {
        use crate::model::VpnState;
        let v = VpnState::Connected;
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<VpnState>(&s).unwrap(), v);
    }
}
