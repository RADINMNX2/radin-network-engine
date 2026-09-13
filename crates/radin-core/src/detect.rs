//! Restrictive network detection (spec 26).
//!
//! We detect *symptoms* and describe them cautiously. The engine NEVER
//! declares "your ISP is blocking you" — only "UDP connectivity appears
//! degraded", because the observed pattern is compatible with many causes
//! (congestion, NAT, radio, middlebox policy, carrier transit, …).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Symptom {
    RepeatedUdpTimeout,
    QuicHandshakeFailure,
    TcpSuccessUdpFailure,
    DnsFailure,
    TlsHandshakeFailure,
    ConnectionReset,
    EndpointSpecificBlocking,
}

impl Symptom {
    /// Precise, non-accusatory natural-language wording.
    pub fn description(self) -> &'static str {
        match self {
            Symptom::RepeatedUdpTimeout => "Repeated UDP packets are timing out.",
            Symptom::QuicHandshakeFailure => "QUIC handshakes are failing consistently.",
            Symptom::TcpSuccessUdpFailure => "TCP succeeds while UDP traffic appears degraded.",
            Symptom::DnsFailure => "DNS resolution is failing or slow.",
            Symptom::TlsHandshakeFailure => "TLS handshakes are failing.",
            Symptom::ConnectionReset => "Connections are being reset between retransmits.",
            Symptom::EndpointSpecificBlocking => "A specific endpoint is unreachable across transports.",
        }
    }

    /// Diagnostic summary when multiple symptoms co-occur.
    pub fn summary(symptoms: &[Symptom]) -> String {
        if symptoms.is_empty() {
            return "No restrictive-network symptoms detected.".to_string();
        }
        let udp_degraded = symptoms
            .iter()
            .any(|s| matches!(s, Symptom::RepeatedUdpTimeout | Symptom::QuicHandshakeFailure));
        let mut labels = symptoms
            .iter()
            .map(|s| s.description())
            .collect::<Vec<_>>();
        labels.sort();
        let mut out = String::from("UDP connectivity appears degraded. ");
        out.push_str(&labels.join(" "));
        let _ = udp_degraded;
        out
    }
}

/// A short-lived evidence store observed by the engine, aggregated over a
/// rolling window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestrictionModel {
    pub counts: std::collections::BTreeMap<String, u32>,
}

impl RestrictionModel {
    pub fn observe(&mut self, symptom: Symptom) {
        let key = format!("{symptom:?}");
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// Which symptoms have crossed `min_occurrences` in the window?
    pub fn flagged(&self, min_occurrences: u32) -> Vec<Symptom> {
        let mut out = Vec::new();
        for (key, count) in &self.counts {
            if *count >= min_occurrences {
                if let Some(s) = key_to_symptom(key) {
                    out.push(s);
                }
            }
        }
        out
    }

    pub fn describe(&self, min_occurrences: u32) -> String {
        Symptom::summary(&self.flagged(min_occurrences))
    }

    pub fn reset(&mut self) {
        self.counts.clear();
    }
}

fn key_to_symptom(key: &str) -> Option<Symptom> {
    match key {
        "RepeatedUdpTimeout" => Some(Symptom::RepeatedUdpTimeout),
        "QuicHandshakeFailure" => Some(Symptom::QuicHandshakeFailure),
        "TcpSuccessUdpFailure" => Some(Symptom::TcpSuccessUdpFailure),
        "DnsFailure" => Some(Symptom::DnsFailure),
        "TlsHandshakeFailure" => Some(Symptom::TlsHandshakeFailure),
        "ConnectionReset" => Some(Symptom::ConnectionReset),
        "EndpointSpecificBlocking" => Some(Symptom::EndpointSpecificBlocking),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording_is_beautifully_vague_on_purpose() {
        let mut model = RestrictionModel::default();
        for _ in 0..5 {
            model.observe(Symptom::RepeatedUdpTimeout);
            model.observe(Symptom::QuicHandshakeFailure);
        }
        model.observe(Symptom::DnsFailure);
        let msg = model.describe(3);
        assert!(msg.contains("appears degraded"), "msg: {msg}");
        assert!(!msg.to_lowercase().contains("blocking your isp"), "msg: {msg}");
        assert!(!msg.to_uppercase().contains("CENSOR"), "msg: {msg}");
    }

    #[test]
    fn low_occurrences_stay_unflagged() {
        let mut model = RestrictionModel::default();
        model.observe(Symptom::DnsFailure);
        assert!(model.flagged(3).is_empty());
    }

    #[test]
    fn empty_model_is_benign() {
        let model = RestrictionModel::default();
        assert_eq!(model.describe(1), "No restrictive-network symptoms detected.");
    }
}