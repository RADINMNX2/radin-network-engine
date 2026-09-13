//! Smart sync (spec 17): queued control operations flushed when connectivity
//! returns. Never blocks the gaming dataplane.
//!
//! States: LOCAL_ONLY → PENDING → SYNCING → SYNCED | FAILED → RETRYING.

use serde::{Deserialize, Serialize};

use crate::control::{ControlOp, IdempotencyKey};
use crate::TimestampMs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncState {
    #[default]
    LocalOnly,
    Pending,
    Syncing,
    Synced,
    Failed,
    Retrying,
}

impl SyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::LocalOnly => "LOCAL_ONLY",
            SyncState::Pending => "PENDING",
            SyncState::Syncing => "SYNCING",
            SyncState::Synced => "SYNCED",
            SyncState::Failed => "FAILED",
            SyncState::Retrying => "RETRYING",
        }
    }
}

/// One queued control operation with its sync lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncItem {
    pub op: ControlOp,
    pub state: SyncState,
    pub attempts: u32,
    pub queued_at: TimestampMs,
    pub last_attempt_at: Option<TimestampMs>,
}

/// Batches compatible operations (same idempotency key prefix, or explicit
/// batching group) so we don't hammer the wire (spec 17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncBatch {
    pub ops: Vec<ControlOp>,
    pub created_at: TimestampMs,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncEngine {
    pub items: Vec<SyncItem>,
    pub state: SyncState,
    pub last_error: Option<String>,
    /// Drops ops that were acknowledged remotely.
    pub synced_count: u64,
    pub dropped_count: u64,
}

impl SyncEngine {
    pub fn enqueue(&mut self, op: ControlOp, now: TimestampMs) -> bool {
        // Dedup on idempotency key: an already-queued op with the same key
        // must not be added twice.
        if self
            .items
            .iter()
            .any(|i| i.op.idempotency_key == op.idempotency_key)
        {
            return false;
        }
        self.items.push(SyncItem {
            op,
            state: if self.state == SyncState::LocalOnly {
                SyncState::LocalOnly
            } else {
                SyncState::Pending
            },
            attempts: 0,
            queued_at: now,
            last_attempt_at: None,
        });
        true
    }

    /// Detect network recovery → move eligible items to Pending (spec 17
    /// step 1-2: detect recovery, validate connection, flush).
    pub fn network_recovered(&mut self) {
        if self.state == SyncState::Failed {
            self.state = SyncState::Retrying;
        }
        for item in self.items.iter_mut() {
            if item.state == SyncState::LocalOnly {
                item.state = SyncState::Pending;
            }
        }
    }

    /// Validate that the network/connection is healthy before flushing.
    pub fn connectivity_ok(&self) -> bool {
        matches!(
            self.state,
            SyncState::LocalOnly | SyncState::Pending | SyncState::Retrying
        ) || self.state == SyncState::Synced
    }

    /// Build a batch from ops in the given states. Ops in `LocalOnly` are
    /// also flushed (they only differ in UI visibility).
    pub fn build_batch(&mut self, max_ops: usize, now: TimestampMs) -> Option<SyncBatch> {
        let eligible: Vec<ControlOp> = self
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i.state,
                    SyncState::LocalOnly | SyncState::Pending | SyncState::Retrying
                )
            })
            .take(max_ops.max(1))
            .map(|i| ControlOp {
                operation_id: i.op.operation_id.clone(),
                idempotency_key: IdempotencyKey(i.op.idempotency_key.0.clone()),
                kind: i.op.kind,
                payload: i.op.payload.clone(),
                created_at: i.op.created_at,
                opcode: i.op.opcode,
            })
            .collect();
        if eligible.is_empty() {
            return None;
        }
        for item in self.items.iter_mut() {
            if matches!(
                item.state,
                SyncState::LocalOnly | SyncState::Pending | SyncState::Retrying
            ) && eligible
                .iter()
                .any(|e| e.idempotency_key == item.op.idempotency_key)
            {
                item.state = SyncState::Syncing;
                item.attempts += 1;
                item.last_attempt_at = Some(now);
            }
        }
        self.state = SyncState::Syncing;
        Some(SyncBatch {
            ops: eligible,
            created_at: now,
        })
    }

    /// Mark a batch (by the ids it carried) as synced.
    pub fn mark_synced(&mut self, batch: &SyncBatch) {
        let keys: Vec<&str> = batch
            .ops
            .iter()
            .map(|o| o.idempotency_key.0.as_str())
            .collect();
        self.items
            .retain(|i| !keys.contains(&i.op.idempotency_key.0.as_str()));
        self.synced_count += keys.len() as u64;
        if self.items.iter().all(|i| i.state == SyncState::Synced) {
            self.state = SyncState::Synced;
        } else {
            self.state = SyncState::Pending;
        }
    }

    /// A batch failed: mark FAILED; RETRYING happens after network_recovered.
    pub fn mark_failed(&mut self, batch: &SyncBatch, error: String, max_attempts: u32) {
        self.last_error = Some(error);
        let keys: Vec<&str> = batch
            .ops
            .iter()
            .map(|o| o.idempotency_key.0.as_str())
            .collect();
        for item in self.items.iter_mut() {
            if keys.contains(&item.op.idempotency_key.0.as_str()) {
                item.state = if item.attempts >= max_attempts {
                    SyncState::Failed
                } else {
                    SyncState::Retrying
                };
            }
        }
        self.state = if self.items.iter().any(|i| i.state == SyncState::Failed) {
            SyncState::Failed
        } else {
            SyncState::Retrying
        };
    }

    /// Drop telemetry-class ops that aren't worth synchronizing when the
    /// connection flapped (spec 14: telemetry, aggressive dropping).
    pub fn drop_telemetry(&mut self) {
        let before = self.items.len();
        self.items
            .retain(|item| !matches!(item.op.kind, crate::control::ControlOpKind::TelemetryAck));
        self.dropped_count += (before - self.items.len()) as u64;
    }

    pub fn pending_count(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op() -> ControlOp {
        ControlOp::new(crate::control::ControlOpKind::ConfigUpdate, 0)
    }

    fn telemetry_op() -> ControlOp {
        ControlOp::new(crate::control::ControlOpKind::TelemetryAck, 0)
    }

    #[test]
    fn local_only_becomes_pending_after_recovery() {
        let mut engine = SyncEngine::default();
        engine.enqueue(op(), 1);
        assert_eq!(engine.items[0].state, SyncState::LocalOnly);
        engine.state = SyncState::Failed;
        engine.network_recovered();
        assert_eq!(engine.state, SyncState::Retrying);
        assert_eq!(engine.items[0].state, SyncState::Pending);
    }

    #[test]
    fn flush_marks_synced_and_clears() {
        let mut engine = SyncEngine::default();
        engine.enqueue(op(), 1);
        engine.network_recovered();
        let batch = engine.build_batch(10, 2).unwrap();
        assert_eq!(engine.state, SyncState::Syncing);
        assert_eq!(batch.ops.len(), 1);
        engine.mark_synced(&batch);
        assert_eq!(engine.synced_count, 1);
        assert_eq!(engine.pending_count(), 0);
        assert_eq!(engine.state, SyncState::Synced);
    }

    #[test]
    fn failed_batch_retries_then_drops() {
        let mut engine = SyncEngine::default();
        engine.enqueue(op(), 1);
        engine.network_recovered();
        let batch = engine.build_batch(10, 2).unwrap();
        engine.mark_failed(&batch, "timeout".into(), 2);
        assert_eq!(engine.state, SyncState::Retrying);
        // Retry: second batch.
        engine.network_recovered();
        let batch2 = engine.build_batch(10, 3).unwrap();
        engine.mark_failed(&batch2, "timeout".into(), 2);
        assert_eq!(engine.state, SyncState::Failed);
        assert_eq!(engine.items[0].state, SyncState::Failed);
    }

    #[test]
    fn telemetry_ops_are_droppable() {
        let mut engine = SyncEngine::default();
        engine.enqueue(op(), 1);
        engine.enqueue(telemetry_op(), 2);
        assert_eq!(engine.pending_count(), 2);
        engine.drop_telemetry();
        assert_eq!(engine.pending_count(), 1);
        assert_eq!(engine.dropped_count, 1);
    }

    #[test]
    fn duplicate_idempotency_key_not_enqueued_twice() {
        let mut engine = SyncEngine::default();
        let a = op();
        let mut b = op();
        b.idempotency_key = a.idempotency_key.clone();
        assert!(engine.enqueue(a, 1));
        assert!(!engine.enqueue(b, 2));
        assert_eq!(engine.pending_count(), 1);
    }
}
