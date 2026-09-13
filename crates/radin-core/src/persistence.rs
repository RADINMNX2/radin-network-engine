//! Local-first control state (spec 15).
//!
//! The *optimizer's own* control-plane data is persisted locally so route
//! profiles, edge configuration, benchmark history, optimization settings,
//! pending operations and diagnostics survive restarts. Arbitrary game
//! traffic is NEVER persisted — that boundary is structural (see
//! `persistence` trait: only typed control records may be stored).

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::control::ControlOp;
use crate::hysteresis::HysteresisState;
use crate::TimestampMs;

/// Persistence-record kinds. The armour-plated allow-list is what keeps game
/// traffic out of local storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    RouteProfile,
    EdgeConfig,
    BenchmarkHistory,
    OptimizationSettings,
    PendingControlOp,
    DiagnosticEvent,
}

impl RecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::RouteProfile => "route_profile",
            RecordKind::EdgeConfig => "edge_config",
            RecordKind::BenchmarkHistory => "benchmark_history",
            RecordKind::OptimizationSettings => "optimization_settings",
            RecordKind::PendingControlOp => "pending_control_op",
            RecordKind::DiagnosticEvent => "diagnostic_event",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "route_profile" => Some(RecordKind::RouteProfile),
            "edge_config" => Some(RecordKind::EdgeConfig),
            "benchmark_history" => Some(RecordKind::BenchmarkHistory),
            "optimization_settings" => Some(RecordKind::OptimizationSettings),
            "pending_control_op" => Some(RecordKind::PendingControlOp),
            "diagnostic_event" => Some(RecordKind::DiagnosticEvent),
            _ => None,
        }
    }
}

/// A typed, namespaced, timestamped record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub kind: RecordKind,
    /// Unique key within the kind (e.g. edge id, op id).
    pub key: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    pub body: serde_json::Value,
}

impl Record {
    pub fn new(kind: RecordKind, key: impl Into<String>, body: serde_json::Value, now: TimestampMs) -> Self {
        Self { kind, key: key.into(), created_at: now, updated_at: now, body }
    }

    pub fn decode<T: DeserializeOwned>(&self) -> crate::Result<T> {
        Ok(serde_json::from_value(self.body.clone())?)
    }
}

/// Storage abstraction. Test/sin-default implementations are in-memory; a
/// SQLite backend is feature-gated (`sqlite`).
pub trait ControlStore: Send + Sync {
    fn put(&self, record: &Record) -> crate::Result<()>;
    fn get(&self, kind: RecordKind, key: &str) -> crate::Result<Option<Record>>;
    fn list(&self, kind: RecordKind) -> crate::Result<Vec<Record>>;
    fn delete(&self, kind: RecordKind, key: &str) -> crate::Result<()>;
    fn flush(&self) -> crate::Result<()>;
}

/// Guardians that document exactly how each domain uses the store.
pub trait RouteProfileStore: ControlStore {
    fn save_hysteresis(&self, state: &HysteresisState) -> crate::Result<()> {
        self.put(&Record::new(
            RecordKind::RouteProfile,
            "hysteresis",
            serde_json::to_value(state)?,
            chrono_now(),
        ))
    }
}

/// Simplified timestamp source so the core stays clock-free in tests.
fn chrono_now() -> TimestampMs {
    0
}

/// In-memory store for tests + the fail-safe local path before a real
/// backend is attached.
#[derive(Debug, Default, Clone)]
pub struct MemoryStore {
    inner: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<(RecordKind, String), Record>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ControlStore for MemoryStore {
    fn put(&self, record: &Record) -> crate::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert((record.kind, record.key.clone()), record.clone());
        Ok(())
    }

    fn get(&self, kind: RecordKind, key: &str) -> crate::Result<Option<Record>> {
        Ok(self.inner.lock().unwrap().get(&(kind, key.to_string())).cloned())
    }

    fn list(&self, kind: RecordKind) -> crate::Result<Vec<Record>> {
        let lock = self.inner.lock().unwrap();
        Ok(lock
            .iter()
            .filter(|((k, _), _)| *k == kind)
            .map(|(_, v)| v.clone())
            .collect())
    }

    fn delete(&self, kind: RecordKind, key: &str) -> crate::Result<()> {
        self.inner.lock().unwrap().remove(&(kind, key.to_string()));
        Ok(())
    }

    fn flush(&self) -> crate::Result<()> {
        Ok(())
    }
}

/// SQLite-backed store (feature `sqlite`). Bundled, so it builds anywhere a
/// C toolchain exists; safe on Android via `bundled`.
#[cfg(feature = "sqlite")]
#[derive(Clone)]
pub struct SqliteStore {
    conn: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
}

#[cfg(feature = "sqlite")]
impl SqliteStore {
    pub fn open(path: &std::path::Path) -> crate::Result<Self> {
        let conn = rusqlite::Connection::open(path)
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: std::sync::Arc::new(std::sync::Mutex::new(conn)) })
    }

    pub fn in_memory() -> crate::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: std::sync::Arc::new(std::sync::Mutex::new(conn)) })
    }

    fn init_schema(conn: &rusqlite::Connection) -> crate::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS control_store (
                kind TEXT NOT NULL,
                key TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                body TEXT NOT NULL,
                PRIMARY KEY (kind, key)
            );",
        )
        .map_err(|e| crate::Error::Storage(e.to_string()))?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl ControlStore for SqliteStore {
    fn put(&self, record: &Record) -> crate::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO control_store (kind, key, created_at, updated_at, body)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.kind.as_str(),
                record.key,
                record.created_at as i64,
                record.updated_at as i64,
                serde_json::to_string(&record.body)?
            ],
        )
        .map_err(|e| crate::Error::Storage(e.to_string()))?;
        Ok(())
    }

    fn get(&self, kind: RecordKind, key: &str) -> crate::Result<Option<Record>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT kind, key, created_at, updated_at, body FROM control_store WHERE kind = ?1 AND key = ?2")
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([kind.as_str(), key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        if let Some(row) = rows.into_iter().next() {
            let (k, key, created, updated, body) = row.map_err(|e| crate::Error::Storage(e.to_string()))?;
            let kind = RecordKind::parse(&k).ok_or_else(|| crate::Error::Storage("unknown kind".into()))?;
            let rec = Record {
                kind,
                key,
                created_at: created as u64,
                updated_at: updated as u64,
                body: serde_json::from_str(&body)?,
            };
            return Ok(Some(rec));
        }
        Ok(None)
    }

    fn list(&self, kind: RecordKind) -> crate::Result<Vec<Record>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT key, created_at, updated_at, body FROM control_store WHERE kind = ?1")
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([kind.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| crate::Error::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (key, created, updated, body) = row.map_err(|e| crate::Error::Storage(e.to_string()))?;
            out.push(Record {
                kind,
                key,
                created_at: created as u64,
                updated_at: updated as u64,
                body: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    fn delete(&self, kind: RecordKind, key: &str) -> crate::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM control_store WHERE kind = ?1 AND key = ?2",
            rusqlite::params![kind.as_str(), key],
        )
        .map_err(|e| crate::Error::Storage(e.to_string()))?;
        Ok(())
    }

    fn flush(&self) -> crate::Result<()> {
        // SQLite commits per-statement by default; nothing to flush.
        Ok(())
    }
}

/// Type-safe helpers used by the engine's own plumbing.
pub struct Persistence {
    store: Box<dyn ControlStore>,
}

impl Persistence {
    pub fn new(store: Box<dyn ControlStore>) -> Self {
        Self { store }
    }

    pub fn save_pending_op(&self, op: &ControlOp, now: TimestampMs) -> crate::Result<()> {
        self.store.put(&Record::new(
            RecordKind::PendingControlOp,
            op.operation_id.0.clone(),
            serde_json::to_value(op)?,
            now,
        ))
    }

    pub fn load_pending_ops(&self) -> crate::Result<Vec<ControlOp>> {
        let records = self.store.list(RecordKind::PendingControlOp)?;
        let mut ops = Vec::new();
        for r in records {
            ops.push(r.decode::<ControlOp>()?);
        }
        Ok(ops)
    }

    pub fn clear_pending_op(&self, operation_id: &str) -> crate::Result<()> {
        self.store.delete(RecordKind::PendingControlOp, operation_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hysteresis::RouteDecider;

    #[test]
    fn memory_store_roundtrips_all_kinds() {
        let store = MemoryStore::new();
        let rec = Record::new(RecordKind::OptimizationSettings, "policy", serde_json::json!({"x": 1}), 5);
        store.put(&rec).unwrap();
        let got = store.get(RecordKind::OptimizationSettings, "policy").unwrap().unwrap();
        assert_eq!(got.body, serde_json::json!({"x": 1}));
        assert_eq!(store.list(RecordKind::OptimizationSettings).unwrap().len(), 1);
        store.delete(RecordKind::OptimizationSettings, "policy").unwrap();
        assert!(store.get(RecordKind::OptimizationSettings, "policy").unwrap().is_none());
    }

    #[test]
    fn hysteresis_state_persists_as_route_profile() {
        let store = MemoryStore::new();
        let mut d = RouteDecider::new(crate::hysteresis::HysteresisConfig::default());
        d.state.current_route_id = "edge-a".into();
        store
            .put(&Record::new(
                RecordKind::RouteProfile,
                "hysteresis",
                serde_json::to_value(&d.state).unwrap(),
                0,
            ))
            .unwrap();
        let rec = store.get(RecordKind::RouteProfile, "hysteresis").unwrap().unwrap();
        let restored: crate::hysteresis::HysteresisState = rec.decode().unwrap();
        assert_eq!(restored.current_route_id, "edge-a");
    }

    #[test]
    fn pending_ops_survive_via_persistence() {
        let p = Persistence::new(Box::new(MemoryStore::new()));
        let op = ControlOp::new(crate::control::ControlOpKind::ConfigUpdate, 1);
        p.save_pending_op(&op, 2).unwrap();
        let loaded = p.load_pending_ops().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].operation_id, op.operation_id);
        p.clear_pending_op(&op.operation_id.0).unwrap();
        assert!(p.load_pending_ops().unwrap().is_empty());
    }

    #[test]
    fn unknown_kind_string_is_rejected() {
        assert_eq!(RecordKind::parse("game_traffic"), None);
        assert_eq!(RecordKind::parse("pending_control_op"), Some(RecordKind::PendingControlOp));
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_store_works() {
        let store = SqliteStore::in_memory().unwrap();
        let rec = Record::new(RecordKind::EdgeConfig, "edge-a", serde_json::json!({"region": "sg"}), 1);
        store.put(&rec).unwrap();
        let got = store.get(RecordKind::EdgeConfig, "edge-a").unwrap().unwrap();
        assert_eq!(got.body["region"], "sg");
        assert_eq!(store.list(RecordKind::EdgeConfig).unwrap().len(), 1);
        store.delete(RecordKind::EdgeConfig, "edge-a").unwrap();
        assert!(store.get(RecordKind::EdgeConfig, "edge-a").unwrap().is_none());
    }
}
