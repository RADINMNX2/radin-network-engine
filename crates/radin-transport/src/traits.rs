//! Transport trait (spec 2): the abstraction every adapter implements.
//!
//! The core engine drives `Transport` instances via `connect`, `send`,
//! `recv`, health feedback and `close`. Concrete adapters (UDP/TCP/QUIC/
//! HTTP2/WebSocket) live behind the corresponding crate features.

use std::fmt::Debug;

use radin_core::model::TransportKind;

/// A stateless, intentionally minimal datagram interface. Each adapter turns
/// this into what its protocol provides:
/// - UDP: direct datagram hookup.
/// - TCP/TLS / HTTP/2 / WebSocket: reliable byte-streams or framing over the
///   encapsulated tunnel bytes.
/// - QUIC: reliable streams; the data plane forwards IP packets as opaque
///   bytes on a dedicated stream.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("transport failed: {0}")]
    Failed(String),
    #[error("timeout")]
    Timeout,
    #[error("closed")]
    Closed,
    #[error("unsupported in this build: transport feature not enabled")]
    Unsupported,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct PendingSend {
    pub bytes: Vec<u8>,
    /// Best-effort tagging: true when this frame should not be retransmitted
    /// even if the transport is reliable (latency-sensitive, spec 4).
    pub latency_sensitive: bool,
}

#[derive(Debug, Clone)]
pub struct RecvResult {
    pub bytes: Vec<u8>,
    /// Sequence number if the adapter tracks one (UDP tunnel).
    pub sequence: Option<u64>,
    /// Wall-clock receive timestamp, ms.
    pub received_at_ms: u64,
}

/// Minimal per-connection state the engine inspects for health decisions.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionStats {
    pub rtt_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub loss_ratio: Option<f64>,
    pub handshake_ms: Option<f64>,
    pub reconnects: u64,
}

/// The trait implemented by each transport adapter.
pub trait Transport: Send + Debug {
    fn kind(&self) -> TransportKind;

    /// Establish the connection to `endpoint`. Performs handshake/timeout
    /// measurement; returns a fresh `ConnectionStats` on success.
    fn connect(&mut self, endpoint: &str) -> Result<ConnectionStats, TransportError>;

    /// Send a tunnel datagram (opaque IP packet bytes). For reliable
    ///-transport adapters, framing/encapsulation happens inside.
    fn send(&mut self, frame: PendingSend) -> Result<(), TransportError>;

    /// Non-blocking-ish receive of one tunnel datagram.
    fn recv(&mut self) -> Result<Option<RecvResult>, TransportError>;

    /// Feed observed failures/successes so the adapter-level circuit breaker
    /// within the transport can self-correct.
    fn note_failure(&mut self, detail: &str);
    fn note_success(&mut self);

    fn close(&mut self);
}

/// Marker for adapters that prefer a single shared keepalive stream.
pub trait HasKeepalive {
    fn keepalive(&mut self) -> Result<(), TransportError>;
}