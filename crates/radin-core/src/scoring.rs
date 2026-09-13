//! Weighted, configurable route scoring (spec 11).
//!
//! Scores are sub-scores in [0, 1] (1 = best), combined with configurable
//! weights into a final score in [0, 100]. We NEVER optimize for average
//! ping alone: a jittery slightly-faster route loses to a stable one, as in
//! the spec's canonical example (65 ms/30 ms jitter/2% loss vs
//! 80 ms/2 ms jitter/0% loss).

use serde::{Deserialize, Serialize};

use crate::model::{MeasuredSource, MetricsSnapshot, RouteCandidate};

/// Configurable weights (spec 11). Defaults are calibrated so stability and
/// loss dominate for gaming traffic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreWeights {
    pub latency_weight: f64,
    pub jitter_weight: f64,
    pub loss_weight: f64,
    pub stability_weight: f64,
    pub handshake_weight: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            latency_weight: 0.30,
            jitter_weight: 0.20,
            loss_weight: 0.30,
            stability_weight: 0.15,
            handshake_weight: 0.05,
        }
    }
}

impl ScoreWeights {
    pub fn normalize(&self) -> Self {
        let sum = self.latency_weight
            + self.jitter_weight
            + self.loss_weight
            + self.stability_weight
            + self.handshake_weight;
        if sum <= f64::EPSILON {
            return Self::default();
        }
        Self {
            latency_weight: self.latency_weight / sum,
            jitter_weight: self.jitter_weight / sum,
            loss_weight: self.loss_weight / sum,
            stability_weight: self.stability_weight / sum,
            handshake_weight: self.handshake_weight / sum,
        }
    }
}

/// Per-component normalized scores (1 = perfect), kept for transparency and
/// for the diagnostic report (spec 30).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub latency: f64,
    pub jitter: f64,
    pub loss: f64,
    pub stability: f64,
    pub handshake: f64,
    pub total: f64,
}

/// Saturating monotonic mapping helpers. Each returns 1.0 at the reference
/// perfect point and falls toward 0 as the metric degrades. Shapes are
/// documented, stable, and never fabricate a value.
///
/// latencyScore: 1.0 at 0 ms, 0.5 at ~100 ms, → 0 asymptotically.
pub fn latency_score(latency_ms: f64) -> f64 {
    if latency_ms <= 0.0 {
        return 1.0;
    }
    1.0 / (1.0 + (latency_ms / 100.0))
}

/// jitterScore: 1.0 at 0 ms jitter, 0.5 at ~10 ms, → 0.
pub fn jitter_score(jitter_ms: f64) -> f64 {
    if jitter_ms <= 0.0 {
        return 1.0;
    }
    1.0 / (1.0 + (jitter_ms / 10.0))
}

/// lossScore: 1.0 at 0% loss, 0.5 at ~1.5%, strongly punished above 3%.
pub fn loss_score(loss_ratio: f64) -> f64 {
    if loss_ratio <= 0.0 {
        return 1.0;
    }
    if loss_ratio >= 0.5 {
        return 0.0; // 50%+ loss: unusable
    }
    let pct = loss_ratio * 100.0;
    1.0 / (1.0 + (pct / 1.5)).powf(1.6)
}

/// stabilityScore: direct [0,1] stability index.
pub fn stability_score(stability: f64) -> f64 {
    stability.clamp(0.0, 1.0)
}

/// handshakeScore: 1.0 at < 50 ms handshake, decaying past 250 ms.
pub fn handshake_score(handshake_ms: f64) -> f64 {
    if handshake_ms <= 50.0 {
        return 1.0;
    }
    1.0 / (1.0 + ((handshake_ms - 50.0) / 250.0))
}

pub fn breakdown_for(metrics: &MetricsSnapshot) -> ScoreBreakdown {
    ScoreBreakdown {
        latency: latency_score(metrics.latency_ms),
        jitter: jitter_score(metrics.jitter_ms),
        loss: loss_score(metrics.packet_loss_ratio),
        stability: stability_score(metrics.stability),
        handshake: handshake_score(metrics.handshake_latency_ms),
        total: 0.0,
    }
}

/// Combine sub-scores with a (normalized) weight set → score in [0, 100].
pub fn combine(weights: &ScoreWeights, sub: &ScoreBreakdown) -> f64 {
    let w = weights.normalize();
    let raw = w.latency_weight * sub.latency
        + w.jitter_weight * sub.jitter
        + w.loss_weight * sub.loss
        + w.stability_weight * sub.stability
        + w.handshake_weight * sub.handshake;
    (raw * 100.0).clamp(0.0, 100.0)
}

/// Rate a snapshot. Returns (total_score, breakdown).
pub fn score_snapshot(weights: &ScoreWeights, metrics: &MetricsSnapshot) -> (f64, ScoreBreakdown) {
    let mut b = breakdown_for(metrics);
    b.total = combine(weights, &b);
    (b.total, b)
}

/// Rate all route candidates, writing `score` into each (spec 10).
/// Synthetic candidates are rejected unless explicitly allowed by the caller
/// (the chaos harness passes `allow_synthetic = true`).
pub fn score_all(
    weights: &ScoreWeights,
    candidates: &mut [RouteCandidate],
    allow_synthetic: bool,
) -> crate::Result<()> {
    for c in candidates.iter_mut() {
        if c.source == MeasuredSource::Synthetic && !allow_synthetic {
            return Err(crate::Error::InvalidInput(
                "synthetic candidate reached production scoring".into(),
            ));
        }
        let metrics = c.metrics();
        let (score, _) = score_snapshot(weights, &metrics);
        c.score = Some(score);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(latency: f64, jitter: f64, loss: f64, stability: f64) -> RouteCandidate {
        RouteCandidate {
            id: format!("c-{latency}-{jitter}-{loss}"),
            endpoint: "edge".into(),
            transport: crate::model::TransportKind::Quic,
            region: "test".into(),
            latency_ms: latency,
            jitter_ms: jitter,
            packet_loss_ratio: loss,
            stability,
            handshake_latency_ms: 80.0,
            reconnect_rate: 0.0,
            score: None,
            source: MeasuredSource::Real,
        }
    }

    #[test]
    fn spec_example_stable_slightly_slower_wins() {
        // 65 ms avg / 30 ms jitter / 2% loss
        let fast_jittery = cand(65.0, 30.0, 0.02, 0.7);
        // 80 ms avg / 2 ms jitter / 0% loss
        let slow_stable = cand(80.0, 2.0, 0.0, 0.9);
        let (s_fast, b_fast) = score_snapshot(&ScoreWeights::default(), &fast_jittery.metrics());
        let (s_slow, b_slow) = score_snapshot(&ScoreWeights::default(), &slow_stable.metrics());
        assert!(b_slow.loss > b_fast.loss, "loss sub-score should rank stable higher");
        assert!(b_slow.jitter > b_fast.jitter, "jitter sub-score should rank stable higher");
        assert!(
            s_slow > s_fast,
            "stable 80 ms route must outscore jittery 65 ms route for gaming;\
             fast={s_fast:.2} slow={s_slow:.2}"
        );
    }

    #[test]
    fn score_all_populates_and_rejects_synthetic() {
        let mut real = vec![cand(50.0, 5.0, 0.001, 0.9)];
        score_all(&ScoreWeights::default(), &mut real, false).unwrap();
        assert!(real[0].score.unwrap() > 70.0, "healthy route must score high");

        let mut synthetic = vec![RouteCandidate {
            source: MeasuredSource::Synthetic,
            ..cand(10.0, 1.0, 0.0, 1.0)
        }];
        let err = score_all(&ScoreWeights::default(), &mut synthetic, false);
        assert!(err.is_err(), "production scoring must reject synthetic data");
        score_all(&ScoreWeights::default(), &mut synthetic, true).unwrap();
    }

    #[test]
    fn zero_weight_set_falls_back_to_defaults() {
        let weights = ScoreWeights {
            latency_weight: 0.0,
            jitter_weight: 0.0,
            loss_weight: 0.0,
            stability_weight: 0.0,
            handshake_weight: 0.0,
        };
        let (s, _) = score_snapshot(&weights, &cand(40.0, 3.0, 0.0, 1.0).metrics());
        assert!(s > 0.0);
    }

    #[test]
    fn heavy_loss_is_punished() {
        let (s3, _) = score_snapshot(&ScoreWeights::default(), &cand(70.0, 5.0, 0.03, 0.8).metrics());
        let (s10, _) = score_snapshot(&ScoreWeights::default(), &cand(70.0, 5.0, 0.10, 0.8).metrics());
        assert!(s3 > s10);
    }
}