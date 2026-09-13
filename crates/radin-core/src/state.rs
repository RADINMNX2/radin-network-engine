//! Per-transport health state machine (spec 6).
//!
//! HEALTHY → DEGRADED → FAILING → FAILED, with RECOVERING as the exit hatch.
//! Transitions require *consecutive* failures or sustained timeouts, never a
//! single lost packet. Hysteresis and cooldowns live in later-stage windows.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportHealth {
    Healthy,
    Degraded,
    Failing,
    Failed,
    Recovering,
}

impl std::fmt::Display for TransportHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportHealth::Healthy => write!(f, "HEALTHY"),
            TransportHealth::Degraded => write!(f, "DEGRADED"),
            TransportHealth::Failing => write!(f, "FAILING"),
            TransportHealth::Failed => write!(f, "FAILED"),
            TransportHealth::Recovering => write!(f, "RECOVERING"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfig {
    /// Consecutive failures to drop HEALTHY → DEGRADED.
    pub degraded_after_failures: u32,
    /// Additional consecutive failures DEGRADED → FAILING.
    pub failing_after_failures: u32,
    /// Consecutive failures FAILING → FAILED.
    pub failed_after_failures: u32,
    /// After a recovery signal, require this many successes to leave
    /// RECOVERING back to HEALTHY.
    pub recover_after_successes: u32,
    /// Consecutive timeouts that also count as failures.
    pub timeout_failure_count: u32,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            degraded_after_failures: 2,
            failing_after_failures: 3,
            failed_after_failures: 5,
            recover_after_successes: 2,
            timeout_failure_count: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportStateMachine {
    pub transport: crate::model::TransportKind,
    pub cfg: StateConfig,
    pub state: TransportHealth,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    /// Monotonic failure count within the current regime (diagnostics only).
    pub stale_packets_dropped: u64,
}

impl TransportStateMachine {
    pub fn new(transport: crate::model::TransportKind, cfg: StateConfig) -> Self {
        Self {
            transport,
            cfg,
            state: TransportHealth::Healthy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            stale_packets_dropped: 0,
        }
    }

    /// Drop a single observed failure (timeout, handshake error, loss spike).
    pub fn record_failure(&mut self) {
        self.consecutive_successes = 0;
        self.consecutive_failures += 1;
        self.apply_failure_window();
    }

    fn apply_failure_window(&mut self) {
        let f = self.consecutive_failures;
        match self.state {
            TransportHealth::Healthy if f >= self.cfg.degraded_after_failures => {
                self.state = TransportHealth::Degraded;
            }
            TransportHealth::Degraded if f >= self.cfg.failing_after_failures => {
                self.state = TransportHealth::Failing;
            }
            TransportHealth::Failing if f >= self.cfg.failed_after_failures => {
                self.state = TransportHealth::Failed;
            }
            _ => {}
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        match self.state {
            TransportHealth::Recovering | TransportHealth::Failed => {
                self.consecutive_successes += 1;
                if self.consecutive_successes >= self.cfg.recover_after_successes {
                    self.state = TransportHealth::Healthy;
                    self.consecutive_successes = 0;
                } else {
                    self.state = TransportHealth::Recovering;
                }
            }
            _ => {
                self.consecutive_successes = 0;
                self.state = TransportHealth::Healthy;
            }
        }
    }

    /// Explicit recovery trigger after reconnection/migration succeeded.
    pub fn begin_recovery(&mut self) {
        self.state = TransportHealth::Recovering;
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
    }

    pub fn is_usable(&self) -> bool {
        !matches!(self.state, TransportHealth::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TransportKind;

    #[test]
    fn progresses_through_states_on_consecutive_failures() {
        let mut sm = TransportStateMachine::new(TransportKind::Udp, StateConfig::default());
        for _ in 0..1 {
            sm.record_failure();
        }
        assert_eq!(sm.state, TransportHealth::Healthy, "single failure is not enough");
        sm.record_failure();
        assert_eq!(sm.state, TransportHealth::Degraded);
        for _ in 0..1 {
            sm.record_failure();
        }
        assert_eq!(sm.state, TransportHealth::Failing);
        for _ in 0..2 {
            sm.record_failure();
        }
        assert_eq!(sm.state, TransportHealth::Failed);
        assert!(!sm.is_usable());
    }

    #[test]
    fn recovers_after_successes() {
        let mut sm = TransportStateMachine::new(TransportKind::Udp, StateConfig::default());
        for _ in 0..5 {
            sm.record_failure();
        }
        assert_eq!(sm.state, TransportHealth::Failed);
        sm.begin_recovery();
        assert_eq!(sm.state, TransportHealth::Recovering);
        sm.record_success();
        assert_eq!(sm.state, TransportHealth::Recovering);
        sm.record_success();
        assert_eq!(sm.state, TransportHealth::Healthy);
    }

    #[test]
    fn single_packet_loss_never_fails_transport() {
        let mut sm = TransportStateMachine::new(TransportKind::TcpTls, StateConfig::default());
        sm.record_failure();
        assert_eq!(sm.state, TransportHealth::Healthy);
    }
}