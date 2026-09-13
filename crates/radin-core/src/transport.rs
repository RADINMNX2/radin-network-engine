//! Transport abstraction at the core level (spec 2, 3).
//!
//! Selection is capability- and benchmark-driven. The core makes **no**
//! assumption that QUIC is always faster. Actual socket implementations live
//! in `radin-transport`; here we define the capability model and the
//! benchmark state machine that decides *which* transport deserves the
//! user's traffic right now.

use serde::{Deserialize, Serialize};

use crate::model::TransportKind;

/// Honest, static capability model per transport kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransportCapabilities {
    pub kind: TransportKind,
    /// Typical first-byte/handshake setup cost in ms (used only as a weak
    /// prior when no benchmark data exists, never as ground truth).
    pub setup_latency_ms: u64,
    /// Connection migration support (QUIC yes, UDP no, etc).
    pub supports_connection_migration: bool,
    /// Independent stream multiplexing (one lost packet doesn't block other
    /// streams at the app layer — spec 3 note).
    pub stream_multiplexing: bool,
    /// End-to-end reliability at the transport layer.
    pub reliable: bool,
    /// Encryption at rest-in-transit for the control segment.
    pub encrypted: bool,
    /// ~Per-packet overhead in bytes (documented, not exact).
    pub overhead_bytes: usize,
}

pub fn capabilities(kind: TransportKind) -> TransportCapabilities {
    match kind {
        TransportKind::Quic => TransportCapabilities {
            kind,
            setup_latency_ms: 60,
            supports_connection_migration: true,
            stream_multiplexing: true,
            reliable: true,
            encrypted: true,
            overhead_bytes: 40,
        },
        TransportKind::Udp => TransportCapabilities {
            kind,
            setup_latency_ms: 0,
            supports_connection_migration: false,
            stream_multiplexing: false,
            reliable: false,
            encrypted: false,
            overhead_bytes: 8,
        },
        TransportKind::TcpTls => TransportCapabilities {
            kind,
            setup_latency_ms: 80,
            supports_connection_migration: false,
            stream_multiplexing: false,
            reliable: true,
            encrypted: true,
            overhead_bytes: 20,
        },
        TransportKind::Http2 => TransportCapabilities {
            kind,
            setup_latency_ms: 90,
            supports_connection_migration: false,
            stream_multiplexing: true,
            reliable: true,
            encrypted: true,
            overhead_bytes: 32,
        },
        TransportKind::WebSocketTls => TransportCapabilities {
            kind,
            setup_latency_ms: 100,
            supports_connection_migration: false,
            stream_multiplexing: false,
            reliable: true,
            encrypted: true,
            overhead_bytes: 28,
        },
        TransportKind::Direct => TransportCapabilities {
            kind,
            setup_latency_ms: 0,
            supports_connection_migration: false,
            stream_multiplexing: false,
            reliable: false,
            encrypted: false,
            overhead_bytes: 0,
        },
    }
}

/// Compiled benchmark measurements (spec 2: handshake latency, sustained
/// RTT, jitter, packet loss, reconnect time, throughput, stability).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub transport: TransportKind,
    pub handshake_latency_ms: f64,
    pub sustained_rtt_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub reconnect_time_ms: f64,
    /// Throughput in bits/s (0 = not measured / unexercised).
    pub throughput_bps: f64,
    /// % of probe sessions that survived a 30s stability window.
    pub stability: f64,
    pub source: crate::model::MeasuredSource,
}

/// The benchmark phase machine (spec 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkPhase {
    NotStarted,
    Handshake,
    Warmup,
    RttSampling,
    Throughput,
    Stability,
    Done,
}

impl BenchmarkPhase {
    pub fn next(self) -> BenchmarkPhase {
        match self {
            BenchmarkPhase::NotStarted => BenchmarkPhase::Handshake,
            BenchmarkPhase::Handshake => BenchmarkPhase::Warmup,
            BenchmarkPhase::Warmup => BenchmarkPhase::RttSampling,
            BenchmarkPhase::RttSampling => BenchmarkPhase::Throughput,
            BenchmarkPhase::Throughput => BenchmarkPhase::Stability,
            BenchmarkPhase::Stability => BenchmarkPhase::Done,
            BenchmarkPhase::Done => BenchmarkPhase::Done,
        }
    }
}

/// Transport selection from a set of benchmark results.
///
/// Selection minimizes the *weighted* score already computed by `scoring`
/// against the benchmark's measured RTT/jitter/loss; it never defaults to
/// QUIC. Unmeasured transports rank below measured ones.
pub fn select_transport(results: &[BenchmarkResult]) -> Option<TransportKind> {
    if results.is_empty() {
        return None;
    }
    let mut best: Option<(f64, TransportKind)> = None;
    for r in results {
        if r.source != crate::model::MeasuredSource::Real {
            continue;
        }
        let metrics = crate::model::MetricsSnapshot {
            latency_ms: r.sustained_rtt_ms,
            jitter_ms: r.jitter_ms,
            packet_loss_ratio: r.packet_loss_ratio,
            stability: r.stability,
            handshake_latency_ms: r.handshake_latency_ms,
            reconnect_rate: if r.reconnect_time_ms <= 0.0 { 0.0 } else { 60_000.0 / r.reconnect_time_ms.max(1.0) },
        };
        let (score, _) = crate::scoring::score_snapshot(&crate::scoring::ScoreWeights::default(), &metrics);
        match best {
            None => best = Some((score, r.transport)),
            Some((prev, _)) if score > prev => best = Some((score, r.transport)),
            _ => {}
        }
    }
    best.map(|(_, t)| t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MeasuredSource;

    fn result(t: TransportKind, rtt: f64, jit: f64, loss: f64, stability: f64) -> BenchmarkResult {
        BenchmarkResult {
            transport: t,
            handshake_latency_ms: 60.0,
            sustained_rtt_ms: rtt,
            jitter_ms: jit,
            packet_loss_ratio: loss,
            reconnect_time_ms: 300.0,
            throughput_bps: 0.0,
            stability,
            source: MeasuredSource::Real,
        }
    }

    #[test]
    fn selection_does_not_bias_toward_quic() {
        // UDP measured fast + stable; QUIC measured jittery + lossy.
        let results = vec![
            result(TransportKind::Quic, 60.0, 40.0, 0.05, 0.6),
            result(TransportKind::Udp, 75.0, 2.0, 0.0, 0.95),
        ];
        assert_eq!(select_transport(&results), Some(TransportKind::Udp));
    }

    #[test]
    fn quic_wins_when_it_actually_wins() {
        let results = vec![
            result(TransportKind::Quic, 40.0, 2.0, 0.0, 0.98),
            result(TransportKind::TcpTls, 90.0, 30.0, 0.03, 0.7),
        ];
        assert_eq!(select_transport(&results), Some(TransportKind::Quic));
    }

    #[test]
    fn synthetic_results_are_ignored() {
        let mut fake = result(TransportKind::Udp, 0.1, 0.1, 0.0, 1.0);
        fake.source = MeasuredSource::Synthetic;
        let results = vec![fake, result(TransportKind::Udp, 50.0, 5.0, 0.0, 0.9)];
        assert_eq!(select_transport(&results), Some(TransportKind::Udp));
    }

    #[test]
    fn empty_input_selects_nothing() {
        assert_eq!(select_transport(&[]), None);
    }

    #[test]
    fn capabilities_are_steady() {
        let quic = capabilities(TransportKind::Quic);
        assert!(quic.supports_connection_migration);
        assert!(quic.stream_multiplexing);
        assert!(quic.reliable);
        let udp = capabilities(TransportKind::Udp);
        assert!(!udp.reliable);
        assert_eq!(udp.overhead_bytes, 8);
    }
}