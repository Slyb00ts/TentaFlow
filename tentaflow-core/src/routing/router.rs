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
use crate::routing::backend::BackendClient;
use crate::routing::service_manager::ServiceManager;
use crate::services::rag::RAGClient;
use crate::services::tts::TTSClient;

use std::collections::HashMap;
use std::sync::Arc;
use tentaflow_protocol::*;
use tokio::sync::mpsc;
use tracing::info;

/// Router zarzadzajacy routing requestow do backendow.
///
/// Trzyma mape serwisow i backend clients (pre-alokowane).
/// Zunifikowana architektura obsluguje wszystkie typy serwisow: LLM, Embedding, RAG, Vision, STT, TTS.
///
/// **ARCHITEKTURA NON-BLOCKING:**
/// - Router::new() zwraca NATYCHMIAST (synchronicznie)
/// - Polaczenia QUIC uruchamiane w background taskach
/// - Auto-reconnect dziala w tle bez blokowania requestow
/// - Serwisy niedostepne zwracaja blad natychmiast
#[derive(Clone)]
pub struct Router {
    /// Konfiguracja routera
    pub(crate) config: Arc<RouterConfig>,

    /// Service Manager - zarzadza wszystkimi serwisami asynchronicznie
    pub(crate) service_manager: Arc<ServiceManager>,

    /// Response middleware dla filtrowania PII
    pub(crate) response_middleware: Arc<ResponseMiddleware>,

    /// Mowcy potrzebujacy dodatkowych sampli glosu (speaker_id -> remaining_samples)
    /// Po enrollment zbieramy 3 dodatkowe probki zeby wzmocnic model glosu
    pub(crate) pending_voice_samples:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, u8>>>,

    /// Flow Engine dispatcher - opcjonalny, aktywny gdy DB jest dostepna
    pub(crate) flow_dispatcher: Option<Arc<FlowDispatcher>>,

    /// Baza danych (do resolve aliasow modeli)
    pub(crate) db: Option<DbPool>,

    /// Lokalna inferencja in-process (MLX, llama.cpp) — bez HTTP/QUIC
    pub(crate) local_inference: Arc<super::local_inference::LocalInferenceHandler>,

    /// Lokalna transkrypcja in-process (Whisper) — bez HTTP/QUIC
    pub(crate) local_stt: Arc<super::local_stt::LocalSttHandler>,

    /// Mesh manager — do forwardowania requestow do zdalnych nodow
    pub(crate) mesh_manager:
        Arc<parking_lot::RwLock<Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>>>,

    /// Cache aliasow modeli z DB (alias -> DbModelAlias)
    pub(crate) alias_cache: Arc<
        parking_lot::RwLock<std::collections::HashMap<String, crate::db::models::DbModelAlias>>,
    >,

    /// Globalny counter do round-robin w middleware routing
    pub(crate) route_counter: Arc<std::sync::atomic::AtomicUsize>,

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
    /// - Polaczenia QUIC (RAG, Embeddings) sa uruchamiane w background taskach
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
        let db_clone = db.clone();
        let flow_dispatcher = db.map(|pool| {
            Arc::new(FlowDispatcher::new(
                pool,
                service_manager.clone(),
                config.clone(),
            ))
        });

        // === KROK 7: INICJALIZUJ LOKALNA INFERENCJE ===
        let local_inference = Arc::new(super::local_inference::LocalInferenceHandler::new(
            crate::inference::shared_inference_manager(),
        ));

        // === KROK 8: INICJALIZUJ LOKALNE STT ===
        let local_stt = Arc::new(super::local_stt::LocalSttHandler::new(
            crate::stt::shared_stt_manager(),
        ));

        info!("Router: Inicjalizacja zakonczona (QUIC connections spawning in background)");

        let alias_cache = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));

        let router = Self {
            config,
            service_manager,
            response_middleware,
            pending_voice_samples: Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            flow_dispatcher,
            db: db_clone,
            local_inference,
            local_stt,
            mesh_manager: Arc::new(parking_lot::RwLock::new(None)),
            alias_cache,
            route_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            services_snapshot_rx: Arc::new(parking_lot::RwLock::new(None)),
        };

        router.reload_alias_cache();

        Ok(router)
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

    pub fn start(&self) {
        info!("Router: Starting callback handler...");
        self.spawn_callback_handler();
        info!("Router: Callback handler started");
        // QUIC service connection tasks are owned by the supervisor (krok N7.2):
        // it spawns reconnect loops directly per `BackendHandle::Quic` planted
        // into `live_handles`. The reverse router for incoming container streams
        // is wired the same way through `Router::set_mesh_manager` and the
        // dispatcher's `MeshForward` path.
        self.service_manager.set_reverse_router(self.clone());
    }

    /// Wysyla sygnal shutdown do wszystkich komponentow routera.
    pub fn shutdown(&self) {
        info!("Router shutdown initiated...");
        self.service_manager.shutdown();
        info!("Shutdown signal sent to all components");
    }

    /// Resolves an HTTP backend client for `service_name` (the model name) via
    /// the V2 snapshot pipeline: `find_http_backend_for_model` consults the live
    /// handles cache; on miss `resolve_http_backends_via_snapshot` materialises
    /// a fresh client straight from the snapshot.
    pub(crate) fn select_http_backend(&self, service_name: &str) -> Option<Arc<BackendClient>> {
        self.service_manager
            .find_http_backend_for_model(service_name)
            .or_else(|| {
                self.service_manager
                    .resolve_http_backends_via_snapshot(service_name)
                    .and_then(|v| v.into_iter().next())
            })
    }

    /// Pobierz RAG client (async - sprawdza czy polaczony)
    #[allow(dead_code)]
    pub(crate) async fn get_rag_client(&self, service_name: &str) -> Option<Arc<RAGClient>> {
        self.service_manager.get_rag_client(service_name).await
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

    /// Pobierz TTS client po nazwie serwisu
    #[allow(dead_code)]
    pub(crate) fn get_tts_client(&self, service_name: &str) -> Option<Arc<TTSClient>> {
        self.service_manager.get_tts_client(service_name)
    }

    /// Pobierz TTS client po nazwie modelu (publiczne dla QUIC server).
    pub async fn get_tts_client_by_model(&self, model: &str) -> Option<TTSClient> {
        if let Some(client) = self.service_manager.get_tts_client(model) {
            return Some((*client).clone());
        }
        self.service_manager
            .get_first_tts_client()
            .map(|c| (*c).clone())
    }

    /// Pobierz callback receiver dla RAG
    pub(crate) fn get_callback_rx(
        &self,
    ) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(ModelRequest, mpsc::Sender<ModelResponse>)>>>
    {
        self.service_manager.get_callback_rx()
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

    /// Rozwiazuje alias modelu na canonical name. Aliasy nie pochodza juz z
    /// config.toml — DB `service_aliases` (uzywane przez middleware route
    /// resolver) jest jedynym zrodlem, wiec tutaj zwracamy nazwe bez zmian.
    pub(crate) fn resolve_model_alias(&self, model: &str) -> String {
        model.to_string()
    }

    // ========================================================================
    // HEALTH & MONITORING METHODS
    // ========================================================================

    /// Whether the V2 snapshot exposes at least one routable service. Used by
    /// health probes; does not consider per-backend liveness.
    pub fn has_healthy_backends(&self) -> bool {
        let snap = self.service_manager.current_snapshot();
        !snap.services.is_empty()
    }

    /// Distinct model names exposed by the V2 services snapshot.
    pub fn list_available_models(&self) -> Vec<String> {
        let snap = self.service_manager.current_snapshot();
        let mut models: Vec<String> = snap.models_by_name.keys().cloned().collect();
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
