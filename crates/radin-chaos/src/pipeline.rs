//! PacketPipeline: applies a `ChaosProfile` to a packet flow.
//!
//! This is the TEST-ONLY transformer the entire 38-matrix suite runs
//! against. It is a pure function of (packet, state, rng) using a
//! deterministic seeded RNG — the same run always produces the same chaos,
//! making failure scenarios reproducible.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::profile::{ChaosProfile, FailureMode};

/// A packet as it enters the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketRecord {
    pub seq: u64,
    pub payload: Vec<u8>,
    /// Injection time (tick count) — relative, test-relative.
    pub at_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PacketResult {
    /// Delivered after `delay_ms` (relative delivery monotonic time).
    Delivered {
        seq: u64,
        delay_ms: u64,
        /// True when a reorder queue delivered it out of order.
        reordered: bool,
    },
    /// Dropped by loss or bandwidth cap.
    Dropped { seq: u64 },
    /// Connection is down (reset/outage): the whole flow restarts.
    ConnectionDown { seq: u64 },
}

/// Mutable pipeline state (whose lifetime spans the connection).
#[derive(Debug, Clone)]
pub struct PipelineState {
    pub profile: ChaosProfile,
    /// Single-slot reorder buffer: a packet held for reordering, released
    /// out-of-order when the next packet arrives.
    pub reorder_pending: Option<(PacketRecord, u64)>,
    pub packets_processed: u64,
    pub packets_dropped: u64,
    pub resets: u64,
    pub bytes_in_flight: u64,
    /// When the connection is in an outage window [down_until_tick, ...).
    pub down_until_tick: Option<u64>,
    rng: StdRng,
}

impl PipelineState {
    pub fn from_profile(profile: ChaosProfile, seed: u64) -> Self {
        Self {
            profile,
            reorder_pending: None,
            packets_processed: 0,
            packets_dropped: 0,
            resets: 0,
            bytes_in_flight: 0,
            down_until_tick: None,
            rng: StdRng::seed_from_u64(seed),
        }
    }

    pub fn seed_from(profile: ChaosProfile, seed: u64) -> Self {
        Self::from_profile(profile, seed)
    }

    /// Push one packet; produce its fate. `tick` is a monotonic test-time
    /// counter (ms).
    pub fn push(&mut self, pkt: PacketRecord, tick: u64) -> PacketResult {
        self.packets_processed += 1;

        // Connection-level failure handling.
        match self.profile.failure {
            FailureMode::None => {}
            FailureMode::ResetEveryPackets(n) if n > 0 => {
                if self.packets_processed % n == 0 {
                    self.resets += 1;
                    self.reorder_pending = None;
                    self.bytes_in_flight = 0;
                    return PacketResult::ConnectionDown { seq: pkt.seq };
                }
            }
            FailureMode::ResetEveryPackets(_) => {}
            FailureMode::Intermittent {
                down_ratio,
                mean_down_ms,
            } => {
                if let Some(until) = self.down_until_tick {
                    if tick < until {
                        return PacketResult::ConnectionDown { seq: pkt.seq };
                    }
                    self.down_until_tick = None;
                }
                if down_ratio > 0.0 && self.rng.gen_bool(down_ratio) {
                    self.resets += 1;
                    let outage = self.rng.gen_range(1..=mean_down_ms.max(1));
                    self.down_until_tick = Some(tick + outage);
                    self.reorder_pending = None;
                    self.bytes_in_flight = 0;
                    return PacketResult::ConnectionDown { seq: pkt.seq };
                }
            }
        }

        // Loss.
        if self.profile.loss_ratio > 0.0 && self.rng.gen_bool(self.profile.loss_ratio) {
            self.packets_dropped += 1;
            return PacketResult::Dropped { seq: pkt.seq };
        }

        // Bandwidth cap: approximate with a byte-bucket on outstanding
        // packets (drop when over budget).
        if self.profile.bandwidth_bps > 0 {
            let bits = pkt.payload.len() as u64 * 8;
            // Token-bucket approximation: 1ms of budget per bit of rate.
            let budget_per_ms = self.profile.bandwidth_bps / 1000;
            if budget_per_ms > 0 && bits > budget_per_ms {
                self.packets_dropped += 1;
                return PacketResult::Dropped { seq: pkt.seq };
            }
        }

        // Latency + jitter.
        let base = self.profile.latency_ms.max(0.0);
        let jitter = if self.profile.jitter_ms > 0.0 {
            // Two-sided uniform around 0.
            self.rng
                .gen_range(-self.profile.jitter_ms..=self.profile.jitter_ms)
        } else {
            0.0
        };

        // Reordering: hold the current packet; emit the previously held one
        // out-of-order (single-slot swap gives reproducible reordering).
        let do_reorder =
            self.profile.reorder_ratio > 0.0 && self.rng.gen_bool(self.profile.reorder_ratio);

        if let Some((held, held_tick)) = self.reorder_pending.take() {
            // Release the held packet first (it belongs BEFORE the current).
            if held_tick <= tick {
                self.bytes_in_flight = self
                    .bytes_in_flight
                    .saturating_sub(held.payload.len() as u64);
                return PacketResult::Delivered {
                    seq: held.seq,
                    delay_ms: (tick.saturating_sub(held_tick)),
                    reordered: true,
                };
            }
            // Not due yet — keep it for now and treat current normally.
            self.reorder_pending = Some((held, held_tick));
        }

        let delay = (base + jitter).max(0.0) as u64;

        if do_reorder {
            self.reorder_pending = Some((pkt.clone(), tick));
            self.bytes_in_flight += pkt.payload.len() as u64;
            // The current packet is accepted but its delivery is deferred to
            // the next push; report it as delivered-with-delay so a caller
            // batch still observes every packet once.
            return PacketResult::Delivered {
                seq: pkt.seq,
                delay_ms: delay,
                reordered: false,
            };
        }

        PacketResult::Delivered {
            seq: pkt.seq,
            delay_ms: delay,
            reordered: false,
        }
    }

    /// Run `n` packets through the pipeline, collecting fates.
    pub fn run(&mut self, n: u64, tick: u64) -> Vec<PacketResult> {
        let mut out = Vec::with_capacity(n as usize);
        for seq in 0..n {
            out.push(self.push(
                PacketRecord {
                    seq,
                    payload: vec![0u8; 64],
                    at_tick: tick,
                },
                tick,
            ));
        }
        out
    }

    /// Measured-loss summary of a run's fates.
    pub fn measure(results: &[PacketResult]) -> (Vec<PacketResult>, f64, u64, u64) {
        let dropped = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    PacketResult::Dropped { .. } | PacketResult::ConnectionDown { .. }
                )
            })
            .count();
        let total = results.len().max(1);
        let ratio = dropped as f64 / total as f64;
        let resets = results
            .iter()
            .filter(|r| matches!(r, PacketResult::ConnectionDown { .. }))
            .count() as u64;
        (results.to_vec(), ratio, dropped as u64, resets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pristine_profile_has_zero_loss_and_no_resets() {
        let mut p = PipelineState::from_profile(ChaosProfile::with_latency(30.0), 1);
        let (_, ratio, dropped, resets) = PipelineState::measure(&p.run(1000, 0));
        assert_eq!(dropped, 0);
        assert_eq!(resets, 0);
        assert_eq!(ratio, 0.0);
    }

    #[test]
    fn loss_ratio_is_approximated_statistically() {
        for loss in [0.0, 0.005, 0.01, 0.03, 0.05, 0.10] {
            let mut p = PipelineState::from_profile(ChaosProfile::with_loss(loss), 42);
            let (_, ratio, _, _) = PipelineState::measure(&p.run(20_000, 0));
            let tolerance = 0.02;
            assert!(
                (ratio - loss).abs() < tolerance,
                "loss {loss}: measured {ratio:.4} outside +-{tolerance}"
            );
        }
    }

    #[test]
    fn latency_profile_delivers_with_delay() {
        let mut p = PipelineState::from_profile(ChaosProfile::with_latency(120.0), 7);
        let results = p.run(100, 0);
        let delivered: Vec<&PacketResult> = results
            .iter()
            .filter(|r| matches!(r, PacketResult::Delivered { .. }))
            .collect();
        let first_delay = match delivered[0] {
            PacketResult::Delivered { delay_ms, .. } => *delay_ms,
            _ => 0,
        };
        assert!(first_delay >= 120);
    }

    #[test]
    fn resets_trigger_on_connection_failure_mode() {
        let mut p = PipelineState::from_profile(
            ChaosProfile::with_failure(FailureMode::ResetEveryPackets(25)),
            3,
        );
        let results = p.run(200, 0);
        let resets = results
            .iter()
            .filter(|r| matches!(r, PacketResult::ConnectionDown { .. }))
            .count();
        assert!(resets > 0, "expected at least one connection reset");
        assert_eq!(p.resets, resets as u64);
    }

    #[test]
    fn deterministic_seed_reproduces_fates() {
        let mut a = PipelineState::from_profile(ChaosProfile::with_loss(0.05), 99);
        let mut b = PipelineState::from_profile(ChaosProfile::with_loss(0.05), 99);
        let ra = a.run(2000, 0);
        let rb = b.run(2000, 0);
        assert_eq!(ra, rb, "seeded chaos must be reproducible");
    }

    #[test]
    fn reordering_can_emit_out_of_order() {
        let mut p = PipelineState::from_profile(ChaosProfile::with_reordering(1.0), 5);
        // With 100% reorder, packets queue; delivering happens on release.
        // Function-level check: some results are marked reordered.
        let results = p.run(50, 0);
        let reordered = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    PacketResult::Delivered {
                        reordered: true,
                        ..
                    }
                )
            })
            .count();
        assert!(reordered > 0);
    }

    #[test]
    fn bandwidth_cap_drops_oversize_bursts() {
        // 8 kbps cap ≈ 1 byte/ms of budget → 64-byte packets exceed it.
        let mut p = PipelineState::from_profile(
            ChaosProfile {
                bandwidth_bps: 8_000,
                latency_ms: 30.0,
                ..ChaosProfile::default()
            },
            11,
        );
        let (_, _, dropped, _) = PipelineState::measure(&p.run(1000, 0));
        assert!(dropped > 0, "packets should exceed the byte budget");
    }
}
