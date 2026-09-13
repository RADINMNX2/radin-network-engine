//! Transport / connection migration (spec 27).
//!
//! When the device moves Wi-Fi → 5G (or any characteristic change), the
//! engine attempts *in-place connection migration* where the protocol
//! supports it (QUIC connection migration, TCP keep-on-with-MPTCP-ish
//! semantics), preserving control-plane state. If migration fails, it does a
//! controlled reconnection through `Session::reconnect`.

use serde::{Deserialize, Serialize};

use crate::model::NetworkType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationOutcome {
    /// Protocol support exists and the migration was executed.
    MigrationStarted,
    /// Stable session state was preserved across the switch.
    Preserved,
    /// Migration unsupported/unhelpful → controlled reconnection.
    ControlledReconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkChangeKind {
    None,
    InterfaceSwitch,
    TypeChange,
    LossOfConnectivity,
}

/// Analyze a network change to decide the migration strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MigrationPolicy {
    /// Protocols that may attempt in-place migration. Serde-skipped: it
    /// is a const capability table, not runtime-config data.
    #[serde(skip)]
    pub migration_capable_transports: &'static [TransportMigrationCap],
    pub preserve_control_state: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportMigrationCap {
    Quic,
    None,
}

impl Default for MigrationPolicy {
    fn default() -> Self {
        Self {
            migration_capable_transports: &[TransportMigrationCap::Quic],
            preserve_control_state: true,
        }
    }
}

/// Classify the change between two network states.
pub fn classify_change(
    before: Option<NetworkType>,
    after: Option<NetworkType>,
) -> NetworkChangeKind {
    match (before, after) {
        (Some(a), Some(b)) if a == b => NetworkChangeKind::None,
        (Some(_), Some(_)) => NetworkChangeKind::TypeChange,
        (None, Some(_)) | (Some(_), None) => NetworkChangeKind::LossOfConnectivity,
        (None, None) => NetworkChangeKind::None,
    }
}

/// Decide what to do given the change + protocol capability.
/// Returns whether the control-plane state should be preserved.
pub fn decide_migration(
    change: NetworkChangeKind,
    supports_connection_migration: bool,
    policy: &MigrationPolicy,
) -> MigrationOutcome {
    match change {
        NetworkChangeKind::LossOfConnectivity => MigrationOutcome::ControlledReconnect,
        NetworkChangeKind::InterfaceSwitch | NetworkChangeKind::TypeChange => {
            if supports_connection_migration && policy.preserve_control_state {
                MigrationOutcome::MigrationStarted
            } else {
                MigrationOutcome::ControlledReconnect
            }
        }
        NetworkChangeKind::None => MigrationOutcome::Preserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_to_5g_is_a_type_change_and_can_migrate() {
        assert_eq!(
            classify_change(Some(NetworkType::Wifi), Some(NetworkType::FiveG)),
            NetworkChangeKind::TypeChange
        );
        let out = decide_migration(
            NetworkChangeKind::TypeChange,
            true,
            &MigrationPolicy::default(),
        );
        assert_eq!(out, MigrationOutcome::MigrationStarted);
    }

    #[test]
    fn non_migration_capable_transport_does_controlled_reconnect() {
        let out = decide_migration(
            NetworkChangeKind::TypeChange,
            false,
            &MigrationPolicy::default(),
        );
        assert_eq!(out, MigrationOutcome::ControlledReconnect);
    }

    #[test]
    fn loss_of_connectivity_always_drops_to_reconnect() {
        let out = decide_migration(
            NetworkChangeKind::LossOfConnectivity,
            true,
            &MigrationPolicy::default(),
        );
        assert_eq!(out, MigrationOutcome::ControlledReconnect);
    }

    #[test]
    fn no_change_preserves() {
        let out = decide_migration(NetworkChangeKind::None, false, &MigrationPolicy::default());
        assert_eq!(out, MigrationOutcome::Preserved);
    }
}
