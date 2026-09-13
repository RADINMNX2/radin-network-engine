//! Smart retry (spec 14): exponential backoff with full jitter.
//!
//! Never `while failure { retry immediately }`. Each operation class has its
//! own policy:
//! - Telemetry: aggressive dropping is acceptable.
//! - Configuration: retryable with a cap.
//! - Authentication: controlled (capped, must not hammer).
//! - Real-time dataplane: no application-level retries unless explicitly
//!   required.

use rand::Rng;
use serde::{Deserialize, Serialize};

/// Operation classes with distinct retry personalities (spec 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    /// Fire-and-forget diagnostics. Drop aggressively.
    Telemetry,
    /// Control-plane configuration ops.
    Config,
    /// Authentication / session bootstrap.
    Auth,
    /// Real-time forwarding packets. No app-level retries.
    Dataplane,
}

impl RetryClass {
    /// Maximum total attempts before we stop (0 = no retry at all).
    pub fn max_attempts(self) -> u32 {
        match self {
            RetryClass::Telemetry => 2,
            RetryClass::Config => 6,
            RetryClass::Auth => 4,
            RetryClass::Dataplane => 1,
        }
    }

    pub fn base_delay_ms(self) -> u64 {
        match self {
            RetryClass::Telemetry => 250,
            RetryClass::Config => 500,
            RetryClass::Auth => 1_000,
            RetryClass::Dataplane => 0,
        }
    }

    pub fn cap_ms(self) -> u64 {
        match self {
            RetryClass::Telemetry => 2_000,
            RetryClass::Config => 15_000,
            RetryClass::Auth => 8_000,
            RetryClass::Dataplane => 0,
        }
    }
}

/// Full jitter backoff: `delay = random(0, min(cap, base × 2^attempt))`.
/// `attempt` is zero-based. Returns ms.
pub fn full_jitter_backoff<R: Rng>(rng: &mut R, base_ms: u64, cap_ms: u64, attempt: u32) -> u64 {
    if attempt == 0 || base_ms == 0 {
        return 0;
    }
    let exp = base_ms.saturating_mul(1u64 << attempt.min(20));
    let ceiling = exp.min(cap_ms);
    if ceiling <= 1 {
        return 0;
    }
    rng.gen_range(0..ceiling)
}

/// Deterministic non-jittered upper bound (used in tests and scheduling
/// guarantees).
pub fn backoff_upper_bound_ms(base_ms: u64, cap_ms: u64, attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    let exp = base_ms.saturating_mul(1u64 << attempt.min(20));
    exp.min(cap_ms)
}

/// Scheduler: computes the exact delay for a next attempt under a class
/// policy. Returns `None` when the class forbids further retries.
pub fn next_retry_delay<R: Rng>(
    rng: &mut R,
    class: RetryClass,
    attempt: u32, // 0-based: first retry = attempt 1
) -> Option<u64> {
    if attempt >= class.max_attempts() {
        return None;
    }
    if class == RetryClass::Dataplane {
        return None; // no application-level retries on the real-time path
    }
    Some(full_jitter_backoff(
        rng,
        class.base_delay_ms(),
        class.cap_ms(),
        attempt,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn jitter_stays_within_upper_bound() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        for attempt in 1..=10 {
            for _ in 0..1000 {
                let delay = full_jitter_backoff(&mut rng, 500, 15_000, attempt);
                let bound = backoff_upper_bound_ms(500, 15_000, attempt);
                assert!(delay <= bound, "delay {delay} > bound {bound}");
            }
        }
    }

    #[test]
    fn jitter_is_not_degenerate_fixed_point() {
        // Full jitter must not collapse to a constant.
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut min = u64::MAX;
        let mut max = 0;
        for _ in 0..500 {
            let d = full_jitter_backoff(&mut rng, 1_000, 10_000, 4);
            min = min.min(d);
            max = max.max(d);
        }
        assert!(max > min, "jitter produced a degenerate fixed delay");
    }

    #[test]
    fn telemetry_drops_aggressively() {
        // Telemetry: attempt 0 → immediate-ish, after 2 attempts it's done.
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        assert_eq!(RetryClass::Telemetry.max_attempts(), 2);
        assert!(next_retry_delay(&mut rng, RetryClass::Telemetry, 0).is_some());
        assert!(next_retry_delay(&mut rng, RetryClass::Telemetry, 1).is_some());
        assert!(next_retry_delay(&mut rng, RetryClass::Telemetry, 2).is_none());
    }

    #[test]
    fn dataplane_never_retries() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        assert!(next_retry_delay(&mut rng, RetryClass::Dataplane, 0).is_none());
        assert!(next_retry_delay(&mut rng, RetryClass::Dataplane, 5).is_none());
    }

    #[test]
    fn config_caps_at_policy() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        assert!(next_retry_delay(&mut rng, RetryClass::Config, 5).is_some());
        assert!(next_retry_delay(&mut rng, RetryClass::Config, 6).is_none());
    }
}
