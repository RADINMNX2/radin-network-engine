//! Multi-edge topology (spec 8), active-active probing policy (spec 9) and
//! edge discovery/validation (spec 33).
//!
//! The engine probes several candidate edges in parallel, scores them, picks
//! the best, then maintains *lightweight* health probes on the alternatives.
//! Full user traffic is NEVER duplicated by default.

use serde::{Deserialize, Serialize};

use crate::model::{EdgeHealth, EdgeInfo, MeasuredSource, RouteCandidate, TransportKind};
use crate::scoring;
use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EdgeProbeWeights {
    pub latency_weight: f64,
    pub jitter_weight: f64,
    pub loss_weight: f64,
    pub availability_weight: f64,
    pub handshake_weight: f64,
    pub stability_weight: f64,
}

impl Default for EdgeProbeWeights {
    fn default() -> Self {
        Self {
            latency_weight: 0.20,
            jitter_weight: 0.15,
            loss_weight: 0.25,
            availability_weight: 0.15,
            handshake_weight: 0.10,
            stability_weight: 0.15,
        }
    }
}

/// Edge-level metrics aggregation: EWMA over time with straightforward math.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeStats {
    pub edge_id: String,
    pub latency_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_ratio: f64,
    pub availability: f64,
    pub handshake_latency_ms: f64,
    pub samples: u64,
    pub reconnects: u64,
    pub successes: u64,
    pub failures: u64,
    pub last_probed_at: Option<TimestampMs>,
    pub expires_at: TimestampMs,
}

impl EdgeStats {
    pub fn new(edge: &EdgeInfo) -> Self {
        Self {
            edge_id: edge.id.clone(),
            latency_ms: f64::MAX,
            jitter_ms: f64::MAX,
            packet_loss_ratio: 1.0,
            availability: 0.0,
            handshake_latency_ms: f64::MAX,
            samples: 0,
            reconnects: 0,
            successes: 0,
            failures: 0,
            last_probed_at: None,
            expires_at: edge.expires_at,
        }
    }

    pub fn observes(&mut self, latency_ms: f64, jitter_ms: f64, lost: bool) {
        self.samples += 1;
        self.last_probed_at = Some(self.last_probed_at.unwrap_or(0)); // set by caller
        if lost {
            self.failures += 1;
        } else {
            self.successes += 1;
        }
        // EWMA updates (alpha = 0.25 samples-driven).
        let a = 0.25;
        if self.samples == 1 || self.latency_ms == f64::MAX {
            self.latency_ms = latency_ms;
        } else {
            self.latency_ms = a * latency_ms + (1.0 - a) * self.latency_ms;
        }
        if self.samples == 1 || self.jitter_ms == f64::MAX {
            self.jitter_ms = jitter_ms;
        } else {
            self.jitter_ms = a * jitter_ms + (1.0 - a) * self.jitter_ms;
        }
        let total = (self.successes + self.failures) as f64;
        if total > 0.0 {
            self.packet_loss_ratio = self.failures as f64 / total;
            self.availability = self.successes as f64 / total;
        }
    }

    pub fn record_handshake(&mut self, ms: f64) {
        self.handshake_latency_ms = if self.handshake_latency_ms == f64::MAX {
            ms
        } else {
            0.25 * ms + 0.75 * self.handshake_latency_ms
        };
    }

    pub fn record_reconnect(&mut self) {
        self.reconnects += 1;
    }

    /// Recent stability: 1.0 ideal. Derived from loss + availability.
    pub fn stability(&self) -> f64 {
        let loss_penalty = 1.0 - self.packet_loss_ratio.clamp(0.0, 1.0);
        let avail_penalty = self.availability;
        (loss_penalty * avail_penalty).clamp(0.0, 1.0)
    }

    pub fn reconnect_rate(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.reconnects as f64 / (self.samples as f64 / 60.0).max(1.0)
    }

    /// Edge Health Score (spec 8) in [0, 100].
    pub fn health_score(&self, weights: &EdgeProbeWeights) -> f64 {
        let w = weights.normalize();
        let lat = scoring::latency_score(self.latency_ms.min(1_000.0));
        let jit = scoring::jitter_score(self.jitter_ms.min(500.0));
        let loss = score_loss(self.packet_loss_ratio);
        let handshake = if self.handshake_latency_ms == f64::MAX {
            0.0
        } else {
            scoring::handshake_score(self.handshake_latency_ms)
        };
        let stability = self.stability();
        let availability = self.availability.clamp(0.0, 1.0);
        let raw = w.latency_weight * lat
            + w.jitter_weight * jit
            + w.loss_weight * loss
            + w.stability_weight * stability
            + w.handshake_weight * handshake
            + w.availability_weight * availability;
        (raw * 100.0).clamp(0.0, 100.0)
    }
}

impl EdgeProbeWeights {
    pub fn normalize(&self) -> Self {
        let sum = self.latency_weight
            + self.jitter_weight
            + self.loss_weight
            + self.availability_weight
            + self.handshake_weight
            + self.stability_weight;
        if sum <= f64::EPSILON {
            return Self::default();
        }
        Self {
            latency_weight: self.latency_weight / sum,
            jitter_weight: self.jitter_weight / sum,
            loss_weight: self.loss_weight / sum,
            availability_weight: self.availability_weight / sum,
            handshake_weight: self.handshake_weight / sum,
            stability_weight: self.stability_weight / sum,
        }
    }
}

fn score_loss(loss_ratio: f64) -> f64 {
    if loss_ratio <= 0.0 {
        return 1.0;
    }
    if loss_ratio >= 0.5 {
        return 0.0;
    }
    let pct = loss_ratio * 100.0;
    1.0 / (1.0 + (pct / 1.5)).powf(1.3)
}

/// Shared edge registry (client-side mirror of the server list).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EdgeRegistry {
    pub edges: Vec<EdgeInfo>,
    pub stats: Vec<EdgeStats>,
}

impl EdgeRegistry {
    /// Validate then ingest a server-provided edge list (spec 33: signed /
    /// configured, with expiration). Returns the ids accepted.
    pub fn ingest(&mut self, infos: Vec<EdgeInfo>, now: TimestampMs) -> crate::Result<Vec<String>> {
        let mut accepted = Vec::new();
        for info in infos {
            if info.expires_at <= now {
                continue; // expired → refuse
            }
            if info.address.is_empty() || info.id.is_empty() {
                continue;
            }
            let id = info.id.clone();
            match self.edges.iter_mut().find(|e| e.id == id) {
                Some(existing) => *existing = info,
                None => {
                    self.stats.push(EdgeStats::new(&info));
                    self.edges.push(info);
                }
            }
            accepted.push(id);
        }
        // Drop stats for removed edges.
        let live: Vec<&str> = self.edges.iter().map(|e| e.id.as_str()).collect();
        self.stats.retain(|s| live.contains(&s.edge_id.as_str()));
        Ok(accepted)
    }

    pub fn stats_mut(&mut self, edge_id: &str) -> Option<&mut EdgeStats> {
        self.stats.iter_mut().find(|s| s.edge_id == edge_id)
    }

    pub fn stats(&self, edge_id: &str) -> Option<&EdgeStats> {
        self.stats.iter().find(|s| s.edge_id == edge_id)
    }

    /// Full per-edge health report with computed scores.
    pub fn health_report(
        &self,
        weights: &EdgeProbeWeights,
        source: MeasuredSource,
    ) -> Vec<EdgeHealth> {
        self.stats
            .iter()
            .map(|s| EdgeHealth {
                edge_id: s.edge_id.clone(),
                latency_ms: if s.latency_ms == f64::MAX {
                    f64::NAN
                } else {
                    s.latency_ms
                },
                jitter_ms: if s.jitter_ms == f64::MAX {
                    f64::NAN
                } else {
                    s.jitter_ms
                },
                packet_loss_ratio: s.packet_loss_ratio,
                availability: s.availability,
                handshake_latency_ms: if s.handshake_latency_ms == f64::MAX {
                    f64::NAN
                } else {
                    s.handshake_latency_ms
                },
                reconnect_rate: s.reconnect_rate(),
                historical_stability: s.stability(),
                score: s.health_score(weights),
                updated_at: s.last_probed_at.unwrap_or(0),
                source,
            })
            .collect()
    }

    /// Build route candidates per edge × mutually supported transport, for
    /// the score-based selector. Lightweight probing of ALL candidates is
    /// allowed; only the winner carries user traffic (spec 9).
    pub fn route_candidates(
        &self,
        desired_transports: &[TransportKind],
        source: MeasuredSource,
    ) -> Vec<RouteCandidate> {
        let mut out = Vec::new();
        for s in &self.stats {
            let Some(info) = self.edges.iter().find(|e| e.id == s.edge_id) else {
                continue;
            };
            for t in desired_transports {
                if !info.supported_transports.contains(t) {
                    continue;
                }
                out.push(RouteCandidate {
                    id: format!("{}-{}", s.edge_id, t),
                    endpoint: info.address.clone(),
                    transport: *t,
                    region: info.region.clone(),
                    latency_ms: if s.latency_ms == f64::MAX {
                        0.0
                    } else {
                        s.latency_ms
                    },
                    jitter_ms: if s.jitter_ms == f64::MAX {
                        0.0
                    } else {
                        s.jitter_ms
                    },
                    packet_loss_ratio: s.packet_loss_ratio,
                    stability: s.stability(),
                    handshake_latency_ms: if s.handshake_latency_ms == f64::MAX {
                        0.0
                    } else {
                        s.handshake_latency_ms
                    },
                    reconnect_rate: s.reconnect_rate(),
                    score: None,
                    source,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(id: &str, region: &str, address: &str, expires: u64) -> EdgeInfo {
        EdgeInfo {
            id: id.into(),
            region: region.into(),
            address: address.into(),
            supported_transports: vec![TransportKind::Quic, TransportKind::Udp],
            priority: None,
            expires_at: expires,
            signature_b64: None,
        }
    }

    #[test]
    fn expired_edges_are_rejected() {
        let mut reg = EdgeRegistry::default();
        let accepted = reg
            .ingest(
                vec![
                    edge("a", "sg", "1.2.3.4", 100),
                    edge("b", "eu", "5.6.7.8", 10_000),
                ],
                1000,
            )
            .unwrap();
        assert_eq!(accepted, vec!["b"]);
    }

    #[test]
    fn edge_health_scores_rank_stable_edges_first() {
        let mut reg = EdgeRegistry::default();
        reg.ingest(
            vec![
                edge("sg", "ap-sg", "10.0.0.1", 1_000_000),
                edge("eu", "eu-west", "10.0.0.2", 1_000_000),
            ],
            0,
        )
        .unwrap();
        // Stable edge: low latency, low jitter, no loss.
        let s = reg.stats_mut("sg").unwrap();
        for _ in 0..30 {
            s.observes(40.0, 2.0, false);
        }
        s.record_handshake(120.0);
        // Jittery edge: slightly lower avg latency but heavy jitter + loss.
        let j = reg.stats_mut("eu").unwrap();
        for i in 0..30 {
            j.observes(38.0, 40.0, i % 5 == 0); // 20% loss
        }
        j.record_handshake(400.0);
        let report = reg.health_report(&EdgeProbeWeights::default(), MeasuredSource::Real);
        let sg = report.iter().find(|h| h.edge_id == "sg").unwrap();
        let eu = report.iter().find(|h| h.edge_id == "eu").unwrap();
        assert!(
            sg.score > eu.score,
            "stable 40ms edge must beat fast-but-jittery/lossy edge ({} vs {})",
            sg.score,
            eu.score
        );
    }

    #[test]
    fn route_candidates_only_span_supported_transports() {
        let mut reg = EdgeRegistry::default();
        reg.ingest(vec![edge("sg", "ap-sg", "10.0.0.1", 1_000_000)], 0)
            .unwrap();
        // Only QUIC is desired here.
        let cands = reg.route_candidates(&[TransportKind::Quic], MeasuredSource::Real);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].transport, TransportKind::Quic);
    }

    #[test]
    fn synthetic_edges_tag_health_as_synthetic() {
        let mut reg = EdgeRegistry::default();
        reg.ingest(vec![edge("x", "na", "10.9.9.9", 1_000_000)], 0)
            .unwrap();
        reg.stats_mut("x").unwrap().observes(20.0, 1.0, false);
        let report = reg.health_report(&EdgeProbeWeights::default(), MeasuredSource::Synthetic);
        assert_eq!(report[0].source, MeasuredSource::Synthetic);
    }
}
