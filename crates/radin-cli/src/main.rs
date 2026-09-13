//! radin-cli: local demo + benchmark runner (spec 30 diagnostic).
//!
//! Drives a real `RouteEngine` through a scripted network timeline using the
//! TEST-ONLY chaos pipeline (`radin-chaos`). Every number printed is honest:
//! the source is synthetic (harness-generated) and it is labelled as such.
//! Nothing here is production telemetry, ever.

use radin_chaos::pipeline::{PacketResult, PipelineState};
use radin_chaos::profile::{ChaosProfile, FailureMode};
use radin_core::engine::{EdgeObservation, EngineConfig, RouteEngine};
use radin_core::events::EventKind;
use radin_core::health::NetworkHealthGrade;
use radin_core::model::{DataPlanePolicy, EdgeInfo, NetworkType, TransportKind, VpnState};

/// Candidate id is "<edge>-<transport>"; strip the transport suffix for the
/// edge label the way the resolver does.
fn edge_of(route_id: &str) -> String {
    route_id.rsplit_once('-').map(|(e, _)| e.to_string()).unwrap_or_else(|| route_id.to_string())
}

fn edge_info(id: &str, region: &str, address: &str) -> EdgeInfo {
    EdgeInfo {
        id: id.to_string(),
        region: region.to_string(),
        address: address.to_string(),
        supported_transports: vec![TransportKind::Quic, TransportKind::Udp, TransportKind::TcpTls],
        priority: None,
        expires_at: 1_000_000_000,
        signature_b64: None,
    }
}

fn main() {
    println!("╭──────────────────────────────────────────────────────────╮");
    println!("│ RADIN NETWORK ENGINE — scripted fail-over demonstration  │");
    println!("│  All measurements are SYNTHETIC (radin-chaos harness).    │");
    println!("│  No production telemetry is involved. (spec 39/41)        │");
    println!("╰──────────────────────────────────────────────────────────╯");
    println!();

    let cfg = EngineConfig {
        supported_transports: vec![
            TransportKind::Quic,
            TransportKind::Udp,
            TransportKind::TcpTls,
            TransportKind::Http2,
            TransportKind::WebSocketTls,
        ],
        data_plane_policy: DataPlanePolicy::AllowDirectFallback,
        ..EngineConfig::default()
    };
    let mut engine = RouteEngine::new(cfg);
    engine
        .set_edges(
            vec![
                edge_info("Edge-SG-01", "sgp", "10.0.0.1"),
                edge_info("Edge-TYO-01", "tyo", "10.0.0.2"),
                edge_info("Edge-FRA-01", "fra", "10.0.0.3"),
            ],
            0,
        )
        .expect("edges accepted");
    engine.set_network_type(NetworkType::Wifi, 0);
    engine.set_vpn_state(VpnState::Disconnected, 0);

    let mut edges = vec![
        SimEdge::new("Edge-SG-01", ChaosProfile::with_latency(12.0), 501),
        SimEdge::new("Edge-TYO-01", ChaosProfile::with_latency(60.0), 502),
        SimEdge::new("Edge-FRA-01", ChaosProfile::with_latency(110.0), 503),
    ];

    println!("Phase A — healthy network. Expect the best edge, stable route.\n");
    drive(&mut engine, &mut edges, 20, 200);

    println!("Phase B — Edge-SG-01 sags (8% loss + rising jitter). Expect a switch.\n");
    edges[0] = SimEdge::new(
        "Edge-SG-01",
        ChaosProfile { latency_ms: 80.0, jitter_ms: 25.0, loss_ratio: 0.08, ..ChaosProfile::default() },
        504,
    );
    drive(&mut engine, &mut edges, 50, 200);

    println!("Phase C — every edge dies. Expect Direct fail-safe (policy allows it).\n");
    for _ in 0..edges.len() {
        edges[0] = SimEdge::new("Edge-SG-01", death(), 505);
        edges[1] = SimEdge::new("Edge-TYO-01", death(), 506);
        edges[2] = SimEdge::new("Edge-FRA-01", death(), 507);
    }
    // The data plane's typed signals march the whole fallback chain to Direct.
    for _ in 0..8 {
        for t in [
            TransportKind::Quic,
            TransportKind::Udp,
            TransportKind::TcpTls,
            TransportKind::Http2,
            TransportKind::WebSocketTls,
        ] {
            engine.report_transport_failure(t, now_ms());
        }
    }
    drive(&mut engine, &mut edges, 10, 200);
    println!(
        "  fail_safe_action={:?} may_forward_direct={} fallback_current={:?}",
        engine.fail_safe_action(),
        engine.may_forward_direct(),
        engine.fallback.current()
    );

    println!("Phase D — edges recover. Expect reselection + health recovery.\n");
    edges[0] = SimEdge::new("Edge-SG-01", ChaosProfile::with_latency(12.0), 508);
    edges[1] = SimEdge::new("Edge-TYO-01", ChaosProfile::with_latency(60.0), 509);
    edges[2] = SimEdge::new("Edge-FRA-01", ChaosProfile::with_latency(110.0), 510);
    drive(&mut engine, &mut edges, 60, 200);
    for t in [TransportKind::Quic, TransportKind::Udp, TransportKind::TcpTls] {
        engine.report_transport_success(t, now_ms());
    }
    // Long sustained-clean window lets health climb (upgrades need sustain,
    // and the edge keeps a conservative loss memory — spec 24).
    drive(&mut engine, &mut edges, 90, 200);

    println!("╭────────────────────── DIAGNOSTIC REPORT ───────────────────╮");
    let report = engine.diagnostic_report(now_ms()).expect("report builds");
    println!("{}", report.render_plaintext());
    println!("╰─────────────────────────────────────────────────────────────╯");
    println!();
    println!("Event log (tail):");
    for ev in engine.telemetry.events.iter().rev().take(8).rev() {
        println!(
            "  t={:>6}  {:<20} {:?} severity={:?} — {}",
            ev.timestamp, ev.kind.as_str(), ev.route.as_deref().map(edge_of), ev.severity, ev.reason
        );
    }
}

fn death() -> ChaosProfile {
    ChaosProfile::with_failure(FailureMode::ResetEveryPackets(1))
}

fn now_ms() -> u64 {
    1_000_000 // monotonic-ish sim clock; timeline is tick-relative
}

/// One simulated edge backed by a seeded chaos pipeline.
struct SimEdge {
    id: String,
    pipeline: PipelineState,
}

impl SimEdge {
    fn new(id: &str, profile: ChaosProfile, seed: u64) -> Self {
        Self { id: id.to_string(), pipeline: PipelineState::from_profile(profile, seed) }
    }

    fn probe(&mut self, n: u64, tick: u64) -> EdgeObservation {
        let fates = self.pipeline.run(n, tick);
        let delivered: Vec<u64> = fates
            .iter()
            .filter_map(|r| match r {
                PacketResult::Delivered { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        let lost = fates.iter().any(|r| matches!(r, PacketResult::ConnectionDown { .. }))
            || (fates.len() > 1 && (delivered.len() as f64 / fates.len() as f64) < 0.90);
        let latency = if delivered.is_empty() {
            9999.0
        } else {
            delivered.iter().sum::<u64>() as f64 / delivered.len() as f64
        };
        let mut js = delivered.windows(2).map(|w| w[1].abs_diff(w[0]) as f64).collect::<Vec<_>>();
        js.sort_by(|a, b| a.total_cmp(b));
        let jitter = js.get(js.len() / 2).copied().unwrap_or(0.0);
        EdgeObservation { edge_id: self.id.clone(), latency_ms: latency, jitter_ms: jitter, lost, handshake_ms: None }
    }
}

fn drive(engine: &mut RouteEngine, edges: &mut [SimEdge], rounds: u64, interval_ms: u64) {
    let mut clock = now_ms();
    for _ in 0..rounds {
        clock += interval_ms;
        for edge in edges.iter_mut() {
            let obs = edge.probe(16, clock);
            engine.on_edge_observation(&obs, clock).expect("observation accepted");
        }
    }
    let route = engine.current_route().map(|c| c.id.clone()).unwrap_or_else(|| "— none —".into());
    let grade = engine.network_health_grade();
    println!(
        "  route={:<24} grade={:<10} health_worse-than-excellent={} rtt={:.1}ms loss={:.2}%",
        edge_of(&route),
        grade_name(grade),
        grade.is_worse_than(NetworkHealthGrade::Excellent),
        engine
            .telemetry
            .samples
            .back()
            .map(|s| s.latency_ms)
            .unwrap_or(0.0),
        engine.telemetry.samples.back().map(|s| s.packet_loss_ratio * 100.0).unwrap_or(0.0)
    );
    let _ = EventKind::RouteChanged; // (spike kinds surfaced in the event log)
}

fn grade_name(g: NetworkHealthGrade) -> String {
    format!("{g:?}").to_uppercase()
}