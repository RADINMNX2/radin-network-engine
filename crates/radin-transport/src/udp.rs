//! UDP data path (spec 4).
//!
//! Provides encapsulation/decapsulation over raw UDP with:
//! - sequence tracking,
//! - timeout detection (stale-connection signal),
//! - keepalive envelopes,
//! - endpoint health monitoring.
//!
//! We deliberately do NOT invent retransmission here — for latency-sensitive
//! traffic LOW LATENCY > PERFECT DELIVERY. Reliability is opt-in via other
//! adapters (KCP-style logic lives at the application layer when configured).

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use radin_core::model::TransportKind;

use crate::traits::{ConnectionStats, PendingSend, RecvResult, Transport, TransportError};

/// Wire header: byte0 = flags (low bit keepalive), 8 bytes LE sequence.
pub const HEADER_LEN: usize = 9;
pub const FLAG_KEEPALIVE: u8 = 0x01;

#[derive(Debug)]
pub struct UdpTunnel {
    socket: UdpSocket,
    next_tx_seq: u64,
    last_rx: Option<Instant>,
    last_rx_seq: Option<u64>,
    tx_packets: u64,
    rx_packets: u64,
    timeouts: u64,
    connected: bool,
}

impl UdpTunnel {
    /// Bind a socket and connect to the edge endpoint.
    /// `endpoint` = "host:port".
    pub fn connect(endpoint: &str, read_timeout: Duration) -> Result<Self, TransportError> {
        let addr: SocketAddr = endpoint
            .parse()
            .map_err(|_| TransportError::Handshake(format!("bad endpoint {endpoint}")))?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(addr)?;
        socket.set_read_timeout(Some(read_timeout))?;
        socket.set_write_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            socket,
            next_tx_seq: 1,
            last_rx: None,
            last_rx_seq: None,
            tx_packets: 0,
            rx_packets: 0,
            timeouts: 0,
            connected: true,
        })
    }

    fn encode(&self, payload: &[u8], keepalive: bool) -> Vec<u8> {
        encode_frame(self.next_tx_seq, payload, keepalive)
    }

    fn decode(buf: &[u8]) -> Option<(bool, u64, Vec<u8>)> {
        decode_frame(buf)
    }

    /// Send an opaque packet. Returns bytes actually sent on the wire
    /// (header + payload) so callers can meter overhead.
    pub fn send_packet(
        &mut self,
        payload: &[u8],
        latency_sensitive: bool,
    ) -> Result<usize, TransportError> {
        if !self.connected {
            return Err(TransportError::Closed);
        }
        let keepalive = payload.is_empty();
        let frame = self.encode(payload, keepalive);
        let sent = self.socket.send(&frame)?;
        if sent > 0 {
            self.next_tx_seq = self.next_tx_seq.wrapping_add(1);
            self.tx_packets += 1;
        }
        let _ = latency_sensitive; // UDP never retransmits — spec 4
        Ok(sent)
    }

    /// Non-blocking timed receive: Ok(None) on read-timeout (caller decides
    /// whether that means a stale connection).
    pub fn recv_packet(&mut self) -> Result<Option<(u64, Vec<u8>)>, TransportError> {
        let mut buf = [0u8; 65535];
        match self.socket.recv(&mut buf) {
            Ok(n) => {
                let Some((keepalive, seq, payload)) = Self::decode(&buf[..n]) else {
                    return Err(TransportError::Failed("malformed envelope".into()));
                };
                self.last_rx = Some(Instant::now());
                self.last_rx_seq = Some(seq);
                self.rx_packets += 1;
                if keepalive {
                    // Swallow keepalives; return None so callers see "alive,
                    // nothing to deliver".
                    return Ok(None);
                }
                Ok(Some((seq, payload)))
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                self.timeouts += 1;
                Ok(None)
            }
            Err(e) => Err(TransportError::Io(e)),
        }
    }

    pub fn send_keepalive(&mut self) -> Result<(), TransportError> {
        self.send_packet(&[], true).map(|_| ())
    }

    pub fn time_since_last_rx(&self) -> Option<Duration> {
        self.last_rx.map(|i| i.elapsed())
    }

    pub fn health_snapshot(&self) -> ConnectionStats {
        let outage = if self.rx_packets + self.timeouts == 0 {
            None
        } else {
            Some(self.timeouts as f64 / (self.rx_packets + self.timeouts) as f64)
        };
        ConnectionStats {
            rtt_ms: None, // UDP tunnel RTT is supplied by edge probes
            jitter_ms: None,
            loss_ratio: outage,
            handshake_ms: Some(0.0),
            reconnects: 0,
        }
    }
}

impl Transport for UdpTunnel {
    fn kind(&self) -> TransportKind {
        TransportKind::Udp
    }

    fn connect(&mut self, _endpoint: &str) -> Result<ConnectionStats, TransportError> {
        Ok(self.health_snapshot())
    }

    fn send(&mut self, frame: PendingSend) -> Result<(), TransportError> {
        self.send_packet(&frame.bytes, frame.latency_sensitive)
            .map(|_| ())
    }

    fn recv(&mut self) -> Result<Option<RecvResult>, TransportError> {
        self.recv_packet().map(|opt| {
            opt.map(|(seq, bytes)| RecvResult {
                bytes,
                sequence: Some(seq),
                received_at_ms: now_ms(),
            })
        })
    }

    fn note_failure(&mut self, _detail: &str) {
        if self.connected {
            self.connected = false;
        }
    }

    fn note_success(&mut self) {
        self.connected = true;
    }

    fn close(&mut self) {
        self.connected = false;
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Pure frame encoder (sequence-driven).
pub fn encode_frame(seq: u64, payload: &[u8], keepalive: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.push(if keepalive { FLAG_KEEPALIVE } else { 0 });
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Pure frame decoder.
pub fn decode_frame(buf: &[u8]) -> Option<(bool, u64, Vec<u8>)> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let flags = buf[0];
    let seq = u64::from_le_bytes(buf[1..9].try_into().ok()?);
    let keepalive = flags & FLAG_KEEPALIVE != 0;
    Some((keepalive, seq, buf[HEADER_LEN..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips_pure() {
        let frame = encode_frame(1, &[1, 2, 3], false);
        let (keepalive, seq, payload) = decode_frame(&frame).unwrap();
        assert!(!keepalive);
        assert_eq!(seq, 1);
        assert_eq!(payload, vec![1, 2, 3]);
    }

    #[test]
    fn malformed_envelope_rejected() {
        assert!(UdpTunnel::decode(&[0]).is_none());
        assert!(UdpTunnel::decode(&[0x01, 1, 2, 3]).is_none());
    }

    #[test]
    fn keepalive_flag_decodes() {
        let frame = [FLAG_KEEPALIVE, 1, 0, 0, 0, 0, 0, 0, 0];
        let (keepalive, seq, payload) = decode_frame(&frame).unwrap();
        assert!(keepalive);
        assert_eq!(seq, 1);
        assert!(payload.is_empty());
    }

    #[test]
    fn loopback_send_recv_works() {
        // Real socket path: client sends a sequence-tracked payload, server
        // echoes it back inside its own envelope.
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let server_addr = server.local_addr().unwrap();

        let mut client =
            UdpTunnel::connect(&server_addr.to_string(), Duration::from_millis(200)).unwrap();
        client.send_packet(&[7, 8, 9], false).unwrap();

        let mut buf = [0u8; 65535];
        let (n, peer) = server.recv_from(&mut buf).unwrap();
        let (keepalive, seq, payload) = decode_frame(&buf[..n]).unwrap();
        assert!(!keepalive);
        assert_eq!(seq, 1);
        assert_eq!(payload, vec![7, 8, 9]);

        // Server responds with seq 100 to prove decap + re-envelope.
        server
            .send_to(&encode_frame(100, &[4, 5], false), peer)
            .unwrap();
        let got = client.recv_packet().unwrap().unwrap();
        assert_eq!(got.0, 100);
        assert_eq!(got.1, vec![4, 5]);
    }
}
