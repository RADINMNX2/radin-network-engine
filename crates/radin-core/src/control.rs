//! Idempotent control-plane operations (spec 16, 18).
//!
//! Mutating requests carry `operationId` (client UUIDv4) + `Idempotency-Key`
//! so servers can safely replay after reconnect/retry without duplicating
//! configuration changes. Binary encoding follows the `.proto` schemas in
//! `/proto`; JSON is reserved for human-readable debugging (spec 18).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TimestampMs;

/// Client-generated operation id. UUIDv4 per spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationId(pub String);

impl OperationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(s: &str) -> crate::Result<Self> {
        // Accept any well-formed id; UUID is preferred but not required.
        if s.trim().is_empty() {
            return Err(crate::Error::InvalidInput("empty operation id".into()));
        }
        Ok(Self(s.to_string()))
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyKey(pub String);

impl IdempotencyKey {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_client_id(k: &str) -> Self {
        Self(k.to_string())
    }
}

impl Default for IdempotencyKey {
    fn default() -> Self {
        Self::new()
    }
}

/// Control-plane operation kinds. All defaults are *mutating*, hence
/// idempotency-protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOpKind {
    EdgeListRefresh,
    ConfigUpdate,
    OptimizationPolicy,
    TelemetryAck,
    RoutePin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlOp {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub kind: ControlOpKind,
    /// Opaque payload (config JSON / binary config message).
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    pub created_at: TimestampMs,
    pub opcode: u8,
}

impl ControlOp {
    pub fn new(kind: ControlOpKind, created_at: TimestampMs) -> Self {
        Self {
            operation_id: OperationId::new(),
            idempotency_key: IdempotencyKey::new(),
            kind,
            payload: None,
            created_at,
            opcode: kind.opcode(),
        }
    }

    /// Wire tag for the binary (protobuf) encoding (spec 18).
    pub fn proto_tag(&self) -> &'static str {
        match self.kind {
            ControlOpKind::EdgeListRefresh => "EdgeListRefresh",
            ControlOpKind::ConfigUpdate => "ConfigUpdate",
            ControlOpKind::OptimizationPolicy => "OptimizationPolicy",
            ControlOpKind::TelemetryAck => "TelemetryAck",
            ControlOpKind::RoutePin => "RoutePin",
        }
    }
}

impl ControlOpKind {
    pub fn opcode(self) -> u8 {
        match self {
            ControlOpKind::EdgeListRefresh => 1,
            ControlOpKind::ConfigUpdate => 2,
            ControlOpKind::OptimizationPolicy => 3,
            ControlOpKind::TelemetryAck => 4,
            ControlOpKind::RoutePin => 5,
        }
    }

    pub fn from_opcode(code: u8) -> Option<Self> {
        match code {
            1 => Some(ControlOpKind::EdgeListRefresh),
            2 => Some(ControlOpKind::ConfigUpdate),
            3 => Some(ControlOpKind::OptimizationPolicy),
            4 => Some(ControlOpKind::TelemetryAck),
            5 => Some(ControlOpKind::RoutePin),
            _ => None,
        }
    }
}

/// Result of submitting a control op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitOutcome {
    /// New operation accepted.
    Accepted,
    /// Duplicate seen (same idempotency key) → no-op, first result returned.
    Duplicate,
    /// Rejected because the id was malformed.
    Rejected(String),
}

/// Idempotency store. Stateless servers may delegate this to a KV; the trait
/// keeps the client's expectation honest and testable in-process.
pub trait IdempotencyStore: Send + Sync {
    /// Returns `Some(previous outcome)` if this key was already processed.
    fn lookup(&self, key: &IdempotencyKey) -> Option<SubmitOutcome>;
    fn record(&self, key: IdempotencyKey, outcome: SubmitOutcome, op: &ControlOp);
    fn prune(&self, before: TimestampMs);
}

/// In-memory store (also used by the stateless reference server and tests).
#[derive(Debug, Default, Clone)]
pub struct InMemoryIdempotencyStore {
    inner: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, SubmitOutcome>>>,
}

impl InMemoryIdempotencyStore {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

impl IdempotencyStore for InMemoryIdempotencyStore {
    fn lookup(&self, key: &IdempotencyKey) -> Option<SubmitOutcome> {
        self.inner.lock().unwrap().get(&key.0).cloned()
    }

    fn record(&self, key: IdempotencyKey, outcome: SubmitOutcome, _op: &ControlOp) {
        self.inner.lock().unwrap().insert(key.0, outcome);
    }

    fn prune(&self, _before: TimestampMs) {
        // In-memory: prune is a no-op (TTL handled by the caller).
    }
}

/// Client-side guard: dedupe submission before it ever hits the wire.
#[derive(Debug)]
pub struct ClientIdempotencyGuard {
    seen: std::collections::HashMap<String, TimestampMs>,
    ttl_ms: u64,
}

impl ClientIdempotencyGuard {
    pub fn new(ttl_ms: u64) -> Self {
        Self { seen: std::collections::HashMap::new(), ttl_ms }
    }

    /// true → this op may be sent (first time). false → duplicate, skip.
    pub fn first_time(&mut self, op: &ControlOp, now: TimestampMs) -> bool {
        self.seen.retain(|_, ts| now.saturating_sub(*ts) < self.ttl_ms);
        if self.seen.contains_key(&op.idempotency_key.0) {
            return false;
        }
        self.seen.insert(op.idempotency_key.0.clone(), now);
        true
    }
}

/// Wire envelope: op tag + binary payload. Mirrors `proto/control.proto`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEnvelope {
    pub opcode: u8,
    pub operation_id: String,
    pub idempotency_key: String,
    /// 0 = JSON payload (debug), 1 = protobuf payload.
    pub payload_format: u8,
    pub payload_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_id_is_uuid_shaped() {
        let id = OperationId::new();
        assert_eq!(id.0.len(), 36);
        assert!(Uuid::parse_str(&id.0).is_ok());
    }

    #[test]
    fn in_memory_store_dedupes_keys() {
        let store = InMemoryIdempotencyStore::new();
        let key = IdempotencyKey::new();
        let op = ControlOp::new(ControlOpKind::ConfigUpdate, 1);
        assert!(store.lookup(&key).is_none());
        store.record(key.clone(), SubmitOutcome::Accepted, &op);
        assert_eq!(store.lookup(&key), Some(SubmitOutcome::Accepted));
    }

    #[test]
    fn client_guard_blocks_resubmission() {
        let mut g = ClientIdempotencyGuard::new(60_000);
        let op = ControlOp::new(ControlOpKind::ConfigUpdate, 1);
        assert!(g.first_time(&op, 100));
        assert!(!g.first_time(&op, 101));
        // After TTL it can go again.
        assert!(g.first_time(&op, 100 + 60_000));
    }

    #[test]
    fn opcodes_roundtrip() {
        for kind in [
            ControlOpKind::EdgeListRefresh,
            ControlOpKind::ConfigUpdate,
            ControlOpKind::OptimizationPolicy,
            ControlOpKind::TelemetryAck,
            ControlOpKind::RoutePin,
        ] {
            assert_eq!(ControlOpKind::from_opcode(kind.opcode()), Some(kind));
        }
        assert_eq!(ControlOpKind::from_opcode(99), None);
    }

    #[test]
    fn proto_tag_matches_schema_names() {
        assert_eq!(ControlOp::new(ControlOpKind::ConfigUpdate, 0).proto_tag(), "ConfigUpdate");
    }
}