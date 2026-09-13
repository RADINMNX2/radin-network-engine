//! Measurement aggregation + the honesty boundary (spec 22, 29, 39).
//!
//! `WindowStats` computes latency/jitter/loss estimates from a sliding
//! window of real samples. The JSON-reported view of any measurement is
//! gated on `MeasuredSource::Real`; synthetic chaos-harness values can never
//! cross into a production sink.

use serde::{Deserialize, Serialize};

use crate::model::MeasuredSource;
use crate::Error;

/// Sliding-window statistics over latency samples with a loss counter.
/// Deliberately simple and deterministic — suitable for on-device budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowStats {
    /// Max samples retained.
    pub capacity: usize,
    pub samples: Vec<f64>,
    pub sent: u64,
    pub lost: u64,
    /// Epoch-ms of last observed packet.
    pub last_sample_at: Option<u64>,
}

impl WindowStats {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), samples: Vec::with_capacity(capacity.max(1)), sent: 0, lost: 0, last_sample_at: None }
    }

    pub fn record_latency(&mut self, latency_ms: f64, at: u64) {
        if self.samples.len() >= self.capacity {
            self.samples.remove(0);
        }
        self.samples.push(latency_ms);
        self.sent += 1;
        self.last_sample_at = Some(at);
    }

    pub fn record_expected(&mut self, _at: u64) {
        self.sent += 1;
    }

    pub fn record_loss(&mut self) {
        self.lost += 1;
        self.sent += 1;
    }

    /// Average of retained samples (ms). `None` if window is empty.
    pub fn avg_latency_ms(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        Some(self.samples.iter().sum::<f64>() / self.samples.len() as f64)
    }

    /// Std-dev of retained samples (ms) — our jitter estimate.
    pub fn jitter_ms(&self) -> Option<f64> {
        let avg = self.avg_latency_ms()?;
        if self.samples.len() < 2 {
            return Some(0.0);
        }
        let var = self.samples.iter().map(|x| (x - avg) * (x - avg)).sum::<f64>() / self.samples.len() as f64;
        Some(var.sqrt())
    }

    /// Packet loss ratio in [0,1]-ish (can momentarily exceed 1 if a caller
    /// mis-uses record_loss; we clamp here).
    pub fn loss_ratio(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        (self.lost as f64 / self.sent as f64).min(1.0)
    }

    /// Window stability in [0,1]: 1.0 when no loss and low jitter.
    pub fn stability(&self) -> f64 {
        let jitter = self.jitter_ms().unwrap_or(0.0);
        let loss = self.loss_ratio();
        // 1.0 at 0 jitter / 0 loss, decay on each axis.
        let jitter_penalty = 1.0 / (1.0 + jitter / 25.0);
        let loss_penalty = 1.0 - loss.clamp(0.0, 1.0);
        jitter_penalty * loss_penalty
    }
}

/// Harness for the honesty checkpoint: any API that *exports* a measurement
/// (reports, telemetry flush, diagnostics) must prove the source is real.
pub fn ensure_real(source: MeasuredSource) -> crate::Result<()> {
    match source {
        MeasuredSource::Real => Ok(()),
        MeasuredSource::Synthetic => Err(Error::InvalidInput(
            "refusing to export synthetic measurement into production telemetry".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_averages_and_estimates_jitter() {
        let mut w = WindowStats::new(10);
        for (i, x) in [10.0, 12.0, 11.0, 13.0].iter().enumerate() {
            w.record_latency(*x, i as u64);
        }
        assert!((w.avg_latency_ms().unwrap() - 11.5).abs() < 1e-9);
        assert!(w.jitter_ms().unwrap() > 0.9);
    }

    #[test]
    fn loss_ratio_is_clamped_and_stability_decays() {
        let mut w = WindowStats::new(5);
        for i in 0..5 {
            w.record_latency(40.0, i);
        }
        assert_eq!(w.loss_ratio(), 0.0);
        let s0 = w.stability();
        for _ in 0..5 {
            w.record_loss();
        }
        assert_eq!(w.loss_ratio(), 0.5);
        assert!(w.stability() < s0);
    }

    #[test]
    fn synthetic_never_exports() {
        assert!(ensure_real(MeasuredSource::Real).is_ok());
        let err = ensure_real(MeasuredSource::Synthetic);
        assert!(matches!(err, Err(Error::InvalidInput(_))));
    }
}