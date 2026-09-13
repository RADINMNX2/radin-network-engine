//! WebSocket/TLS transport (spec 2 final fallback tier) — feature `ws`.
//!
//! `tokio-tungstenite` over a TCP/TLS stream. WebSocket adds per-frame
//! overhead and is the most "compatible-with-everything" option when QUIC,
//! UDP and even plain TCP/TLS are unusable. The engine only lands here after
//! measurable failure of the higher tiers.

use futures_util::{SinkExt, StreamExt};
use radin_core::model::TransportKind;
use tokio_tungstenite::tungstenite::Message;

use crate::traits::{ConnectionStats, PendingSend, RecvResult, Transport, TransportError};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

#[derive(Debug)]
pub struct WebSocketTunnel {
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
}

impl WebSocketTunnel {
    /// Connect with `ws://host:port/path`. TLS variants upstream belong to
    /// the caller (rustls handshake then wrap the TcpStream).
    pub fn connect(endpoint: &str, path: &str) -> Result<Self, TransportError> {
        let url = format!("ws://{endpoint}{path}");
        let (ws, _) = block_on(async {
            let socket = tokio::net::TcpStream::connect(endpoint)
                .await
                .map_err(|e| TransportError::Handshake(e.to_string()))?;
            tokio_tungstenite::client_async(url, socket)
                .await
                .map_err(|e| TransportError::Handshake(e.to_string()))
        })?;
        let (sink, stream) = ws.split();
        Ok(Self { sink, stream })
    }

    /// Send one binary frame (tunnel bytes).
    pub async fn send_frame(&mut self, payload: Vec<u8>) -> Result<(), TransportError> {
        self.sink
            .send(Message::Binary(payload))
            .await
            .map_err(|e| TransportError::Failed(e.to_string()))
    }

    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.stream.next().await {
            Some(Ok(Message::Binary(data))) => Ok(Some(data.to_vec())),
            Some(Ok(Message::Ping(_))) => Ok(None),
            Some(Ok(_)) => Ok(None),
            Some(Err(e)) => Err(TransportError::Failed(e.to_string())),
            None => Err(TransportError::Closed),
        }
    }
}

impl Transport for WebSocketTunnel {
    fn kind(&self) -> TransportKind {
        TransportKind::WebSocketTls
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
        block_on(self.send_frame(frame.bytes))
    }

    fn recv(&mut self) -> Result<Option<RecvResult>, TransportError> {
        block_on(self.recv_frame()).map(|opt| {
            opt.map(|bytes| RecvResult {
                bytes,
                sequence: None,
                received_at_ms: 0,
            })
        })
    }

    fn note_failure(&mut self, _detail: &str) {}
    fn note_success(&mut self) {}
    fn close(&mut self) {}
}
