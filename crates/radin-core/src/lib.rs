//! RADIN Network Engine — core library.
//!
//! Platform-independent resiliency + adaptive routing logic for the RADIN
//! Android VPN optimizer. This crate deliberately contains *no socket code*:
//! every decision (routing, scoring, fallback, keepalive, sync, telemetry)
//! is pure logic over measured inputs, so it is fully unit-testable and can
//! never fabricate network numbers.
//!
//! # Honesty contract
//!
//! Every value surfaced by this crate derives from `MeasuredSource::Real`
//! probe/observation data. Synthetic values exist only behind the
//! `benchmark-synthetic` feature, are tagged `MeasuredSource::Synthetic`,
//! and are rejected by production telemetry sinks (see `metrics`).
//!
//! # Planes
//!
//! - Control plane: config, route discovery, edge health, telemetry,
//!   session management, optimization policy (this crate + `radin-server`).
//! - Data plane: opaque IP forwarding through the Android `VpnService`/TUN
//!   layer (see `crates/radin-transport` + the `android/` scaffold). Game
//!   traffic is never decrypted, modified, or interpreted.

pub mod circuit;
pub mod control;
pub mod compression;
pub mod delta;
pub mod detect;
pub mod edges;
pub mod engine;
pub mod events;
pub mod fallback;
pub mod health;
pub mod hysteresis;
pub mod keepalive;
pub mod metrics;
pub mod migration;
pub mod model;
pub mod persistence;
pub mod probe;
pub mod retry;
pub mod scoring;
pub mod security;
pub mod session;
pub mod state;
pub mod sync;
pub mod telemetry;
pub mod transport;

pub use model::*;

/// Crate-wide alias for timestamps. Epoch milliseconds, UTC.
pub type TimestampMs = u64;

/// Application-facing error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("state conflict: {0}")]
    StateConflict(String),
    #[error("unknown transport: {0}")]
    UnknownTransport(String),
    #[error("unknown edge: {0}")]
    UnknownEdge(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod sanity {
    use super::*;

    #[test]
    fn timestamp_alias_is_u64() {
        let t: TimestampMs = 1_700_000_000_000;
        assert!(t > 0);
    }
}