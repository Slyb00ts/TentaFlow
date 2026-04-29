// ============ File: services/transport.rs — runtime transport variants for unified services ============

use anyhow::{anyhow, Result};

/// How a deployed engine is reached at runtime.
///
/// `Embedded` means the engine runs in-process (e.g. llama.cpp, MLX, sherpa-onnx)
/// and is invoked via direct Rust calls; no network endpoint exists.
/// `HttpDirect` means the engine exposes its own HTTP server (e.g. vLLM, ollama).
/// `SidecarQuic` means a Rust sidecar speaks QUIC to TentaFlow but proxies to a
/// language-specific runtime (e.g. Python bundle behind a sidecar).
/// `ExternalHttp` means an external daemon discovered in PATH and probed via HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Embedded,
    HttpDirect,
    SidecarQuic,
    ExternalHttp,
}

impl Transport {
    /// Stable string used in the `services_v2.transport` column.
    pub fn as_db_tag(self) -> &'static str {
        match self {
            Transport::Embedded => "embedded",
            Transport::HttpDirect => "http_direct",
            Transport::SidecarQuic => "sidecar_quic",
            Transport::ExternalHttp => "external_http",
        }
    }

    pub fn from_db_tag(tag: &str) -> Result<Self> {
        Ok(match tag {
            "embedded" => Transport::Embedded,
            "http_direct" => Transport::HttpDirect,
            "sidecar_quic" => Transport::SidecarQuic,
            "external_http" => Transport::ExternalHttp,
            other => return Err(anyhow!("unknown transport tag: {}", other)),
        })
    }

    /// Builds a canonical endpoint URL for HTTP-style transports.
    /// Returns `None` for `Embedded` (no network endpoint).
    pub fn endpoint_url(self, host: &str, port: u16) -> Option<String> {
        match self {
            Transport::Embedded => None,
            Transport::HttpDirect | Transport::ExternalHttp => {
                Some(format!("http://{}:{}", host, port))
            }
            Transport::SidecarQuic => Some(format!("quic://{}:{}", host, port)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_tag_roundtrip() {
        for t in [
            Transport::Embedded,
            Transport::HttpDirect,
            Transport::SidecarQuic,
            Transport::ExternalHttp,
        ] {
            let tag = t.as_db_tag();
            let parsed = Transport::from_db_tag(tag).unwrap();
            assert_eq!(parsed, t, "roundtrip failed for {:?}", t);
        }
    }

    #[test]
    fn endpoint_url_for_each_variant() {
        assert_eq!(Transport::Embedded.endpoint_url("localhost", 8000), None);
        assert_eq!(
            Transport::HttpDirect
                .endpoint_url("127.0.0.1", 8000)
                .as_deref(),
            Some("http://127.0.0.1:8000")
        );
        assert_eq!(
            Transport::ExternalHttp
                .endpoint_url("localhost", 11434)
                .as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            Transport::SidecarQuic
                .endpoint_url("node1", 5500)
                .as_deref(),
            Some("quic://node1:5500")
        );
    }

    #[test]
    fn from_db_tag_rejects_unknown() {
        assert!(Transport::from_db_tag("magic").is_err());
    }
}
