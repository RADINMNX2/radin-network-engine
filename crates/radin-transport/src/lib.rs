//! RADIN transport adapters.
//!
//! Implements the core `Transport` abstraction over real sockets.
//! Feature-gated so the workspace builds on any host:
//! - `udp`, `tcp` (default): std sockets, zero external deps.
//! - `quic`, `http2`, `ws`: protocol-specific implementations behind
//!   optional features.
//!
//! Adapters are thin by design. The *decisions* about which transport to use
//! belong to `radin-core`; the adapters only speak, measure, and report.

pub mod traits;

#[cfg(feature = "http2")]
pub mod h2;
#[cfg(feature = "quic")]
pub mod quic;
#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "udp")]
pub mod udp;
#[cfg(feature = "ws")]
pub mod ws;

/// Optional KCP-style reliable-UDP shim (spec 5).
///
/// KCP is NOT a default for game packets. When enabled it applies selective
/// ARQ to a UDP *tunnel* only where the benchmark shows reliability
/// helps (control/application flows). The knobs below mirror the KCP
/// surface: MTU, interval, nodelay, resend, and a congestion-control switch.
/// Conservative defaults. The engine can disable KCP when measurements show
/// it adds latency or jitter.
#[cfg(feature = "kcp")]
pub mod kcp;

#[cfg(feature = "kcp")]
pub use kcp::{KcpConfig, KcpSession};

#[cfg(not(feature = "kcp"))]
/// Compile-time honest marker: this build has no KCP layer.
pub const KCP_ENABLED: bool = false;

#[cfg(not(feature = "kcp"))]
#[doc(hidden)]
pub mod kcp {
    /// Placeholder so feature-gated imports don't surprise downstream crates.
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct KcpConfig {
        pub mtu: usize,
        pub interval_ms: u32,
        pub nodelay: bool,
        pub resend: u32,
        pub congestion_control: bool,
    }
    #[derive(Debug)]
    pub struct KcpSession;
}
