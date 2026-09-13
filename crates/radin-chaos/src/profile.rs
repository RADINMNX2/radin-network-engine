//! ChaosProfile: the network pathology model used by the TEST harness.
//!
//! TEST ONLY (spec 39). Never connected to production telemetry. Simulates
//! latency, jitter, loss, reordering, bandwidth limits, connection resets
//! and intermittent connectivity.

use serde::{Deserialize, Serialize};

/// When a connection-level reset or outage happens, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    /// No connection-level failures.
    None,
    /// Deterministic: N packets pass, then a reset happens.
    ResetEveryPackets(u64),
    /// Random fraction of the time the connection is down.
    Intermittent { down_ratio: f64, mean_down_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChaosProfile {
    /// Base one-way latency added to every packet (ms).
    pub latency_ms: f64,
    /// Std-dev / amplitude of latency variation (ms).
    pub jitter_ms: f64,
    /// Probability any individual packet is dropped (0.0 .. 1.0).
    pub loss_ratio: f64,
    /// Probability a packet pair is swapped (reordering).
    pub reorder_ratio: f64,
    /// Bits-per-second cap (0 = unlimited).
    pub bandwidth_bps: u64,
    /// Connection-level failure behavior.
    pub failure: FailureMode,
}

impl Default for ChaosProfile {
    fn default() -> Self {
        Self {
            latency_ms: 0.0,
            jitter_ms: 0.0,
            loss_ratio: 0.0,
            reorder_ratio: 0.0,
            bandwidth_bps: 0,
            failure: FailureMode::None,
        }
    }
}

/// Named presets matching the test matrix rows (spec 38).
impl ChaosProfile {
    /// Perfect network: no impairments at 30 ms base latency.
    pub fn pristine() -> Self {
        Self { latency_ms: 30.0, ..Self::default() }
    }

    /// The 38-matrix latency axis presets.
    pub fn with_latency(ms: f64) -> Self {
        Self { latency_ms: ms, ..Self::default() }
    }

    /// The 38-matrix jitter axis presets.
    pub fn with_jitter(ms: f64) -> Self {
        Self { jitter_ms: ms, latency_ms: 30.0, ..Self::default() }
    }

    /// The 38-matrix loss axis presets (ratio 0.0-0.10).
    pub fn with_loss(ratio: f64) -> Self {
        Self { loss_ratio: ratio, latency_ms: 30.0, ..Self::default() }
    }

    pub fn with_reordering(ratio: f64) -> Self {
        Self { reorder_ratio: ratio, latency_ms: 30.0, ..Self::default() }
    }

    pub fn with_bandwidth(bps: u64) -> Self {
        Self { bandwidth_bps: bps, latency_ms: 30.0, ..Self::default() }
    }

    pub fn with_failure(mode: FailureMode) -> Self {
        Self { failure: mode, latency_ms: 30.0, ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_presets_map_to_the_38_table() {
        assert_eq!(ChaosProfile::with_latency(300.0).latency_ms, 300.0);
        assert_eq!(ChaosProfile::with_jitter(50.0).jitter_ms, 50.0);
        assert_eq!(ChaosProfile::with_loss(0.10).loss_ratio, 0.10);
        assert_eq!(ChaosProfile::with_bandwidth(1_000_000).bandwidth_bps, 1_000_000);
    }

    #[test]
    fn defaults_are_benign() {
        let p = ChaosProfile::default();
        assert_eq!(p.latency_ms, 0.0);
        assert_eq!(p.loss_ratio, 0.0);
        assert!(p.bandwidth_bps == 0);
    }
}