// =============================================================================
// Plik: flow_engine/dispatcher.rs
// Opis: Decyduje czy request powinien isc przez Flow Engine czy stary pipeline.
//       Sprawdza feature flag, resolwuje flow, uruchamia executor.
// =============================================================================

use crate::config::RouterConfig;
use crate::db::{repository, DbPool};
use crate::flow_engine::adapters::conversation_history::ConversationHistoryAdapter;
use crate::flow_engine::adapters::embeddings::EmbeddingsNodeAdapter;
use crate::flow_engine::adapters::llm::LlmNodeAdapter;
use crate::flow_engine::adapters::memory::MemoryNodeAdapter;
use crate::flow_engine::adapters::rag::RagNodeAdapter;
use crate::flow_engine::adapters::session_context::SessionContextAdapter;
use crate::flow_engine::adapters::speaker_context::SpeakerContextAdapter;
use crate::flow_engine::adapters::stt::SttNodeAdapter;
use crate::flow_engine::adapters::tts::TtsNodeAdapter;
use crate::flow_engine::adapters::AdapterRegistry;
use crate::flow_engine::cache::FlowCache;
use crate::flow_engine::executor_async::FlowExecutorAsync;
use crate::flow_engine::resolver;
use crate::flow_engine::types::{FlowContext, FlowExecutionResult};
use crate::routing::service_manager::ServiceManager;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::warn;

const FLOW_TIMEOUT_SECS: u64 = 120;
const ENABLED_CACHE_TTL_SECS: u64 = 5;

/// Dispatcher flow engine - brama wejsciowa do systemu flow
pub struct FlowDispatcher {
    db: DbPool,
    cache: FlowCache,
    registry: Arc<AdapterRegistry>,
    enabled_cache: AtomicBool,
    enabled_last_check: std::sync::Mutex<std::time::Instant>,
}

impl FlowDispatcher {
    pub fn new(
        db: DbPool,
        service_manager: Arc<ServiceManager>,
        config: Arc<RouterConfig>,
    ) -> Self {
        let mut registry = AdapterRegistry::new();
        registry.register(LlmNodeAdapter::new(service_manager.clone(), config.clone()));
        registry.register(RagNodeAdapter::new(service_manager.clone(), config.clone()));
        registry.register(SttNodeAdapter::new(service_manager.clone(), config.clone()));
        registry.register(TtsNodeAdapter::new(service_manager.clone(), config.clone()));
        registry.register(EmbeddingsNodeAdapter::new(
            service_manager.clone(),
            config.clone(),
        ));
        registry.register(MemoryNodeAdapter::new(
            service_manager.clone(),
            config.clone(),
        ));
        registry.register(ConversationHistoryAdapter::new(
            service_manager.clone(),
            config.clone(),
        ));
        registry.register(SessionContextAdapter::new(
            service_manager.clone(),
            config.clone(),
        ));
        registry.register(SpeakerContextAdapter::new(service_manager, config));

        Self {
            db,
            cache: FlowCache::new(60),
            registry: Arc::new(registry),
            enabled_cache: AtomicBool::new(false),
            enabled_last_check: std::sync::Mutex::new(
                std::time::Instant::now()
                    - std::time::Duration::from_secs(ENABLED_CACHE_TTL_SECS + 1),
            ),
        }
    }

    /// Sprawdza czy flow engine jest wlaczony (setting w DB, cache na 5s)
    async fn is_enabled(&self) -> bool {
        let should_refresh = {
            if let Ok(last_check) = self.enabled_last_check.lock() {
                last_check.elapsed().as_secs() >= ENABLED_CACHE_TTL_SECS
            } else {
                true
            }
        };

        if should_refresh {
            let db_clone = self.db.clone();
            let result = tokio::task::spawn_blocking(move || {
                repository::get_setting(&db_clone, "flow_engine_enabled")
            })
            .await;

            match result {
                Ok(Ok(value)) => {
                    let enabled = value.as_deref() == Some("true");
                    self.enabled_cache.store(enabled, Ordering::Relaxed);
                    if let Ok(mut last_check) = self.enabled_last_check.lock() {
                        *last_check = std::time::Instant::now();
                    }
                }
                Ok(Err(e)) => {
                    warn!("Blad odczytu ustawienia flow_engine_enabled z DB: {}", e);
                }
                Err(e) => {
                    warn!(
                        "Blad spawn_blocking przy sprawdzaniu flow_engine_enabled: {}",
                        e
                    );
                }
            }
        }

        self.enabled_cache.load(Ordering::Relaxed)
    }

    /// Probuje znalezc i wykonac flow dla danego modelu/service_type.
    /// Zwraca None jesli flow engine wylaczony lub brak flow (fallback na stary pipeline).
    pub async fn try_dispatch(
        &self,
        model_name: &str,
        service_type: &str,
        mut ctx: FlowContext,
    ) -> Result<Option<FlowExecutionResult>> {
        if !self.is_enabled().await {
            return Ok(None);
        }

        let cache_key = format!("{}:{}", model_name, service_type);

        let flow = match self.cache.get(&cache_key) {
            Some(Some(cached_flow)) => cached_flow,
            Some(None) => return Ok(None),
            None => {
                let db_clone = self.db.clone();
                let model_owned = model_name.to_string();
                let svc_owned = service_type.to_string();
                let resolved = tokio::task::spawn_blocking(move || {
                    resolver::resolve_flow(&db_clone, &model_owned, &svc_owned)
                })
                .await??;
                match resolved {
                    Some(f) => {
                        self.cache.set(&cache_key, Some(f.clone()));
                        f
                    }
                    None => {
                        self.cache.set(&cache_key, None);
                        return Ok(None);
                    }
                }
            }
        };

        let flow_id = flow.id;

        // ACL — flow ma resource_type='flow', resource_id=flow.id (string).
        // Skipujemy gdy ctx nie ma user_id (internal caller).
        if let Some(uid) = ctx.user_id {
            let role = ctx.user_role.clone().unwrap_or_else(|| "user".to_string());
            if !crate::routing::acl::check_access_safe(
                &self.db,
                "flow",
                &flow_id.to_string(),
                uid,
                &role,
            ) {
                tracing::warn!(user_id = uid, flow_id, "ACL denied flow execution");
                // Skipujemy flow → fallback na stary pipeline (zachowanie identyczne
                // jak gdy flow nie istnieje — user moze uzyc bezposredniego routingu).
                return Ok(None);
            }
        }

        let executor = FlowExecutorAsync::new(self.db.clone(), self.registry.clone());
        match timeout(
            Duration::from_secs(FLOW_TIMEOUT_SECS),
            executor.execute(&flow, &mut ctx),
        )
        .await
        {
            Ok(Ok(result)) => Ok(Some(result)),
            Ok(Err(e)) => {
                warn!(
                    "Blad wykonania flow {}: {}. Fallback na stary pipeline.",
                    flow_id, e
                );
                Ok(None)
            }
            Err(_) => {
                warn!(
                    "Timeout flow {} po {}s. Fallback na stary pipeline.",
                    flow_id, FLOW_TIMEOUT_SECS
                );
                Ok(None)
            }
        }
    }

    /// Inwaliduj cache (wywoływane po zmianach w flow/bindings przez dashboard)
    pub fn invalidate_cache(&self) {
        self.cache.invalidate_all();
    }
}
