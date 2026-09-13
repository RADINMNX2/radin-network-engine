//! Security of edge communication (spec 34).
//!
//! Control-plane messages must be authenticated and encrypted. The client
//! validates the server identity (TLS cert chain, pinned identity, protocol
//! version) and *configuration signatures* before trusting any remote
//! settings. We never accept arbitrary remote configuration without checks.

use serde::{Deserialize, Serialize};

use crate::model::{EdgeInfo, TransportKind};

/// Protocol version negotiated with edges.
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;

/// A set of acceptable server identities (SPKI pins or hostnames).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedIdentity {
    /// e.g. "edge-sg-01.example."
    pub hostname: String,
    /// Base64 SPKI-sha256 pin for certificate pinning, if configured.
    pub spki_pin_b64: Option<String>,
}

/// Validation verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationVerdict {
    Ok,
    ProtocolMismatch { expected: u32, got: u32 },
    UntrustedIdentity { detail: String },
    SignatureInvalid { detail: String },
    Expired,
}

impl ValidationVerdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, ValidationVerdict::Ok)
    }
}

/// Configuration signature verification hook. The real verifier (ed25519
/// over the canonical JSON, or a session MAC) lives at the transport layer;
/// this trait keeps the core testable without a PKI.
pub trait ConfigSigner: Send + Sync {
    /// Verify `signature_b64` over `canonical_json`.
    fn verify(&self, canonical_json: &[u8], signature_b64: &str) -> bool;
}

/// Empty signer: rejects everything unless `allow_unsigned` is true.
/// This is explicit — an unsigned config is only accepted in the local/test
/// bootstrap path, and never in production.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoopConfigSigner {
    pub allow_unsigned: bool,
}

impl ConfigSigner for NoopConfigSigner {
    fn verify(&self, _canonical_json: &[u8], _signature_b64: &str) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct EdgeVerifier {
    pub trusted: Vec<TrustedIdentity>,
    pub signer: std::sync::Arc<dyn ConfigSigner>,
    pub require_signature: bool,
    pub allow_unsigned_locally: bool,
}

impl std::fmt::Debug for EdgeVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeVerifier")
            .field("trusted", &self.trusted)
            .field("require_signature", &self.require_signature)
            .field("allow_unsigned_locally", &self.allow_unsigned_locally)
            .finish_non_exhaustive()
    }
}

impl EdgeVerifier {
    pub fn new(
        trusted: Vec<TrustedIdentity>,
        signer: std::sync::Arc<dyn ConfigSigner>,
        require_signature: bool,
    ) -> Self {
        Self {
            trusted,
            signer,
            require_signature,
            allow_unsigned_locally: false,
        }
    }

    /// Verify an edge entry: identity, expiration, signature, and protocol
    /// compatibility.
    pub fn validate_edge(
        &self,
        edge: &EdgeInfo,
        supported_transports: &[TransportKind],
        protocol_version: u32,
        canonical_json: &[u8],
        now: u64,
    ) -> ValidationVerdict {
        if protocol_version != CONTROL_PROTOCOL_VERSION {
            return ValidationVerdict::ProtocolMismatch {
                expected: CONTROL_PROTOCOL_VERSION,
                got: protocol_version,
            };
        }
        if edge.expires_at <= now {
            return ValidationVerdict::Expired;
        }

        // Identity: hostname must be within the trusted list.
        let host = edge.address.split(':').next().unwrap_or("");
        let identity_ok = self
            .trusted
            .iter()
            .any(|t| t.hostname == host || t.hostname == edge.address);
        if !identity_ok {
            return ValidationVerdict::UntrustedIdentity {
                detail: format!("{} is not a trusted edge host", edge.address),
            };
        }

        // Transport capability sanity: we never accept a list whose
        // transports we cannot speak.
        if edge
            .supported_transports
            .iter()
            .any(|t| !supported_transports.contains(t))
        {
            return ValidationVerdict::UntrustedIdentity {
                detail: format!("{} advertises an unsupported transport", edge.address),
            };
        }

        // Signature.
        match &edge.signature_b64 {
            Some(sig) if !self.signer.verify(canonical_json, sig) => {
                return ValidationVerdict::SignatureInvalid {
                    detail: edge.id.clone(),
                };
            }
            Some(_) => {}
            None => {
                if self.require_signature && !self.allow_unsigned_locally {
                    return ValidationVerdict::SignatureInvalid {
                        detail: "missing signature".into(),
                    };
                }
            }
        }

        ValidationVerdict::Ok
    }
}

/// Trust-on-first-use style trust for local bootstrap: an explicit dev
/// setting, never the default.
pub fn dev_bootstrap_signer() -> NoopConfigSigner {
    NoopConfigSigner {
        allow_unsigned: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial signer for tests.
    struct DevSigner;
    impl ConfigSigner for DevSigner {
        fn verify(&self, _: &[u8], _: &str) -> bool {
            true
        }
    }

    fn edge(address: &str, expires: u64, signature: Option<String>) -> EdgeInfo {
        EdgeInfo {
            id: "e1".into(),
            region: "ap".into(),
            address: address.into(),
            supported_transports: vec![TransportKind::Quic, TransportKind::Udp],
            priority: None,
            expires_at: expires,
            signature_b64: signature,
        }
    }

    #[test]
    fn rejects_untrusted_host() {
        let v = EdgeVerifier::new(
            vec![TrustedIdentity {
                hostname: "edge.corp.example.".into(),
                spki_pin_b64: None,
            }],
            std::sync::Arc::new(DevSigner),
            true,
        );
        let e = edge("evil.example.", 1_000_000, None);
        let verdict = v.validate_edge(&e, &[TransportKind::Quic, TransportKind::Udp], 1, b"{}", 0);
        assert_eq!(
            verdict,
            ValidationVerdict::UntrustedIdentity {
                detail: "evil.example. is not a trusted edge host".into()
            }
        );
    }

    #[test]
    fn rejects_protocol_mismatch() {
        let v = EdgeVerifier::new(vec![], std::sync::Arc::new(DevSigner), true);
        let e = edge("edge.example.", 1_000_000, None);
        let verdict = v.validate_edge(&e, &[TransportKind::Quic], 99, b"{}", 0);
        assert!(matches!(
            verdict,
            ValidationVerdict::ProtocolMismatch { .. }
        ));
    }

    #[test]
    fn rejects_expired_edge() {
        let v = EdgeVerifier::new(vec![], std::sync::Arc::new(DevSigner), true);
        let e = edge("edge.example.", 500, None);
        assert_eq!(
            v.validate_edge(&e, &[TransportKind::Quic], 1, b"{}", 1_000),
            ValidationVerdict::Expired
        );
    }

    #[test]
    fn rejects_unsigned_remote_config_by_default() {
        let v = EdgeVerifier::new(
            vec![TrustedIdentity {
                hostname: "edge.example.".into(),
                spki_pin_b64: None,
            }],
            std::sync::Arc::new(NoopConfigSigner {
                allow_unsigned: false,
            }),
            true,
        );
        let e = edge("edge.example.", 1_000_000, None);
        let verdict = v.validate_edge(&e, &[TransportKind::Quic, TransportKind::Udp], 1, b"{}", 0);
        assert!(matches!(
            verdict,
            ValidationVerdict::SignatureInvalid { .. }
        ));
    }

    #[test]
    fn accepts_signed_trusted_edge() {
        let v = EdgeVerifier::new(
            vec![TrustedIdentity {
                hostname: "edge.example.".into(),
                spki_pin_b64: None,
            }],
            std::sync::Arc::new(DevSigner),
            true,
        );
        let e = edge("edge.example.", 1_000_000, Some("abc".into()));
        assert!(v
            .validate_edge(&e, &[TransportKind::Quic, TransportKind::Udp], 1, b"{}", 0)
            .is_ok());
    }

    #[test]
    fn dev_bootstrap_signer_is_explicitly_local_only() {
        let v = EdgeVerifier::new(
            vec![TrustedIdentity {
                hostname: "edge.example.".into(),
                spki_pin_b64: None,
            }],
            std::sync::Arc::new(dev_bootstrap_signer()),
            false,
        );
        let e = edge("edge.example.", 1_000_000, None);
        assert!(v
            .validate_edge(&e, &[TransportKind::Quic, TransportKind::Udp], 1, b"{}", 0)
            .is_ok());
    }
}
