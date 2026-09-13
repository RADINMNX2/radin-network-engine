//! QUIC transport (spec 3) — feature `quic`.
//!
//! Uses quinn. Honest notes the spec demands stay visible:
//! - QUIC multiplexes streams; one lost packet does NOT block unrelated
//!   streams at the application layer.
//! - QUIC does NOT eliminate packet loss and is not magically lowest-ping.
//!   The benchmark layer decides if QUIC actually wins for this route.
//!
//! `dev_crypto()` produces a local-only self-signed setup for the chaos
//! harness. Production must pin real edge certs (`CryptoConfig::Pinned`).

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use radin_core::model::TransportKind;

use crate::traits::{ConnectionStats, PendingSend, RecvResult, Transport, TransportError};

type Result<T> = std::result::Result<T, TransportError>;

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

#[derive(Debug, Clone)]
pub enum CryptoConfig {
    /// Self-signed dev cert, local testing only.
    Dev,
    /// Trust given CA roots (pinned deployment).
    Pinned(Arc<rustls::RootCertStore>),
}

fn build_client_config(crypto: &CryptoConfig) -> Result<quinn::ClientConfig> {
    // Pin one TLS crypto provider regardless of which optional features the
    // workspace enables (ring is what this crate declares; ws pulls in
    // aws-lc-rs, which would otherwise make auto-detection ambiguous).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = match crypto {
        CryptoConfig::Dev => {
            let mut params =
                rcgen::CertificateParams::new(vec!["localhost".to_string()])
                    .map_err(|e| TransportError::Handshake(e.to_string()))?;
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let key_pair = rcgen::KeyPair::generate()
                .map_err(|e| TransportError::Handshake(e.to_string()))?;
            let cert = params
                .self_signed(&key_pair)
                .map_err(|e| TransportError::Handshake(e.to_string()))?;
            let der = cert.der().clone();
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(der)
                .map_err(|e| TransportError::Handshake(e.to_string()))?;
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        CryptoConfig::Pinned(roots) => rustls::ClientConfig::builder()
            .with_root_certificates(roots.as_ref().clone())
            .with_no_client_auth(),
    };
    let quic_client = quinn::crypto::rustls::QuicClientConfig::try_from(client)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic_client)))
}

#[derive(Debug)]
pub struct QuicTransport {
    connection: quinn::Connection,
}

impl QuicTransport {
    /// Async connect to a QUIC server; returns after the handshake settles.
    pub async fn connect_async(
        addr: SocketAddr,
        server_name: &str,
        crypto: CryptoConfig,
        handshake_timeout: Duration,
    ) -> Result<Self> {
        let client_config = build_client_config(&crypto)?;
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(|e: std::net::AddrParseError| TransportError::Handshake(e.to_string()))?)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        endpoint.set_default_client_config(client_config);
        let connecting = endpoint
            .connect(addr, server_name)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        let connection = tokio::time::timeout(handshake_timeout, connecting)
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        Ok(Self { connection })
    }

    /// Sync wrapper for trait compat (reference adapter uses its own loop).
    pub fn connect(
        addr: SocketAddr,
        server_name: &str,
        crypto: CryptoConfig,
        timeout: Duration,
    ) -> Result<Self> {
        block_on(Self::connect_async(addr, server_name, crypto, timeout))
    }

    /// Open a tunnel stream (data plane carries opaque IP packets on a
    /// dedicated stream — spec 3: native QUIC streams for multiplexing).
    pub fn open_stream(&self) -> Result<quinn::SendStream> {
        block_on(self.connection.open_bi())
            .map(|(s, _r)| s)
            .map_err(|e| TransportError::Failed(e.to_string()))
    }
}

impl Transport for QuicTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Quic
    }

    fn connect(&mut self, _endpoint: &str) -> std::result::Result<ConnectionStats, TransportError> {
        Ok(ConnectionStats {
            rtt_ms: None,
            jitter_ms: None,
            loss_ratio: None,
            handshake_ms: None,
            reconnects: 0,
        })
    }

    fn send(&mut self, frame: PendingSend) -> std::result::Result<(), TransportError> {
        let (mut send, _recv) = block_on(self.connection.open_bi())
            .map_err(|e| TransportError::Failed(e.to_string()))?;
        // Tag first byte: 1 = latency-sensitive (caller may drop-in-queue).
        let mut payload = frame.bytes;
        let mut buf = Vec::with_capacity(payload.len() + 1);
        buf.push(if frame.latency_sensitive { 1 } else { 0 });
        buf.append(&mut payload);
        block_on(async {
            send.write_all(&buf).await?;
            Ok::<(), std::io::Error>(())
        })
        .map_err(TransportError::Io)?;
        send.finish().map_err(|e| TransportError::Failed(e.to_string()))
    }

    fn recv(&mut self) -> std::result::Result<Option<RecvResult>, TransportError> {
        let (_send, mut recv) = block_on(self.connection.open_bi())
            .map_err(|e| TransportError::Failed(e.to_string()))?;
        let mut buf = vec![0u8; 2048];
        let n = block_on(recv.read(&mut buf)).map_err(|e| TransportError::Failed(e.to_string()))?;
        match n {
            Some(n) if n > 0 => {
                buf.truncate(n);
                Ok(Some(RecvResult { bytes: buf, sequence: None, received_at_ms: 0 }))
            }
            _ => Ok(None),
        }
    }

    fn note_failure(&mut self, _detail: &str) {}
    fn note_success(&mut self) {}
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_crypto_builds_a_client_config() {
        let cfg = build_client_config(&CryptoConfig::Dev).unwrap();
        let _ = cfg;
    }
}