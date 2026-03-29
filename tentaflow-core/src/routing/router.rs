// =============================================================================
// Plik: routing/router.rs
// Opis: Glowna struktura Router, inicjalizacja, alias retentaflown, model/service
//       lookup, publiczne API diagnostyczne. Deleguje do podmodulow chat,
//       streaming, embeddings, tts, stt.
// =============================================================================

use crate::routing::backend::BackendClient;
use crate::config::RouterConfig;
use crate::db::DbPool;
use crate::error::Result;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::middleware::ResponseMiddleware;
use crate::routing::loadbalancer::LoadBalancingStrategy;
use crate::routing::memory_integration::MemoryIntegration;
use crate::routing::service_manager::ServiceManager;
use crate::services::rag::RAGClient;
use crate::services::tts::TTSClient;
use crate::intent_analyzer::IntentAnalyzer;

use tentaflow_protocol::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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

    /// Memory Integration - integracja z TentaFlow.Memory
    pub(crate) memory_integration: Arc<MemoryIntegration>,

    /// Intent Analyzer - wykrywanie intencji uzywajac Bielika 11B
    pub(crate) intent_analyzer: Arc<IntentAnalyzer>,

    /// Mowcy potrzebujacy dodatkowych sampli glosu (speaker_id -> remaining_samples)
    /// Po enrollment zbieramy 3 dodatkowe probki zeby wzmocnic model glosu
    pub(crate) pending_voice_samples: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u8>>>,

    /// Flow Engine dispatcher - opcjonalny, aktywny gdy DB jest dostepna
    pub(crate) flow_dispatcher: Option<Arc<FlowDispatcher>>,

    /// Baza danych (do resolve aliasow modeli)
    pub(crate) db: Option<DbPool>,

    /// Lokalna inferencja in-process (MLX, llama.cpp) — bez HTTP/QUIC
    pub(crate) local_inference: Arc<super::local_inference::LocalInferenceHandler>,

    /// Mesh manager — do forwardowania requestow do zdalnych nodow
    pub(crate) mesh_manager: Arc<parking_lot::RwLock<Option<Arc<crate::mesh::quic_mesh::QuicMeshManager>>>>,
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
    /// Memory Analyzer - query decision (bielik-1.5b)
    pub query_analysis_ms: Option<u64>,
    /// Memory query (QUIC do Memory Engine)
    pub memory_query_ms: Option<u64>,
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
        lines.push(format!("│ REQUEST TIMING {:>20} │", self.model_name.as_deref().unwrap_or("-")));
        lines.push(format!("├─────────────────────────────────────┤"));

        if let Some(ms) = self.stt_ms {
            lines.push(format!("│ STT              {:>10} ms     │", ms));
        }
        if let Some(ms) = self.speaker_id_ms {
            lines.push(format!("│ Speaker ID       {:>10} ms     │", ms));
        }
        if let Some(ms) = self.query_analysis_ms {
            lines.push(format!("│ Query Analysis   {:>10} ms     │", ms));
        }
        if let Some(ms) = self.memory_query_ms {
            lines.push(format!("│ Memory Query     {:>10} ms     │", ms));
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
        lines.push(format!("│ TOTAL            {:>10} ms     │", self.total_ms()));
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
        let service_manager = Arc::new(
            ServiceManager::new(config.clone(), db.clone())?
        );

        // === KROK 2: SPAWN BACKGROUND CONNECTION TASKS ===
        service_manager.spawn_connection_tasks();

        // === KROK 3: INICJALIZUJ RESPONSE MIDDLEWARE ===
        let response_middleware = Arc::new(ResponseMiddleware::new(
            config.middleware.response_filtering_enabled,
        ));

        // === KROK 4: INICJALIZUJ MEMORY INTEGRATION ===
        let memory_integration = Arc::new(MemoryIntegration::new(
            service_manager.clone(),
            None,
        ));

        // === KROK 5: INICJALIZUJ INTENT ANALYZER ===
        let intent_analyzer = Arc::new(IntentAnalyzer::new(
            service_manager.clone(),
            None,
        ));

        // === KROK 6: INICJALIZUJ FLOW DISPATCHER ===
        let db_clone = db.clone();
        let flow_dispatcher = db.map(|pool| {
            Arc::new(FlowDispatcher::new(
                pool,
                service_manager.clone(),
                config.clone(),
            ))
        });

        // === KROK 7: INICJALIZUJ LOKALNA INFERENCJE ===
        let local_inference = Arc::new(
            super::local_inference::LocalInferenceHandler::new(
                crate::inference::shared_inference_manager()
            )
        );

        info!("Router: Inicjalizacja zakonczona (QUIC connections spawning in background)");

        Ok(Self {
            config,
            service_manager,
            response_middleware,
            memory_integration,
            intent_analyzer,
            pending_voice_samples: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            flow_dispatcher,
            db: db_clone,
            local_inference,
            mesh_manager: Arc::new(parking_lot::RwLock::new(None)),
        })
    }

    // ========================================================================
    // HELPER METHODS - deleguja do ServiceManager
    // ========================================================================

    /// Zwraca referencje do ServiceManager.
    pub fn service_manager(&self) -> &Arc<ServiceManager> {
        &self.service_manager
    }

    pub fn start(&self) {
        info!("Router: Starting callback handler...");
        self.spawn_callback_handler();
        info!("Router: Callback handler started");
    }

    /// Wysyla sygnal shutdown do wszystkich komponentow routera.
    pub fn shutdown(&self) {
        info!("Router shutdown initiated...");
        self.service_manager.shutdown();
        info!("Shutdown signal sent to all components");
    }

    /// Laduje serwisy QUIC z bazy danych i rejestruje w ServiceManager.
    /// Uzywany przez Router.New, Desktop i Mobile po inicjalizacji routera.
    pub fn load_db_services(&self) {
        let db = match &self.db {
            Some(db) => db,
            None => {
                warn!("load_db_services: brak bazy danych");
                return;
            }
        };
        let sm = &self.service_manager;
        match crate::db::repository::list_services(db) {
            Ok(services) => {
                let mut loaded = 0;
                for svc in &services {
                    let svc_config: serde_json::Value =
                        serde_json::from_str(&svc.config_json).unwrap_or_default();
                    let backends =
                        crate::db::repository::list_backends_for_service(db, svc.id).unwrap_or_default();
                    let mut registered = false;

                    for backend in &backends {
                        if !backend.is_active {
                            continue;
                        }
                        let config: serde_json::Value =
                            serde_json::from_str(&backend.config_json).unwrap_or_default();
                        let url = config["url"].as_str().unwrap_or("");
                        if url.is_empty() {
                            continue;
                        }

                        let host = url
                            .trim_start_matches("http://")
                            .trim_start_matches("https://")
                            .split(':')
                            .next()
                            .unwrap_or("127.0.0.1");
                        let quic_port = svc_config["quic_port"].as_u64().unwrap_or(5010);
                        let quic_url = format!("quic://{}:{}", host, quic_port);
                        let tls_ca = crate::db::repository::get_setting(db, "tls_cert_pem")
                            .ok()
                            .flatten();
                        let server_name = svc_config["agent_domain"]
                            .as_str()
                            .map(|s| s.to_string());

                        info!(
                            "DB service '{}' (typ={}) -> {} (SNI: {:?})",
                            svc.name, svc.service_type, quic_url, server_name
                        );
                        sm.register_quic_service(
                            svc.name.clone(),
                            &svc.service_type,
                            quic_url,
                            tls_ca,
                            server_name,
                        );
                        registered = true;
                        loaded += 1;
                        break;
                    }

                    if registered {
                        if let Some(deployed_model) = svc_config["deployed_model"].as_str() {
                            if !deployed_model.is_empty() {
                                sm.register_model_mapping(deployed_model, &svc.name);
                            }
                        }
                    }
                }
                if loaded > 0 {
                    info!("Zaladowano {} serwisow QUIC z bazy danych", loaded);
                }
            }
            Err(e) => {
                warn!("Nie udalo sie zaladowac serwisow z DB: {}", e);
            }
        }
    }

    /// Przywraca natywne serwisy (in-process MLX/llama.cpp) z bazy po restarcie.
    /// Skanuje DB pod katem serwisow z deploy_mode=native, laduje model i rejestruje.
    pub async fn restore_native_services(&self) {
        let db = match &self.db {
            Some(db) => db,
            None => {
                warn!("restore_native_services: brak bazy danych — pomijam");
                return;
            }
        };

        let services = match crate::db::repository::list_services(db) {
            Ok(s) => s,
            Err(e) => {
                warn!("restore_native_services: blad odczytu serwisow: {}", e);
                return;
            }
        };

        info!("restore_native_services: znaleziono {} serwisow w DB", services.len());
        for svc in &services {
            info!("  serwis '{}': config={}", svc.name, svc.config_json);
            let config: serde_json::Value =
                serde_json::from_str(&svc.config_json).unwrap_or_default();

            if config["deploy_mode"].as_str() != Some("native") {
                continue;
            }

            let model_id = match config["deployed_model"].as_str() {
                Some(m) if !m.is_empty() => m.to_string(),
                _ => continue,
            };
            let engine_id = config["engine"].as_str().unwrap_or("mlx").to_string();
            let model_path_str = config["model_path"].as_str().unwrap_or("");

            // Sprawdz czy model jest na dysku
            let mut model_path = std::path::PathBuf::from(model_path_str);

            // iOS/Android: UUID kontenera zmienia sie przy reinstalacji.
            // Jesli sciezka nie istnieje, sprobuj przebudowac z aktualnego data_dir.
            if !model_path.exists() {
                // Wyciagnij relatywna czesc sciezki po "models/"
                if let Some(pos) = model_path_str.find("/models/") {
                    let relative = &model_path_str[pos..]; // np. "/models/huggingface/vqstudio/..."
                    let data_dir = dirs::data_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join("TentaFlow.AI");
                    let rebuilt = data_dir.join(&relative[1..]); // usun wiodacy "/"
                    debug!("Sciezka nie istnieje, probuje przebudowac: {}", rebuilt.display());
                    if rebuilt.exists() {
                        model_path = rebuilt;
                    }
                }
            }

            debug!("model_path={}, exists={}", model_path.display(), model_path.exists());
            if !model_path.exists() {
                warn!(
                    "Natywny serwis '{}': sciezka modelu nie istnieje: {}",
                    svc.name, model_path.display()
                );
                continue;
            }

            info!(
                "Przywracanie natywnego serwisu '{}': model={}, engine={}",
                svc.name, model_id, engine_id
            );

            // Zaladuj model do wspoldzielonego InferenceManager
            let shared = crate::inference::shared_inference_manager();
            let mp = model_path.clone();
            let eng = engine_id.clone();
            let load_result = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    let mut mgr = shared.write().await;
                    mgr.load_model(&mp, None, Some(&eng)).await
                })
            }).await;

            match load_result {
                Ok(Ok(model_info)) => {
                    info!(
                        "Przywrocono model '{}' ({}, {}MB, ctx={})",
                        model_info.name, model_info.backend,
                        model_info.vram_used_mb, model_info.context_length
                    );
                }
                Ok(Err(e)) => {
                    warn!("Blad ladowania modelu '{}': {}", model_id, e);
                    continue;
                }
                Err(e) => {
                    warn!("Blad tasku ladowania '{}': {}", model_id, e);
                    continue;
                }
            }

            // Rejestruj w service_manager
            self.service_manager.register_model_mapping(&model_id, &svc.name);
            self.service_manager.register_local_inference_model(&model_id);
            self.service_manager.register_local_inference_model(&svc.name);
            info!("Natywny serwis '{}' przywrocony pomyslnie", svc.name);
        }
    }

    /// Pobierz backend clients dla serwisu (HTTP backends)
    pub(crate) fn get_service_backends(&self, service_name: &str) -> Option<&Vec<Arc<BackendClient>>> {
        self.service_manager.get_service_backends(service_name)
    }

    /// Pobierz load balancing strategy dla serwisu
    pub(crate) fn get_strategy(&self, service_name: &str) -> Option<&Box<dyn LoadBalancingStrategy>> {
        self.service_manager.get_strategy(service_name)
    }

    /// Pobierz RAG client (async - sprawdza czy polaczony)
    #[allow(dead_code)]
    pub(crate) async fn get_rag_client(&self, service_name: &str) -> Option<Arc<RAGClient>> {
        self.service_manager.get_rag_client(service_name).await
    }

    /// Pobierz QUIC embedding client (async - sprawdza czy polaczony)
    #[allow(dead_code)]
    pub(crate) async fn get_quic_embedding_client(&self, service_name: &str) -> Option<Arc<crate::net::quic::QuicClient>> {
        self.service_manager.get_quic_embedding_client(service_name).await
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
        self.service_manager.get_first_tts_client().map(|c| (*c).clone())
    }

    /// Pobierz callback receiver dla RAG
    pub(crate) fn get_callback_rx(&self) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(ModelRequest, mpsc::Sender<ModelResponse>)>>> {
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
    pub fn set_mesh_manager(&self, manager: Arc<crate::mesh::quic_mesh::QuicMeshManager>) {
        *self.mesh_manager.write() = Some(manager);
    }

    /// Przekieruj request do zdalnego noda przez mesh.
    /// Wysyla surowe bajty requestu i zwraca surowe bajty odpowiedzi.
    pub(crate) async fn route_through_mesh(
        &self,
        node_id: &str,
        request_bytes: &[u8],
    ) -> crate::error::Result<Vec<u8>> {
        let mesh = self.mesh_manager.read().clone().ok_or_else(|| {
            crate::error::CoreError::InternalError {
                message: "Mesh manager niedostepny".to_string(),
                source: None,
            }
        })?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let response = mesh
            .forward_request(node_id, &request_id, request_bytes.to_vec())
            .await
            .map_err(|e| crate::error::CoreError::NetworkError {
                message: format!("Blad forwardowania przez mesh do {}: {}", node_id, e),
                source: e,
            })?;

        Ok(response)
    }

    // ========================================================================
    // ALIAS RETENTAFLOWN
    // ========================================================================

    /// Rozwiazuje nazwe modelu na nazwe serwisu.
    /// 1. Config.toml aliasy (szybkie, in-memory)
    /// 2. Jesli rozwiazana nazwa to istniejacy serwis -> uzyj
    /// 3. DB model_aliases (fallback)
    /// 4. model_pool: model_name -> wybierz serwis (round-robin)
    pub(crate) fn resolve_to_service_name(&self, model: &str) -> String {
        let resolved = self.resolve_model_alias(model);

        if self.service_manager.has_quic_llm_service(&resolved)
            || self.service_manager.get_service_backends(&resolved).is_some()
            || self.service_manager.has_rag_service(&resolved)
            || self.service_manager.has_local_inference_service(&resolved)
        {
            return resolved;
        }

        if let Some(ref db) = self.db {
            if let Ok(Some(target)) = crate::db::repository::resolve_model_alias(db, &resolved) {
                debug!("DB alias resolved: {} -> {}", resolved, target);
                return target;
            }
        }

        if let Some(service_name) = self.service_manager.select_service_for_model(&resolved) {
            debug!("ModelPool resolved: {} -> {}", resolved, service_name);
            return service_name;
        }

        resolved
    }

    /// Rozwiazuje alias modelu na canonical name.
    pub(crate) fn resolve_model_alias(&self, model: &str) -> String {
        for alias in &self.config.service_aliases {
            if alias.alias == model {
                debug!("Alias resolved: {} -> {}", model, alias.target);
                return alias.target.clone();
            }
        }
        model.to_string()
    }

    /// Sprawdza czy model jest RAG engine.
    pub(crate) fn is_rag_model(&self, model_name: &str) -> bool {
        self.service_manager.has_rag_service(model_name)
    }

    /// Sprawdza czy model jest QUIC LLM
    pub(crate) fn is_quic_llm_model(&self, model_name: &str) -> bool {
        self.service_manager.has_quic_llm_service(model_name)
    }

    /// Sprawdza czy model jest obslugiwany przez lokalna inferencje in-process
    pub(crate) fn is_local_inference_model(&self, model_name: &str) -> bool {
        self.service_manager.has_local_inference_service(model_name)
    }

    // ========================================================================
    // CONVERSATION CONTEXT BUILDERS
    // ========================================================================

    /// Buduje kontekst rozmowy z ostatnich wiadomosci dla Intent Analyzera.
    #[allow(dead_code)]
    pub(crate) fn build_conversation_context_for_intent(
        &self,
        messages: &[crate::api::openai::types::Message],
        max_turns: usize,
    ) -> Option<String> {
        if messages.is_empty() {
            return None;
        }

        let skip_last = if messages.len() > 1 { 1 } else { 0 };
        let start = messages.len().saturating_sub(max_turns + skip_last);
        let end = messages.len().saturating_sub(skip_last);

        if start >= end {
            return None;
        }

        let mut context_parts = Vec::new();
        for msg in &messages[start..end] {
            let role = match msg.role.as_str() {
                "assistant" => "ASSISTANT",
                "user" => "USER",
                "system" => continue,
                _ => &msg.role,
            };

            let text = match &msg.content {
                Some(crate::api::openai::types::MessageContent::Text(s)) => Some(s.clone()),
                Some(crate::api::openai::types::MessageContent::Parts(parts)) => {
                    let texts: Vec<String> = parts.iter().filter_map(|p| {
                        if let crate::api::openai::types::ContentPart::Text { text } = p {
                            Some(text.clone())
                        } else {
                            None
                        }
                    }).collect();
                    if texts.is_empty() { None } else { Some(texts.join(" ")) }
                }
                None => None,
            };

            if let Some(content) = text {
                let truncated = if content.chars().count() > 200 {
                    format!("{}...", content.chars().take(200).collect::<String>())
                } else {
                    content
                };
                context_parts.push(format!("{}: {}", role, truncated));
            }
        }

        if context_parts.is_empty() {
            None
        } else {
            Some(context_parts.join("\n"))
        }
    }

    /// Buduje kontekst konwersacji z historii w ConversationCache.
    pub(crate) fn build_context_from_conversation_cache(
        &self,
        history: &[crate::routing::memory_integration::ConversationMessage],
        max_turns: usize,
    ) -> Option<String> {
        if history.is_empty() {
            return None;
        }

        let start = history.len().saturating_sub(max_turns);
        let messages_to_use = &history[start..];

        let mut context_parts = Vec::new();
        for msg in messages_to_use {
            let role = match msg.role.as_str() {
                "assistant" => "ASSISTANT",
                "user" => "USER",
                "system" => continue,
                _ => &msg.role,
            };

            let content = if msg.content.chars().count() > 200 {
                format!("{}...", msg.content.chars().take(200).collect::<String>())
            } else {
                msg.content.clone()
            };
            context_parts.push(format!("{}: {}", role, content));
        }

        if context_parts.is_empty() {
            None
        } else {
            Some(context_parts.join("\n"))
        }
    }

    // ========================================================================
    // HEALTH & MONITORING METHODS
    // ========================================================================

    /// Sprawdza czy jest dostepny przynajmniej jeden zdrowy backend
    pub fn has_healthy_backends(&self) -> bool {
        self.service_manager.has_service_backends() || self.service_manager.has_rag_services()
    }

    /// Zwraca liste wszystkich dostepnych modeli (model pools + RAG engines + aliasy)
    pub fn list_available_models(&self) -> Vec<String> {
        let mut models = Vec::new();

        for model_name in self.service_manager.service_backend_names().into_iter() {
            models.push(model_name.clone());
        }

        for rag_name in self.service_manager.rag_service_names().into_iter() {
            models.push(rag_name.clone());
        }

        for alias in &self.config.service_aliases {
            models.push(alias.alias.clone());
        }

        models.sort();
        models.dedup();
        models
    }

    // TODO: zaimplementowac rzeczywiste metryki (aktualnie zwraca hardcoded zera)
    pub fn get_metrics(&self) -> RouterMetrics {
        let mut backend_metrics = HashMap::new();

        for (model_name, backends) in &self.service_manager.service_backends {
            let mut model_backend_metrics = Vec::new();

            for _backend in backends {
                model_backend_metrics.push(BackendMetric {
                    is_healthy: true,
                    active_requests: 0,
                });
            }

            backend_metrics.insert(model_name.clone(), model_backend_metrics);
        }

        RouterMetrics {
            backends: backend_metrics,
            total_requests: 0,
            active_connections: 0,
        }
    }
}
