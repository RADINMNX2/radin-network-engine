//! Session recovery (spec 28).
//!
//! Every network session carries a stable `sessionId` (survives reconnect),
//! a per-connection `connectionId`, transport, edge, start time and
//! `lastHealthyTime`. After a reconnect the engine resumes session state
//! where possible; it never assumes the network is stable.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::TransportKind;
use crate::TimestampMs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Stable across reconnects / migrations.
    pub session_id: String,
    /// Changes on every connection attempt.
    pub connection_id: String,
    pub transport: TransportKind,
    pub edge_id: Option<String>,
    pub start_time: TimestampMs,
    pub last_healthy_time: TimestampMs,
    pub reconnect_count: u64,
    pub migration_count: u64,
}

impl Session {
    pub fn begin(transport: TransportKind, edge_id: Option<String>, now: TimestampMs) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            connection_id: Uuid::new_v4().to_string(),
            transport,
            edge_id,
            start_time: now,
            last_healthy_time: now,
            reconnect_count: 0,
            migration_count: 0,
        }
    }

    /// Resume the *same* session with a fresh connection.
    pub fn reconnect(
        &mut self,
        transport: TransportKind,
        edge_id: Option<String>,
        now: TimestampMs,
    ) {
        self.connection_id = Uuid::new_v4().to_string();
        self.transport = transport;
        self.edge_id = edge_id;
        self.reconnect_count += 1;
        self.last_healthy_time = now;
    }

    /// Keep session id, swap connection + transport (network migration path).
    pub fn migrate(&mut self, transport: TransportKind, edge_id: Option<String>, now: TimestampMs) {
        self.connection_id = Uuid::new_v4().to_string();
        self.transport = transport;
        self.edge_id = edge_id;
        self.migration_count += 1;
        self.last_healthy_time = now;
    }

    pub fn mark_healthy(&mut self, now: TimestampMs) {
        self.last_healthy_time = now;
    }

    /// Seconds since last confirmed-healthy signal.
    pub fn unhealthy_seconds(&self, now: TimestampMs) -> u64 {
        now.saturating_sub(self.last_healthy_time) / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_keeps_session_id() {
        let mut s = Session::begin(TransportKind::Quic, Some("Edge-A".into()), 1_000);
        let sid = s.session_id.clone();
        s.reconnect(TransportKind::Udp, Some("Edge-B".into()), 2_000);
        assert_eq!(s.session_id, sid);
        assert_ne!(s.connection_id, sid);
        assert_eq!(s.reconnect_count, 1);
        assert_eq!(s.edge_id.as_deref(), Some("Edge-B"));
    }

    #[test]
    fn migration_increments_its_own_counter() {
        let mut s = Session::begin(TransportKind::Udp, None, 0);
        for _ in 0..3 {
            s.migrate(TransportKind::Quic, Some("Edge-C".into()), 10);
        }
        assert_eq!(s.migration_count, 3);
        assert_eq!(s.reconnect_count, 0);
    }

    #[test]
    fn unhealthy_time_is_tracked() {
        let mut s = Session::begin(TransportKind::TcpTls, None, 0);
        s.mark_healthy(5_000);
        assert_eq!(s.unhealthy_seconds(9_000), 4);
    }
}
