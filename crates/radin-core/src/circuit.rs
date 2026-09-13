//! Circuit breaker (spec 13). Independent per edge and per transport.
//!
//! CLOSED → (repeated failures) → OPEN → (cooldown) → HALF_OPEN → (probe
//! success) → CLOSED; (probe failure) → OPEN. Prevents flooding dead routes.

use serde::{Deserialize, Serialize};

use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    /// Fails open until `until` (in ms epoch). All traffic is short-circuited.
    Open { until: TimestampMs },
    /// Single trial probe allowed.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "CLOSED"),
            CircuitState::Open { .. } => write!(f, "OPEN"),
            CircuitState::HalfOpen => write!(f, "HALF_OPEN"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitConfig {
    /// Consecutive failures needed to trip CLOSED → OPEN.
    pub failure_threshold: u32,
    /// Cooldown while OPEN before a HALF_OPEN trial.
    pub open_cooldown_ms: u64,
    /// How many successes the HALF_OPEN probe needs to go back CLOSED.
    pub half_open_required_successes: u32,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            open_cooldown_ms: 5_000,
            half_open_required_successes: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreaker {
    pub key: String,
    pub cfg: CircuitConfig,
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub half_open_successes: u32,
    /// Total trips (lifetime diagnostics).
    pub trip_count: u64,
}

impl CircuitBreaker {
    pub fn new(key: impl Into<String>, cfg: CircuitConfig) -> Self {
        Self {
            key: key.into(),
            cfg,
            state: CircuitState::Closed,
            consecutive_failures: 0,
            half_open_successes: 0,
            trip_count: 0,
        }
    }

    /// `allow_traffic`: with `now`, report whether a new request may proceed.
    /// OPEN short-circuits until cooldown expires; then we transition to
    /// HALF_OPEN and allow exactly one probe.
    pub fn allow(&mut self, now: TimestampMs) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open { until } => {
                if now >= until {
                    self.state = CircuitState::HalfOpen;
                    self.half_open_successes = 0;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a success. In HALF_OPEN we need `half_open_required_successes`
    /// before re-closing; otherwise we reset the failure counter.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        match self.state {
            CircuitState::HalfOpen => {
                self.half_open_successes += 1;
                if self.half_open_successes >= self.cfg.half_open_required_successes {
                    self.state = CircuitState::Closed;
                    self.half_open_successes = 0;
                }
            }
            _ => {
                self.state = CircuitState::Closed;
                self.half_open_successes = 0;
            }
        }
    }

    /// Record a failure. From HALF_OPEN any failure re-opens (spec 13).
    pub fn record_failure(&mut self, now: TimestampMs) {
        self.consecutive_failures += 1;
        self.half_open_successes = 0;
        match self.state {
            CircuitState::HalfOpen => {
                self.trip_count += 1;
                self.state = CircuitState::Open { until: now + self.cfg.open_cooldown_ms };
            }
            CircuitState::Closed if self.consecutive_failures >= self.cfg.failure_threshold => {
                self.trip_count += 1;
                self.state = CircuitState::Open { until: now + self.cfg.open_cooldown_ms };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_after_threshold_repeated_failures() {
        let mut cb = CircuitBreaker::new("edge-a", CircuitConfig::default());
        assert!(matches!(cb.state, CircuitState::Closed));
        cb.record_failure(1_000);
        cb.record_failure(2_000);
        assert!(matches!(cb.state, CircuitState::Closed), "one left");
        cb.record_failure(3_000);
        assert!(matches!(cb.state, CircuitState::Open { .. }));
    }

    #[test]
    fn opens_then_half_opens_after_cooldown() {
        let mut cb = CircuitBreaker::new("t", CircuitConfig {
            failure_threshold: 1,
            open_cooldown_ms: 1_000,
            ..CircuitConfig::default()
        });
        cb.record_failure(0);
        assert!(!cb.allow(500), "still OPEN");
        assert!(cb.allow(1_001), "cooldown expired → HALF_OPEN probe allowed");
        assert!(matches!(cb.state, CircuitState::HalfOpen));
        cb.record_failure(1_010);
        assert!(matches!(cb.state, CircuitState::Open { .. }), "half-open probe failure → OPEN");
    }

    #[test]
    fn half_open_requires_successes_before_closing() {
        let mut cb = CircuitBreaker::new("t", CircuitConfig {
            failure_threshold: 1,
            open_cooldown_ms: 1_000,
            half_open_required_successes: 2,
        });
        cb.record_failure(0);
        cb.allow(1_000);
        cb.record_success();
        assert!(matches!(cb.state, CircuitState::HalfOpen), "needs 2 successes");
        cb.record_success();
        assert!(matches!(cb.state, CircuitState::Closed));
        assert_eq!(cb.trip_count, 1);
    }

    #[test]
    fn success_resets_failure_counter() {
        let mut cb = CircuitBreaker::new("t", CircuitConfig::default());
        cb.record_failure(0);
        cb.record_failure(1);
        cb.record_success();
        cb.record_failure(2);
        cb.record_failure(3);
        assert!(matches!(cb.state, CircuitState::Closed), "threshold reset by success");
    }
}