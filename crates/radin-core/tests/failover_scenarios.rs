//! SPEC 38 FAILOVER SCENARIOS — engine-level integration tests.
//!
//! These drive a real `RouteEngine` with observations produced by the chaos
//! pipeline (radin-chaos, TEST ONLY). They verify the detect→measure→adapt→
//! recover→fail-over loop at the orchestration layer:
//!
//!   - Fast path stays stable on a good edge (no flapping).
//!   - Sustained degradation on the current edge triggers a switch.
//!   - Whole-edge failure walks the dynamic fallback chain, ending at the
//!     fail-safe Direct hop (or refusing to leak under StrictVpnOnly).
//!   - Recovery re-establishes the best route; health recovers.
//!   - Circuit breakers quarantine repeatedly-failing transports.
//!   - Loss/jitter/latency spikes surface as structured events.
//!
//! The harness is a pure test simulator: packet fates come from a seeded
//! chaos pipeline and are converted into honest `EdgeObservation`s. Fully
//! synthetic by construction — never wired into production telemetry.

use radin_chaos::pipeline::{PacketResult, PipelineState};
use radin_chaos::profile::ChaosProfile;

use radin_core::circuit::CircuitState;
use radin_core::engine::{EdgeObservation, EngineConfig, RouteEngine};
use radin_core::events::EventKind;
use radin_core::health::NetworkHealthGrade;
use radin_core::model::{
    DataPlanePolicy, EdgeInfo, MeasuredSource, NetworkType, TransportKind, VpnState,
};
use radin_core::telemetry::DiagnosticBuilder;

mod harness {
    use super::*;

    /// A simulated edge = a chaos pipeline producing probe fates.
    pub struct SimEdge {
        pub id: String,
        pub region: String,
        pub address: String,
        pub pipeline: PipelineState,
    }

    impl SimEdge {
        pub fn new(
            id: &str,
            region: &str,
            address: &str,
            profile: ChaosProfile,
            seed: u64,
        ) -> Self {
            Self {
                id: id.to_string(),
                region: region.to_string(),
                address: address.to_string(),
                pipeline: PipelineState::from_profile(profile, seed),
            }
        }

        /// Probe the edge this tick: run `n` packets, summarize into an
        /// observation. `probe_ms` is the pipeline tick (ms of sim time).
        pub fn probe(&mut self, n: u64, probe_ms: u64) -> EdgeObservation {
            let fates = self.pipeline.run(n, probe_ms);
            let delivered: Vec<u64> = fates
                .iter()
                .filter_map(|r| match r {
                    PacketResult::Delivered { delay_ms, .. } => Some(*delay_ms),
                    _ => None,
                })
                .collect();
            let lost = fates
                .iter()
                .any(|r| matches!(r, PacketResult::ConnectionDown { .. }))
                || (fates.len() > 1 && (delivered.len() as f64 / fates.len() as f64) < 0.90);
            let latency = if delivered.is_empty() {
                9999.0
            } else {
                delivered.iter().sum::<u64>() as f64 / delivered.len() as f64
            };
            let mut js = delivered
                .windows(2)
                .map(|w| w[1].abs_diff(w[0]) as f64)
                .collect::<Vec<_>>();
            js.sort_by(|a, b| a.total_cmp(b));
            let jitter = js.get(js.len() / 2).copied().unwrap_or(0.0);
            EdgeObservation {
                edge_id: self.id.clone(),
                latency_ms: latency,
                jitter_ms: jitter,
                lost,
                handshake_ms: None,
            }
        }
    }

    /// The full simulated world: N edges + the engine being tested.
    pub struct Sim {
        pub engine: RouteEngine,
        pub edges: Vec<SimEdge>,
        pub now: u64,
    }

    impl Sim {
        pub fn new(cfg: EngineConfig, edges: Vec<SimEdge>) -> Self {
            let mut engine = RouteEngine::new(cfg);
            let infos: Vec<EdgeInfo> = edges
                .iter()
                .map(|e| edge_info(&e.id, &e.region, &e.address))
                .collect();
            engine.set_edges(infos, 0).unwrap();
            engine.set_network_type(NetworkType::Wifi, 0);
            engine.set_vpn_state(VpnState::Disconnected, 0);
            Self {
                engine,
                edges,
                now: 0,
            }
        }

        /// Run `rounds` probe rounds at `interval_ms` spacing, feeding each
        /// edge's probe result into the engine.
        pub fn run(&mut self, rounds: u64, interval_ms: u64) {
            for _ in 0..rounds {
                self.now += interval_ms;
                let obs = self.sample_all();
                for o in obs {
                    self.engine.on_edge_observation(&o, self.now).unwrap();
                }
            }
        }

        pub fn sample_all(&mut self) -> Vec<EdgeObservation> {
            let mut obs = Vec::new();
            for edge in self.edges.iter_mut() {
                obs.push(edge.probe(16, self.now));
            }
            obs
        }
    }

    pub fn edge_info(id: &str, region: &str, address: &str) -> EdgeInfo {
        EdgeInfo {
            id: id.to_string(),
            region: region.to_string(),
            address: address.to_string(),
            supported_transports: vec![
                TransportKind::Quic,
                TransportKind::Udp,
                TransportKind::TcpTls,
            ],
            priority: None,
            expires_at: 1_000_000_000,
            signature_b64: None,
        }
    }

    /// A standard config over the full optimizer chain (Direct excluded —
    /// it's the fail-safe, never an edge transport).
    pub fn default_cfg() -> EngineConfig {
        EngineConfig {
            supported_transports: vec![
                TransportKind::Quic,
                TransportKind::Udp,
                TransportKind::TcpTls,
                TransportKind::Http2,
                TransportKind::WebSocketTls,
            ],
            ..EngineConfig::default()
        }
    }
}

use harness::{default_cfg, Sim, SimEdge};

fn route_id(sim: &Sim) -> Option<String> {
    sim.engine.current_route().map(|c| c.id.clone())
}

/// A clearly-better edge stays selected while the current edge is healthy.
/// No flapping on sub-threshold differences (spec 11/12).
#[test]
fn fast_path_is_stable_on_healthy_top_edge() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![
            SimEdge::new(
                "Edge-SG-01",
                "sgp",
                "10.0.0.1",
                ChaosProfile::with_latency(15.0),
                501,
            ),
            SimEdge::new(
                "Edge-TYO-01",
                "tyo",
                "10.0.0.2",
                ChaosProfile::with_latency(90.0),
                502,
            ),
        ],
    );
    sim.run(10, 200);
    let first = route_id(&sim);
    assert!(first.is_some(), "engine must have a route");
    sim.run(30, 200);
    assert_eq!(
        route_id(&sim),
        first,
        "no flapping while routes stay healthy"
    );
}

/// The current edge silently degrades (loss creeps in); once the degradation
/// is sustained past the hysteresis window the engine moves off it (spec 9/10/11).
#[test]
fn sustained_degradation_eventually_switches_edge() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![
            SimEdge::new(
                "Edge-SG-01",
                "sgp",
                "10.0.0.1",
                ChaosProfile::with_latency(15.0),
                601,
            ),
            SimEdge::new(
                "Edge-FRA-01",
                "fra",
                "10.0.0.3",
                ChaosProfile::with_latency(120.0),
                602,
            ),
        ],
    );
    sim.run(10, 200);
    let before = route_id(&sim);
    assert_eq!(
        before.as_deref(),
        Some("Edge-SG-01-quic"),
        "SG wins on latency"
    );
    // Sickness: rear the SG edge into heavy loss + slow.
    sim.edges[0] = SimEdge::new(
        "Edge-SG-01",
        "sgp",
        "10.0.0.1",
        ChaosProfile::with_loss(0.08),
        603,
    );
    sim.run(60, 200); // 12 s of sustained degradation.
    let after = route_id(&sim);
    assert_ne!(
        after.as_deref(),
        Some("Edge-SG-01-quic"),
        "engine must leave the degrading edge"
    );
}

/// Complete loss of every edge: the engine fails back through the chain and
/// lands on the Direct fail-safe when the policy allows it (spec 13/35/36).
#[test]
fn total_outage_fails_safe_to_direct_when_allowed() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![
            SimEdge::new(
                "Edge-SG-01",
                "sgp",
                "10.0.0.1",
                ChaosProfile::with_latency(15.0),
                701,
            ),
            SimEdge::new(
                "Edge-TYO-01",
                "tyo",
                "10.0.0.2",
                ChaosProfile::with_latency(20.0),
                702,
            ),
        ],
    );
    sim.run(6, 200);
    assert!(sim.engine.current_route().is_some());
    // Kill every edge cold: every packet fates a connection-down.
    let death = ChaosProfile::with_failure(radin_chaos::profile::FailureMode::ResetEveryPackets(1));
    sim.edges[0] = SimEdge::new("Edge-SG-01", "sgp", "10.0.0.1", death, 703);
    sim.edges[1] = SimEdge::new("Edge-TYO-01", "tyo", "10.0.0.2", death, 704);
    sim.run(40, 200);
    assert_eq!(sim.engine.fail_safe_action(), TransportKind::Direct);
    assert!(
        sim.engine.may_forward_direct(),
        "allow-direct policy lets us fall back"
    );
    // The data plane's typed signals then march the fallback chain down to
    // Direct (spec 6/13), advancing one transport at a time in order.
    for (i, t) in [
        TransportKind::Quic,
        TransportKind::Udp,
        TransportKind::TcpTls,
        TransportKind::Http2,
        TransportKind::WebSocketTls,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            sim.engine.fallback.current(),
            t,
            "chain position {i} must be {t:?} before failing it"
        );
        for _ in 0..8 {
            sim.engine.report_transport_failure(t, sim.now);
        }
    }
    assert_eq!(
        sim.engine.fallback.current(),
        TransportKind::Direct,
        "chain ends at Direct"
    );
    assert!(
        sim.engine.counters.transport_fallbacks >= 1,
        "fallback must be recorded"
    );
}

/// The StrictVpnOnly policy must never leak traffic to Direct (spec 35).
#[test]
fn strict_vpn_policy_never_leaks_direct() {
    let cfg = EngineConfig {
        data_plane_policy: DataPlanePolicy::StrictVpnOnly,
        ..default_cfg()
    };
    let death = ChaosProfile::with_failure(radin_chaos::profile::FailureMode::ResetEveryPackets(1));
    let mut sim = Sim::new(
        cfg,
        vec![SimEdge::new("Edge-SG-01", "sgp", "10.0.0.1", death, 801)],
    );
    sim.run(40, 200);
    assert!(
        !sim.engine.may_forward_direct(),
        "must NOT leak direct under strict policy"
    );
    // The data plane is gated on may_forward_direct before honoring the
    // fail-safe action; direct forwarding is simply refused.
}

/// After a total outage the engine ran the Direct fail-safe. When edges come
/// back healthy the engine reconnects and health recovers (spec 24).
#[test]
fn recovery_after_outage_restores_best_route_and_health() {
    let death = ChaosProfile::with_failure(radin_chaos::profile::FailureMode::ResetEveryPackets(1));
    let healthy = ChaosProfile::with_latency(25.0);
    let mut sim = Sim::new(
        default_cfg(),
        vec![SimEdge::new("Edge-SG-01", "sgp", "10.0.0.1", death, 901)],
    );
    sim.run(30, 200); // fully dead → Direct fail-safe + degraded health.
    assert!(sim.engine.may_forward_direct());
    let bleak = sim.engine.network_health_grade();
    assert!(bleak.is_worse_than(NetworkHealthGrade::Good));
    // Edge recovers: probe healthy, reselect, resume transport.
    sim.edges[0] = SimEdge::new("Edge-SG-01", "sgp", "10.0.0.1", healthy, 902);
    sim.run(10, 200);
    assert_eq!(
        route_id(&sim).as_deref(),
        Some("Edge-SG-01-quic"),
        "recovery reselects best route"
    );
    sim.engine
        .report_transport_success(TransportKind::Quic, sim.now);
    // Let the health tracker's sustain window elapse before expecting a climb.
    sim.run(40, 200);
    let recovered = sim.engine.network_health_grade();
    assert!(
        recovered.is_better_than(NetworkHealthGrade::Critical)
            || recovered == NetworkHealthGrade::Good,
        "health must recover after sustained improvement"
    );
}

/// A circuit breaker quarantines a route that keeps failing (spec 6/13).
#[test]
fn circuit_breaker_quarantines_failing_transport() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![SimEdge::new(
            "Edge-SG-01",
            "sgp",
            "10.0.0.1",
            ChaosProfile::with_latency(10.0),
            1001,
        )],
    );
    sim.run(6, 200);
    let route = sim.engine.current_route().expect("route").clone();
    for _ in 0..8 {
        sim.engine
            .report_transport_failure(route.transport, sim.now);
    }
    // Breakers are keyed by "<route-id>:<transport>".
    let key = format!("{}:{}", route.id, route.transport);
    let breaker = sim.engine.breaker(&key).expect("breaker exists");
    assert!(
        matches!(breaker.state, CircuitState::Open { .. }),
        "repeated failures trip the breaker"
    );
    assert!(
        sim.engine.counters.circuit_trips >= 1,
        "circuit trip counted"
    );
}

/// Degradation surfaces as structured events (spec 26, spec 29 vocabulary).
#[test]
fn spikes_surface_as_structured_events() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![SimEdge::new(
            "Edge-SG-01",
            "sgp",
            "10.0.0.1",
            ChaosProfile::with_latency(10.0),
            1101,
        )],
    );
    sim.run(4, 200);
    let before = sim.engine.telemetry.events.len();
    sim.edges[0] = SimEdge::new(
        "Edge-SG-01",
        "sgp",
        "10.0.0.1",
        ChaosProfile::with_loss(0.10),
        1102,
    );
    sim.run(12, 200);
    assert!(
        sim.engine.telemetry.events.len() > before,
        "degradation must produce events"
    );
    let kinds: Vec<EventKind> = sim.engine.telemetry.events.iter().map(|e| e.kind).collect();
    assert!(
        kinds.contains(&EventKind::PacketLossSpike)
            || kinds.contains(&EventKind::EdgeDegraded)
            || kinds.contains(&EventKind::RouteChanged)
            || kinds.contains(&EventKind::TransportFallback),
        "expected a spike/degradation kind, got {kinds:?}"
    );
}

/// The DiagnosticReport is honest: only Real provenance, and the builder
/// refuses Synthetic input (see also radin-core telemetry unit tests).
#[test]
fn telemetry_export_is_honest_and_real() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![SimEdge::new(
            "Edge-SG-01",
            "sgp",
            "10.0.0.1",
            ChaosProfile::with_latency(20.0),
            1201,
        )],
    );
    sim.run(6, 200);
    let rep = sim.engine.diagnostic_report(sim.now).unwrap();
    assert_eq!(rep.source, MeasuredSource::Real);
    assert!(rep.rtt_ms.is_some());
    assert!(
        DiagnosticBuilder::new(MeasuredSource::Synthetic)
            .build(0, None, None, &[], 0, 0, 0, 0)
            .is_err(),
        "Synthetic diagnostics are refused"
    );
}

/// Restrictive-network detection records an actionable symptom (spec 30).
#[test]
fn restrictive_network_symptom_is_recorded() {
    let mut sim = Sim::new(
        default_cfg(),
        vec![SimEdge::new(
            "Edge-SG-01",
            "sgp",
            "10.0.0.1",
            ChaosProfile::with_latency(25.0),
            1301,
        )],
    );
    sim.engine.report_udp_timeout(sim.now);
    sim.engine.report_udp_timeout(sim.now + 1);
    sim.engine.report_udp_timeout(sim.now + 2);
    assert!(
        !sim.engine.restriction_description().is_empty(),
        "restriction hint must be actionable"
    );
}
