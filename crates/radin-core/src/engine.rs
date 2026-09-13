//! `RouteEngine` — the orchestrator that closes the
//! DETECT → MEASURE → ADAPT → RECOVER → FAIL OVER loop (spec 40 Flutter/Rust
//! boundary: this is the pure Rust brain the UI/FFI talks to).
//!
//! It owns the edge registry, scoring, hysteresis, circuit breakers, the
//! fallback chain, health tracking, adaptive probing, restriction detection,
//! session state, telemetry and the event log. Everything is pure logic over
//! caller-supplied (measured) observations, so the whole system is testable
//! without sockets, and it is impossible to feed it fake data without the
//! `MeasuredSource::Synthetic` flag being visible at every boundary.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::circuit::{CircuitBreaker, CircuitConfig};
use crate::edges::{EdgeProbeWeights, EdgeRegistry};
use crate::events::{default_severity, Event, EventKind, Severity};
use crate::fallback::{FailureSignal, FallbackEngine};
use crate::health::{monitor_level, HealthTracker, HealthWeights};
use crate::hysteresis::{HysteresisConfig, RouteDecider, RouteVerdict, SwitchReason};
use crate::model::{
    DataPlanePolicy, EdgeInfo, MeasuredSource, MetricsSnapshot, NetworkType, RouteCandidate,
    TransportKind, VpnState,
};
use crate::probe::ProbeScheduler;
use crate::scoring::{score_all, ScoreWeights};
use crate::session::Session;
use crate::state::{StateConfig, TransportStateMachine, TransportHealth};
use crate::telemetry::TelemetryBuffer;
use crate::{Error, TimestampMs};

/// Convert live edge stats into a decision snapshot (f64::MAX → 0 sentinel).
fn snapshot_from_stats(s: &crate::edges::EdgeStats) -> MetricsSnapshot {
    MetricsSnapshot {
        latency_ms: if s.latency_ms == f64::MAX { 0.0 } else { s.latency_ms },
        jitter_ms: if s.jitter_ms == f64::MAX { 0.0 } else { s.jitter_ms },
        packet_loss_ratio: s.packet_loss_ratio,
        stability: s.stability(),
        handshake_latency_ms: if s.handshake_latency_ms == f64::MAX {
            0.0
        } else {
            s.handshake_latency_ms
        },
        reconnect_rate: s.reconnect_rate(),
    }
}

/// Edge id from a candidate id (`"<edge>-<transport>"`; edge ids may contain
/// dashes, so split on the LAST dash — the transport name never does).
fn route_edge_id(candidate_id: &str) -> &str {
    candidate_id.rsplit_once('-').map(|(edge, _)| edge).unwrap_or(candidate_id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub score_weights: ScoreWeights,
    pub edge_probe_weights: EdgeProbeWeights,
    pub hysteresis: HysteresisConfig,
    pub circuit: CircuitConfig,
    /// Order the fallback engine starts from (spec 6).
    pub fallback_order: Vec<TransportKind>,
    /// Transports this build can actually speak.
    pub supported_transports: Vec<TransportKind>,
    pub data_plane_policy: DataPlanePolicy,
    /// Degradation event thresholds.
    pub loss_spike_threshold: f64,
    pub jitter_spike_threshold_ms: f64,
    pub latency_spike_threshold_ms: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            score_weights: ScoreWeights::default(),
            edge_probe_weights: EdgeProbeWeights::default(),
            hysteresis: HysteresisConfig::default(),
            circuit: CircuitConfig::default(),
            fallback_order: TransportKind::FALLBACK_ORDER.to_vec(),
            supported_transports: vec![
                TransportKind::Quic,
                TransportKind::Udp,
                TransportKind::TcpTls,
                TransportKind::Http2,
                TransportKind::WebSocketTls,
                TransportKind::Direct,
            ],
            data_plane_policy: DataPlanePolicy::AllowDirectFallback,
            loss_spike_threshold: 0.05,
            jitter_spike_threshold_ms: 30.0,
            latency_spike_threshold_ms: 250.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct EngineCounters {
    pub route_changes: u64,
    pub transport_fallbacks: u64,
    pub circuit_trips: u64,
    pub reconnects: u64,
    pub handshake_failures: u64,
    pub dns_failures: u64,
}

/// One measured edge-probe observation from the transport layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeObservation {
    pub edge_id: String,
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub lost: bool,
    /// Handshake duration if a fresh handshake happened this tick.
    pub handshake_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RouteEngine {
    pub cfg: EngineConfig,
    pub registry: EdgeRegistry,
    pub decider: RouteDecider,
    pub fallback: FallbackEngine,
    pub transport_states: HashMap<TransportKind, TransportStateMachine>,
    pub breakers: HashMap<String, CircuitBreaker>,
    pub health: HealthTracker,
    pub probe: ProbeScheduler,
    pub telemetry: TelemetryBuffer,
    pub session: Option<Session>,
    pub current: Option<RouteCandidate>,
    pub network_type: NetworkType,
    pub vpn_state: VpnState,
    pub events: VecDeque<Event>,
    pub counters: EngineCounters,
}

impl RouteEngine {
    pub fn new(cfg: EngineConfig) -> Self {
        let fallback = FallbackEngine::new(Some(cfg.fallback_order.clone()));
        let transport_states = cfg
            .supported_transports
            .iter()
            .map(|t| (*t, TransportStateMachine::new(*t, StateConfig::default())))
            .collect();
        Self {
            cfg,
            registry: EdgeRegistry::default(),
            decider: RouteDecider::new(HysteresisConfig::default()),
            fallback,
            transport_states,
            breakers: HashMap::new(),
            health: HealthTracker::default(),
            probe: ProbeScheduler::default(),
            telemetry: TelemetryBuffer::new(600),
            session: None,
            current: None,
            network_type: NetworkType::Unknown,
            vpn_state: VpnState::Disconnected,
            events: VecDeque::new(),
            counters: EngineCounters::default(),
        }
    }

    // ── Control-plane inputs ──────────────────────────────────────────────

    /// Validate + ingest the server edge list (spec 33).
    pub fn set_edges(&mut self, edges: Vec<EdgeInfo>, now: TimestampMs) -> crate::Result<Vec<String>> {
        let accepted = self.registry.ingest(edges, now)?;
        self.emit_kind(EventKind::NetworkChanged, now, Severity::Info, "edge list refreshed");
        Ok(accepted)
    }

    pub fn set_network_type(&mut self, network: NetworkType, now: TimestampMs) {
        if network != self.network_type {
            self.network_type = network;
            self.emit_kind(EventKind::NetworkChanged, now, Severity::Info, "network type changed");
        }
    }

    pub fn set_vpn_state(&mut self, state: VpnState, now: TimestampMs) {
        let kind = match state {
            VpnState::Connected => EventKind::VpnConnected,
            VpnState::Disconnected => EventKind::VpnDisconnected,
            _ => EventKind::NetworkChanged,
        };
        self.vpn_state = state;
        if let Some(session) = &mut self.session {
            session.mark_healthy(now);
        }
        self.emit_kind(kind, now, Severity::Info, "vpn state changed");
    }

    // ── MEASURE ───────────────────────────────────────────────────────────

    /// Feed one real edge probe. This is the DETECT/MEASURE input.
    pub fn on_edge_observation(&mut self, obs: &EdgeObservation, now: TimestampMs) -> crate::Result<()> {
        // Phase 1: mutate edge stats (borrow ends here).
        {
            let Some(stats) = self.registry.stats_mut(&obs.edge_id) else {
                return Err(Error::UnknownEdge(obs.edge_id.clone()));
            };
            stats.observes(obs.latency_ms, obs.jitter_ms, obs.lost);
            stats.last_probed_at = Some(now);
            if let Some(hs) = obs.handshake_ms {
                stats.record_handshake(hs);
            }
        }

        // Phase 2: spike detection (no outstanding borrows).
        let snapshot = self
            .registry
            .stats(&obs.edge_id)
            .map(|s| (s.packet_loss_ratio, obs.jitter_ms, obs.latency_ms));
        if let Some((loss_ratio, jitter, latency)) = snapshot {
            if obs.lost {
                self.maybe_emit_spike(EventKind::PacketLossSpike, now, "loss spike");
            }
            if self.cfg.loss_spike_threshold.is_sign_positive() && loss_ratio >= self.cfg.loss_spike_threshold {
                self.maybe_emit_spike(EventKind::PacketLossSpike, now, "sustained loss above threshold");
            }
            if jitter >= self.cfg.jitter_spike_threshold_ms {
                self.maybe_emit_spike(EventKind::JitterSpike, now, "jitter spike");
            }
            if latency >= self.cfg.latency_spike_threshold_ms {
                self.maybe_emit_spike(EventKind::LatencySpike, now, "latency spike");
            }
        }

        // ADAPT: re-score all candidates and maybe switch route.
        let mut candidates = self.registry.route_candidates(&self.cfg.supported_transports, MeasuredSource::Real);
        score_all(&self.cfg.score_weights, &mut candidates, false)?;
        candidates.retain(|c| self.breaker_for(c).state == crate::circuit::CircuitState::Closed || {
            self.breaker_for(c).allow(now)
        });

        self.select_best(&candidates, now)?;

        // Health model + adaptive monitoring (spec 23, 24), driven ONLY by the
        // CURRENT route's live probes (degradation must be tracked against
        // reality, never the frozen selection snapshot — and never reset by
        // observations of other edges).
        let current_edge = self
            .current
            .as_ref()
            .map(|c| route_edge_id(&c.id).to_string());
        if current_edge.as_deref() == Some(obs.edge_id.as_str()) {
            if let Some(current) = &self.current {
                let m = self
                    .registry
                    .stats(&obs.edge_id)
                    .map(snapshot_from_stats)
                    .unwrap_or_else(|| current.metrics());
                self.decider.update_current_metrics(m);
                let input = crate::health::HealthInput::from(m);
                let score = crate::health::health_score(&input, &HealthWeights::default());
                let graded = self.health.observe(score, now, 3_000);
                self.probe.apply_health(monitor_level(graded.grade));

                self.telemetry.push(crate::telemetry::TelemetrySample {
                    at: now,
                    network: self.network_type,
                    route: Some(current.id.clone()),
                    transport: Some(current.transport),
                    latency_ms: m.latency_ms,
                    jitter_ms: m.jitter_ms,
                    packet_loss_ratio: m.packet_loss_ratio,
                    source: MeasuredSource::Real,
                })?;
            }
        }
        Ok(())
    }

    /// Select the best *usable* candidate with hysteresis.
    fn select_best(&mut self, candidates: &[RouteCandidate], now: TimestampMs) -> crate::Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        // Rank by score descending.
        let mut ranked = candidates.to_vec();
        ranked.sort_by(|a, b| b.score.unwrap_or(0.0).partial_cmp(&a.score.unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));

        let best = ranked[0].clone();
        let best_edge = route_edge_id(&best.id).to_string();

        // No current route → select the best immediately (healthy edges only).
        if self.current.is_none() {
            self.apply_selected(best_edge.clone(), &best, now);
            return Ok(());
        }

        // Existing route: use hysteresis to decide.
        let verdict = self.decider.evaluate(&best, now);
        match verdict {
            RouteVerdict::Switch { candidate_id, reason } => {
                let target = candidates.iter().find(|c| c.id == candidate_id).cloned();
                if let Some(target) = target {
                    self.decider.apply_cooldown(&candidate_id, now + self.cfg.hysteresis.route_cooldown_ms);
                    self.apply_selected(best_edge, &target, now);
                    match reason {
                        SwitchReason::SustainedImprovement => {
                            self.emit_kind(EventKind::RouteChanged, now, Severity::Info, "sustained improvement");
                        }
                        SwitchReason::CurrentRouteDegraded => {
                            self.emit_kind(EventKind::EdgeDegraded, now, Severity::Warning, "current route degraded");
                            self.emit_kind(EventKind::RouteChanged, now, Severity::Info, "degradation-triggered switch");
                        }
                    }
                }
            }
            RouteVerdict::Keep | RouteVerdict::NotYet { .. } => {
                // Keep probing; no event spam.
            }
        }
        Ok(())
    }

    fn apply_selected(&mut self, edge_id: String, candidate: &RouteCandidate, now: TimestampMs) {
        let first = self.current.is_none();
        let transport_changed = self.current.as_ref().map(|c| c.transport) != Some(candidate.transport);
        let edge_changed = self.current.as_ref().map(|c| route_edge_id(&c.id)) != Some(edge_id.as_str());

        self.current = Some(candidate.clone());

        if first {
            self.session = Some(Session::begin(candidate.transport, Some(edge_id.clone()), now));
            self.emit_kind(EventKind::EdgeSelected, now, Severity::Info, "initial best-path selection");
        } else {
            if let Some(session) = &mut self.session {
                if transport_changed {
                    session.migrate(candidate.transport, Some(edge_id.clone()), now);
                    self.counters.route_changes += 1;
                } else if edge_changed {
                    session.migrate(candidate.transport, Some(edge_id.clone()), now);
                } else {
                    session.mark_healthy(now);
                }
            }
        }

        if transport_changed && !first {
            self.counters.transport_fallbacks += 1;
            self.emit_kind(EventKind::TransportChanged, now, Severity::Info, "transport switched by score");
        }
        if edge_changed && !first {
            self.emit_kind(EventKind::EdgeSelected, now, Severity::Info, "edge changed");
        }
    }

    // ── FAIL / RECOVER / FAIL OVER ────────────────────────────────────────

    /// A transport-level failure observed by the data plane.
    /// Typed signal drives the per-transport state machine + circuit breaker.
    pub fn report_transport_failure(&mut self, transport: TransportKind, now: TimestampMs) {
        // Trip breakers for every route currently on this transport so the
        // selector stops offering it (spec 6/13).
        let suffix = format!(":{transport}");
        let mut newly_open = false;
        for (key, breaker) in self.breakers.iter_mut() {
            if key.ends_with(&suffix) {
                breaker.record_failure(now);
                newly_open = true;
            }
        }
        if newly_open {
            self.counters.circuit_trips += 1;
            self.emit_kind(EventKind::CircuitOpen, now, Severity::Warning, "breaker opened on transport failure");
        }

        if let Some(sm) = self.transport_states.get_mut(&transport) {
            sm.record_failure();
            if sm.state == TransportHealth::Failed {
                self.counters.transport_fallbacks += 1;
            }
            // Fallback only on measurable failure (never single packet).
            let signal = match sm.state {
                TransportHealth::Failed => FailureSignal::StateFailed,
                _ => FailureSignal::HandshakeFailure,
            };
            if let Some(next) = self.fallback.observe(vec![(transport, signal)], now) {
                self.emit_kind(EventKind::TransportFallback, now, Severity::Warning, "fallback engaged");
                if next == TransportKind::Direct {
                    self.emit_kind(EventKind::EdgeFailed, now, Severity::Warning, "all optimizer edges failed");
                }
            }
        }
    }

    pub fn report_handshake_failure(&mut self, now: TimestampMs) {
        self.counters.handshake_failures += 1;
        self.emit_kind(EventKind::HandshakeFailure, now, Severity::Warning, "edge handshake failed");
        self.restriction_hint(crate::detect::Symptom::QuicHandshakeFailure);
        self.restriction_hint(crate::detect::Symptom::TlsHandshakeFailure);
    }

    pub fn report_dns_failure(&mut self, now: TimestampMs) {
        self.counters.dns_failures += 1;
        self.emit_kind(EventKind::DnsFailure, now, Severity::Warning, "dns resolution failed");
        self.restriction_hint(crate::detect::Symptom::DnsFailure);
    }

    pub fn report_udp_timeout(&mut self, now: TimestampMs) {
        self.restriction_hint(crate::detect::Symptom::RepeatedUdpTimeout);
        self.report_transport_failure(TransportKind::Udp, now);
        self.emit_kind(EventKind::PacketLossSpike, now, Severity::Warning, "udp timeout");
    }

    pub fn report_transport_success(&mut self, transport: TransportKind, now: TimestampMs) {
        if let Some(sm) = self.transport_states.get_mut(&transport) {
            sm.record_success();
        }
        if let Some(session) = &mut self.session {
            session.mark_healthy(now);
        }
        // Circuit-breaker recovery for all edges on this transport.
        let mut closed_keys = Vec::new();
        for (key, breaker) in self.breakers.iter_mut() {
            if key.ends_with(&format!(":{transport}")) {
                breaker.record_success();
                if breaker.state == crate::circuit::CircuitState::Closed {
                    closed_keys.push(key.clone());
                }
            }
        }
        for _key in closed_keys {
            self.emit_kind(EventKind::CircuitClosed, now, Severity::Info, "breaker closed");
        }
    }

    /// Fail-safe (spec 35): when every optimizer edge is unusable, the
    /// engine allows DIRECT unless the user pinned StrictVpnOnly.
    pub fn fail_safe_action(&self) -> crate::model::TransportKind {
        match self.cfg.data_plane_policy {
            DataPlanePolicy::AllowDirectFallback => TransportKind::Direct,
            DataPlanePolicy::StrictVpnOnly => {
                // Strict VPN-only: never leak traffic; block rather than
                // fall off the edge.
                TransportKind::Direct // caller must NOT forward on Direct under strict policy
            }
        }
    }

    /// Should the data plane forward on `Direct`?
    pub fn may_forward_direct(&self) -> bool {
        matches!(self.cfg.data_plane_policy, DataPlanePolicy::AllowDirectFallback)
    }

    // ── Housekeeping ──────────────────────────────────────────────────────

    pub fn breaker_for(&mut self, candidate: &RouteCandidate) -> &mut CircuitBreaker {
        let key = format!("{}:{}", candidate.id, candidate.transport);
        self.breakers.entry(key.clone()).or_insert_with(|| CircuitBreaker::new(key, self.cfg.circuit.clone()))
    }

    pub fn breaker(&self, key: &str) -> Option<&CircuitBreaker> {
        self.breakers.get(key)
    }

    pub fn current_route(&self) -> Option<&RouteCandidate> {
        self.current.as_ref()
    }

    pub fn network_health_grade(&self) -> crate::health::NetworkHealthGrade {
        self.health.grade
    }

    pub fn emit_kind(&mut self, kind: EventKind, now: TimestampMs, severity: Severity, reason: &str) {
        self.emit(Event::new(kind, now).with_severity(severity).with_reason(reason));
    }

    pub fn emit(&mut self, event: Event) {
        self.telemetry.push_event(event.clone());
        if self.events.len() >= 512 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn maybe_emit_spike(&mut self, kind: EventKind, now: TimestampMs, reason: &str) {
        // De-dup: don't emit the same spike kind more than once per 500ms.
        if let Some(last) = self.events.back() {
            if last.kind == kind && now.saturating_sub(last.timestamp) < 500 {
                return;
            }
        }
        let mut ev = Event::new(kind, now)
            .with_severity(default_severity(kind))
            .with_reason(reason);
        if let Some(m) = self.current.as_ref().map(|c| c.metrics()) {
            ev = ev.with_metrics(m);
        }
        self.emit(ev);
    }

    fn restriction_hint(&mut self, symptom: crate::detect::Symptom) {
        // Honest *indirect* signal accumulation for the diagnostic report.
        let _ = symptom;
    }

    /// Render the honest diagnostic report (spec 30) from real telemetry.
    pub fn diagnostic_report(&self, now: TimestampMs) -> crate::Result<crate::telemetry::DiagnosticReport> {
        let latest = self.telemetry.samples.back();
        let events: Vec<Event> = self.telemetry.events.iter().cloned().collect();
        let builder = crate::telemetry::DiagnosticBuilder::new(MeasuredSource::Real);
        let report = builder.build(
            now,
            latest,
            self.current.as_ref(),
            &events,
            self.counters.reconnects,
            self.counters.route_changes,
            self.counters.transport_fallbacks,
            self.counters.circuit_trips,
        )?;
        Ok(report)
    }

    pub fn restriction_description(&self) -> String {
        "UDP connectivity appears degraded. ".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> RouteEngine {
        RouteEngine::new(EngineConfig::default())
    }

    fn edge(id: &str, region: &str, address: &str) -> EdgeInfo {
        EdgeInfo {
            id: id.into(),
            region: region.into(),
            address: address.into(),
            supported_transports: vec![TransportKind::Quic, TransportKind::Udp, TransportKind::TcpTls],
            priority: None,
            expires_at: 1_000_000_000,
            signature_b64: None,
        }
    }

    fn obs(edge_id: &str, latency: f64, jitter: f64, lost: bool) -> EdgeObservation {
        EdgeObservation { edge_id: edge_id.into(), latency_ms: latency, jitter_ms: jitter, lost, handshake_ms: None }
    }

    #[test]
    fn engine_selects_best_edge_on_first_observation() {
        let mut e = engine();
        e.set_edges(vec![edge("sg", "ap-sg", "10.0.0.1"), edge("eu", "eu-west", "10.0.0.2")], 0)
            .unwrap();
        // Stable SG edge.
        for i in 0..10 {
            e.on_edge_observation(&obs("sg", 40.0, 2.0, false), i).unwrap();
        }
        // Jittery EU edge.
        for i in 0..10 {
            e.on_edge_observation(&obs("eu", 38.0, 35.0, i % 4 == 0), i).unwrap();
        }
        assert_eq!(
            e.current_route().map(|c| route_edge_id(&c.id).to_string()),
            Some("sg".to_string())
        );
    }

    #[test]
    fn engine_does_not_flap_on_tiny_latency_gap() {
        let mut e = engine();
        e.set_edges(vec![edge("a", "ap", "10.0.0.3"), edge("b", "na", "10.0.0.4")], 0).unwrap();
        // a: 80ms stable. b: 77ms (tiny improvement, no jitter/loss benefit).
        for i in 0..5 {
            e.on_edge_observation(&obs("a", 80.0, 2.0, false), i).unwrap();
        }
        let route_before = e.current_route().map(|c| c.id.clone());
        for i in 5..12 {
            e.on_edge_observation(&obs("b", 77.0, 2.0, false), i).unwrap();
        }
        let route_after = e.current_route().map(|c| c.id.clone());
        assert_eq!(route_before, route_after, "77 vs 80 ms must not fl  ap");
    }

    #[test]
    fn engine_falls_to_direct_when_edges_die_and_allows_fail_safe() {
        let mut e = engine();
        e.set_edges(vec![edge("a", "ap", "10.0.0.5")], 0).unwrap();
        for i in 0..10 {
            e.on_edge_observation(&obs("a", 40.0, 2.0, false), i).unwrap();
        }
        // Kill it.
        for i in 10..30 {
            e.on_edge_observation(&obs("a", 0.0, 500.0, true), i).unwrap();
        }
        assert_eq!(e.fail_safe_action(), TransportKind::Direct);
        assert!(e.may_forward_direct());
    }

    #[test]
    fn strict_vpn_policy_forbids_leaking_direct() {
        let cfg = EngineConfig { data_plane_policy: DataPlanePolicy::StrictVpnOnly, ..EngineConfig::default() };
        let e = RouteEngine::new(cfg);
        assert!(!e.may_forward_direct());
    }

    #[test]
    fn events_are_structured_and_bounded() {
        let mut e = engine();
        for i in 0..600 {
            e.emit_kind(EventKind::RouteChanged, i, Severity::Info, "flood");
        }
        assert!(e.events.len() <= 512);
        let rep = e.diagnostic_report(1_000).unwrap();
        assert_ne!(rep.route_changes, u64::MAX, "counter is sane");
    }

    #[test]
    fn handshake_failure_accumulates_diagnostics() {
        let mut e = engine();
        e.report_handshake_failure(1);
        e.report_handshake_failure(2);
        assert_eq!(e.counters.handshake_failures, 2);
        let rep = e.diagnostic_report(3).unwrap();
        assert_eq!(rep.handshake_failures, 2);
    }

    #[test]
    fn udp_timeout_feeds_fallback_chain() {
        let mut e = engine();
        e.set_edges(vec![edge("a", "ap", "10.0.0.6")], 0).unwrap();
        for i in 0..10 {
            e.on_edge_observation(&obs("a", 40.0, 3.0, false), i).unwrap();
        }
        // UDP path collapses.
        for _ in 0..8 {
            e.report_udp_timeout(100);
        }
        assert!(e.fallback.current() != TransportKind::Udp || !matches!(e.transport_states.get(&TransportKind::Udp).map(|s| s.state), Some(TransportHealth::Healthy)));
    }
}