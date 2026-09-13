//! Rolling telemetry buffer (spec 22) + exportable local diagnostic report
//! (spec 30).
//!
//! The buffer holds recent samples of the *governing* metrics — never packet
//! payloads, credentials, or arbitrary game traffic. The diagnostic report is
//! derived from `MeasuredSource::Real` data only.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::events::{Event, EventKind};
use crate::metrics::ensure_real;
use crate::model::{MeasuredSource, NetworkType, RouteCandidate, TransportKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySample {
    pub at: u64,
    pub network: NetworkType,
    pub route: Option<String>,
    pub transport: Option<TransportKind>,
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub source: MeasuredSource,
}

#[derive(Debug, Clone)]
pub struct TelemetryBuffer {
    pub capacity: usize,
    pub samples: VecDeque<TelemetrySample>,
    pub events: VecDeque<Event>,
}

impl Default for TelemetryBuffer {
    fn default() -> Self {
        Self::new(600)
    }
}

impl TelemetryBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            samples: VecDeque::with_capacity(capacity.max(1)),
            events: VecDeque::new(),
        }
    }

    /// Push a sample. Enforces the honesty boundary: synthetic data is
    /// refused (chaos harness must use `TelemetryBuffer::synthetic_channel`).
    pub fn push(&mut self, sample: TelemetrySample) -> crate::Result<()> {
        ensure_real(sample.source)?;
        if self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        Ok(())
    }

    /// TEST-ONLY channel. Never wired to a production sink (checked again on
    /// export).
    pub fn push_synthetic(&mut self, sample: TelemetrySample) {
        debug_assert_eq!(sample.source, MeasuredSource::Synthetic);
        if self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn push_event(&mut self, event: Event) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Reject a buffer that somehow contains synthetic data.
    pub fn is_pristine(&self) -> bool {
        self.samples
            .iter()
            .all(|s| s.source == MeasuredSource::Real)
    }
}

/// The locally-exportable diagnostic report (spec 30). Nothing sensitive:
/// no payloads, no credentials, no user data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub generated_at: u64,
    pub network: Option<NetworkType>,
    pub transport: Option<TransportKind>,
    pub selected_edge: Option<String>,
    pub rtt_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub packet_loss_pct: Option<f64>,
    pub reconnects: u64,
    pub route_changes: u64,
    pub transport_fallbacks: u64,
    pub circuit_trips: u64,
    pub handshake_failures: u64,
    pub dns_failures: u64,
    pub source: MeasuredSource,
}

pub struct DiagnosticBuilder {
    pub source: MeasuredSource,
}

impl DiagnosticBuilder {
    pub fn new(source: MeasuredSource) -> Self {
        Self { source }
    }

    /// A single positional call keeps counter plumbing auditable end-to-end;
    /// the builder *is* the grouping object, so a struct of 7 counters would
    /// just defer the same verbosity to the engine call site.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &self,
        generated_at: u64,
        latest: Option<&TelemetrySample>,
        candidate: Option<&RouteCandidate>,
        event_log: &[Event],
        reconnects: u64,
        route_changes: u64,
        transport_fallbacks: u64,
        circuit_trips: u64,
    ) -> crate::Result<DiagnosticReport> {
        // HONESTY RULE (spec 41): we must never present better numbers than
        // we measured. If source is synthetic, refuse to build a production
        // report (chaos tests use the report-shaped struct directly).
        ensure_real(self.source)?;
        let handshake_failures = event_log
            .iter()
            .filter(|e| e.kind == EventKind::HandshakeFailure)
            .count() as u64;
        let dns_failures = event_log
            .iter()
            .filter(|e| e.kind == EventKind::DnsFailure)
            .count() as u64;

        Ok(DiagnosticReport {
            generated_at,
            network: latest.map(|l| l.network),
            transport: latest
                .and_then(|l| l.transport)
                .or(candidate.map(|c| c.transport)),
            selected_edge: candidate
                .map(|c| c.id.clone())
                .or(latest.and_then(|l| l.route.clone())),
            rtt_ms: latest
                .map(|l| l.latency_ms)
                .or(candidate.map(|c| c.latency_ms)),
            jitter_ms: latest.map(|l| l.jitter_ms),
            packet_loss_pct: latest.map(|l| l.packet_loss_ratio * 100.0),
            reconnects,
            route_changes,
            transport_fallbacks,
            circuit_trips,
            handshake_failures,
            dns_failures,
            source: self.source,
        })
    }
}

/// Plaintext rendering for the "show the truth" diagnostic (spec 30 example).
impl DiagnosticReport {
    pub fn render_plaintext(&self) -> String {
        let mut o = String::new();
        o.push_str("RADIN Network Diagnostic Report\n");
        o.push_str("===============================\n");
        if let Some(n) = self.network {
            o.push_str(&format!("Network:                {:?}\n", n));
        }
        if let Some(t) = self.transport {
            o.push_str(&format!("Transport:              {}\n", t));
        }
        if let Some(edge) = &self.selected_edge {
            o.push_str(&format!("Selected Edge:          {}\n", edge));
        }
        if let Some(rtt) = self.rtt_ms {
            o.push_str(&format!("RTT:                    {:.1} ms\n", rtt));
        }
        if let Some(j) = self.jitter_ms {
            o.push_str(&format!("Jitter:                 {:.1} ms\n", j));
        }
        if let Some(l) = self.packet_loss_pct {
            o.push_str(&format!("Packet Loss:            {:.2}%\n", l));
        }
        o.push_str(&format!("Reconnects:             {}\n", self.reconnects));
        o.push_str(&format!("Route Changes:          {}\n", self.route_changes));
        o.push_str(&format!(
            "Transport Fallbacks:    {}\n",
            self.transport_fallbacks
        ));
        o.push_str(&format!("Circuit Trips:          {}\n", self.circuit_trips));
        o.push_str(&format!(
            "Handshake Failures:     {}\n",
            self.handshake_failures
        ));
        o.push_str(&format!("DNS Failures:           {}\n", self.dns_failures));
        o.push_str("(No packet payloads, credentials, or user data included.)\n");
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: u64, source: MeasuredSource) -> TelemetrySample {
        TelemetrySample {
            at,
            network: NetworkType::FiveG,
            route: Some("Edge-SG-01".into()),
            transport: Some(TransportKind::Quic),
            latency_ms: 72.0,
            jitter_ms: 2.4,
            packet_loss_ratio: 0.003,
            source,
        }
    }

    #[test]
    fn rolling_buffer_drops_oldest() {
        let mut buf = TelemetryBuffer::new(3);
        for i in 0..5 {
            buf.push(sample(i, MeasuredSource::Real)).unwrap();
        }
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.samples.front().unwrap().at, 2);
    }

    #[test]
    fn refuses_synthetic_data_on_production_channel() {
        let mut buf = TelemetryBuffer::new(10);
        assert!(buf.push(sample(0, MeasuredSource::Synthetic)).is_err());
        assert!(buf.is_empty());
    }

    #[test]
    fn report_never_builds_from_synthetic() {
        let b = DiagnosticBuilder::new(MeasuredSource::Synthetic);
        assert!(b.build(0, None, None, &[], 0, 0, 0, 0).is_err());
    }

    #[test]
    fn report_shows_measured_truth() {
        let b = DiagnosticBuilder::new(MeasuredSource::Real);
        let events = vec![
            crate::events::Event::new(EventKind::HandshakeFailure, 1),
            crate::events::Event::new(EventKind::DnsFailure, 2),
        ];
        let rep = b
            .build(
                100,
                Some(&sample(99, MeasuredSource::Real)),
                None,
                &events,
                1,
                2,
                0,
                1,
            )
            .unwrap();
        assert_eq!(rep.rtt_ms, Some(72.0));
        let loss_pct = rep.packet_loss_pct.unwrap();
        assert!(
            (loss_pct - 0.3).abs() < 1e-9,
            "packet loss % honest: {loss_pct}"
        );
        assert_eq!(rep.handshake_failures, 1);
        assert_eq!(rep.dns_failures, 1);
        let text = rep.render_plaintext();
        assert!(text.contains("RTT:                    72.0 ms"));
        assert!(text.contains("Selected Edge:          Edge-SG-01"));
        assert!(!text.contains("password"));
    }
}
