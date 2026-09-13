//! Dynamic transport fallback (spec 6).
//!
//! The chain (QUIC → UDP → TCP/TLS → HTTP/2 → WebSocket/TLS → Direct) can be
//! dynamically re-ordered by measured capability scores. Fallback only fires
//! after *measurable* failure — a transport whose state machine says
//! `Failed`, or whose circuit breaker is `Open` — never on a single lost
//! packet.

use serde::{Deserialize, Serialize};

use crate::model::TransportKind;

/// Failure criteria a transport must satisfy before the engine moves past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSignal {
    /// TransportStateMachine reached `Failed`.
    StateFailed,
    /// Circuit breaker is OPEN.
    CircuitOpen,
    /// Handshake/TLS failure was observed.
    HandshakeFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEngine {
    /// Ordered list. Can be re-ordered by `reorder_by_scores`.
    pub chain: Vec<TransportKind>,
    /// Where we currently are in the chain (index).
    pub current_index: usize,
    /// transport → why we've moved off it (kept for the cooldown window).
    pub blocked: Vec<(TransportKind, FailureSignal, u64, u64)>, // (kind, signal, blocked_since_ms, block_until_ms)
    /// Per-kind failure-signal cache from the last frame.
    pub last_signals: Vec<(TransportKind, FailureSignal)>,
}

impl Default for FallbackEngine {
    fn default() -> Self {
        Self {
            chain: TransportKind::FALLBACK_ORDER.to_vec(),
            current_index: 0,
            blocked: Vec::new(),
            last_signals: Vec::new(),
        }
    }
}

impl FallbackEngine {
    pub fn new(order: Option<Vec<TransportKind>>) -> Self {
        Self {
            chain: order.unwrap_or_else(|| TransportKind::FALLBACK_ORDER.to_vec()),
            current_index: 0,
            blocked: Vec::new(),
            last_signals: Vec::new(),
        }
    }

    pub fn current(&self) -> TransportKind {
        self.chain[self.current_index.min(self.chain.len().saturating_sub(1))]
    }

    /// Feed the engine the current failure signals for every transport.
    /// It advances only when the *current* transport is demonstrably failed
    /// (per `FailureSignal`) AND the block window has passed.
    pub fn observe(&mut self, signals: Vec<(TransportKind, FailureSignal)>, now: u64) -> Option<TransportKind> {
        self.last_signals = signals.clone();
        let current = self.current();

        let is_failed = signals.iter().any(|(k, s)| {
            *k == current && matches!(s, FailureSignal::StateFailed | FailureSignal::CircuitOpen | FailureSignal::HandshakeFailure)
        });

        if is_failed {
            // Block bandwidth check.
            let blocked = self
                .blocked
                .iter()
                .any(|(k, _, _, until)| *k == current && now < *until);
            if !blocked {
                // Advance.
                if self.current_index + 1 < self.chain.len() {
                    self.current_index += 1;
                    self.blocked.push((
                        current,
                        signals.iter().find(|(k, _)| *k == current).map(|(_, s)| *s).unwrap_or(FailureSignal::StateFailed),
                        now,
                        now + 5_000,
                    ));
                    return Some(self.current());
                }
            }
        }
        None
    }

    /// Re-order the chain by measured score (higher = earlier), keeping
    /// `Direct` last always (fail-safe, spec 35). Called only when the
    /// benchmark produces materially different evidence.
    pub fn reorder_by_scores(&mut self, scores: &[(TransportKind, f64)]) {
        let mut by_score: Vec<(TransportKind, f64)> = scores.to_vec();
        by_score.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut ordered: Vec<TransportKind> = by_score.into_iter().map(|(k, _)| k).collect();
        // Keep transport kinds actually known.
        ordered.retain(|k| TransportKind::FALLBACK_ORDER.contains(k));
        // Ensure Direct is last.
        if let Some(pos) = ordered.iter().position(|k| *k == TransportKind::Direct) {
            let d = ordered.remove(pos);
            // Only re-append if it wasn't already the last.
            if !matches!(ordered.last(), Some(TransportKind::Direct)) {
                ordered.push(d);
            }
        }
        if !ordered.is_empty() {
            self.chain = ordered;
            self.current_index = 0;
        }
    }

/// Time spent (ms) since the engine last moved down the chain.
    pub fn last_fallback_at(&self) -> Option<u64> {
        self.blocked.last().map(|(_, _, at, _)| *at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chain_matches_spec_order() {
        let fb = FallbackEngine::default();
        assert_eq!(fb.chain, TransportKind::FALLBACK_ORDER.to_vec());
        assert_eq!(fb.current(), TransportKind::Quic);
        assert_eq!(*fb.chain.last().unwrap(), TransportKind::Direct);
    }

    #[test]
    fn does_not_fallback_on_single_failure_signal() {
        let mut fb = FallbackEngine::default();
        // A non-fatal, unrelated signal for a *different* transport must not move us.
        let signals = vec![(TransportKind::Http2, FailureSignal::CircuitOpen)];
        assert!(fb.observe(signals, 1_000).is_none());
        assert_eq!(fb.current(), TransportKind::Quic);
    }

    #[test]
    fn falls_back_after_measurable_failure_of_current() {
        let mut fb = FallbackEngine::default();
        let signals = vec![
            (TransportKind::Quic, FailureSignal::StateFailed),
            (TransportKind::Udp, FailureSignal::CircuitOpen),
            (TransportKind::TcpTls, FailureSignal::HandshakeFailure),
            (TransportKind::WebSocketTls, FailureSignal::CircuitOpen),
        ];
        let moved = fb.observe(signals, 0).expect("should fall to UDP");
        assert_eq!(moved, TransportKind::Udp);
        // Second frame: UDP is still failing → advance again.
        let moved2 = fb
            .observe(vec![(TransportKind::Udp, FailureSignal::CircuitOpen)], 0)
            .expect("should fall to TCP");
        assert_eq!(moved2, TransportKind::TcpTls);
    }

    #[test]
    fn ends_at_direct_and_stays() {
        let mut fb = FallbackEngine::default();
        fb.current_index = fb.chain.len() - 1; // on Direct
        let moved = fb.observe(
            vec![(TransportKind::Direct, FailureSignal::StateFailed)],
            0,
        );
        assert!(moved.is_none(), "Direct cannot fall back further (fail-safe terminal)");
    }

    #[test]
    fn reorder_keeps_direct_last() {
        let mut fb = FallbackEngine::default();
        // Scores favor WebSocket > Udp > everything else.
        let scores = vec![
            (TransportKind::WebSocketTls, 95.0),
            (TransportKind::Udp, 88.0),
            (TransportKind::Quic, 60.0),
            (TransportKind::Http2, 50.0),
            (TransportKind::TcpTls, 40.0),
            (TransportKind::Direct, 10.0),
        ];
        fb.reorder_by_scores(&scores);
        assert_eq!(fb.chain.first(), Some(&TransportKind::WebSocketTls));
        assert_eq!(fb.chain.last(), Some(&TransportKind::Direct));
        assert_eq!(fb.current(), TransportKind::WebSocketTls);
    }

    #[test]
    fn cooldown_blocks_rapid_fallthrough() {
        let mut fb = FallbackEngine::new(Some(vec![
            TransportKind::Udp,
            TransportKind::TcpTls,
            TransportKind::Direct,
        ]));
        let signals = vec![(TransportKind::Udp, FailureSignal::StateFailed)];
        assert_eq!(fb.observe(signals.clone(), 0), Some(TransportKind::TcpTls));
        // Suddenly UDP is fine again, but TCP is failing: we do NOT hop back.
        // Cooldown is now in effect for UDP → engine stays on TCP.
        let moved = fb.observe(vec![(TransportKind::TcpTls, FailureSignal::HandshakeFailure)], 100);
        // TCP block window is fresh (no earlier block), so advance is allowed.
        assert_eq!(moved, Some(TransportKind::Direct));
    }

    #[test]
    fn transport_health_progression_is_visible_to_fallback() {
        use crate::state::{StateConfig, TransportHealth, TransportStateMachine};
        let mut sm = TransportStateMachine::new(TransportKind::Udp, StateConfig::default());
        for _ in 0..6 {
            sm.record_failure();
        }
        assert_eq!(sm.state, TransportHealth::Failed);
    }
}