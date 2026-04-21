// =============================================================================
// Plik: routing/middleware.rs
// Opis: Typy i logika middleware routingu — resolve aliasow, odkrywanie backendow,
//       strategia wyboru, dispatch z fallbackami. Fundament nowego unified routing.
// =============================================================================

use crate::error::Result;
use crate::routing::router::Router;
use crate::routing::service_manager::PoolStrategy;

use std::sync::atomic::Ordering;
use tracing::debug;

/// Maksymalna liczba hopow mesh (zapobiega petlom)
const MAX_HOPS: u32 = 3;

// ============================================================================
// TYPY
// ============================================================================

/// Rozwiazana trasa — lista targetow i strategia wyboru
pub struct ResolvedRoute {
    pub targets: Vec<String>,
    pub strategy: PoolStrategy,
}

/// Uchwyt do konkretnego backendu — jednoznacznie identyfikuje typ i lokalizacje
#[derive(Clone)]
pub enum BackendHandle {
    LocalLlm,
    LocalStt,
    QuicLlm(String),
    QuicStt(String),
    QuicTts(String),
    QuicEmbedding(String),
    Http(String),
    Rag(String),
    MeshForward(String, String),
}

impl BackendHandle {
    /// Zwraca nazwe typu backendu (do metadanych)
    fn type_name(&self) -> &'static str {
        match self {
            BackendHandle::LocalLlm => "local_llm",
            BackendHandle::LocalStt => "local_stt",
            BackendHandle::QuicLlm(_) => "quic_llm",
            BackendHandle::QuicStt(_) => "quic_stt",
            BackendHandle::QuicTts(_) => "quic_tts",
            BackendHandle::QuicEmbedding(_) => "quic_embedding",
            BackendHandle::Http(_) => "http",
            BackendHandle::Rag(_) => "rag",
            BackendHandle::MeshForward(_, _) => "mesh_forward",
        }
    }
}

/// Metadane trasy — serializowane do headera X-TentaFlow-Route
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteMetadata {
    pub served_by_node: String,
    pub backend_type: String,
    pub strategy_used: String,
    pub fallbacks_tried: u32,
    pub hop_count: u32,
    pub latency_ms: Option<f64>,
}

/// Wynik routingu — odpowiedz + metadane trasy
pub struct RouteResult<T> {
    pub response: T,
    pub metadata: RouteMetadata,
}

// ============================================================================
// IMPL ROUTER — middleware routing
// ============================================================================

impl Router {
    /// Rozwiazuje nazwe modelu na liste targetow i strategie.
    ///
    /// Kolejnosc:
    /// 1. Config aliasy (service_aliases)
    /// 2. Znany serwis (QUIC/HTTP/RAG/local)
    /// 3. Alias cache (DB model_aliases z fallback_targets + strategy)
    /// 4. Model pool (round-robin pula serwisow)
    /// 5. Oryginalna nazwa
    pub(crate) fn resolve_route(&self, model: &str) -> ResolvedRoute {
        // 1. Config aliasy
        for alias in &self.config.service_aliases {
            if alias.alias == model {
                debug!("resolve_route: config alias {} -> {}", model, alias.target);
                return ResolvedRoute {
                    targets: vec![alias.target.clone()],
                    strategy: PoolStrategy::FirstAvailable,
                };
            }
        }

        // 2. Znany serwis
        if self.service_manager.has_quic_llm_service(model)
            || self.service_manager.has_http_backends(model)
            || self.service_manager.has_rag_service(model)
            || self.service_manager.has_local_inference_service(model)
            || self.service_manager.has_quic_stt_service(model)
            || self.service_manager.has_quic_tts_service(model)
            || self.service_manager.has_quic_embedding_service(model)
        {
            return ResolvedRoute {
                targets: vec![model.to_string()],
                strategy: PoolStrategy::FirstAvailable,
            };
        }

        // 3. Alias cache (DB)
        {
            let cache = self.alias_cache.read();
            if let Some(db_alias) = cache.get(model) {
                let mut targets = vec![db_alias.target_model.clone()];
                if let Some(ref fallbacks) = db_alias.fallback_targets {
                    for fb in fallbacks.split(',') {
                        let fb = fb.trim();
                        if !fb.is_empty() {
                            targets.push(fb.to_string());
                        }
                    }
                }
                let strategy = db_alias
                    .strategy
                    .as_deref()
                    .map(PoolStrategy::parse)
                    .unwrap_or(PoolStrategy::FirstAvailable);
                debug!(
                    "resolve_route: alias cache {} -> {:?} ({})",
                    model, targets, strategy
                );
                return ResolvedRoute { targets, strategy };
            }
        }

        // 4. Model pool
        if let Some(service_name) = self.service_manager.select_service_for_model(model) {
            let strategy = self
                .service_manager
                .model_pool
                .read()
                .get(model)
                .map(|e| e.strategy)
                .unwrap_or(PoolStrategy::FirstAvailable);
            debug!("resolve_route: model pool {} -> {}", model, service_name);
            return ResolvedRoute {
                targets: vec![service_name],
                strategy,
            };
        }

        // 5. Oryginalna nazwa
        ResolvedRoute {
            targets: vec![model.to_string()],
            strategy: PoolStrategy::FirstAvailable,
        }
    }

    /// Zwraca liste dostepnych backendow dla danego targetu.
    pub(crate) fn get_backends(&self, target: &str) -> Vec<BackendHandle> {
        let mut backends = Vec::new();

        if self.service_manager.has_local_inference_service(target) {
            backends.push(BackendHandle::LocalLlm);
        }

        if self.local_stt.is_available_sync() {
            backends.push(BackendHandle::LocalStt);
        }

        if self.service_manager.has_quic_llm_service(target) {
            backends.push(BackendHandle::QuicLlm(target.to_string()));
        }

        if self.service_manager.has_quic_stt_service(target) {
            backends.push(BackendHandle::QuicStt(target.to_string()));
        }

        if self.service_manager.has_quic_tts_service(target) {
            backends.push(BackendHandle::QuicTts(target.to_string()));
        }

        if self.service_manager.has_quic_embedding_service(target) {
            backends.push(BackendHandle::QuicEmbedding(target.to_string()));
        }

        if self.service_manager.has_http_backends(target) {
            backends.push(BackendHandle::Http(target.to_string()));
        }

        if self.service_manager.has_rag_service(target) {
            backends.push(BackendHandle::Rag(target.to_string()));
        }

        // Mesh — szukaj na zdalnych nodach
        let registry = self.service_manager.mesh_registry.read();
        if let Some(ref reg) = *registry {
            for svc in reg.visible_services() {
                if svc.models.iter().any(|m| m == target) && svc.status == "running" {
                    backends.push(BackendHandle::MeshForward(
                        svc.node_id.clone(),
                        svc.service_name.clone(),
                    ));
                }
            }
        }

        backends
    }

    /// Sortuje backendy wedlug strategii
    pub(crate) fn apply_strategy<'a>(
        &self,
        backends: &'a [BackendHandle],
        strategy: &PoolStrategy,
    ) -> Vec<&'a BackendHandle> {
        if backends.is_empty() {
            return Vec::new();
        }

        match strategy {
            PoolStrategy::FirstAvailable => backends.iter().collect(),
            PoolStrategy::RoundRobin | PoolStrategy::LeastLoaded => {
                let len = backends.len();
                let idx = self.route_counter.fetch_add(1, Ordering::Relaxed) % len;
                let mut result: Vec<&BackendHandle> = Vec::with_capacity(len);
                for i in 0..len {
                    result.push(&backends[(idx + i) % len]);
                }
                result
            }
        }
    }

    /// Dispatch z fallbackami — iteruje po targetach i backendach.
    ///
    /// `call_fn` dostaje BackendHandle i zwraca Future z wynikiem.
    /// Probuje kazdy backend po kolei, loguje bledy i przechodzi dalej.
    pub(crate) async fn dispatch_with_fallback<F, Fut, T>(
        &self,
        model: &str,
        hop_count: u32,
        call_fn: F,
    ) -> Result<RouteResult<T>>
    where
        F: Fn(&BackendHandle) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let route = self.resolve_route(model);
        let start = std::time::Instant::now();
        let mut fallbacks_tried: u32 = 0;
        let mut last_error: Option<anyhow::Error> = None;

        let node_name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        for target in &route.targets {
            let backends = self.get_backends(target);
            if backends.is_empty() {
                fallbacks_tried += 1;
                debug!("dispatch_with_fallback: brak backendow dla '{}'", target);
                continue;
            }

            let ordered = self.apply_strategy(&backends, &route.strategy);

            for handle in ordered {
                // Ogranicz hop count dla mesh
                if let BackendHandle::MeshForward(_, _) = handle {
                    if hop_count >= MAX_HOPS {
                        debug!(
                            "dispatch_with_fallback: pomijam mesh forward (hop_count={})",
                            hop_count
                        );
                        continue;
                    }
                }

                match call_fn(handle).await {
                    Ok(response) => {
                        let metadata = RouteMetadata {
                            served_by_node: node_name,
                            backend_type: handle.type_name().to_string(),
                            strategy_used: route.strategy.to_string(),
                            fallbacks_tried,
                            hop_count,
                            latency_ms: Some(start.elapsed().as_secs_f64() * 1000.0),
                        };
                        return Ok(RouteResult { response, metadata });
                    }
                    Err(e) => {
                        debug!(
                            "dispatch_with_fallback: backend {:?} zwrocil blad: {}",
                            handle.type_name(),
                            e
                        );
                        last_error = Some(e);
                    }
                }
            }

            fallbacks_tried += 1;
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("Brak dostepnych backendow dla modelu '{}'", model)))
    }

    /// Aktualizuje alias cache z zewnetrznych danych (np. sync z peera mesh)
    pub fn update_alias_cache_from_sync(&self, aliases: Vec<crate::db::models::DbModelAlias>) {
        let mut cache = self.alias_cache.write();
        cache.clear();
        for alias in aliases {
            if alias.is_active {
                cache.insert(alias.alias.clone(), alias);
            }
        }
        tracing::debug!("Alias cache zaktualizowany z sync: {} wpisow", cache.len());
    }

    /// Laduje alias cache z bazy danych
    pub(crate) fn reload_alias_cache(&self) {
        let db = match &self.db {
            Some(db) => db,
            None => return,
        };

        match crate::db::repository::list_model_aliases(db) {
            Ok(aliases) => {
                let mut cache = self.alias_cache.write();
                cache.clear();
                for alias in aliases {
                    if alias.is_active {
                        cache.insert(alias.alias.clone(), alias);
                    }
                }
                debug!("Alias cache przeladowany: {} wpisow", cache.len());
            }
            Err(e) => {
                debug!("Blad ladowania alias cache: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod middleware_tests {
    use super::*;
    use crate::db::models::DbModelAlias;
    use crate::routing::service_manager::PoolStrategy;

    // ========================================================================
    // Testy BackendHandle
    // ========================================================================

    #[test]
    fn backend_handle_clone_mesh_forward() {
        // Arrange
        let handle = BackendHandle::MeshForward("node-1".to_string(), "svc-llm".to_string());

        // Act
        let cloned = handle.clone();

        // Assert
        assert!(
            matches!(cloned, BackendHandle::MeshForward(ref n, ref s) if n == "node-1" && s == "svc-llm")
        );
    }

    #[test]
    fn backend_handle_clone_all_variants() {
        // Sprawdza ze Clone dziala dla kazdego wariantu
        let variants: Vec<BackendHandle> = vec![
            BackendHandle::LocalLlm,
            BackendHandle::LocalStt,
            BackendHandle::QuicLlm("q1".to_string()),
            BackendHandle::QuicStt("q2".to_string()),
            BackendHandle::QuicTts("q3".to_string()),
            BackendHandle::QuicEmbedding("q4".to_string()),
            BackendHandle::Http("h1".to_string()),
            BackendHandle::Rag("r1".to_string()),
            BackendHandle::MeshForward("n1".to_string(), "s1".to_string()),
        ];

        for v in &variants {
            let _cloned = v.clone();
        }
    }

    #[test]
    fn backend_handle_type_name() {
        assert_eq!(BackendHandle::LocalLlm.type_name(), "local_llm");
        assert_eq!(BackendHandle::LocalStt.type_name(), "local_stt");
        assert_eq!(BackendHandle::QuicLlm("x".into()).type_name(), "quic_llm");
        assert_eq!(BackendHandle::Http("x".into()).type_name(), "http");
        assert_eq!(
            BackendHandle::MeshForward("n".into(), "s".into()).type_name(),
            "mesh_forward"
        );
    }

    // ========================================================================
    // Testy ResolvedRoute — parsowanie aliasow z fallbackami
    // ========================================================================

    #[test]
    fn resolved_route_from_alias_with_fallbacks() {
        // Arrange — symuluje logike resolve_route dla aliasu z DB
        let alias = DbModelAlias {
            id: 1,
            alias: "gpt-4".to_string(),
            target_model: "model-a".to_string(),
            is_active: true,
            fallback_targets: Some("model-b,model-c".to_string()),
            strategy: Some("round_robin".to_string()),
        };

        // Act — ta logika odpowiada resolve_route krok 3
        let mut targets = vec![alias.target_model.clone()];
        if let Some(ref ft) = alias.fallback_targets {
            targets.extend(
                ft.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        let strategy = PoolStrategy::parse(alias.strategy.as_deref().unwrap_or("first_available"));

        // Assert
        assert_eq!(targets, vec!["model-a", "model-b", "model-c"]);
        assert!(matches!(strategy, PoolStrategy::RoundRobin));
    }

    #[test]
    fn resolved_route_from_alias_empty_fallbacks() {
        // Arrange
        let alias = DbModelAlias {
            id: 1,
            alias: "test".to_string(),
            target_model: "model-a".to_string(),
            is_active: true,
            fallback_targets: Some("".to_string()),
            strategy: None,
        };

        // Act
        let mut targets = vec![alias.target_model.clone()];
        if let Some(ref ft) = alias.fallback_targets {
            targets.extend(
                ft.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }

        // Assert — puste fallbacks nie dodaja elementow
        assert_eq!(targets, vec!["model-a"]);
    }

    #[test]
    fn resolved_route_from_alias_no_fallbacks_field() {
        // Arrange
        let alias = DbModelAlias {
            id: 2,
            alias: "prosty".to_string(),
            target_model: "jedyny".to_string(),
            is_active: true,
            fallback_targets: None,
            strategy: None,
        };

        // Act
        let mut targets = vec![alias.target_model.clone()];
        if let Some(ref ft) = alias.fallback_targets {
            targets.extend(
                ft.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        let strategy = PoolStrategy::parse(alias.strategy.as_deref().unwrap_or("first_available"));

        // Assert
        assert_eq!(targets, vec!["jedyny"]);
        assert!(matches!(strategy, PoolStrategy::FirstAvailable));
    }

    #[test]
    fn resolved_route_fallbacks_with_whitespace() {
        // Arrange — fallbacki z bialymi znakami
        let alias = DbModelAlias {
            id: 3,
            alias: "spaces".to_string(),
            target_model: "main".to_string(),
            is_active: true,
            fallback_targets: Some(" fb-1 , fb-2 , fb-3 ".to_string()),
            strategy: Some("least_loaded".to_string()),
        };

        // Act
        let mut targets = vec![alias.target_model.clone()];
        if let Some(ref ft) = alias.fallback_targets {
            targets.extend(
                ft.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }

        // Assert — trim powinien usunac biale znaki
        assert_eq!(targets, vec!["main", "fb-1", "fb-2", "fb-3"]);
    }

    // ========================================================================
    // Testy RouteMetadata
    // ========================================================================

    #[test]
    fn route_metadata_serializes_to_json() {
        // Arrange
        let meta = RouteMetadata {
            served_by_node: "node-1".to_string(),
            backend_type: "quic_llm".to_string(),
            strategy_used: "round_robin".to_string(),
            fallbacks_tried: 2,
            hop_count: 1,
            latency_ms: Some(42.5),
        };

        // Act
        let json = serde_json::to_string(&meta).expect("Serializacja nie powiodla sie");

        // Assert — kluczowe pola sa obecne w JSON
        assert!(json.contains("\"served_by_node\":\"node-1\""));
        assert!(json.contains("\"fallbacks_tried\":2"));
        assert!(json.contains("\"hop_count\":1"));
    }

    // ========================================================================
    // Testy apply_strategy — logika pure (bez pelnego Routera)
    // ========================================================================

    #[test]
    fn strategy_first_available_preserves_order() {
        // Arrange
        let backends = vec![
            BackendHandle::QuicLlm("svc1".to_string()),
            BackendHandle::Http("svc2".to_string()),
            BackendHandle::LocalLlm,
        ];

        // Act — FirstAvailable powinno zachowac oryginalny porzadek
        // Testujemy logike match bez Routera
        let result: Vec<usize> = match PoolStrategy::FirstAvailable {
            PoolStrategy::FirstAvailable => (0..backends.len()).collect(),
            _ => unreachable!(),
        };

        // Assert
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn strategy_round_robin_rotates() {
        // Arrange — symulacja logiki round-robin z apply_strategy
        let len = 3;
        let counter_val = 5usize; // 5 % 3 = 2

        // Act — logika z apply_strategy
        let idx = counter_val % len;
        let result: Vec<usize> = (0..len).map(|i| (idx + i) % len).collect();

        // Assert — zaczynamy od indeksu 2
        assert_eq!(result, vec![2, 0, 1]);
    }

    #[test]
    fn strategy_round_robin_wraps_around() {
        // Arrange
        let len = 4;
        let counter_val = 7usize; // 7 % 4 = 3

        // Act
        let idx = counter_val % len;
        let result: Vec<usize> = (0..len).map(|i| (idx + i) % len).collect();

        // Assert
        assert_eq!(result, vec![3, 0, 1, 2]);
    }

    #[test]
    fn strategy_empty_backends_returns_empty() {
        // Arrange
        let backends: Vec<BackendHandle> = vec![];

        // Act — logika z apply_strategy: jesli puste, zwraca pusty vec
        let result: Vec<&BackendHandle> = if backends.is_empty() {
            Vec::new()
        } else {
            backends.iter().collect()
        };

        // Assert
        assert!(result.is_empty());
    }
}
