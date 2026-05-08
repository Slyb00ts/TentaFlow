// ============ File: services/runtime/transport_client.rs — Transport-aware client glue for snapshot-driven routing ============
//
// Builds a legacy `ServiceBackend` (and a ready-to-use `BackendClient`) from a
// `ServiceEntry` materialised by the supervisor snapshot. Phase 8b-1 wires the
// fundament; chat.rs / middleware.rs / adapters call sites are migrated in
// subsequent passes (8b-2, 8b-3) and consume `entry_to_backend_client` +
// `pick_service` directly.

use std::collections::HashMap;
use std::sync::Arc;

use rand::RngExt;

use crate::config::{ConnectionType, ServiceBackend};
use crate::net::quic::QuicClient;
use crate::services::backend::client::BackendClient;
use crate::services::handles_cache::BackendHandle;
use crate::services::supervisor::ServiceEntry;
use crate::services::transport::Transport;

#[derive(Debug, thiserror::Error)]
pub enum TransportClientError {
    #[error("model not found in snapshot: {0}")]
    ModelNotFound(String),
    #[error("no candidate services for model {0}")]
    NoCandidates(String),
    #[error("transport not supported by glue: {0}")]
    UnsupportedTransport(String),
    #[error("backend client init failed: {0}")]
    BackendInit(String),
    #[error("QUIC handle not connected for {0}")]
    QuicNotConnected(String),
    #[error("embedded engine cross-node dispatch is not supported (node {0})")]
    EmbeddedRemote(String),
}

/// Materialised view returned by `resolve_quic_client`. Contains the connected
/// `QuicClient` ready to issue `send_request` / `send_request_stream`. The
/// caller decides which payload variant to wrap (chat / embeddings / stt /
/// tts / memory) — the helper is transport-agnostic.
pub struct ResolvedQuic {
    pub client: Arc<QuicClient>,
}

/// Materialised view returned by `resolve_http_client`. The caller picks one
/// of the `BackendClient::*` methods (`chat_completion`, `embedding`,
/// `audio_transcription`, `audio_speech`, `embeddings_request`, etc.) based on
/// the request kind.
pub struct ResolvedHttp {
    pub client: Arc<BackendClient>,
}

/// Resolves a `BackendHandle` to a concrete QUIC client when the handle is the
/// `Quic` variant and the underlying connection is currently connected.
/// Returns `Err(QuicNotConnected)` while the reconnect loop is still trying;
/// caller must decide whether to wait, retry, or fall back to HTTP.
pub async fn resolve_quic_client(
    handle: &BackendHandle,
    label: &str,
) -> Result<ResolvedQuic, TransportClientError> {
    match handle {
        BackendHandle::Quic(qh) => match qh.get_client().await {
            Some(client) => Ok(ResolvedQuic { client }),
            None => Err(TransportClientError::QuicNotConnected(label.to_string())),
        },
        _ => Err(TransportClientError::UnsupportedTransport(format!(
            "expected Quic, got non-quic handle for {}",
            label
        ))),
    }
}

/// Resolves a `BackendHandle` to a HTTP `BackendClient`. Returns
/// `Err(UnsupportedTransport)` for non-HTTP variants.
pub fn resolve_http_client(
    handle: &BackendHandle,
    label: &str,
) -> Result<ResolvedHttp, TransportClientError> {
    match handle {
        BackendHandle::Http(client) => Ok(ResolvedHttp {
            client: client.clone(),
        }),
        _ => Err(TransportClientError::UnsupportedTransport(format!(
            "expected Http, got non-http handle for {}",
            label
        ))),
    }
}

/// Validates that an `Embedded` handle belongs to `local_node_id`. Cross-node
/// embedded dispatch is rejected because embedded engines only run in-process
/// — the request must be forwarded to the owning node before reaching the
/// dispatch helper.
pub fn ensure_local_embedded(
    handle: &BackendHandle,
    local_node_id: &str,
) -> Result<String, TransportClientError> {
    match handle {
        BackendHandle::Embedded {
            model_name,
            node_id,
            engine_id: _,
        } => {
            if node_id == local_node_id {
                Ok(model_name.clone())
            } else {
                Err(TransportClientError::EmbeddedRemote(node_id.clone()))
            }
        }
        _ => Err(TransportClientError::UnsupportedTransport(
            "expected Embedded handle".into(),
        )),
    }
}

/// Builds a `ServiceBackend` (legacy config struct) from a snapshot
/// `ServiceEntry`. `Embedded` transport is rejected — embedded engines must be
/// dispatched directly through `LocalInferenceManager` before reaching the
/// HTTP/QUIC glue.
pub fn entry_to_service_backend(
    svc: &ServiceEntry,
) -> Result<ServiceBackend, TransportClientError> {
    let connection = match svc.transport {
        Transport::Embedded => {
            return Err(TransportClientError::UnsupportedTransport(
                "Embedded must be dispatched directly via LocalInferenceManager".into(),
            ));
        }
        Transport::HttpDirect | Transport::ExternalHttp => {
            let url = svc.endpoint_url.clone().ok_or_else(|| {
                TransportClientError::BackendInit("endpoint_url missing for HTTP transport".into())
            })?;
            ConnectionType::OpenAIApi {
                url,
                api_key: svc.extra_config.get("api_key").cloned(),
                api_key_env: svc.extra_config.get("api_key_env").cloned(),
                custom_endpoint: svc.extra_config.get("custom_endpoint").cloned(),
                request_format: svc.extra_config.get("request_format").cloned(),
                extra_headers: parse_headers(svc.extra_config.get("custom_headers_json")),
                tts_config: None,
            }
        }
        Transport::SidecarQuic => {
            let port = svc.sidecar_quic_port.ok_or_else(|| {
                TransportClientError::BackendInit("sidecar_quic_port missing".into())
            })?;
            ConnectionType::QUIC {
                quic_url: format!("quic://127.0.0.1:{}", port),
                tls_ca: None,
                auto_reconnect: true,
                reconnect_interval_ms: 1_000,
                keepalive_interval_ms: 5_000,
                tts_config: None,
            }
        }
    };

    Ok(ServiceBackend {
        connection,
        max_concurrent: svc.max_concurrent.max(1) as usize,
        timeout_ms: svc.timeout_ms,
        weight: svc.weight,
        model_name_override: svc.model_name_override.clone(),
        health_check_path: None,
    })
}

/// Resolves an API key from the snapshot's `extra_config`. Direct `api_key`
/// always wins; otherwise the named env var is consulted. Returns `None` when
/// neither is present (anonymous backend, e.g. local ollama).
#[allow(dead_code)]
fn resolve_api_key(cfg: &HashMap<String, String>) -> Option<String> {
    if let Some(direct) = cfg.get("api_key") {
        return Some(direct.clone());
    }
    if let Some(env_name) = cfg.get("api_key_env") {
        return std::env::var(env_name).ok();
    }
    None
}

/// Parses an `extra_headers` payload from the JSON object stored in
/// `custom_headers_json`. Accepts either `{"H": "v"}` or `[["H","v"]]` and
/// flattens both into the `Vec<(String, String)>` shape expected by
/// `ConnectionType::OpenAIApi`. Malformed JSON yields an empty vec rather than
/// failing the build — the snapshot fields are best-effort hints.
fn parse_headers(maybe_json: Option<&String>) -> Vec<(String, String)> {
    let Some(s) = maybe_json else {
        return Vec::new();
    };
    let value: serde_json::Value = match serde_json::from_str(s) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    if let Some(obj) = value.as_object() {
        return obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
    }
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|pair| {
                let pair = pair.as_array()?;
                let k = pair.first()?.as_str()?;
                let v = pair.get(1)?.as_str()?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();
    }
    Vec::new()
}

/// Picks one `ServiceEntry` from the candidate list using weighted random
/// distribution. Returns `None` only when `candidates` is empty. Single
/// candidate fast-path skips RNG.
pub fn pick_service<'a>(candidates: &[&'a ServiceEntry]) -> Option<&'a ServiceEntry> {
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    let total_weight: u32 = candidates.iter().map(|s| s.weight.max(1)).sum();
    if total_weight == 0 {
        return Some(candidates[0]);
    }
    let mut roll = rand::rng().random_range(0..total_weight);
    for c in candidates {
        let w = c.weight.max(1);
        if roll < w {
            return Some(*c);
        }
        roll -= w;
    }
    candidates.last().copied()
}

/// Builds a `BackendClient` from a snapshot `ServiceEntry`. `Embedded`
/// transport returns `Err(UnsupportedTransport)`; `SidecarQuic` returns
/// `Err(BackendInit)` because `BackendClient` only speaks OpenAI-compatible
/// HTTP — the QUIC sidecar path is owned by `quic_*_services` in
/// `ServiceManager`.
#[allow(dead_code)]
pub fn entry_to_backend_client(svc: &ServiceEntry) -> Result<BackendClient, TransportClientError> {
    let cfg = entry_to_service_backend(svc)?;
    BackendClient::new(cfg, None).map_err(|e| TransportClientError::BackendInit(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::supervisor::ModelEntry;
    use crate::services_repo::services::{DeployMethod, ServiceStatus};

    fn fixture_entry(transport: Transport, weight: u32) -> ServiceEntry {
        ServiceEntry {
            id: 1,
            engine_id: "test".into(),
            category: "llm".into(),
            display_name: "test".into(),
            deploy_method: DeployMethod::NativePythonBundle,
            transport,
            status: ServiceStatus::Running,
            pinned: false,
            paused: false,
            endpoint_url: Some("http://127.0.0.1:5012".into()),
            runtime_pid: None,
            runtime_port: Some(5012),
            sidecar_quic_port: Some(5013),
            models: vec![ModelEntry {
                id: 1,
                model_name: "test-model".into(),
                display_name: Some("Test".into()),
                is_default: true,
            }],
            timeout_ms: 30_000,
            max_concurrent: 16,
            weight,
            model_name_override: None,
            extra_config: HashMap::new(),
            progress_message: None,
            health_last_err: None,
        }
    }

    #[test]
    fn entry_to_backend_http_direct() {
        let svc = fixture_entry(Transport::HttpDirect, 100);
        let backend = entry_to_service_backend(&svc).unwrap();
        assert!(matches!(
            backend.connection,
            ConnectionType::OpenAIApi { .. }
        ));
        assert_eq!(backend.timeout_ms, 30_000);
        assert_eq!(backend.weight, 100);
    }

    #[test]
    fn entry_to_backend_external_http() {
        let svc = fixture_entry(Transport::ExternalHttp, 50);
        let backend = entry_to_service_backend(&svc).unwrap();
        match backend.connection {
            ConnectionType::OpenAIApi { url, .. } => {
                assert_eq!(url, "http://127.0.0.1:5012");
            }
            _ => panic!("expected OpenAIApi"),
        }
    }

    #[test]
    fn entry_to_backend_sidecar_quic() {
        let svc = fixture_entry(Transport::SidecarQuic, 100);
        let backend = entry_to_service_backend(&svc).unwrap();
        match backend.connection {
            ConnectionType::QUIC {
                quic_url,
                auto_reconnect,
                ..
            } => {
                assert_eq!(quic_url, "quic://127.0.0.1:5013");
                assert!(auto_reconnect);
            }
            _ => panic!("expected QUIC"),
        }
    }

    #[test]
    fn entry_to_backend_embedded_returns_unsupported() {
        let svc = fixture_entry(Transport::Embedded, 100);
        assert!(matches!(
            entry_to_service_backend(&svc),
            Err(TransportClientError::UnsupportedTransport(_))
        ));
    }

    #[test]
    fn entry_to_backend_http_direct_propagates_extra_config() {
        let mut svc = fixture_entry(Transport::HttpDirect, 100);
        svc.extra_config
            .insert("api_key".into(), "sk-direct".into());
        svc.extra_config
            .insert("custom_endpoint".into(), "/custom".into());
        svc.extra_config
            .insert("request_format".into(), "openai".into());
        let backend = entry_to_service_backend(&svc).unwrap();
        match backend.connection {
            ConnectionType::OpenAIApi {
                api_key,
                custom_endpoint,
                request_format,
                ..
            } => {
                assert_eq!(api_key.as_deref(), Some("sk-direct"));
                assert_eq!(custom_endpoint.as_deref(), Some("/custom"));
                assert_eq!(request_format.as_deref(), Some("openai"));
            }
            _ => panic!("expected OpenAIApi"),
        }
    }

    #[test]
    fn parse_headers_object() {
        let json = r#"{"X-Foo":"bar","X-Baz":"qux"}"#.to_string();
        let mut headers = parse_headers(Some(&json));
        headers.sort();
        assert_eq!(
            headers,
            vec![
                ("X-Baz".to_string(), "qux".to_string()),
                ("X-Foo".to_string(), "bar".to_string())
            ]
        );
    }

    #[test]
    fn parse_headers_array() {
        let json = r#"[["X-Foo","bar"],["X-Baz","qux"]]"#.to_string();
        let headers = parse_headers(Some(&json));
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn parse_headers_malformed_returns_empty() {
        let json = "not-json".to_string();
        assert!(parse_headers(Some(&json)).is_empty());
        assert!(parse_headers(None).is_empty());
    }

    #[test]
    fn resolve_api_key_direct_wins() {
        let mut cfg = HashMap::new();
        cfg.insert("api_key".into(), "direct".into());
        cfg.insert("api_key_env".into(), "NONEXISTENT_VAR_8B1".into());
        assert_eq!(resolve_api_key(&cfg), Some("direct".into()));
    }

    #[test]
    fn resolve_api_key_from_env() {
        std::env::set_var("TEST_API_KEY_8B1", "sekrecik");
        let mut cfg = HashMap::new();
        cfg.insert("api_key_env".into(), "TEST_API_KEY_8B1".into());
        assert_eq!(resolve_api_key(&cfg), Some("sekrecik".into()));
        std::env::remove_var("TEST_API_KEY_8B1");
    }

    #[test]
    fn resolve_api_key_missing_returns_none() {
        let cfg = HashMap::new();
        assert_eq!(resolve_api_key(&cfg), None);
    }

    #[test]
    fn pick_service_single_candidate() {
        let svc = fixture_entry(Transport::HttpDirect, 100);
        let candidates = vec![&svc];
        assert_eq!(pick_service(&candidates).unwrap().id, 1);
    }

    #[test]
    fn pick_service_empty_returns_none() {
        let candidates: Vec<&ServiceEntry> = vec![];
        assert!(pick_service(&candidates).is_none());
    }

    #[test]
    fn pick_service_weighted_distribution() {
        let mut a = fixture_entry(Transport::HttpDirect, 30);
        a.id = 1;
        let mut b = fixture_entry(Transport::HttpDirect, 70);
        b.id = 2;
        let candidates = vec![&a, &b];
        let mut count_a = 0;
        let mut count_b = 0;
        for _ in 0..2_000 {
            match pick_service(&candidates).unwrap().id {
                1 => count_a += 1,
                2 => count_b += 1,
                _ => unreachable!(),
            }
        }
        let ratio_a = count_a as f64 / 2_000.0;
        // Expected 30% with ±5pp tolerance — large N keeps flake risk negligible.
        assert!(
            (ratio_a - 0.30).abs() < 0.05,
            "ratio_a = {} (count_a={}, count_b={})",
            ratio_a,
            count_a,
            count_b
        );
    }

    // ---- N7.3 dispatch helpers ----------------------------------------------

    #[test]
    fn ensure_local_embedded_accepts_local_node() {
        let h = BackendHandle::Embedded {
            model_name: "qwen-tiny".into(),
            node_id: "local".into(),
            engine_id: "test-engine".into(),
        };
        let model = ensure_local_embedded(&h, "local").unwrap();
        assert_eq!(model, "qwen-tiny");
    }

    #[test]
    fn ensure_local_embedded_rejects_remote_node() {
        let h = BackendHandle::Embedded {
            model_name: "qwen-tiny".into(),
            node_id: "peerB".into(),
            engine_id: "test-engine".into(),
        };
        let err = ensure_local_embedded(&h, "local").unwrap_err();
        assert!(matches!(err, TransportClientError::EmbeddedRemote(ref n) if n == "peerB"));
    }

    #[test]
    fn ensure_local_embedded_rejects_non_embedded() {
        let mut svc = fixture_entry(Transport::HttpDirect, 100);
        svc.extra_config
            .insert("api_key".into(), "sk-test".into());
        let backend = entry_to_service_backend(&svc).unwrap();
        let client = BackendClient::new(backend, None).expect("client");
        let h = BackendHandle::Http(Arc::new(client));
        let err = ensure_local_embedded(&h, "local").unwrap_err();
        assert!(matches!(err, TransportClientError::UnsupportedTransport(_)));
    }

    #[test]
    fn resolve_http_client_returns_arc_for_http_handle() {
        let mut svc = fixture_entry(Transport::HttpDirect, 100);
        svc.extra_config
            .insert("api_key".into(), "sk-test".into());
        let backend = entry_to_service_backend(&svc).unwrap();
        let client = BackendClient::new(backend, None).expect("client");
        let h = BackendHandle::Http(Arc::new(client));
        let resolved = resolve_http_client(&h, "test-model").unwrap();
        // The Arc strong-count is at least 2 here (the original + the resolved
        // clone), proving the helper handed back a usable reference.
        assert!(Arc::strong_count(&resolved.client) >= 2);
    }

    #[test]
    fn resolve_http_client_rejects_quic_handle() {
        let qcfg = crate::net::quic::QuicConfig {
            name: "test".into(),
            url: "iroh://aaaa".into(),
            tls_ca: None,
            server_name: None,
            alpn: "tentaflow-service/v1".into(),
            timeout_ms: 1_000,
            auto_reconnect: false,
            reconnect_interval_ms: 500,
            keepalive_interval_ms: 5_000,
            skip_tls_verify: true,
            direct_addrs: Vec::new(),
        };
        let qh = Arc::new(crate::services::runtime::quic_handle::QuicServiceHandle::new(
            qcfg,
        ));
        let h = BackendHandle::Quic(qh);
        match resolve_http_client(&h, "test") {
            Err(TransportClientError::UnsupportedTransport(_)) => {}
            Err(other) => panic!("expected UnsupportedTransport, got {:?}", other),
            Ok(_) => panic!("expected error for QUIC handle on HTTP path"),
        }
    }

    #[tokio::test]
    async fn resolve_quic_client_reports_not_connected_when_loop_idle() {
        // Build a QUIC handle with no reconnect loop attached — `get_client()`
        // returns None, so the helper must produce `QuicNotConnected`.
        let qcfg = crate::net::quic::QuicConfig {
            name: "test".into(),
            url: "iroh://aaaa".into(),
            tls_ca: None,
            server_name: None,
            alpn: "tentaflow-service/v1".into(),
            timeout_ms: 1_000,
            auto_reconnect: false,
            reconnect_interval_ms: 500,
            keepalive_interval_ms: 5_000,
            skip_tls_verify: true,
            direct_addrs: Vec::new(),
        };
        let qh = Arc::new(crate::services::runtime::quic_handle::QuicServiceHandle::new(
            qcfg,
        ));
        let h = BackendHandle::Quic(qh);
        match resolve_quic_client(&h, "test-model").await {
            Err(TransportClientError::QuicNotConnected(ref m)) if m == "test-model" => {}
            Err(other) => panic!("expected QuicNotConnected(\"test-model\"), got {:?}", other),
            Ok(_) => panic!("expected error when reconnect loop idle"),
        }
    }
}
