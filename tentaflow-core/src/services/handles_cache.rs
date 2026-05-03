// ============ File: services/handles_cache.rs — Live runtime handles derived from snapshot ============
//
// Skeleton wprowadzony w kroku N7.1c. Ten moduł trzyma jeden lock-free DashMap
// `(node_id, service_id) -> BackendHandle`, ktory jest derived state z
// `ServicesSnapshot`. Lifecycle (insert/drop diff) bedzie spawnowany przez
// supervisor w kroku N7.2; routing call sites migruja na `get_for_model`
// w kroku N7.3. W N7.1 nikt jeszcze nie inserts/lookupuje — pusty cache.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use tentaflow_protocol::ServiceInfo;
use tokio::task::JoinHandle;

use crate::config::ConnectionType;
use crate::services::backend::client::BackendClient;
use crate::services::runtime::CircuitBreakerConfig;
use crate::services::runtime::quic_handle::{QuicServiceHandle, QuicServiceState};
use crate::services::mesh_registry::MeshServicesRegistry;
use crate::services::transport::Transport;

/// Live runtime handle dla pojedynczej (node_id, service_id) pary. Jednolity
/// enum nad trzema kanalami transportu uzywanymi przez routing path.
#[derive(Clone)]
pub enum BackendHandle {
    /// HTTP backend (vLLM, ollama, OpenAI-compatible). Stan trzyma
    /// `BackendClient` (circuit breaker + reqwest pool).
    Http(Arc<BackendClient>),
    /// QUIC sidecar backend (Python bundles za rust sidecarem). Stan
    /// (connecting/connected/disconnected) trzyma `QuicServiceHandle`;
    /// reconnect loop spawnowany przez supervisor (krok N7.2).
    Quic(Arc<QuicServiceHandle>),
    /// In-process embedded engine (llama.cpp, MLX, sherpa-onnx itp.). Brak
    /// kanalu sieciowego — dispatch idzie bezposrednio do silnika
    /// po `model_name` (LLM/embeddings → `LocalInferenceManager`,
    /// STT → `SttManager`) lub po `engine_id` (TTS → `TtsManager` keys
    /// engines pod manifestowym `engine.id`, np. "apple-tts").
    Embedded {
        model_name: String,
        node_id: String,
        engine_id: String,
    },
}

impl BackendHandle {
    /// Czy handle jest gotowy do obsługi requestu w tej chwili.
    /// HTTP/Embedded są zawsze "alive" (stan polaczenia trzymamy w Quic).
    /// Dla QUIC sprawdzamy czy `client` jest ustawiony przez reconnect loop.
    pub fn is_alive(&self) -> bool {
        match self {
            BackendHandle::Http(_) => true,
            BackendHandle::Embedded { .. } => true,
            BackendHandle::Quic(handle) => {
                // `client` siedzi pod async RwLock; tu tylko fast-path. Kiedy
                // reconnect loop jeszcze nie ustawil clienta, traktujemy jako
                // not-alive bez blokowania na await.
                handle
                    .client
                    .try_read()
                    .map(|c| c.is_some())
                    .unwrap_or(false)
            }
        }
    }

    /// Sygnal shutdown. Dla QUIC propaguje do reconnect loop'u; dla pozostalych
    /// jest no-op (HTTP nie ma loop'u, embedded zatrzymuje sie wraz z procesem).
    pub fn shutdown(&self) {
        if let BackendHandle::Quic(handle) = self {
            handle.shutdown();
        }
    }
}

/// Lock-free cache live handlow keyed by `(node_id, service_id)`. Lokalne i
/// zdalne serwisy zywą w jednej mapie — supervisor rozne nie rozdziela
/// klucza, decyduje wlasciciel snapshotu.
pub struct LiveHandlesCache {
    handles: Arc<DashMap<(String, i64), BackendHandle>>,
}

impl Default for LiveHandlesCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveHandlesCache {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(DashMap::new()),
        }
    }

    pub fn insert(&self, node_id: String, service_id: i64, handle: BackendHandle) {
        if let Some(prev) = self.handles.insert((node_id, service_id), handle) {
            // Stara wersja (np. po endpoint flip) powinna zostac zatrzymana
            // zanim ten Arc upadnie — tu mamy ostatni moment zeby wyslac
            // shutdown signal do QUIC reconnect loop'u poprzedniego handle.
            prev.shutdown();
        }
    }

    pub fn upsert_service_info(&self, svc: &ServiceInfo) -> Result<()> {
        let handle = build_handle(svc)?;
        let quic_inner = match &handle {
            BackendHandle::Quic(h) => Some(h.clone()),
            _ => None,
        };
        self.insert(svc.node_id.clone(), svc.id, handle);
        if let Some(qh) = quic_inner {
            spawn_quic_reconnect_loop(qh);
        }
        Ok(())
    }

    pub fn get(&self, node_id: &str, service_id: i64) -> Option<BackendHandle> {
        self.handles
            .get(&(node_id.to_string(), service_id))
            .map(|e| e.value().clone())
    }

    pub fn remove(&self, node_id: &str, service_id: i64) -> Option<BackendHandle> {
        self.handles
            .remove(&(node_id.to_string(), service_id))
            .map(|(_, v)| v)
    }

    /// Find handle for `model_name` across local + remote nodes by walking
    /// `registry.unique_models()` first match. Returns `None` jezeli model
    /// nie wystepuje w zadnym snapshocie albo handle jeszcze nie zostal
    /// zarejestrowany w cache (race window startup-time).
    pub fn get_for_model(
        &self,
        model_name: &str,
        registry: &MeshServicesRegistry,
    ) -> Option<BackendHandle> {
        let owner = registry.find_node_for_model(model_name)?;
        // Per (node_id, service_id) — szukamy w services tego node'a service
        // ktory eksponuje ten model_name.
        let svc_id = registry
            .visible_services()
            .into_iter()
            .find(|s| s.node_id == owner && s.models.iter().any(|m| m.model_name == model_name))
            .map(|s| s.id)?;
        self.get(&owner, svc_id)
    }

    /// Wszystkie aktualne klucze cache. Uzywane przez supervisor w kroku N7.2
    /// do diff'a snapshot'u (insert nowe, remove znikniete).
    pub fn keys(&self) -> Vec<(String, i64)> {
        self.handles.iter().map(|e| e.key().clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

/// Buduje `BackendHandle` dla `ServiceInfo`. **Nie spawnuje reconnect loop'u**
/// dla QUIC — supervisor robi to osobnym taskiem w kroku N7.2 zaraz po
/// `insert`. Embedded nie wymaga zadnej infrastruktury sieciowej.
pub fn build_handle(svc: &ServiceInfo) -> Result<BackendHandle> {
    let transport = Transport::from_db_tag(&svc.transport)?;
    match transport {
        Transport::Embedded => {
            let model_name = svc
                .models
                .first()
                .map(|m| m.model_name.clone())
                .unwrap_or_default();
            Ok(BackendHandle::Embedded {
                model_name,
                node_id: svc.node_id.clone(),
                engine_id: svc.engine_id.clone(),
            })
        }
        Transport::HttpDirect | Transport::ExternalHttp => {
            let url = svc
                .endpoint_url
                .clone()
                .ok_or_else(|| anyhow!("endpoint_url missing for HTTP transport"))?;
            // HTTP backend bez wymaganego api_key — wkladamy pusty `api_key`
            // zeby `BackendClient::new` nie wybuchnal na lokalnym serwisie
            // (ollama / vllm dostepne anonymously). Realne sekrety dla
            // hosted endpointow przyjda z `extra_config` w pelnym scieprze
            // (krok N7.2 supervisor passowac bedzie te pola).
            let backend = crate::config::ServiceBackend {
                connection: ConnectionType::OpenAIApi {
                    url,
                    api_key: Some(String::new()),
                    api_key_env: None,
                    extra_headers: Vec::new(),
                    custom_endpoint: None,
                    request_format: None,
                    tts_config: None,
                },
                max_concurrent: 8,
                timeout_ms: 120_000,
                weight: 1,
                model_name_override: None,
                health_check_path: None,
            };
            let client = BackendClient::new(backend, None::<CircuitBreakerConfig>)
                .map_err(|e| anyhow!("BackendClient init failed: {}", e))?;
            Ok(BackendHandle::Http(Arc::new(client)))
        }
        Transport::SidecarQuic => {
            let port = svc
                .sidecar_quic_port
                .ok_or_else(|| anyhow!("sidecar_quic_port missing for SidecarQuic"))?;
            let quic_config = crate::net::quic::QuicConfig {
                name: svc.engine_id.clone(),
                url: format!("quic://127.0.0.1:{}", port),
                tls_ca: None,
                server_name: None,
                alpn: "tentaflow-service/v1".to_string(),
                timeout_ms: 120_000,
                auto_reconnect: true,
                reconnect_interval_ms: 1_000,
                keepalive_interval_ms: 5_000,
                skip_tls_verify: true,
                direct_addrs: Vec::new(),
            };
            Ok(BackendHandle::Quic(Arc::new(QuicServiceHandle::new(
                quic_config,
            ))))
        }
    }
}

/// Spawn a per-handle reconnect loop for a `QuicServiceHandle`. The supervisor
/// (krok N7.2) calls this immediately after `LiveHandlesCache::insert` for any
/// handle whose transport is `SidecarQuic`. The loop:
///   - attempts `QuicClient::connect` while the state is not `Connected`,
///   - flips the handle to `Connected` on success / `Disconnected` on failure,
///   - exits when `handle.shutdown()` flips the per-service watch channel.
/// The returned `JoinHandle` is owned by the supervisor's per-key task map so
/// it can be aborted on `disconnect_peer` / shutdown.
pub fn spawn_quic_reconnect_loop(handle: Arc<QuicServiceHandle>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let reconnect_interval =
            Duration::from_millis(handle.config.reconnect_interval_ms.max(500));
        let mut shutdown_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() {
                handle
                    .shutdown_client_and_mark_disconnected("shutdown")
                    .await;
                return;
            }

            // Snapshot the current state without holding the lock across awaits
            // that mutate it (set_connected/set_disconnected take the same
            // RwLock).
            let state = handle.state.read().await.clone();
            match state {
                QuicServiceState::ConfigError { .. } => {
                    // Permanent failure — supervisor must replace the handle
                    // explicitly via `cache.insert(...)` after fixing the
                    // config; the loop simply exits here.
                    return;
                }
                QuicServiceState::Connected => {
                    // Park until either a shutdown signal arrives or the
                    // connection is reported lost by an external caller (e.g.
                    // request path observed a closed stream and called
                    // `set_disconnected`).
                    tokio::select! {
                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                handle.shutdown_client_and_mark_disconnected("shutdown").await;
                                return;
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            if !handle.is_available().await {
                                continue;
                            }
                        }
                    }
                }
                QuicServiceState::Connecting | QuicServiceState::Disconnected { .. } => {
                    let cfg = handle.config.clone();
                    let inner_shutdown = shutdown_rx.clone();
                    match crate::net::quic::QuicClient::connect(cfg, inner_shutdown).await {
                        Ok(client) => {
                            handle.set_connected(Arc::new(client)).await;
                        }
                        Err(e) => {
                            handle
                                .set_disconnected(format!("connect failed: {}", e))
                                .await;
                            tokio::select! {
                                _ = shutdown_rx.changed() => {
                                    if *shutdown_rx.borrow() {
                                        return;
                                    }
                                }
                                _ = tokio::time::sleep(reconnect_interval) => {}
                            }
                        }
                    }
                }
            }
        }
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};

    fn embedded_svc(id: i64, node: &str, model: &str) -> ServiceInfo {
        ServiceInfo {
            id,
            node_id: node.to_string(),
            engine_id: "llama-cpp".to_string(),
            category: "llm".to_string(),
            display_name: model.to_string(),
            deploy_method: "native_embedded".to_string(),
            transport: "embedded".to_string(),
            status: "running".to_string(),
            pinned: false,
            paused: false,
            runtime_pid: None,
            runtime_port: None,
            sidecar_quic_port: None,
            endpoint_url: None,
            restart_count: 0,
            health_last_err: None,
            models: vec![ServiceModelEntry {
                model_name: model.to_string(),
                display_name: None,
                capabilities: Vec::new(),
                context_length: None,
                quantization: None,
                is_default: true,
            }],
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        }
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let cache = LiveHandlesCache::new();
        assert!(cache.is_empty());

        let h = BackendHandle::Embedded {
            model_name: "qwen3-0.8b".into(),
            node_id: "nodeA".into(), engine_id: "test-engine".into(),
        };
        cache.insert("nodeA".into(), 42, h);
        assert_eq!(cache.len(), 1);

        let got = cache.get("nodeA", 42).expect("present");
        assert!(matches!(got, BackendHandle::Embedded { .. }));

        let removed = cache.remove("nodeA", 42).expect("removed");
        assert!(matches!(removed, BackendHandle::Embedded { .. }));
        assert!(cache.is_empty());
    }

    #[test]
    fn keys_lists_all_pairs() {
        let cache = LiveHandlesCache::new();
        cache.insert(
            "nodeA".into(),
            1,
            BackendHandle::Embedded {
                model_name: "m1".into(),
                node_id: "nodeA".into(), engine_id: "test-engine".into(),
            },
        );
        cache.insert(
            "nodeB".into(),
            2,
            BackendHandle::Embedded {
                model_name: "m2".into(),
                node_id: "nodeB".into(), engine_id: "test-engine".into(),
            },
        );
        let mut keys = cache.keys();
        keys.sort();
        assert_eq!(keys, vec![("nodeA".into(), 1), ("nodeB".into(), 2)]);
    }

    #[test]
    fn get_for_model_resolves_via_registry() {
        let registry = MeshServicesRegistry::new();
        registry.replace_local("local".into(), vec![embedded_svc(7, "local", "qwen-0.8b")]);
        registry.replace_node(
            "remote".into(),
            vec![embedded_svc(11, "remote", "phi-3-mini")],
        );

        let cache = LiveHandlesCache::new();
        cache.insert(
            "local".into(),
            7,
            BackendHandle::Embedded {
                model_name: "qwen-0.8b".into(),
                node_id: "local".into(), engine_id: "test-engine".into(),
            },
        );
        cache.insert(
            "remote".into(),
            11,
            BackendHandle::Embedded {
                model_name: "phi-3-mini".into(),
                node_id: "remote".into(), engine_id: "test-engine".into(),
            },
        );

        let h_local = cache
            .get_for_model("qwen-0.8b", &registry)
            .expect("local present");
        assert!(matches!(
            h_local,
            BackendHandle::Embedded { ref node_id, .. } if node_id == "local"
        ));

        let h_remote = cache
            .get_for_model("phi-3-mini", &registry)
            .expect("remote present");
        assert!(matches!(
            h_remote,
            BackendHandle::Embedded { ref node_id, .. } if node_id == "remote"
        ));

        assert!(cache
            .get_for_model("nonexistent-model", &registry)
            .is_none());
    }

    #[tokio::test]
    async fn quic_reconnect_loop_shutdown_terminates_loop() {
        // Spawn a reconnect loop pointing at an obviously-unroutable endpoint.
        // The loop must not connect; calling `handle.shutdown()` must wake the
        // task and let it terminate within a reasonable budget.
        let cfg = crate::net::quic::QuicConfig {
            name: "test".to_string(),
            // Random fake endpoint id — connect will keep failing.
            url: "iroh://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            tls_ca: None,
            server_name: None,
            alpn: "tentaflow-service/v1".to_string(),
            timeout_ms: 1_000,
            auto_reconnect: true,
            reconnect_interval_ms: 200,
            keepalive_interval_ms: 5_000,
            skip_tls_verify: true,
            direct_addrs: Vec::new(),
        };
        let handle = Arc::new(QuicServiceHandle::new(cfg));
        let task = spawn_quic_reconnect_loop(handle.clone());

        // Let the loop attempt at least one connect cycle.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        handle.shutdown();

        // Wait at most 2 seconds for the task to wind down.
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
        assert!(
            res.is_ok(),
            "reconnect loop did not terminate after shutdown"
        );
    }

    #[test]
    fn build_handle_embedded_uses_first_model_name() {
        let svc = embedded_svc(1, "n", "qwen-tiny");
        let h = build_handle(&svc).expect("build embedded");
        match h {
            BackendHandle::Embedded {
                model_name,
                node_id,
                engine_id: _,
            } => {
                assert_eq!(model_name, "qwen-tiny");
                assert_eq!(node_id, "n");
            }
            _ => panic!("expected Embedded variant"),
        }
    }
}
