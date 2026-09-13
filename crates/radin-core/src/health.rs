//! Unified network health model (spec 23) + adaptive monitoring (spec 24) +
//! network-type awareness hooks (spec 25).
//!
//! `NetworkHealthScore` is derived purely from measured inputs and never
//! hand-set. Monitoring frequency tracks the health state and *gradually*
//! decays back to low-frequency when stability returns.

use serde::{Deserialize, Serialize};

use crate::model::MetricsSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NetworkHealthGrade {
    Excellent,
    Good,
    Degraded,
    Poor,
    Critical,
}

impl NetworkHealthGrade {
    /// Rank used for ordering comparisons (Excellent is the best).
    fn rank(self) -> u8 {
        match self {
            NetworkHealthGrade::Excellent => 5,
            NetworkHealthGrade::Good => 4,
            NetworkHealthGrade::Degraded => 3,
            NetworkHealthGrade::Poor => 2,
            NetworkHealthGrade::Critical => 1,
        }
    }

    pub fn is_worse_than(self, other: NetworkHealthGrade) -> bool {
        self.rank() < other.rank()
    }

    pub fn is_better_than(self, other: NetworkHealthGrade) -> bool {
        self.rank() > other.rank()
    }
}

impl PartialOrd for NetworkHealthGrade {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NetworkHealthGrade {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl std::fmt::Display for NetworkHealthGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NetworkHealthGrade::Excellent => "EXCELLENT",
            NetworkHealthGrade::Good => "GOOD",
            NetworkHealthGrade::Degraded => "DEGRADED",
            NetworkHealthGrade::Poor => "POOR",
            NetworkHealthGrade::Critical => "CRITICAL",
        };
        f.write_str(s)
    }
}

/// Adaptive monitoring intensity (spec 24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorLevel {
    /// EXCELLENT: low-frequency probes.
    Low,
    /// GOOD: normal probes.
    Normal,
    /// DEGRADED: higher-frequency probes.
    High,
    /// CRITICAL: aggressive route evaluation.
    Aggressive,
}

/// Weighted inputs to the health model (configurable).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HealthWeights {
    pub latency: f64,
    pub jitter: f64,
    pub loss: f64,
    pub stability: f64,
    pub reconnect: f64,
    pub availability: f64,
}

impl Default for HealthWeights {
    fn default() -> Self {
        Self {
            latency: 0.20,
            jitter: 0.15,
            loss: 0.30,
            stability: 0.15,
            reconnect: 0.10,
            availability: 0.10,
        }
    }
}

impl HealthWeights {
    fn normalize(&self) -> Self {
        let sum = self.latency
            + self.jitter
            + self.loss
            + self.stability
            + self.reconnect
            + self.availability;
        if sum <= f64::EPSILON {
            return Self::default();
        }
        Self {
            latency: self.latency / sum,
            jitter: self.jitter / sum,
            loss: self.loss / sum,
            stability: self.stability / sum,
            reconnect: self.reconnect / sum,
            availability: self.availability / sum,
        }
    }
}

/// Everything the health model needs. Values must be real (the caller gates
/// with `MeasuredSource`; we do not expose a synthetic path here).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HealthInput {
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub stability: f64,
    /// Reconnects per hour.
    pub reconnect_rate: f64,
    /// Availability in [0,1].
    pub availability: f64,
}

impl From<MetricsSnapshot> for HealthInput {
    fn from(m: MetricsSnapshot) -> Self {
        Self {
            latency_ms: m.latency_ms,
            jitter_ms: m.jitter_ms,
            packet_loss_ratio: m.packet_loss_ratio,
            stability: m.stability,
            reconnect_rate: m.reconnect_rate,
            availability: 1.0 - m.packet_loss_ratio,
        }
    }
}

/// Raw health score in [0, 100].
pub fn health_score(input: &HealthInput, weights: &HealthWeights) -> f64 {
    let w = weights.normalize();
    let lat = 1.0 / (1.0 + input.latency_ms / 150.0);
    let jit = 1.0 / (1.0 + input.jitter_ms / 15.0);
    let loss = if input.packet_loss_ratio <= 0.0 {
        1.0
    } else {
        1.0 / (1.0 + (input.packet_loss_ratio * 100.0) / 1.5).powf(2.0)
    };
    let stab = input.stability.clamp(0.0, 1.0);
    let rec = 1.0 / (1.0 + input.reconnect_rate);
    let avail = input.availability.clamp(0.0, 1.0);
    let raw = w.latency * lat
        + w.jitter * jit
        + w.loss * loss
        + w.stability * stab
        + w.reconnect * rec
        + w.availability * avail;
    (raw * 100.0).clamp(0.0, 100.0)
}

/// Map a raw score to a grade with hysteresis to avoid grade flapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeModel {
    /// Grade transition thresholds (ascending): ExcellentBoundary etc.
    pub excellent_above: f64,
    pub good_above: f64,
    pub degraded_above: f64,
    pub poor_above: f64,
}

impl Default for GradeModel {
    fn default() -> Self {
        Self {
            excellent_above: 80.0,
            good_above: 65.0,
            degraded_above: 50.0,
            poor_above: 30.0,
        }
    }
}

/// Grade with a hang time: upgrades require *sustained* improvement. This is
/// the "hysteresis" that stops the monitor from oscillating EXCELLENT↔GOOD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradedHealth {
    pub grade: NetworkHealthGrade,
    /// Score behind the grade.
    pub score: f64,
    /// How long (ms) we've been at or above this grade.
    pub sustained_ms: u64,
}

/// A deduplicating stateful grader. Feed it every frame; it returns the
/// current grade and tells you when an upgrade actually lands.
#[derive(Debug, Clone)]
pub struct HealthTracker {
    pub model: GradeModel,
    pub grade: NetworkHealthGrade,
    /// When the current (improved) target grade was first observed. `None`
    /// while not in an upgrade attempt.
    pub since: Option<u64>,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self {
            model: GradeModel::default(),
            grade: NetworkHealthGrade::Good,
            since: None,
        }
    }
}

impl HealthTracker {
    /// `upgrade_sustain_ms`: must keep an improved grade for this long before
    /// we move up. Degradations apply immediately.
    pub fn observe(&mut self, score: f64, now: u64, upgrade_sustain_ms: u64) -> GradedHealth {
        let target = grade_for(score, &self.model);
        let improved = target.is_better_than(self.grade);
        if improved {
            self.since.get_or_insert(now);
            if now.saturating_sub(self.since.unwrap_or(now)) >= upgrade_sustain_ms {
                self.grade = target;
            }
        } else {
            // Degrade immediately, reset the sustain clock.
            let _ = target;
            if target != self.grade {
                self.grade = target;
            }
            self.since = Some(now);
        }
        GradedHealth {
            grade: self.grade,
            score,
            sustained_ms: now.saturating_sub(self.since.unwrap_or(now)),
        }
    }
}

pub fn grade_for(score: f64, model: &GradeModel) -> NetworkHealthGrade {
    if score >= model.excellent_above {
        NetworkHealthGrade::Excellent
    } else if score >= model.good_above {
        NetworkHealthGrade::Good
    } else if score >= model.degraded_above {
        NetworkHealthGrade::Degraded
    } else if score >= model.poor_above {
        NetworkHealthGrade::Poor
    } else {
        NetworkHealthGrade::Critical
    }
}

/// Monitor intensity for a grade (spec 24).
pub fn monitor_level(grade: NetworkHealthGrade) -> MonitorLevel {
    match grade {
        NetworkHealthGrade::Excellent => MonitorLevel::Low,
        NetworkHealthGrade::Good => MonitorLevel::Normal,
        NetworkHealthGrade::Degraded => MonitorLevel::High,
        NetworkHealthGrade::Poor | NetworkHealthGrade::Critical => MonitorLevel::Aggressive,
    }
}

/// Network-type-aware policy bundle (spec 25). These are *initial knobs*;
/// the engine benchmarks and adapts rather than trusting them forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkTypePolicies {
    pub probe_factor: f64,
    pub keepalive_factor: f64,
    pub allow_quic: bool,
    pub allow_udp: bool,
}

impl Default for NetworkTypePolicies {
    fn default() -> Self {
        Self {
            probe_factor: 1.0,
            keepalive_factor: 1.0,
            allow_quic: true,
            allow_udp: true,
        }
    }
}

/// Initial policy guess per network type (benchmarked later).
pub fn initial_policy(network: crate::model::NetworkType) -> NetworkTypePolicies {
    match network {
        crate::model::NetworkType::Ethernet | crate::model::NetworkType::Wifi => {
            NetworkTypePolicies::default()
        }
        crate::model::NetworkType::FourG => NetworkTypePolicies {
            probe_factor: 0.8,
            keepalive_factor: 0.8,
            ..NetworkTypePolicies::default()
        },
        crate::model::NetworkType::FiveG => NetworkTypePolicies::default(),
        crate::model::NetworkType::ThreeG => NetworkTypePolicies {
            probe_factor: 0.65,
            keepalive_factor: 0.65,
            allow_quic: false,
            allow_udp: true,
        },
        crate::model::NetworkType::Other | crate::model::NetworkType::Unknown => {
            NetworkTypePolicies::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(lat: f64, jit: f64, loss: f64) -> HealthInput {
        HealthInput {
            latency_ms: lat,
            jitter_ms: jit,
            packet_loss_ratio: loss,
            stability: 0.9,
            reconnect_rate: 0.0,
            availability: 1.0 - loss,
        }
    }

    #[test]
    fn stable_low_latency_is_excellent() {
        let s = health_score(&input(30.0, 1.0, 0.001), &HealthWeights::default());
        let g = grade_for(s, &GradeModel::default());
        assert_eq!(g, NetworkHealthGrade::Excellent);
    }

    #[test]
    fn heavy_loss_demotes_well_below_good() {
        let s = health_score(&input(120.0, 20.0, 0.10), &HealthWeights::default());
        let g = grade_for(s, &GradeModel::default());
        assert!(
            g.is_worse_than(NetworkHealthGrade::Good),
            "score {s} (grade {g:?}) should be at most DEGRADED at 10% loss"
        );
        // And 30% loss is unusable → POOR/CRITICAL.
        let s30 = health_score(&input(120.0, 25.0, 0.30), &HealthWeights::default());
        let g30 = grade_for(s30, &GradeModel::default());
        assert!(
            matches!(g30, NetworkHealthGrade::Poor | NetworkHealthGrade::Critical),
            "30% loss must be POOR/CRITICAL, got {g30:?} (score {s30:.1})"
        );
    }

    #[test]
    fn monitor_levels_follow_health() {
        assert_eq!(
            monitor_level(NetworkHealthGrade::Excellent),
            MonitorLevel::Low
        );
        assert_eq!(
            monitor_level(NetworkHealthGrade::Good),
            MonitorLevel::Normal
        );
        assert_eq!(
            monitor_level(NetworkHealthGrade::Degraded),
            MonitorLevel::High
        );
        assert_eq!(
            monitor_level(NetworkHealthGrade::Critical),
            MonitorLevel::Aggressive
        );
    }

    #[test]
    fn upgrades_require_sustained_improvement_but_degrades_are_immediate() {
        let mut t = HealthTracker::default();
        // Start Excellent.
        let g1 = t.observe(95.0, 0, 3_000);
        assert_eq!(
            g1.grade,
            NetworkHealthGrade::Good,
            "no sustain yet, starts at Good"
        );
        // Sustain to Excellent.
        let g2 = t.observe(95.0, 4_000, 3_000);
        assert_eq!(g2.grade, NetworkHealthGrade::Excellent);
        // Immediate degrade on a bad frame.
        let g3 = t.observe(20.0, 5_000, 3_000);
        assert_eq!(g3.grade, NetworkHealthGrade::Critical);
        // And it cannot fast-track back up.
        let g4 = t.observe(95.0, 5_100, 3_000);
        assert_eq!(
            g4.grade,
            NetworkHealthGrade::Critical,
            "must sustain before climbing again"
        );
    }

    #[test]
    fn initial_policies_are_benchmark_seeds() {
        let wifi = initial_policy(crate::model::NetworkType::Wifi);
        assert!(wifi.allow_quic && wifi.allow_udp);
        let three_g = initial_policy(crate::model::NetworkType::ThreeG);
        assert!(!three_g.allow_quic, "initial seed: conservative on 3G");
        assert!(three_g.allow_udp);
    }
}
