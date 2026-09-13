//! TCP/TLS transport (spec 2 fallback tier).
//!
//! Plain stream tunnel with a 4-byte big-endian length prefix framing so the
//! data plane can delimit opaque IP packets over a reliable stream.
//! TLS-wrapping belongs to the caller's deployment (ALPN, cert pinning via
//! `radin-core::security`); this adapter handles the framing + socket logic.
//!
//! Note on reliability: TCP gives delivery but at the cost of head-of-line
//! blocking. That is exactly why QUIC exists and why the engine benchmarks
//! before choosing — not everyone benefits from TCP in a noisy game flow.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use radin_core::model::TransportKind;

use crate::traits::{ConnectionStats, PendingSend, RecvResult, Transport, TransportError};

#[derive(Debug)]
pub struct TcpTunnel {
    stream: TcpStream,
    rx_buf: Vec<u8>,
}

impl TcpTunnel {
    pub fn connect(endpoint: &str, timeout: Duration) -> Result<Self, TransportError> {
        let addrs: Vec<_> = endpoint
            .to_socket_addrs()
            .map_err(|_| TransportError::Handshake(format!("resolve failed: {endpoint}")))?
            .collect();
        let addr = addrs
            .first()
            .ok_or_else(|| TransportError::Handshake(format!("no address for {endpoint}")))?;
        let stream = TcpStream::connect_timeout(addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            rx_buf: Vec::with_capacity(65_535),
        })
    }

    fn write_frame(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        // 4-byte BE length prefix.
        let mut header = [0u8; 4];
        header.copy_from_slice(&(payload.len() as u32).to_be_bytes());
        self.stream.write_all(&header)?;
        self.stream.write_all(payload)?;
        self.stream.flush()?;
        Ok(())
    }

    fn read_into_buf(&mut self) -> Result<usize, TransportError> {
        let mut chunk = [0u8; 65_535];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        self.rx_buf.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// Pull one complete frame, if available. Returns Ok(None) on a
    /// read-timeout (no data yet, connection still up).
    pub fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if self.rx_buf.len() >= 4 {
                let len = u32::from_be_bytes(self.rx_buf[0..4].try_into().unwrap()) as usize;
                if len > 1400 * 64 {
                    return Err(TransportError::Failed("frame length out of range".into()));
                }
                if self.rx_buf.len() >= 4 + len {
                    let payload = self.rx_buf[4..4 + len].to_vec();
                    self.rx_buf.drain(..4 + len);
                    return Ok(Some(payload));
                }
            }
            let read = self.read_into_buf()?;
            if read == 0 {
                break;
            }
        }
        Ok(None)
    }
}

impl Transport for TcpTunnel {
    fn kind(&self) -> TransportKind {
        TransportKind::TcpTls
    }

    fn connect(&mut self, _endpoint: &str) -> Result<ConnectionStats, TransportError> {
        Ok(ConnectionStats {
            rtt_ms: None,
            jitter_ms: None,
            loss_ratio: Some(0.0), // stream semantics: no packet loss, HOL risk instead
            handshake_ms: None,
            reconnects: 0,
        })
    }

    fn send(&mut self, frame: PendingSend) -> Result<(), TransportError> {
        self.write_frame(&frame.bytes)
    }

    fn recv(&mut self) -> Result<Option<RecvResult>, TransportError> {
        self.recv_frame().map(|opt| {
            opt.map(|bytes| RecvResult {
                bytes,
                sequence: None,
                received_at_ms: now_ms(),
            })
        })
    }

    fn note_failure(&mut self, _detail: &str) {}
    fn note_success(&mut self) {}
    fn close(&mut self) {}
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::PendingSend;
    use std::net::TcpListener;

    #[test]
    fn length_prefix_framing_roundtrips_over_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read a frame: 4-byte len + payload, echo with a marker.
            let mut hdr = [0u8; 4];
            stream.read_exact(&mut hdr).unwrap();
            let len = u32::from_be_bytes(hdr) as usize;
            let mut payload = vec![0u8; len];
            stream.read_exact(&mut payload).unwrap();
            let marker = b"echo:".to_vec();
            let out = [marker, payload].concat();
            stream.write_all(&(out.len() as u32).to_be_bytes()).unwrap();
            stream.write_all(&out).unwrap();
            stream.flush().unwrap();
        });

        let mut client = TcpTunnel::connect(&addr.to_string(), Duration::from_secs(2)).unwrap();
        client
            .send(PendingSend {
                bytes: b"hello".to_vec(),
                latency_sensitive: false,
            })
            .unwrap();
        let recv = client.recv().unwrap().expect("echo frame");
        assert_eq!(recv.bytes, b"echo:hello");
        server_handle.join().unwrap();
    }
}
