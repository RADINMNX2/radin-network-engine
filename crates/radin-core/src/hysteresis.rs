//! Route hysteresis (spec 12): prevent route flapping caused by tiny
//! measurement differences.
//!
//! A candidate only replaces the current route when:
//! - the candidate is *sustainably* better by a configurable percentage AND
//!   absolute ms delta, OR
//! - the current route degrades beyond a threshold *for a sustained period*
//!   (unlike a one-sample spike).
//!
//! Per-candidate cooldown prevents immediate re-switch after a rollback.

use serde::{Deserialize, Serialize};

use crate::model::MetricsSnapshot;
use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HysteresisConfig {
    /// Candidate must beat current latency by this fraction before switching.
    pub minimum_improvement_percent: f64,
    /// And by at least this many ms.
    pub minimum_improvement_ms: f64,
    /// Current route must stay degraded for this long before we tolerate a
    /// *switch away* due to degradation.
    pub minimum_degradation_duration_ms: u64,
    /// After switching away from a route, block returns for this long.
    pub route_cooldown_ms: u64,
    /// Jitter improvement required (ms) before a switch is justified.
    pub minimum_jitter_improvement_ms: f64,
    /// Loss improvement required (ratio) before a switch is justified.
    pub minimum_loss_improvement: f64,
}

impl Default for HysteresisConfig {
    fn default() -> Self {
        Self {
            minimum_improvement_percent: 0.10, // 10%
            minimum_improvement_ms: 5.0,
            minimum_degradation_duration_ms: 5_000,
            route_cooldown_ms: 15_000,
            minimum_jitter_improvement_ms: 4.0,
            minimum_loss_improvement: 0.005,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RouteVerdict {
    /// Stay on the current route.
    Keep,
    /// Switch to `candidate_id`.
    Switch { candidate_id: String, reason: SwitchReason },
    /// Candidate is in cooldown or insufficiently better.
    NotYet { candidate_id: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchReason {
    SustainedImprovement,
    CurrentRouteDegraded,
}

/// State the engine keeps across evaluations (persisted so hysteresis
/// survives process restarts, spec 15).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HysteresisState {
    /// Current route id.
    pub current_route_id: String,
    /// Edge/route ids currently in cooldown → switch-back block time.
    pub cooldowns: Vec<(String, TimestampMs)>,
    /// When the current route first entered its current degradation regime.
    pub degradation_since: Option<TimestampMs>,
    /// Metrics of the current route at the last evaluation (for degradation
    /// comparison).
    pub current_metrics: MetricsSnapshot,
}

impl Default for HysteresisState {
    fn default() -> Self {
        Self {
            current_route_id: String::new(),
            cooldowns: Vec::new(),
            degradation_since: None,
            current_metrics: MetricsSnapshot {
                latency_ms: f64::MAX,
                jitter_ms: f64::MAX,
                packet_loss_ratio: f64::MAX,
                stability: 0.0,
                handshake_latency_ms: f64::MAX,
                reconnect_rate: f64::MAX,
            },
        }
    }
}

/// Is the candidate "meaningfully better" on latency alone?
fn latency_improvement(current_ms: f64, candidate_ms: f64, cfg: &HysteresisConfig) -> bool {
    if candidate_ms >= current_ms {
        return false;
    }
    let percent_rule = (current_ms - candidate_ms) / current_ms >= cfg.minimum_improvement_percent;
    let abs_rule = current_ms - candidate_ms >= cfg.minimum_improvement_ms;
    percent_rule && abs_rule
}

/// Is the current route degraded enough that switching away is warranted?
/// A *sustained* single-axis failure counts: a route with 8% packet loss but
/// fine latency is still a broken tunnel and must eventually be left
/// (spec 9/10). The sustain gate lives in `evaluate`.
fn current_route_degraded(current: &MetricsSnapshot, _cfg: &HysteresisConfig) -> bool {
    current.packet_loss_ratio >= 0.03 // ≥3% loss
        || current.jitter_ms >= 20.0 // ≥20 ms jitter
        || current.latency_ms >= 200.0 // ≥200 ms latency
}

/// A candidate that is bad on *multiple* axes should not become the new
/// current route just because it beats us on latency by 1 ms.
fn candidate_is_falling(candidate: &MetricsSnapshot) -> bool {
    let loss_bad = candidate.packet_loss_ratio >= 0.03;
    let jitter_bad = candidate.jitter_ms >= 20.0;
    let latency_bad = candidate.latency_ms >= 200.0;
    (loss_bad && jitter_bad) || (loss_bad && latency_bad) || (latency_bad && jitter_bad)
}

fn metrics_are_degraded(candidate: &MetricsSnapshot, _cfg: &HysteresisConfig) -> bool {
    candidate_is_falling(candidate)
}

#[derive(Debug, Clone)]
pub struct RouteDecider {
    pub cfg: HysteresisConfig,
    pub state: HysteresisState,
}

impl RouteDecider {
    pub fn new(cfg: HysteresisConfig) -> Self {
        Self { cfg, state: HysteresisState::default() }
    }

    pub fn from_state(cfg: HysteresisConfig, state: HysteresisState) -> Self {
        Self { cfg, state }
    }

    /// Evaluate switching from the current route (with its fresh metrics at
    /// `now`) to a candidate. Returns a verdict.
    ///
    /// `now` is a wall-clock timestamp the caller supplies; the decider is
    /// deterministic and never reads the clock itself (testable).
    pub fn evaluate(
        &mut self,
        candidate: &crate::model::RouteCandidate,
        now: TimestampMs,
    ) -> RouteVerdict {
        // Cooldown check.
        let cooldown_blocked = self
            .state
            .cooldowns
            .iter()
            .any(|(id, until)| id == &candidate.id && now < *until);
        if cooldown_blocked {
            return RouteVerdict::NotYet {
                candidate_id: candidate.id.clone(),
                reason: "in cooldown".into(),
            };
        }

        let current = &self.state.current_metrics;
        let candidate_metrics = candidate.metrics();

        // Track when the current route entered its degradation regime.
        if current_route_degraded(current, &self.cfg) {
            self.state.degradation_since.get_or_insert(now);
        } else {
            self.state.degradation_since = None;
        }

        let degraded_long_enough = self
            .state
            .degradation_since
            .map(|since| now.saturating_sub(since) >= self.cfg.minimum_degradation_duration_ms)
            .unwrap_or(false);

        // Sustainability: candidate must still be meaningfully better or the
        // current route must be demonstrably failing. Never switch because of
        // a one-sample wiggle.
        if latency_improvement(current.latency_ms, candidate_metrics.latency_ms, &self.cfg)
            && !metrics_are_degraded(&candidate_metrics, &self.cfg)
        {
            return self.switch_to(candidate, SwitchReason::SustainedImprovement, now);
        }

        if degraded_long_enough {
            return self.switch_to(candidate, SwitchReason::CurrentRouteDegraded, now);
        }

        // Jitter/loss-only improvement: only when it is large enough to matter.
        let jitter_improvement = current.jitter_ms - candidate_metrics.jitter_ms;
        let loss_improvement = current.packet_loss_ratio - candidate_metrics.packet_loss_ratio;
        if jitter_improvement >= self.cfg.minimum_jitter_improvement_ms
            && loss_improvement >= self.cfg.minimum_loss_improvement
        {
            return self.switch_to(candidate, SwitchReason::SustainedImprovement, now);
        }

        RouteVerdict::Keep
    }

    fn switch_to(
        &mut self,
        candidate: &crate::model::RouteCandidate,
        reason: SwitchReason,
        now: TimestampMs,
    ) -> RouteVerdict {
        // Cooldown the old route (now + cooldown) so we don't flap back.
        let old = std::mem::take(&mut self.state.current_route_id);
        if !old.is_empty() && old != candidate.id {
            let cooldown_until = now.saturating_add(self.cfg.route_cooldown_ms);
            self.apply_cooldown(&old, cooldown_until);
        }
        self.state.current_route_id = candidate.id.clone();
        self.state.current_metrics = candidate.metrics();
        self.state.degradation_since = None;
        RouteVerdict::Switch { candidate_id: candidate.id.clone(), reason }
    }

    /// Record an evaluation where the current route was improved (used after
    /// successful probes) so the state stays fresh.
    pub fn update_current_metrics(&mut self, metrics: MetricsSnapshot) {
        self.state.current_metrics = metrics;
    }

    /// Set/renew the cooldown ban for a route id. Also prunes expired bans.
    pub fn apply_cooldown(&mut self, route_id: &str, until: TimestampMs) {
        let now = until; // pruning is caller-visible via `now`; see prune_cooldowns
        self.state
            .cooldowns
            .retain(|(_, u)| *u > now.saturating_sub(self.cfg.route_cooldown_ms * 2));
        if let Some(entry) = self.state.cooldowns.iter_mut().find(|(id, _)| id == route_id) {
            entry.1 = until;
        } else {
            self.state.cooldowns.push((route_id.to_string(), until));
        }
    }

    /// Drop cooldowns that have expired relative to `now`.
    pub fn prune_cooldowns(&mut self, now: TimestampMs) {
        self.state
            .cooldowns
            .retain(|(_, until)| *until > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MeasuredSource, RouteCandidate, TransportKind};

    fn route(id: &str, lat: f64, jitter: f64, loss: f64) -> RouteCandidate {
        RouteCandidate {
            id: id.into(),
            endpoint: "e".into(),
            transport: TransportKind::Quic,
            region: "r".into(),
            latency_ms: lat,
            jitter_ms: jitter,
            packet_loss_ratio: loss,
            stability: 0.9,
            handshake_latency_ms: 60.0,
            reconnect_rate: 0.0,
            score: None,
            source: MeasuredSource::Real,
        }
    }

    #[test]
    fn does_not_switch_for_tiny_3ms_difference() {
        // Spec: 80 ms current, 77 ms candidate → do NOT necessarily switch.
        let mut d = RouteDecider::new(HysteresisConfig {
            minimum_improvement_percent: 0.10,
            minimum_improvement_ms: 5.0,
            ..HysteresisConfig::default()
        });
        d.state.current_route_id = "current".into();
        d.state.current_metrics = route("current", 80.0, 2.0, 0.0).metrics();

        let candidate = route("cand", 77.0, 2.0, 0.0);
        let verdict = d.evaluate(&candidate, 100_000);
        assert_eq!(verdict, RouteVerdict::Keep, "3 ms on 80 ms is < 10% AND < 5 ms");
        assert_eq!(d.state.current_route_id, "current");
    }

    #[test]
    fn switches_on_sustained_clear_improvement() {
        let mut d = RouteDecider::new(HysteresisConfig::default());
        d.state.current_route_id = "current".into();
        d.state.current_metrics = route("current", 120.0, 10.0, 0.01).metrics();

        let candidate = route("better", 60.0, 2.0, 0.0005);
        let verdict = d.evaluate(&candidate, 100_000);
        assert_eq!(
            verdict,
            RouteVerdict::Switch {
                candidate_id: "better".into(),
                reason: SwitchReason::SustainedImprovement,
            }
        );
        // Old route is now in cooldown.
        assert!(d.state.cooldowns.iter().any(|(id, _)| id == "current"));
    }

    #[test]
    fn switches_after_sustained_degradation() {
        let mut d = RouteDecider::new(HysteresisConfig {
            minimum_degradation_duration_ms: 2_000,
            ..HysteresisConfig::default()
        });
        d.state.current_route_id = "current".into();
        d.state.current_metrics = route("current", 250.0, 40.0, 0.08).metrics(); // very bad

        // Candidate is NOT meaningfully better on any axis (so only the
        // sustained-degradation path can justify a switch).
        let candidate = route("similar", 245.0, 39.0, 0.075);
        assert_eq!(d.evaluate(&candidate, 100_000), RouteVerdict::Keep, "no immediate switch");
        // After the degradation persists past the minimum duration → switch.
        let verdict = d.evaluate(&candidate, 102_000);
        assert_eq!(
            verdict,
            RouteVerdict::Switch {
                candidate_id: "similar".into(),
                reason: SwitchReason::CurrentRouteDegraded,
            }
        );
    }

    #[test]
    fn cooldown_blocks_immediate_switch_back() {
        let mut d = RouteDecider::new(HysteresisConfig::default());
        d.state.current_route_id = "current".into();
        d.state.current_metrics = route("current", 50.0, 2.0, 0.0).metrics();
        // Candidate attempts a repeat switch-back within cooldown.
        d.state.cooldowns.push(("current".into(), 200_000));
        let candidate = route("current", 45.0, 1.0, 0.0);
        let v = d.evaluate(&candidate, 150_000);
        assert!(matches!(v, RouteVerdict::NotYet { reason, .. } if reason == "in cooldown"));
    }
}