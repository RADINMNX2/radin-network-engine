//! RADIN chaos harness.
//!
//! TEST ONLY — nothing in here may ever connect to production telemetry
//! (spec 39). Simulates the network pathologies the engine must survive:
//! latency, jitter, packet loss, reordering, bandwidth limits, connection
//! resets and intermittent connectivity.

/// Packet-level pipeline that applies a `ChaosProfile` to a stream.
pub mod pipeline;
/// Model of network pathologies applied to packet flows.
pub mod profile;

pub use pipeline::{PacketRecord, PacketResult, PipelineState};
pub use profile::{ChaosProfile, FailureMode};
