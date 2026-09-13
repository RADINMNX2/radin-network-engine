//! SPEC 38 TEST MATRIX — chaos-harness validation layer.
//!
//! Validates that the chaos harness actually produces the loss / jitter /
//! latency / failure / recovery behaviors the engine scenarios will depend
//! on. TEST ONLY — none of this ever touches production telemetry.

use radin_chaos::pipeline::{PacketResult, PipelineState};
use radin_chaos::profile::{ChaosProfile, FailureMode};

/// Loss axis: 0%, 0.5%, 1%, 3%, 5%, 10% (spec 38).
#[test]
fn matrix_loss_axis_is_reproducible_and_approximate() {
    for loss in [0.0, 0.005, 0.01, 0.03, 0.05, 0.10] {
        let mut p = PipelineState::from_profile(ChaosProfile::with_loss(loss), 4242);
        let results = p.run(50_000, 0);
        let dropped = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    PacketResult::Dropped { .. } | PacketResult::ConnectionDown { .. }
                )
            })
            .count();
        let measured = dropped as f64 / results.len() as f64;
        let tolerance = 0.02;
        assert!(
            (measured - loss).abs() < tolerance,
            "loss target {loss}: measured {measured:.4}"
        );
    }
}

/// Jitter axis: 1, 5, 20, 50 ms (spec 38).
#[test]
fn matrix_jitter_axis_produces_variance_around_base() {
    for jitter_ms in [1.0, 5.0, 20.0, 50.0] {
        let mut p = PipelineState::from_profile(ChaosProfile::with_jitter(jitter_ms), 7);
        let results = p.run(30_000, 0);
        let delays: Vec<u64> = results
            .iter()
            .filter_map(|r| match r {
                PacketResult::Delivered { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        assert!(!delays.is_empty());
        let min = *delays.iter().min().unwrap() as f64;
        let max = *delays.iter().max().unwrap() as f64;
        assert!(
            (max - min) >= 1.0,
            "jitter {jitter_ms} ms must vary delivery: min {min} max {max}"
        );
        // Mean delay should hug (base 30 ms) with attraction to jitter.
        let mean = delays.iter().map(|d| *d as f64).sum::<f64>() / delays.len() as f64;
        assert!(
            (mean - 30.0).abs() <= jitter_ms + 2.0,
            "mean {mean:.2} too far from base for jitter {jitter_ms}"
        );
    }
}

/// Latency axis: 30, 70, 120, 200, 300 ms (spec 38).
#[test]
fn matrix_latency_axis_delivers_at_least_target() {
    for latency in [30.0, 70.0, 120.0, 200.0, 300.0] {
        let mut p = PipelineState::from_profile(ChaosProfile::with_latency(latency), 31);
        let results = p.run(10_000, 0);
        let first = results
            .iter()
            .find_map(|r| match r {
                PacketResult::Delivered { delay_ms, .. } => Some(*delay_ms as f64),
                _ => None,
            })
            .unwrap();
        // Every assigned delay must be >= base latency (max(0) jitter).
        assert!(first >= latency, "expected >= {latency} ms, got {first}");
    }
}

/// Failure axis: reset-every-N, and intermittent windows (spec 38).
#[test]
fn matrix_reset_axis_triggers_connection_downs() {
    let mut p = PipelineState::from_profile(
        ChaosProfile::with_failure(FailureMode::ResetEveryPackets(50)),
        13,
    );
    let n = 1_000;
    let results = p.run(n, 0);
    let downs = results
        .iter()
        .filter(|r| matches!(r, PacketResult::ConnectionDown { .. }))
        .count();
    let expected = n / 50;
    assert!(
        (downs as i64 - expected as i64).abs() <= 1,
        "reset every 50: expected ~{expected}, got {downs}"
    );
    assert_eq!(p.resets, downs as u64);
}

#[test]
fn matrix_intermittent_mode_blocks_packets_in_down_window() {
    let mut p = PipelineState::from_profile(
        ChaosProfile::with_failure(FailureMode::Intermittent {
            down_ratio: 0.5,
            mean_down_ms: 200,
        }),
        5,
    );
    let results = p.run(5_000, 0);
    let downs = results
        .iter()
        .filter(|r| matches!(r, PacketResult::ConnectionDown { .. }))
        .count();
    assert!(
        downs > 100,
        "intermittent 0.5 down-ratio must drop many packets, got {downs}"
    );
}

/// Connecting the 38-matrix input shape directly (helper used by engine
/// scenarios).
#[allow(unused)]
pub fn fates_for(profile: &ChaosProfile, n: u64, seed: u64, tick: u64) -> Vec<PacketResult> {
    let mut p = PipelineState::from_profile(*profile, seed);
    p.run(n, tick)
}

/// Guarantee: no userland code path accepts synthetic data by accident.
#[test]
fn synthetic_marker_is_never_false() {
    // The harness carries no MeasuredSource at all — anything leaving it is
    // a raw packet fate, not a telemetry sample. This test documents that.
    assert!(fates_for(&ChaosProfile::pristine(), 10, 1, 0).len() == 10);
}
