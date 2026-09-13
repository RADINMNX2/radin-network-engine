//! KCP-style reliable-UDP shim (spec 5) — feature `kcp`.
//!
//! KCP is NOT a default for game packets. When enabled it applies selective
//! ARQ to a UDP *tunnel* only where the benchmark shows reliability helps
//! (control/application flows). The knobs mirror the KCP surface: MTU,
//! interval, nodelay, resend, and a congestion-control switch. Conservative
//! defaults; the engine can disable KCP when measurements show it adds
//! latency or jitter.
//!
//! This module is an honest reference scaffold: config + session seam that
//! the data plane can pull in behind `radin-core`'s per-route decision. The
//! actual ARQ encode/decode loop belongs to the platform adapter layer
//! (android/), which owns the UDP socket lifecycle.

use serde::{Deserialize, Serialize};

/// KCP tuning knobs (spec 5). Conservative defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KcpConfig {
    /// Maximum transmission unit in bytes.
    pub mtu: usize,
    /// Update interval in ms.
    pub interval_ms: u32,
    /// Fast-mode: no delayed ACK, `resend` ramp.
    pub nodelay: bool,
    /// Fast-retransmit threshold.
    pub resend: u32,
    /// Veno-style congestion window control.
    pub congestion_control: bool,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self { mtu: 1400, interval_ms: 30, nodelay: false, resend: 2, congestion_control: true }
    }
}

/// One end of a KCP-style session over a UDP socket.
#[derive(Debug)]
pub struct KcpSession {
    config: KcpConfig,
    /// Protocol version the session was negotiated at (1 = conservative ARQ).
    negotiated_version: u32,
}

impl KcpSession {
    /// Open a session locally (2-tuple bind). Synchronous by design; the
    /// platform layer drives the socket.
    pub fn open(config: KcpConfig) -> Self {
        Self { config, negotiated_version: 1 }
    }

    pub fn config(&self) -> &KcpConfig {
        &self.config
    }

    /// Protocol version negotiated at session open.
    pub fn version(&self) -> u32 {
        self.negotiated_version
    }

    /// Honest liveness marker: this reference session has no wire bytes yet.
    /// The platform adapter validates real round-trips before promotion.
    pub fn is_measurement_ready(&self) -> bool {
        false
    }
}