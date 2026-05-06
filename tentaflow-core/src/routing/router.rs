// =============================================================================
// Plik: routing/router.rs
// Opis: Glowna struktura Router, inicjalizacja, alias retentaflown, model/service
//       lookup, publiczne API diagnostyczne. Deleguje do podmodulow chat,
//       streaming, embeddings, tts, stt.
// =============================================================================

use crate::config::RouterConfig;
use crate::db::DbPool;
use crate::error::Result;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::middleware::ResponseMiddleware;
use crate::services::runtime::quic_handle::ServiceManager;

use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Router zarzadzajacy routing requestow do backendow.
///
/// Trzyma mape serwisow i backend clients (pre-alokowane).
/// Zunifikowana architektura obsluguje wszystkie typy serwisow: LLM, Embedding, Vision, STT, TTS.
///
/// **ARCHITEKTURA NON-BLOCKING:**
/// - Router::new() zwraca NATYCHMIAST (synchronicznie)
/// - Polaczenia QUIC uruchamiane w background taskach
/// - Auto-reconnect dziala w tle bez blokowania requestow
/// - Serwisy niedostepne zwracaja blad natychmiast
#[derive(Clone)]
pub struct Router {
    /// Service Manager - zarzadza wszystkimi serwisami asynchronicznie
    pub(crate) service_manager: Arc<ServiceManager>,

    /// Response middleware dla filtrowania PII
    pub(crate) response_middleware: Arc<ResponseMiddleware>,

    /// Flow Engine dispatcher - opcjonalny, aktywny gdy DB jest dostepna
    pub(crate) flow_dispatcher: Option<Arc<FlowDispatcher>>,

    /// Baza danych (do resolve aliasow modeli)
    pub(crate) db: Option<DbPool>,

    /// Mesh manager — do forwardowania requestow do zdalnych nodow
    pub(crate) mesh_manager:
        Arc<parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>>,

    /// Cache aliasow modeli z DB (alias -> CachedAlias). Wartosci maja
    /// pre-parsed `fallback_targets` (CLAUDE.md §9 — JSON), zeby parser
    /// odpalal sie raz przy reload zamiast per dispatch w hot path.
    pub(crate) alias_cache: Arc<
        parking_lot::RwLock<
            std::collections::HashMap<String, crate::routing::middleware::CachedAlias>,
        >,
    >,

    /// Phase 5 plumbing: read-only handle to the supervisor's services
    /// snapshot. Currently consulted as a fallback after legacy resolution
    /// misses; Phase 8 cleanup will make it the only source of truth.
    pub(crate) services_snapshot_rx: Arc<
        parking_lot::RwLock<
            Option<
                tokio::sync::watch::Receiver<Arc<crate::services::supervisor::ServicesSnapshot>>,
            >,
        >,
    >,

    /// Unified catalog of advertised models — one source of truth for
    /// `/v1/models`, mesh `catalog.list`, and the GUI. Rebuilt by
    /// `rebuild_catalog()` whenever services, aliases, or flow publish
    /// state changes; readers take a lock-free snapshot.
    pub(crate) catalog_provider: Arc<crate::services::catalog::CatalogProvider>,

    /// R1.5e: unified runtime executor. Owns alias resolver + strategy
    /// state; konsumowany przez nowe ścieżki (LlmAdapter cutover w R2a,
    /// chat handler cutover w R3a). Arc<RwLock<...>> bo Router derives
    /// Clone, a RwLock samo przez się nie jest Clone — Arc dzieli ten sam
    /// slot miedzy klonami zeby executor wspolny dla wszystkich call sites.
    pub(crate) executor: Arc<
        parking_lot::RwLock<
            Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>,
        >,
    >,

    /// R2d (Codex M1): SttRuntime wpiety przez `Router::start` po
    /// `Arc::new(router)` zeby trzymal `Weak<Router>` (anty-cykl).
    /// `None` w testach DB-less.
    pub(crate) stt_runtime:
        Arc<parking_lot::RwLock<Option<Arc<crate::services::stt::SttRuntime>>>>,
}

/// Wynik identyfikacji mowcy z poziomem pewnosci.
#[derive(Debug, Clone)]
pub struct SpeakerIdentifyResult {
    /// ID rozpoznanego mowcy (None jesli nieznany)
    pub speaker_id: Option<String>,
    /// Nazwa rozpoznanego mowcy (None jesli nieznany)
    pub speaker_name: Option<String>,
    /// Similarity score (0.0-1.0)
    pub similarity: Option<f32>,
    /// Poziom pewnosci: "HIGH", "MEDIUM", "LOW"
    pub confidence_level: String,
    /// Czy wymaga potwierdzenia od uzytkownika (true gdy MEDIUM)
    pub needs_confirmation: bool,
    /// Sugerowana wiadomosc potwierdzajaca (np. "Czy to ty, Jan?")
    pub confirmation_message: Option<String>,
}

impl SpeakerIdentifyResult {
    /// Zwraca wynik dla nieznanego mowcy
    pub fn unknown() -> Self {
        Self {
            speaker_id: None,
            speaker_name: None,
            similarity: None,
            confidence_level: "LOW".to_string(),
            needs_confirmation: false,
            confirmation_message: None,
        }
    }

    /// Czy mowca zostal rozpoznany z wysoka pewnoscia
    pub fn is_high_confidence(&self) -> bool {
        self.confidence_level == "HIGH" && self.speaker_id.is_some()
    }

    /// Czy mowca zostal rozpoznany ale wymaga potwierdzenia
    pub fn is_medium_confidence(&self) -> bool {
        self.confidence_level == "MEDIUM" && self.speaker_id.is_some()
    }

    /// Czy mowca jest nieznany
    pub fn is_unknown(&self) -> bool {
        self.confidence_level == "LOW" || self.speaker_id.is_none()
    }
}

/// Informacje o mowcy wykrytym przez diarization.
#[derive(Debug, Clone)]
pub struct DiarizedSpeaker {
    /// Etykieta mowcy (np. "SPEAKER_00" lub "Jan Kowalski" jesli znany)
    pub label: String,
    /// Czy mowca zostal rozpoznany z bazy glosow
    pub is_known: bool,
    /// Similarity score (0.0-1.0) jesli mowca znany
    pub similarity: Option<f32>,
    /// Tekst wypowiedziany przez tego mowce
    pub text: String,
}

/// Informacje o glosie zwrocone z audio_input processing.
/// Zawiera transkrypcje i informacje o rozpoznanym mowcy.
#[derive(Debug, Clone)]
pub struct VoiceInfo {
    /// Transkrybowany tekst z audio
    pub transcribed_text: String,
    /// ID rozpoznanego mowcy (None jesli nieznany)
    pub speaker_id: Option<String>,
    /// Nazwa rozpoznanego mowcy (None jesli nieznany)
    pub speaker_name: Option<String>,
    /// Poziom pewnosci rozpoznania (0.0-1.0)
    pub speaker_confidence: Option<f32>,
    /// Poziom pewnosci: "HIGH", "MEDIUM", "LOW"
    pub confidence_level: String,
    /// Czy wymaga potwierdzenia od uzytkownika
    pub needs_confirmation: bool,
    /// Sugerowana wiadomosc potwierdzajaca (np. "Czy to ty, Jan?")
    pub confirmation_message: Option<String>,
    /// Lista mowcow wykrytych przez diarization (jesli multi-speaker audio)
    pub diarized_speakers: Vec<DiarizedSpeaker>,
}

/// Wynik STT z diarization.
/// Zawiera tekst i liste mowcow wykrytych przez diarization.
#[derive(Debug, Clone)]
pub struct SttWithDiarization {
    /// Pelny transkrybowany tekst
    pub text: String,
    /// Lista mowcow z ich tekstami (jesli diarization wlaczone)
    pub speakers: Vec<DiarizedSpeaker>,
}

/// Metryki czasowe dla pojedynczego requestu.
/// Zbiera czasy kazdego etapu przetwarzania do wyswietlenia w logach.
#[derive(Debug, Clone, Default)]
pub struct RequestMetrics {
    /// Czas rozpoczecia requestu
    pub start_time: Option<std::time::Instant>,
    /// STT (Speech-to-Text)
    pub stt_ms: Option<u64>,
    /// Speaker identification
    pub speaker_id_ms: Option<u64>,
    /// Person context lookup
    pub person_context_ms: Option<u64>,
    /// Main LLM inference (bielik-11b)
    pub llm_inference_ms: Option<u64>,
    /// TTS (Text-to-Speech) - jesli wlaczone
    pub tts_ms: Option<u64>,
    /// Nazwa modelu glownego
    pub model_name: Option<String>,
}

impl RequestMetrics {
    pub fn new() -> Self {
        Self {
            start_time: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    /// Zwraca calkowity czas od rozpoczecia
    pub fn total_ms(&self) -> u64 {
        self.start_time
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// Formatuje tabelke z czasami do logowania
    pub fn format_table(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("┌─────────────────────────────────────┐"));
        lines.push(format!(
            "│ REQUEST TIMING {:>20} │",
            self.model_name.as_deref().unwrap_or("-")
        ));
        lines.push(format!("├─────────────────────────────────────┤"));

        if let Some(ms) = self.stt_ms {
            lines.push(format!("│ STT              {:>10} ms     │", ms));
        }
        if let Some(ms) = self.speaker_id_ms {
            lines.push(format!("│ Speaker ID       {:>10} ms     │", ms));
        }
        if let Some(ms) = self.person_context_ms {
            lines.push(format!("│ Person Context   {:>10} ms     │", ms));
        }
        if let Some(ms) = self.llm_inference_ms {
            lines.push(format!("│ LLM Inference    {:>10} ms     │", ms));
        }
        if let Some(ms) = self.tts_ms {
            lines.push(format!("│ TTS              {:>10} ms     │", ms));
        }

        lines.push(format!("├─────────────────────────────────────┤"));
        lines.push(format!(
            "│ TOTAL            {:>10} ms     │",
            self.total_ms()
        ));
        lines.push(format!("└─────────────────────────────────────┘"));

        lines.join("\n")
    }
}

/// Metryki pojedynczego backendu
#[derive(Debug, Clone)]
pub struct BackendMetric {
    pub is_healthy: bool,
    pub active_requests: u64,
}

/// Metryki calego routera
#[derive(Debug, Clone)]
pub struct RouterMetrics {
    pub backends: HashMap<String, Vec<BackendMetric>>,
    pub total_requests: u64,
    pub active_connections: u64,
}

impl Router {
    /// Tworzy nowy router na podstawie konfiguracji.
    ///
    /// **ARCHITEKTURA NON-BLOCKING:**
    /// - Zwraca NATYCHMIAST (nie czeka na polaczenia QUIC)
    /// - Polaczenia QUIC (Embeddings) sa uruchamiane w background taskach
    /// - Auto-reconnect dziala w tle bez blokowania requestow
    /// - Serwisy niedostepne zwracaja blad natychmiast (nie czekaja)
    pub fn new(config: RouterConfig, db: Option<DbPool>) -> Result<Self> {
        let config = Arc::new(config);

        info!("Router: Inicjalizacja (non-blocking)...");

        // === KROK 1: UTWORZ SERVICE MANAGER ===
        let service_manager = Arc::new(ServiceManager::new(config.clone(), db.clone())?);

        // === KROK 2: SPAWN BACKGROUND CONNECTION TASKS ===
        // UWAGA: spawn_connection_tasks przeniesione do Router::start() zeby reverse_router
        // byl ustawiony PRZED uruchomieniem petli (inaczej meeting-bot nie dostanie listenera).
        // service_manager.spawn_connection_tasks();

        // === KROK 3: INICJALIZUJ RESPONSE MIDDLEWARE ===
        let response_middleware = Arc::new(ResponseMiddleware::new(
            config.middleware.response_filtering_enabled,
        ));

        // === KROK 4: INICJALIZUJ FLOW DISPATCHER ===
        // R2a: Adapter LLM potrzebuje executor'a, ktory powstaje DOPIERO po
        // FlowDispatcher (cykl: executor->dispatcher->adapter->executor).
        // Tworzymy pusty slot teraz; Router::new wpisze executor po
        // konstrukcji dispatcher'a.
        let executor_slot: Arc<
            parking_lot::RwLock<
                Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>,
            >,
        > = Arc::new(parking_lot::RwLock::new(None));
        // Codex M1 round 2: SttRuntime slot — wspolny dla `Router.stt_runtime`
        // i `SttNodeAdapter`, zeby flow STT node szedl ta sama sciezka co
        // handler `/v1/audio/transcriptions` (D.3 single owner).
        let stt_runtime_slot: Arc<
            parking_lot::RwLock<Option<Arc<crate::services::stt::SttRuntime>>>,
        > = Arc::new(parking_lot::RwLock::new(None));
        let db_clone = db.clone();
        let flow_dispatcher = db.map(|pool| {
            Arc::new(FlowDispatcher::new(
                pool,
                service_manager.clone(),
                config.clone(),
                executor_slot.clone(),
                stt_runtime_slot.clone(),
            ))
        });

        // Embedded inference handle — owned by `ModelRuntimeExecutor`,
        // not the router. Router only constructs and hands off.
        let local_inference = Arc::new(crate::inference::local::LocalInferenceHandler::new(
            crate::inference::shared_inference_manager(),
        ));

        info!("Router: Inicjalizacja zakonczona (QUIC connections spawning in background)");

        let alias_cache = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));

        let router = Self {
            service_manager: service_manager.clone(),
            response_middleware,
            flow_dispatcher: flow_dispatcher.clone(),
            db: db_clone,
            mesh_manager: Arc::new(parking_lot::RwLock::new(None)),
            alias_cache,
            services_snapshot_rx: Arc::new(parking_lot::RwLock::new(None)),
            catalog_provider: Arc::new(crate::services::catalog::CatalogProvider::new()),
            executor: executor_slot.clone(),
            stt_runtime: stt_runtime_slot,
        };

        // R1.5e: zbuduj ModelRuntimeExecutor i wpiec do routera. Wymaga
        // catalog_provider (juz utworzonego), local_inference, opcjonalnie
        // flow_dispatcher. Resolver (`AliasResolver`) konsumuje
        // `LiveHandlesCache` z service_manager — to jest ten sam cache co
        // dispatcher uzywa do hydratacji `Local` candidates.
        {
            use crate::services::runtime::executor::ModelRuntimeExecutor;
            use crate::services::runtime::resolver::AliasResolver;
            // Codex H2: zamiast capture'owac local_node_id w `Router::new`
            // (registry jeszcze None) — przekazujemy provider closure ktora
            // dynamicznie czyta `local().node_id` z ServiceManager.
            // mesh_services_registry przy kazdym resolve. Inaczej kazdy
            // lokalny ModelInstance trafia w MeshForward → fallback.
            let local_node_id =
                crate::services::runtime::resolver::local_node_id_provider_for_router(
                    &service_manager,
                );
            let resolver = Arc::new(AliasResolver::new(
                service_manager.live_handles.clone(),
                local_node_id,
            ));
            let executor = Arc::new(ModelRuntimeExecutor::new(
                router.catalog_provider.clone(),
                resolver,
                flow_dispatcher.clone(),
                local_inference.clone(),
                router.stt_runtime.clone(),
                router.mesh_manager.clone(),
                Vec::new(),
            ));
            *executor_slot.write() = Some(executor);
        }

        router.reload_alias_cache();

        Ok(router)
    }

    /// R1.5e: udostepnia ModelRuntimeExecutor (gotowy do uzycia po
    /// `Router::new`). Zwraca `None` tylko w testach ktore konstruuja
    /// Router minimalnym konstruktorem omijajacym init.
    pub fn executor(
        &self,
    ) -> Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>> {
        self.executor.read().clone()
    }

    /// Codex M1: udostepnia SttRuntime (single owner STT path zgodnie z
    /// D.3). Zwraca `None` tylko jesli `Router::start` jeszcze nie
    /// odpalono albo Router byl konstruowany w trybie test'owym
    /// pomijajacym init.
    pub fn stt_runtime(&self) -> Option<Arc<crate::services::stt::SttRuntime>> {
        self.stt_runtime.read().clone()
    }

    // ========================================================================
    // HELPER METHODS - deleguja do ServiceManager
    // ========================================================================

    /// Zwraca referencje do ServiceManager.
    pub fn service_manager(&self) -> &Arc<ServiceManager> {
        &self.service_manager
    }

    /// Zwraca referencje do FlowDispatcher jesli Router zostal skonstruowany
    /// z DB (produkcja ma zawsze, niektore testy nie). Handlery zapisu flow
    /// uzywaja go do pobrania AdapterRegistry dla walidacji.
    pub fn flow_dispatcher(&self) -> Option<&Arc<FlowDispatcher>> {
        self.flow_dispatcher.as_ref()
    }

    pub fn start(self: &Arc<Self>) {
        // QUIC service connection tasks are owned by the supervisor (krok N7.2):
        // it spawns reconnect loops directly per `BackendHandle::Quic` planted
        // into `live_handles`. The reverse router for incoming container streams
        // is wired the same way through `Router::set_mesh_manager` and the
        // dispatcher's `MeshForward` path.
        self.service_manager.set_reverse_router((**self).clone());

        // Codex M1: wpięcie SttRuntime — trzyma `Weak<Router>` (anty-cykl).
        // Handler `/v1/audio/transcriptions` woła przez `router.stt_runtime()`
        // zamiast bezposrednio Router::route_audio_transcription, zeby
        // SttRuntime byl single owner STT path (D.3).
        // Codex R5f Blocker fix: SttRuntime ma teraz owned dispatch
        // (bezposrednio przez SttManager). Pre-R5f trzymal Weak<Router>
        // i loopowal przez `Router.route_audio_transcription` ktore po
        // R3b.6 cutover stalo sie stub'em — STT byl wylaczony.
        // Singleton (shared_stt_runtime) — supervisor reconcile uzywa tego
        // samego Arc do `register_backend(SttBackend::Http)` dla python-bundle
        // STT services (qwen-asr/parakeet/kyutai-tts).
        *self.stt_runtime.write() = Some(crate::services::stt::shared_stt_runtime());

        // R3e (F10): subscribe na mutacje mesh registry. Wczesniej peer
        // announce/remove/update aktualizowal registry, ale `/v1/models` /
        // GUI catalog widzialy zmiany dopiero po nastepnym ticku supervisora
        // (1-5s opoznienia). Z observerem rebuild leci natychmiast po
        // mutacji w sub-second.
        //
        // Trzymamy `Weak<Router>` zeby uniknac cyklu: callback → Router →
        // ServiceManager → MeshServicesRegistry → callback. Z `Weak`
        // upgrade zawodzi po shutdown'cie i callback bezglosnie no-op'uje
        // zamiast przedluzac zywotnosc routera.
        if let Some(registry) = self.service_manager.mesh_services_registry.read().clone() {
            let weak_self = Arc::downgrade(self);
            registry.set_on_change(Some(Arc::new(move || {
                if let Some(router) = weak_self.upgrade() {
                    router.rebuild_catalog();
                }
            })));
        }

        // Eager catalog build — `set_mesh_services_registry` runs before
        // `start()` so the registry is already wired here. Without this the
        // first `/v1/models` call returns empty until the first deploy /
        // alias mutation triggers a rebuild.
        self.rebuild_catalog();
    }

    /// Wysyla sygnal shutdown do wszystkich komponentow routera.
    pub fn shutdown(&self) {
        info!("Router shutdown initiated...");
        self.service_manager.shutdown();
        info!("Shutdown signal sent to all components");
    }

    /// Pobierz QUIC embedding client (async - sprawdza czy polaczony)
    #[allow(dead_code)]
    pub(crate) async fn get_quic_embedding_client(
        &self,
        service_name: &str,
    ) -> Option<Arc<crate::net::quic::QuicClient>> {
        self.service_manager
            .get_quic_embedding_client(service_name)
            .await
    }


    /// Pobierz status wszystkich serwisow (do diagnostyki/health check)
    pub async fn get_service_status(&self) -> std::collections::HashMap<String, String> {
        self.service_manager.get_service_status().await
    }

    // ========================================================================
    // MESH ROUTING
    // ========================================================================

    /// Ustawia mesh manager (wywolane po inicjalizacji mesh pipeline)
    pub fn set_mesh_manager(&self, manager: Arc<crate::mesh::iroh_manager::IrohMeshManager>) {
        *self.mesh_manager.write() = Some(manager);
    }

    /// Wires the supervisor's services snapshot into the router. Called once
    /// from `main.rs` after `Supervisor::new` returns. The router consults the
    /// snapshot as a fallback when legacy resolution misses a model — Phase 8
    /// cleanup will make it the only resolution path.
    pub fn set_services_snapshot_rx(
        &self,
        rx: tokio::sync::watch::Receiver<Arc<crate::services::supervisor::ServicesSnapshot>>,
    ) {
        self.service_manager.set_snapshot_rx(rx.clone());
        *self.services_snapshot_rx.write() = Some(rx.clone());

        // Hydrate `local_inference_models` from the snapshot so embedded
        // engines become routable on the first deploy without waiting for the
        // next request cycle. Skipped when no Tokio runtime is available (unit
        // tests wiring a snapshot directly through `set_snapshot_rx`).
        if tokio::runtime::Handle::try_current().is_ok() {
            let manager = self.service_manager.clone();
            let mut rx = rx;
            // Run the initial hydration eagerly so the very first snapshot
            // (typically empty / first-tick result) is reflected immediately.
            manager.hydrate_from_snapshot();
            tokio::spawn(async move {
                while rx.changed().await.is_ok() {
                    manager.hydrate_from_snapshot();
                }
            });
        }
    }

    /// Returns the current services snapshot, or `None` if `main.rs` hasn't
    /// wired it yet (legacy startup paths, tests with `Router::default`).
    pub fn services_snapshot(&self) -> Option<Arc<crate::services::supervisor::ServicesSnapshot>> {
        self.services_snapshot_rx
            .read()
            .as_ref()
            .map(|rx| rx.borrow().clone())
    }

    /// Resolves `model_name` against the V2 services snapshot. Returns
    /// `(service_id, engine_id)` of the first running/degraded service that
    /// hosts the model. Used by routers/handlers as a fallback after the
    /// legacy `model_pool` lookup misses.
    pub fn resolve_model_via_snapshot(&self, model_name: &str) -> Option<(i64, String)> {
        let snap = self.services_snapshot()?;
        let service_id = *snap.models_by_name.get(model_name)?;
        let idx = *snap.services_by_id.get(&service_id)?;
        let entry = snap.services.get(idx)?;
        Some((entry.id, entry.engine_id.clone()))
    }

    // ========================================================================
    // ALIAS RETENTAFLOWN
    // ========================================================================

    // ========================================================================
    // HEALTH & MONITORING METHODS
    // ========================================================================

    /// Whether the V2 snapshot exposes at least one routable service. Used by
    /// health probes; does not consider per-backend liveness.
    pub fn has_healthy_backends(&self) -> bool {
        let snap = self.service_manager.current_snapshot();
        !snap.services.is_empty()
    }

    /// Public catalog snapshot. The router publishes one immutable snapshot
    /// per rebuild; readers (`/v1/models`, mesh `catalog.list`, GUI) take it
    /// lock-free and walk the slice. Aliases, published flows, and service
    /// models all live here.
    pub fn catalog_snapshot(&self) -> Arc<crate::services::catalog::CatalogSnapshot> {
        self.catalog_provider.snapshot()
    }

    /// Shared handle to the catalog provider. The supervisor takes this
    /// during boot so its `reconcile_handles` tick can call `rebuild()`
    /// directly — keeps the catalog consistent with deploy / peer state
    /// without forcing every mutation point to remember to refresh.
    pub fn catalog_provider(&self) -> &Arc<crate::services::catalog::CatalogProvider> {
        &self.catalog_provider
    }

    /// Rebuild the catalog from the current mesh registry plus the local DB
    /// (aliases, published flows). Called after deploy / undeploy / alias
    /// mutation / flow publish; idempotent and safe to call concurrently.
    /// Returns the new snapshot version, or `None` when DB is not attached
    /// (test harnesses).
    pub fn rebuild_catalog(&self) -> Option<u64> {
        let pool = self.db.as_ref()?;
        let registry_arc = self.service_manager.mesh_services_registry.read();
        let registry = registry_arc.as_ref()?.clone();
        drop(registry_arc);
        match self.catalog_provider.rebuild(&registry, pool) {
            Ok(version) => Some(version),
            Err(e) => {
                tracing::warn!("Catalog rebuild failed: {e}");
                None
            }
        }
    }

    /// Distinct model ids advertised on `/v1/models`. Filters out blocking
    /// catalog diagnostics (RemoteShadowed / LocalOverride) so a hidden
    /// remote does not show up as a queryable model. Non-blocking
    /// diagnostics (IncompatibleAliasTargets) keep their entry — the alias
    /// may still resolve fine for requests that match its primary.
    pub fn list_available_models(&self) -> Vec<String> {
        let snap = self.catalog_snapshot();
        let mut models: Vec<String> = snap.advertised_entries().map(|e| e.id.clone()).collect();
        models.sort();
        models.dedup();
        models
    }

    /// Returns one healthy entry per model name from the V2 snapshot. The
    /// per-backend health info is best-effort (treated as healthy when the
    /// snapshot lists the service); active-request counters are not tracked.
    pub fn get_metrics(&self) -> RouterMetrics {
        let snap = self.service_manager.current_snapshot();
        let mut backend_metrics: HashMap<String, Vec<BackendMetric>> = HashMap::new();
        for (model_name, _service_id) in &snap.models_by_name {
            backend_metrics
                .entry(model_name.clone())
                .or_default()
                .push(BackendMetric {
                    is_healthy: true,
                    active_requests: 0,
                });
        }
        RouterMetrics {
            backends: backend_metrics,
            total_requests: 0,
            active_connections: 0,
        }
    }
}
