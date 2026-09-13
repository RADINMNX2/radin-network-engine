//! Active probing (spec 21) — lightweight and advanced probes with
//! rate limiting. Probes never recreate the traffic pattern of a full
//! forwarder; they are sparse, scheduled, and adapt to the health state.

use serde::{Deserialize, Serialize};

use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    DnsResolution,
    TcpConnect,
    UdpProbe,
    QuicHandshake,
    TunnelHealth,
    EdgeToClientRtt,
    ApplicationEndpointRtt,
}

impl ProbeKind {
    pub fn is_lightweight(self) -> bool {
        matches!(
            self,
            ProbeKind::DnsResolution
                | ProbeKind::TcpConnect
                | ProbeKind::UdpProbe
                | ProbeKind::QuicHandshake
        )
    }

    /// Default minimum gap between two probes of this kind (ms).
    pub fn default_min_interval_ms(self) -> u64 {
        match self {
            ProbeKind::DnsResolution => 30_000,
            ProbeKind::TcpConnect => 30_000,
            ProbeKind::UdpProbe => 20_000,
            ProbeKind::QuicHandshake => 60_000,
            ProbeKind::TunnelHealth => 10_000,
            ProbeKind::EdgeToClientRtt => 5_000,
            ProbeKind::ApplicationEndpointRtt => 60_000,
        }
    }
}

/// Per-kind rate limiter state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeBudget {
    pub kind: ProbeKind,
    pub min_interval_ms: u64,
    pub last_sent_at: Option<TimestampMs>,
    pub sent: u64,
}

impl ProbeBudget {
    pub fn new(kind: ProbeKind) -> Self {
        Self {
            kind,
            min_interval_ms: kind.default_min_interval_ms(),
            last_sent_at: None,
            sent: 0,
        }
    }

    /// May we send now? Pure rate-limit logic; `now` must be monotonic-ish.
    pub fn allow(&self, now: TimestampMs) -> bool {
        match self.last_sent_at {
            None => true,
            Some(last) => now.saturating_sub(last) >= self.min_interval_ms,
        }
    }

    pub fn record_send(&mut self, now: TimestampMs) {
        self.last_sent_at = Some(now);
        self.sent += 1;
    }

    /// Tighten intervals (ms) when the network degrades; never below floor.
    pub fn tighten(&mut self, factor: f64, floor_ms: u64) {
        let base = self.kind.default_min_interval_ms();
        let scaled = (base as f64 * factor) as u64;
        self.min_interval_ms = scaled.max(floor_ms);
    }

    /// Loosen intervals when stable: return gradually, never permanently
    /// maxed (spec 24).
    pub fn loosen(&mut self, factor: f64) {
        let base = self.kind.default_min_interval_ms();
        let scaled = (self.min_interval_ms as f64 * factor) as u64;
        self.min_interval_ms = scaled.min(base);
    }
}

/// Centralized probe scheduler across all kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeScheduler {
    pub budgets: Vec<ProbeBudget>,
    /// Whether full-fidelity probes are active (app foreground / VPN on).
    pub enabled: bool,
}

impl Default for ProbeScheduler {
    fn default() -> Self {
        Self {
            budgets: (0..8)
                .map(|i| match i {
                    0 => ProbeBudget::new(ProbeKind::DnsResolution),
                    1 => ProbeBudget::new(ProbeKind::TcpConnect),
                    2 => ProbeBudget::new(ProbeKind::UdpProbe),
                    3 => ProbeBudget::new(ProbeKind::QuicHandshake),
                    4 => ProbeBudget::new(ProbeKind::TunnelHealth),
                    5 => ProbeBudget::new(ProbeKind::EdgeToClientRtt),
                    _ => ProbeBudget::new(ProbeKind::ApplicationEndpointRtt),
                })
                .collect(),
            enabled: true,
        }
    }
}

impl ProbeScheduler {
    pub fn may_send(&self, kind: ProbeKind, now: TimestampMs) -> bool {
        if !self.enabled {
            return false;
        }
        self.budgets
            .iter()
            .find(|b| b.kind == kind)
            .map(|b| b.allow(now))
            .unwrap_or(false)
    }

    pub fn record(&mut self, kind: ProbeKind, now: TimestampMs) {
        if let Some(b) = self.budgets.iter_mut().find(|b| b.kind == kind) {
            b.record_send(now);
        }
    }

    /// Apply adaptive monitoring policy (spec 24): degrade → tight; stable →
    /// loose (gradual return).
    pub fn apply_health(&mut self, level: crate::health::MonitorLevel) {
        for b in self.budgets.iter_mut() {
            match level {
                crate::health::MonitorLevel::Low => b.loosen(1.15),
                crate::health::MonitorLevel::Normal => b.loosen(1.05),
                crate::health::MonitorLevel::High => b.tighten(0.5, 3_000),
                crate::health::MonitorLevel::Aggressive => b.tighten(0.25, 1_000),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_enforces_minimum_gap() {
        let mut p = ProbeBudget::new(ProbeKind::UdpProbe);
        assert!(p.allow(0));
        p.record_send(0);
        assert!(!p.allow(19_999));
        assert!(p.allow(20_000));
    }

    #[test]
    fn scheduler_respects_disabled_state() {
        let mut s = ProbeScheduler {
            enabled: false,
            ..ProbeScheduler::default()
        };
        assert!(!s.may_send(ProbeKind::UdpProbe, 0));
        s.enabled = true;
        assert!(s.may_send(ProbeKind::UdpProbe, 0));
    }

    #[test]
    fn aggressive_health_tightens_dns_probes() {
        let mut s = ProbeScheduler::default();
        let before = s.budgets[0].min_interval_ms;
        s.apply_health(crate::health::MonitorLevel::Aggressive);
        assert!(s.budgets[0].min_interval_ms < before);
        // Return to low after stability → must not stay maxed forever.
        s.apply_health(crate::health::MonitorLevel::Low);
        s.apply_health(crate::health::MonitorLevel::Low);
        assert!(s.budgets[0].min_interval_ms > 1_000);
    }
}
