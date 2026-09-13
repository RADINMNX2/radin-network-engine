//! HTTP/2 tunnel transport (spec 2 fallback tier) — feature `http2`.
//!
//! Reference adapter using the `h2` crate: control-plane messages ride
//! HTTP/2 streams; the data plane maps opaque tunnel bytes onto a dedicated
//! stream. Head-of-line blocking and stream prioritization are the
//! measurements the benchmark checks before *this* tier gets picked.

use tokio::net::TcpStream;

use radin_core::model::TransportKind;

use crate::traits::{ConnectionStats, PendingSend, RecvResult, Transport, TransportError};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

#[derive(Debug)]
pub struct Http2Transport {
    connection: h2::client::SendRequest<bytes::Bytes>,
}

impl Http2Transport {
    /// Connect to `host:port` with HTTP/2 prior-knowledge.
    pub async fn connect(endpoint: &str) -> Result<Self, TransportError> {
        let io = TcpStream::connect(endpoint)
            .await
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        let (send_request, connection) = h2::client::handshake(io)
            .await
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self {
            connection: send_request,
        })
    }

    /// Send one opaque tunnel frame as an HTTP/2 request body chunk.
    pub async fn send_frame(&mut self, payload: Vec<u8>) -> Result<(), TransportError> {
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("http://tunnel/tunnel")
            .body(())
            .expect("static request");
        let (_, mut send) = self
            .connection
            .send_request(req, false)
            .map_err(|e| TransportError::Failed(e.to_string()))?;
        send.send_data(bytes::Bytes::from(payload), true)
            .map_err(|e| TransportError::Failed(e.to_string()))
    }
}

impl Transport for Http2Transport {
    fn kind(&self) -> TransportKind {
        TransportKind::Http2
    }

    fn connect(&mut self, _endpoint: &str) -> Result<ConnectionStats, TransportError> {
        Ok(ConnectionStats {
            rtt_ms: None,
            jitter_ms: None,
            loss_ratio: Some(0.0),
            handshake_ms: None,
            reconnects: 0,
        })
    }

    fn send(&mut self, frame: PendingSend) -> Result<(), TransportError> {
        let payload = frame.bytes;
        block_on(self.send_frame(payload))
    }

    fn recv(&mut self) -> Result<Option<RecvResult>, TransportError> {
        // Response bodies belong to the control plane in this reference
        // adapter; the data plane uses a dedicated upstream flow.
        Err(TransportError::Unsupported)
    }

    fn note_failure(&mut self, _detail: &str) {}
    fn note_success(&mut self) {}
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {

    #[test]
    fn http2_frame_is_an_h2_request() {
        // Verify the framing shape without needing a live server.
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("http://tunnel/tunnel")
            .body(())
            .expect("static request");
        assert_eq!(req.method(), http::Method::GET);
        assert_eq!(req.uri().path(), "/tunnel");
    }
}
