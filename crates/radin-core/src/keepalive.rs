//! Jittered, adaptive keepalive (spec 7).
//!
//! `heartbeat_interval = base_interval ± randomized_jitter`. The base adapts
//! to network type, battery, VPN state, observed NAT idle timeout and
//! connection stability. We never synchronize heartbeats across parallel
//! connections (per-connection randomized phase + jitter). When the
//! application is idle and no edge is in use, keepalive is suspended.

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::model::{BatteryState, NetworkType, VpnState};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KeepaliveInput {
    /// Base candidate interval (ms) from observed NAT idle behavior.
    pub observed_nat_idle_timeout_ms: Option<u64>,
    /// Manual override; if set, beats everything (max 4h).
    pub explicit_interval_ms: Option<u64>,
    pub network: NetworkType,
    pub battery: BatteryState,
    pub vpn: VpnState,
    /// Recent reconnect rate (reconnects/hour); high → keep alive more.
    pub reconnect_rate: Option<f64>,
    /// True when the app is idle (foreground/background policy).
    pub app_idle: bool,
}

impl Default for KeepaliveInput {
    fn default() -> Self {
        Self {
            observed_nat_idle_timeout_ms: None,
            explicit_interval_ms: None,
            network: NetworkType::Unknown,
            battery: BatteryState::High,
            vpn: VpnState::Disconnected,
            reconnect_rate: None,
            app_idle: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeepaliveDecision {
    /// Effective base interval in ms (before jitter).
    pub base_interval_ms: u64,
    /// Randomized interval for *this* heartbeat in ms.
    pub send_after_ms: u64,
    /// Jitter actually applied (fraction of base, ±).
    pub jitter_fraction: f64,
    /// Whether heartbeats should be sent at all right now.
    pub keep_alive_enabled: bool,
}

/// Cap the per-instance jitter fraction (default ±25%, configurable).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct JitterPolicy {
    pub max_fraction: f64,
}

impl Default for JitterPolicy {
    fn default() -> Self {
        Self { max_fraction: 0.25 }
    }
}

/// Compute the base interval from the input, without randomness.
pub fn base_interval(input: &KeepaliveInput) -> u64 {
    if let Some(explicit) = input.explicit_interval_ms {
        return explicit.clamp(1_000, 4 * 60 * 60_000);
    }
    if input.app_idle && input.vpn == VpnState::Disconnected {
        return 0; // suspend keepalive entirely when idle & no tunnel.
    }
    let nat_hint = input.observed_nat_idle_timeout_ms.filter(|v| *v >= 1_000);
    let network_base = input.network.default_keepalive_base_ms();
    let base = nat_hint.unwrap_or(network_base).min(120_000);

    // Battery adaptation: keep the tunnel alive less aggressively on low
    // battery, more on charging.
    let battery_factor = match input.battery {
        BatteryState::Charging => 1.2,
        BatteryState::High => 1.0,
        BatteryState::Medium => 0.9,
        BatteryState::Low => 0.7,
        BatteryState::Critical => 0.5,
    };

    // Stability adaptation: recent reconnects tighten the interval.
    let stable_factor = match input.reconnect_rate {
        Some(rate) if rate >= 4.0 => 0.6,
        Some(rate) if rate >= 1.0 => 0.8,
        _ => 1.0,
    };

    let base = (base as f64 * battery_factor * stable_factor) as u64;
    // Never below 2s (NAT freshness floor), never above 2 min.
    base.clamp(2_000, 120_000)
}

/// Roll a single randomized heartbeat interval for one connection.
/// Independent per connection → no synchronized heartbeat storms.
pub fn next_interval<R: Rng>(rng: &mut R, base_ms: u64, policy: &JitterPolicy) -> (u64, f64) {
    if base_ms == 0 {
        return (0, 0.0);
    }
    // Uniform jitter in [-max_fraction, +max_fraction] of base.
    let frac = rng.gen_range(-policy.max_fraction..=policy.max_fraction);
    let out = (base_ms as f64 * (1.0 + frac)) as u64;
    (out.max(1), frac)
}

/// One-stop decision for a single connection heartbeat.
pub fn decide<R: Rng>(
    rng: &mut R,
    input: &KeepaliveInput,
    policy: &JitterPolicy,
) -> KeepaliveDecision {
    let base = base_interval(input);
    if base == 0 {
        return KeepaliveDecision {
            base_interval_ms: 0,
            send_after_ms: 0,
            jitter_fraction: 0.0,
            keep_alive_enabled: false,
        };
    }
    let (send_after_ms, jitter_fraction) = next_interval(rng, base, policy);
    KeepaliveDecision {
        base_interval_ms: base,
        send_after_ms,
        jitter_fraction,
        keep_alive_enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn heartbeat_is_jittered_within_bounds() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        let input = KeepaliveInput {
            network: NetworkType::Wifi,
            ..KeepaliveInput::default()
        };
        let base = base_interval(&input);
        assert!(base > 0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..500 {
            let d = decide(&mut rng, &input, &JitterPolicy::default());
            assert!(d.keep_alive_enabled);
            let f = d.send_after_ms as f64 / base as f64;
            assert!((0.75..=1.25).contains(&f), "jitter out of ±25%: {f}");
            seen.insert(d.send_after_ms);
        }
        assert!(seen.len() > 1, "no randomization → synchronized heartbeats");
    }

    #[test]
    fn suspended_when_idle_and_disconnected() {
        let input = KeepaliveInput {
            app_idle: true,
            vpn: VpnState::Disconnected,
            ..KeepaliveInput::default()
        };
        assert_eq!(base_interval(&input), 0);
    }

    #[test]
    fn explicit_interval_wins() {
        let input = KeepaliveInput {
            explicit_interval_ms: Some(60_000),
            network: NetworkType::FourG,
            ..KeepaliveInput::default()
        };
        assert_eq!(base_interval(&input), 60_000);
    }

    #[test]
    fn nat_observed_idle_shortens_interval() {
        let with_nat = KeepaliveInput {
            observed_nat_idle_timeout_ms: Some(4_000),
            battery: BatteryState::Charging,
            ..KeepaliveInput::default()
        };
        let b = base_interval(&with_nat);
        assert!(
            (2_000..=6_000).contains(&b),
            "base {b} should hug the NAT hint"
        );
    }

    #[test]
    fn low_battery_tightens_interval() {
        let high = KeepaliveInput {
            battery: BatteryState::Charging,
            ..KeepaliveInput::default()
        };
        let low = KeepaliveInput {
            battery: BatteryState::Critical,
            ..KeepaliveInput::default()
        };
        assert!(base_interval(&low) < base_interval(&high));
    }
}
