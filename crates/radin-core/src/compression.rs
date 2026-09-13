//! Compression policy (spec 19): zstd where available, Brotli where
//! appropriate. Only payloads above `compressionThresholdBytes` are
//! compressed; already-compressed data is never re-compressed.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompressionConfig {
    /// Min payload size before compression is attempted.
    pub compression_threshold_bytes: usize,
    /// Output must be at least this much smaller, else we keep raw.
    pub min_beneficial_ratio: f64,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            compression_threshold_bytes: 1024,
            min_beneficial_ratio: 0.9, // at least 10% smaller
        }
    }
}

/// Heuristic: content types that are effectively already-compressed.
pub fn looks_already_compressed(data: &[u8]) -> bool {
    if data.len() < 16 {
        return false;
    }
    // gzip/zlib/deflate/zstd magic-bytes and audio/video/image containers.
    let prefixes: &[&[u8]] = &[
        &[0x1f, 0x8b],       // gzip
        &[0x28, 0xb5, 0x2f, 0xfd], // zstd
        &[0x78, 0x01],       // zlib (no compression)
        &[0x78, 0x9c],       // zlib default
        &[0x50, 0x4b, 0x03, 0x04], // zip
        &[0xff, 0xd8, 0xff], // jpeg
    ];
    prefixes.iter().any(|p| data.starts_with(p))
}

/// Should we even try to compress this payload?
pub fn eligible(cfg: &CompressionConfig, data: &[u8]) -> bool {
    data.len() >= cfg.compression_threshold_bytes && !looks_already_compressed(data)
}

/// Outcome of a compression attempt — always honest about which path ran.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CompressionOutcome {
    Raw,
    Zstd(Vec<u8>),
    /// Present when a brotli-specific backend is wired (feature-gated).
    Brotli(Vec<u8>),
}

/// Apply zstd when `zstd` feature is enabled; otherwise fall back to a plain
/// honesty pass that reports `Raw`. The decision logic stays identical so
/// tests don't depend on the feature.
pub fn compress(cfg: &CompressionConfig, data: &[u8]) -> CompressionOutcome {
    if data.len() < cfg.compression_threshold_bytes {
        return CompressionOutcome::Raw; // "Do not compress tiny packets."
    }
    if looks_already_compressed(data) {
        return CompressionOutcome::Raw; // "Do not compress already-compressed data."
    }
    #[cfg(feature = "zstd")]
    {
        match zstd::bulk::compress(data, 3) {
            Ok(c) if (c.len() as f64) < (data.len() as f64) * cfg.min_beneficial_ratio => {
                CompressionOutcome::Zstd(c)
            }
            _ => CompressionOutcome::Raw,
        }
    }
    #[cfg(not(feature = "zstd"))]
    {
        CompressionOutcome::Raw
    }
}

/// Ratio before/after; >1 means compression helped. `Raw` yields 1.0.
pub fn measured_ratio(data_len: usize, outcome: &CompressionOutcome) -> f64 {
    match outcome {
        CompressionOutcome::Raw => 1.0,
        CompressionOutcome::Zstd(c) | CompressionOutcome::Brotli(c) => {
            if c.is_empty() {
                1.0
            } else {
                data_len as f64 / c.len() as f64
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)] // only used by the feature-gated zstd test
    fn somewhat_repetitive(n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            v.push((i % 8) as u8);
        }
        v
    }

    #[test]
    fn tiny_payloads_never_compressed() {
        let cfg = CompressionConfig::default();
        assert_eq!(compress(&cfg, &[0u8; 32]), CompressionOutcome::Raw);
    }

    #[test]
    fn already_compressed_never_recompressed() {
        let cfg = CompressionConfig::default();
        let mut gz = vec![0x1f, 0x8b];
        gz.extend(&[0u8; 2048]);
        assert_eq!(compress(&cfg, &gz), CompressionOutcome::Raw);
    }

    #[test]
    fn eligibility_logic() {
        let cfg = CompressionConfig::default();
        assert!(!eligible(&cfg, &[0u8; 512]));
        assert!(eligible(&cfg, &[0u8; 4096]));
        let mut gz = vec![0x1f, 0x8b];
        gz.extend(&[0u8; 4096]);
        assert!(!eligible(&cfg, &gz));
    }

    #[test]
    fn ratio_reporting_is_honest() {
        let raw = CompressionOutcome::Raw;
        assert_eq!(measured_ratio(100, &raw), 1.0);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_feature_actually_compresses_when_beneficial() {
        let cfg = CompressionConfig::default();
        let data = somewhat_repetitive(4096);
        match compress(&cfg, &data) {
            CompressionOutcome::Zstd(c) => assert!(c.len() < data.len()),
            other => panic!("expected zstd path, got {other:?}"),
        }
    }
}