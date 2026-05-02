// =============================================================================
// Plik: flow_engine/dispatcher.rs
// Opis: Decyduje czy request powinien isc przez Flow Engine czy stary pipeline.
//       Sprawdza feature flag, resolwuje flow, uruchamia executor.
// =============================================================================

use crate::config::RouterConfig;
use crate::db::DbPool;
use crate::flow_engine::adapters::condition::ConditionNodeAdapter;
use crate::flow_engine::adapters::conversation_history::ConversationHistoryAdapter;
use crate::flow_engine::adapters::embeddings::EmbeddingsNodeAdapter;
use crate::flow_engine::adapters::llm::LlmNodeAdapter;
use crate::flow_engine::adapters::memory::MemoryNodeAdapter;
use crate::flow_engine::adapters::output::OutputNodeAdapter;
use crate::flow_engine::adapters::pii_filter::PiiFilterNodeAdapter;
use crate::flow_engine::adapters::rag::RagNodeAdapter;
use crate::flow_engine::adapters::session_context::SessionContextAdapter;
use crate::flow_engine::adapters::speaker_context::SpeakerContextAdapter;
use crate::flow_engine::adapters::stt::SttNodeAdapter;
use crate::flow_engine::adapters::trigger::TriggerNodeAdapter;
use crate::flow_engine::adapters::tts::TtsNodeAdapter;
use crate::flow_engine::adapters::tts_clean::TtsCleanNodeAdapter;
use crate::flow_engine::adapters::{AdapterChunkStream, AdapterRegistry};
use crate::flow_engine::cache::{CachedFlow, FlowCache};
use crate::flow_engine::executor_async::{FlowExecutorAsync, ParsedFlow};
use crate::flow_engine::resolver;
use crate::flow_engine::types::{FlowContext, FlowExecutionResult};
use crate::routing::service_manager::ServiceManager;
use anyhow::Result;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::warn;

const FLOW_TIMEOUT_SECS: u64 = 120;

/// Dispatcher flow engine - brama wejsciowa do systemu flow
pub struct FlowDispatcher {
    db: DbPool,
    cache: FlowCache,
    registry: Arc<AdapterRegistry>,
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
        registry.register(TriggerNodeAdapter::new());
        registry.register(OutputNodeAdapter::new());
        registry.register(ConditionNodeAdapter::new());
        registry.register(PiiFilterNodeAdapter::new(db.clone()));
        registry.register(TtsCleanNodeAdapter::new(db.clone()));

        Self {
            db,
            cache: FlowCache::new(60),
            registry: Arc::new(registry),
        }
    }

    /// Udostepnia AdapterRegistry — uzywane przez handlery do walidacji
    /// flow_json przed zapisem (porty krawedzi vs metadata adaptera).
    pub fn registry(&self) -> &Arc<AdapterRegistry> {
        &self.registry
    }

    /// Resolwuje flow z cache albo z DB. Przy cache miss parsuje flow_json
    /// raz i zapisuje gotowy `Arc<CachedFlow>` — chat completion nie placi
    /// re-parse + topological_sort per-request.
    async fn resolve_cached(
        &self,
        cache_key: &str,
        model_name: &str,
        service_type: &str,
    ) -> Result<Option<Arc<CachedFlow>>> {
        if let Some(opt) = self.cache.get(cache_key) {
            return Ok(opt);
        }
        let db_clone = self.db.clone();
        let model_owned = model_name.to_string();
        let svc_owned = service_type.to_string();
        let resolved = tokio::task::spawn_blocking(move || {
            resolver::resolve_flow(&db_clone, &model_owned, &svc_owned)
        })
        .await??;
        match resolved {
            Some(flow) => {
                let parsed = match ParsedFlow::parse(&flow.flow_json) {
                    Ok(p) => Arc::new(p),
                    Err(e) => {
                        warn!(flow_id = flow.id, "Niepoprawny flow_json: {}", e);
                        // Negatywny cache — niepoprawny flow nie ma sensu re-parsowac.
                        self.cache.set(cache_key, None);
                        return Ok(None);
                    }
                };
                let cached = Arc::new(CachedFlow { flow, parsed });
                self.cache.set(cache_key, Some(cached.clone()));
                Ok(Some(cached))
            }
            None => {
                self.cache.set(cache_key, None);
                Ok(None)
            }
        }
    }

    /// Probuje znalezc i wykonac flow dla danego modelu/service_type.
    /// Zwraca None jesli brak flow (fallback na bezposredni dispatch).
    pub async fn try_dispatch(
        &self,
        model_name: &str,
        service_type: &str,
        mut ctx: FlowContext,
    ) -> Result<Option<FlowExecutionResult>> {
        let cache_key = format!("{}:{}", model_name, service_type);

        let cached = match self
            .resolve_cached(&cache_key, model_name, service_type)
            .await?
        {
            Some(c) => c,
            None => return Ok(None),
        };

        let flow_id = cached.flow.id;

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
            executor.execute(&cached.flow, &cached.parsed, &mut ctx),
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

    /// Streaming wariant dispatch. Zwraca `Some(stream)` tylko gdy flow istnieje
    /// i definiuje edge `from_port="stream"` (czyli autor flow'u zdeklarowal
    /// streamowa sciezke). Inaczej `None` — caller uzywa blocking try_dispatch
    /// lub omija flow engine calkowicie.
    pub async fn try_dispatch_streaming(
        &self,
        model_name: &str,
        service_type: &str,
        mut ctx: FlowContext,
    ) -> Result<Option<AdapterChunkStream>> {
        let cache_key = format!("{}:{}", model_name, service_type);

        let cached = match self
            .resolve_cached(&cache_key, model_name, service_type)
            .await?
        {
            Some(c) => c,
            None => return Ok(None),
        };

        // Szybka inspekcja: czy flow zawiera edge from_port="stream"? Jesli nie —
        // blocking path zrobi robote i nie ma po co budowac streaming executor'a.
        // Inspekcja po pre-parsed strukturze unika ponownej deserializacji JSON.
        let has_stream_edge = cached
            .parsed
            .definition
            .edges
            .iter()
            .any(|e| e.from_port == "stream");
        if !has_stream_edge {
            return Ok(None);
        }

        let flow_id = cached.flow.id;

        if let Some(uid) = ctx.user_id {
            let role = ctx.user_role.clone().unwrap_or_else(|| "user".to_string());
            if !crate::routing::acl::check_access_safe(
                &self.db,
                "flow",
                &flow_id.to_string(),
                uid,
                &role,
            ) {
                tracing::warn!(
                    user_id = uid,
                    flow_id,
                    "ACL denied streaming flow execution"
                );
                return Ok(None);
            }
        }

        let executor = FlowExecutorAsync::new(self.db.clone(), self.registry.clone());
        match executor
            .execute_streaming_flow(&cached.flow, &cached.parsed, &mut ctx)
            .await
        {
            Ok(stream) => Ok(Some(stream)),
            Err(e) => {
                warn!(
                    "Blad streaming flow {}: {}. Fallback na blocking/stary pipeline.",
                    flow_id, e
                );
                Ok(None)
            }
        }
    }

    /// Inwaliduj cache (wywoływane po zmianach w flow/bindings przez dashboard)
    pub fn invalidate_cache(&self) {
        self.cache.invalidate_all();
    }

    /// Lista typow node'ow zarejestrowanych w AdapterRegistry. Uzywane przez
    /// snapshot test R0d (parytet z seed flow_node_templates) i przez
    /// walidacje flow_json przy zapisie.
    pub fn registered_node_types(&self) -> Vec<String> {
        self.registry
            .registered_types()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }
}

#[cfg(test)]
mod r0d_snapshot {
    use super::*;
    use crate::config::RouterConfig;
    use crate::db::seed;
    use crate::routing::service_manager::ServiceManager;
    use rusqlite::Connection;
    use std::collections::BTreeSet;

    /// R0d: typy node'ow zarejestrowane w AdapterRegistry musza odpowiadac
    /// dokladnie typom z seedowanych szablonow `flow_node_templates`.
    /// Rozjazd oznacza ze GUI pokazuje element ktorego executor nie umie
    /// wykonac, albo odwrotnie — adapter istnieje ale palette go nie eksponuje.
    #[test]
    fn registered_adapters_match_seeded_node_templates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        seed::seed_defaults(&conn).unwrap();
        let pool = std::sync::Arc::new(std::sync::Mutex::new(conn));

        let mut seeded: BTreeSet<String> = BTreeSet::new();
        {
            let conn = pool.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT node_type FROM flow_node_templates")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap();
            for r in rows {
                seeded.insert(r.unwrap());
            }
        }

        let config = std::sync::Arc::new(RouterConfig::default());
        let service_manager = std::sync::Arc::new(
            ServiceManager::new(config.clone(), None).expect("ServiceManager with empty config"),
        );
        let dispatcher = FlowDispatcher::new(pool, service_manager, config);
        let registered: BTreeSet<String> =
            dispatcher.registered_node_types().into_iter().collect();

        assert_eq!(
            seeded, registered,
            "flow_node_templates seed != AdapterRegistry typy.\nseed: {:?}\nregistry: {:?}",
            seeded, registered
        );
    }

    /// R0e: regression dla B1 — po fresh seed musi istniec domyslny aktywny
    /// flow z service_type='chat' (uzywany przez routing/chat.rs przy
    /// `try_dispatch(model, "chat", ctx)`). Wczesniej seed wpisywal 'llm'
    /// i resolver nigdy nie znajdowal default flow.
    #[test]
    fn seeded_default_flow_uses_chat_service_type() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        seed::seed_defaults(&conn).unwrap();

        let (name, status, is_default): (String, String, i64) = conn
            .query_row(
                "SELECT name, status, is_default FROM flows \
                 WHERE service_type = 'chat' AND is_default = 1 AND status = 'active'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("musi istniec domyslny aktywny flow dla service_type='chat'");

        assert_eq!(name, "Standardowy pipeline LLM");
        assert_eq!(status, "active");
        assert_eq!(is_default, 1);

        let llm_under_old_key: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flows WHERE service_type = 'llm'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            llm_under_old_key, 0,
            "stary klucz service_type='llm' nie powinien wiecej istniec po migracji 66"
        );
    }
}
