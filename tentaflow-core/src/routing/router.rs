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
use crate::routing::loadbalancer::LoadBalancingStrategy;
use crate::routing::service_manager::ServiceManager;
use crate::services::rag::RAGClient;
use crate::services::tts::TTSClient;

use std::collections::HashMap;
use std::sync::Arc;
use tentaflow_protocol::*;
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

        // === KROK 2: INICJALIZUJ MODEL POOL Z DB ===
        if let Some(ref pool) = db {
            service_manager.init_model_pool(pool);
        }

        // === KROK 3: SPAWN BACKGROUND CONNECTION TASKS ===
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

        // Ustaw router dla odwrotnych requestow od kontenerow — MUSI byc przed
        // spawn_connection_tasks, inaczej petle serwisow dostana None zamiast routera
        // i reverse listener dla meeting-bot nie bedzie uruchomiony.
        info!("Router: Registering reverse router for container requests...");
        self.service_manager.set_reverse_router(self.clone());
        info!("Router: Reverse router registered");

        // Dopiero teraz spawnujemy tasks polaczen QUIC — reverse_router jest juz ustawiony
        info!("Router: Spawning QUIC connection tasks...");
        self.service_manager.spawn_connection_tasks();
        info!("Router: QUIC connection tasks spawned");
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
                info!(
                    "load_db_services: znaleziono {} serwisow w DB",
                    services.len()
                );
                let mut loaded = 0;
                for svc in &services {
                    info!(
                        "load_db_services: przetwarzam '{}' (typ={})",
                        svc.name, svc.service_type
                    );
                    let svc_config: serde_json::Value =
                        serde_json::from_str(&svc.config_json).unwrap_or_default();
                    let backends = crate::db::repository::list_backends_for_service(db, svc.id)
                        .unwrap_or_default();
                    let mut registered = false;

                    for backend in &backends {
                        if !backend.is_active {
                            continue;
                        }
                        let config: serde_json::Value =
                            serde_json::from_str(&backend.config_json).unwrap_or_default();

                        // QUIC backendy maja "quic_url", HTTP maja "url"
                        let quic_url = if let Some(qurl) = config["quic_url"].as_str() {
                            qurl.to_string()
                        } else {
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
                            format!("quic://{}:{}", host, quic_port)
                        };

                        let tls_ca =
                            config["tls_ca"]
                                .as_str()
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    crate::db::repository::get_setting(db, "tls_cert_pem")
                                        .ok()
                                        .flatten()
                                });
                        let server_name =
                            svc_config["agent_domain"].as_str().map(|s| s.to_string());

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
    pub async fn restore_native_services(
        &self,
        settings_cipher: &std::sync::Arc<crate::crypto::SettingsCipher>,
    ) {
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

        info!(
            "restore_native_services: znaleziono {} serwisow w DB",
            services.len()
        );
        for svc in &services {
            info!("  serwis '{}': config={}", svc.name, svc.config_json);
            let config: serde_json::Value =
                serde_json::from_str(&svc.config_json).unwrap_or_default();

            if config["deploy_mode"].as_str() != Some("native") {
                continue;
            }

            // Pomijaj serwisy oznaczone jako stopped
            if svc.status == "stopped" {
                debug!("Pomijam zatrzymany serwis '{}' (stopped)", svc.name);
                continue;
            }

            // MemoryGuard: paused = user wylaczyl autostart w GUI. Wpis serwisu
            // zostaje w DB i bedzie widoczny w Services jako gotowy do recznego
            // wlaczenia, ale nie spawnujemy procesu / nie ladujemy modelu.
            if svc.paused {
                debug!("Pomijam paused serwis '{}' (MemoryGuard)", svc.name);
                continue;
            }

            // Natywny python-bundle (vllm, vllm-metal, sglang, xtts, voxcpm, ...)
            // — proces HTTP server OpenAI-compatible uruchamiany z venva w cache.
            // Sprawdz czy poprzedni PID wciaz zyje; jesli tak — tylko rejestrujemy
            // backend (live rekonekcja). Jesli proces padl — relaunchujemy z
            // istniejacego venv (bez reinstall wheeli).
            if config["runtime"].as_str() == Some("python-bundle") {
                if let Err(e) = self
                    .restore_python_bundle_service(db, settings_cipher, &svc, &config)
                    .await
                {
                    warn!(
                        "restore python-bundle '{}': {:#} — pomijam, user moze wlaczyc recznie",
                        svc.name, e
                    );
                }
                continue;
            }

            // Natywny STT (whisper.cpp lub mlx-whisper) — oba wpinaja sie
            // w shared_stt_manager przez ten sam mechanizm `load_model`
            // z preferred_backend = engine_id.
            if svc.service_type == "stt" {
                let engine_id = config["engine"].as_str().unwrap_or("");
                if !matches!(engine_id, "whisper" | "mlx-whisper") {
                    continue;
                }
                let model_path_str = config["model_path"].as_str().unwrap_or("");
                let model_path = std::path::PathBuf::from(model_path_str);

                if !model_path.exists() {
                    warn!(
                        "Natywny STT '{}': sciezka modelu nie istnieje: {}",
                        svc.name,
                        model_path.display()
                    );
                    continue;
                }

                info!(
                    "Przywracanie natywnego STT '{}': engine={} model={}",
                    svc.name,
                    engine_id,
                    model_path.display()
                );

                let shared_stt = crate::stt::shared_stt_manager();
                let mp = model_path.clone();
                let preferred = engine_id.to_string();
                let load_result = tokio::task::spawn_blocking(move || {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async {
                        let mut mgr = shared_stt.write().await;
                        mgr.load_model(&mp, None, Some(&preferred)).await
                    })
                })
                .await;

                match load_result {
                    Ok(Ok(stt_info)) => {
                        info!("Przywrocono STT '{}' ({})", stt_info.name, stt_info.device);
                        let size_mb = stt_info.size_bytes / (1024 * 1024);
                        let model_label = config["deployed_model"]
                            .as_str()
                            .unwrap_or("whisper-large-v3-turbo")
                            .to_string();
                        self.register_native_service_in_mesh(
                            &svc.name,
                            "stt",
                            vec![model_label],
                            Some(stt_info.backend.clone()),
                            vec![size_mb],
                        );
                    }
                    Ok(Err(e)) => {
                        warn!("Blad ladowania STT '{}': {}", svc.name, e);
                    }
                    Err(e) => {
                        warn!("Blad tasku STT '{}': {}", svc.name, e);
                    }
                }
                continue;
            }

            // Natywny TTS embedded (apple-tts, kokoro). Rejestruje silnik
            // w shared_tts_manager() pod nazwa serwisu zeby router mogl
            // dispatche'owac /v1/audio/speech do tej instancji.
            if svc.service_type == "tts" {
                let engine_id = config["engine"].as_str().unwrap_or("");
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                if engine_id == "apple-tts" {
                    info!("Przywracanie Apple TTS '{}'", svc.name);
                    let mut e = crate::tts::apple_tts::AppleTtsEngine::new();
                    use crate::tts::TtsEngine;
                    if let Err(err) = e.load_model(std::path::Path::new("system")) {
                        warn!("Apple TTS restore '{}': {}", svc.name, err);
                    } else {
                        let shared = crate::tts::shared_tts_manager();
                        let mut mgr = shared.write().await;
                        mgr.register(svc.name.clone(), Box::new(e));
                        self.register_native_service_in_mesh(
                            &svc.name, "tts",
                            vec![config["deployed_model"].as_str().unwrap_or("zosia-pl").to_string()],
                            Some("apple-tts".to_string()), vec![0],
                        );
                    }
                    continue;
                }
                #[cfg(feature = "inference-sherpa")]
                if engine_id == "sherpa-onnx" {
                    // Restore embedded sherpa-onnx TTS po restarcie tentaflow.
                    // Czytamy sciezke do bundle z config_json (zapisana przy
                    // deployu) i ladujemy silnik do shared_tts_manager().
                    let model_path_str = config["model_path"].as_str().unwrap_or("");
                    let model_path = std::path::PathBuf::from(model_path_str);
                    if model_path_str.is_empty() || !model_path.exists() {
                        warn!(
                            "sherpa-onnx restore '{}': brak sciezki '{}' — pomijam",
                            svc.name, model_path_str
                        );
                        continue;
                    }
                    info!(
                        "Przywracanie sherpa-onnx TTS '{}': model={}",
                        svc.name,
                        model_path.display()
                    );
                    let mut e = crate::tts::sherpa::SherpaTtsEngine::new();
                    use crate::tts::TtsEngine;
                    match e.load_model(&model_path) {
                        Err(err) => {
                            warn!("sherpa-onnx restore '{}': {}", svc.name, err);
                        }
                        Ok(info) => {
                            let shared = crate::tts::shared_tts_manager();
                            let mut mgr = shared.write().await;
                            mgr.register(svc.name.clone(), Box::new(e));
                            self.register_native_service_in_mesh(
                                &svc.name,
                                "tts",
                                vec![config["deployed_model"]
                                    .as_str()
                                    .unwrap_or("sherpa-onnx")
                                    .to_string()],
                                Some("sherpa-onnx".to_string()),
                                vec![info.sample_rate as u64 / 1000],
                            );
                        }
                    }
                    continue;
                }
                #[cfg(feature = "inference-mlx-kokoro")]
                if engine_id == "kokoro" {
                    let model_path_str = config["model_path"].as_str().unwrap_or("");
                    let model_path = std::path::PathBuf::from(model_path_str);
                    if !model_path.exists() {
                        warn!("Kokoro restore '{}': brak sciezki {}", svc.name, model_path.display());
                        continue;
                    }
                    info!("Przywracanie Kokoro TTS '{}': model={}", svc.name, model_path.display());
                    let mut e = crate::tts::mlx_kokoro::MlxKokoroEngine::new();
                    use crate::tts::TtsEngine;
                    if let Err(err) = e.load_model(&model_path) {
                        warn!("Kokoro restore '{}': {}", svc.name, err);
                    } else {
                        let shared = crate::tts::shared_tts_manager();
                        let mut mgr = shared.write().await;
                        mgr.register(svc.name.clone(), Box::new(e));
                        self.register_native_service_in_mesh(
                            &svc.name, "tts",
                            vec![config["deployed_model"].as_str().unwrap_or("kokoro").to_string()],
                            Some("kokoro".to_string()), vec![0],
                        );
                    }
                    continue;
                }
                let _ = engine_id;
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
                    debug!(
                        "Sciezka nie istnieje, probuje przebudowac: {}",
                        rebuilt.display()
                    );
                    if rebuilt.exists() {
                        model_path = rebuilt;
                    }
                }
            }

            debug!(
                "model_path={}, exists={}",
                model_path.display(),
                model_path.exists()
            );
            if !model_path.exists() {
                warn!(
                    "Natywny serwis '{}': sciezka modelu nie istnieje: {}",
                    svc.name,
                    model_path.display()
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
            })
            .await;

            match load_result {
                Ok(Ok(model_info)) => {
                    info!(
                        "Przywrocono model '{}' ({}, {}MB, ctx={})",
                        model_info.name,
                        model_info.backend,
                        model_info.vram_used_mb,
                        model_info.context_length
                    );
                    let size_mb = model_info.size_bytes / (1024 * 1024);
                    self.register_native_service_in_mesh(
                        &svc.name,
                        "llm",
                        vec![model_id.clone()],
                        Some(model_info.backend.clone()),
                        vec![size_mb],
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
            self.service_manager
                .register_model_mapping(&model_id, &svc.name);
            self.service_manager
                .register_local_inference_model(&model_id);
            self.service_manager
                .register_local_inference_model(&svc.name);
            info!("Natywny serwis '{}' przywrocony pomyslnie", svc.name);
        }
    }

    /// Restore path dla `runtime=python-bundle`: jesli PID w config_json zyje
    /// i HTTP /v1/models odpowiada — tylko rejestruje backend w ServiceManager.
    /// Jesli PID martwy — wola python_venv::relaunch zeby spawnowac proces z
    /// cache'owanego venva (bez ponownej instalacji wheels) i aktualizuje
    /// PID w DB. Zwraca blad tylko gdy bundle nie da sie zrelaunchowac (np.
    /// cache venv usuniety) — wtedy caller powinien pozostawic serwis bez
    /// zmian i czekac na recznego reinstall z GUI.
    async fn restore_python_bundle_service(
        &self,
        db: &crate::db::DbPool,
        settings_cipher: &std::sync::Arc<crate::crypto::SettingsCipher>,
        svc: &crate::db::models::DbService,
        config: &serde_json::Value,
    ) -> anyhow::Result<()> {
        use crate::config::{ConnectionType, ServiceBackend};
        use crate::routing::backend::BackendClient;

        let engine_id = config["engine"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("brak config.engine"))?
            .to_string();
        let host_port = config["port"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("brak config.port"))? as u16;
        let instance_name = config["instance_name"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| svc.name.clone());
        let stored_pid = config["pid"].as_u64().map(|p| p as u32);

        // 1) Sprawdz czy stary PID jeszcze zyje. Proces python moze zyc dalej
        //    po crashu tentaflow (Unix fork survives parent exit).
        let alive = stored_pid
            .map(crate::deploy::process_ctl::is_alive)
            .unwrap_or(false);

        let pid_after_restore: u32 = if alive {
            info!(
                service = %svc.name, pid = stored_pid.unwrap_or(0),
                "python-bundle: poprzedni proces zyje — tylko rejestruje backend"
            );
            stored_pid.unwrap_or(0)
        } else {
            info!(
                service = %svc.name, engine = %engine_id,
                "python-bundle: PID martwy, relaunchuje z cache venv"
            );

            let hf_token = crate::db::repository::get_setting_secure(db, "hf_token", settings_cipher)
                .unwrap_or_default()
                .unwrap_or_default();
            let _ = crate::paths::ensure_models_dirs();
            let hf_home = crate::paths::hf_home();
            let torch_home = crate::paths::torch_home();

            let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            env.insert("PORT".to_string(), host_port.to_string());
            if let Some(model) = config["deployed_model"].as_str() {
                if !model.is_empty() {
                    env.insert("MODEL".to_string(), model.to_string());
                }
            }
            if !hf_token.is_empty() {
                env.insert("HF_TOKEN".to_string(), hf_token.clone());
                env.insert("HUGGING_FACE_HUB_TOKEN".to_string(), hf_token);
            }
            env.insert("HF_HOME".to_string(), hf_home.to_string_lossy().into_owned());
            env.insert(
                "HUGGINGFACE_HUB_CACHE".to_string(),
                hf_home.to_string_lossy().into_owned(),
            );
            env.insert(
                "TRANSFORMERS_CACHE".to_string(),
                hf_home.to_string_lossy().into_owned(),
            );
            env.insert(
                "TORCH_HOME".to_string(),
                torch_home.to_string_lossy().into_owned(),
            );

            let req = crate::deploy::python_venv::NativeDeployRequest {
                engine: engine_id.clone(),
                instance_name: Some(instance_name.clone()),
                env,
            };
            let running = tokio::task::spawn_blocking(move || {
                crate::deploy::python_venv::relaunch(&req)
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking relaunch: {}", e))??;

            let new_pid = running.child.id();
            std::mem::forget(running.child);

            // Zaktualizuj PID w config_json zeby nastepny restart tentaflow
            // mogl znow sprawdzic i zdecydowac czy relaunch nie jest potrzebny.
            let mut new_config = config.clone();
            new_config["pid"] = serde_json::Value::from(new_pid);
            let config_json = new_config.to_string();
            let strategy = "first_available";
            let model_category = if svc.service_type == "llm" {
                Some("llm")
            } else {
                None
            };
            crate::db::repository::update_service(
                db,
                svc.id,
                &svc.name,
                &svc.service_type,
                strategy,
                model_category,
                "active",
                &config_json,
            )?;
            new_pid
        };

        // 2) Rejestracja backendu HTTP — router zacznie routowac do procesu.
        // Dummy api_key bo BackendClient::new wymaga jakiegokolwiek tokenu do
        // budowy `Bearer` headera; lokalne silniki OSS go nie sprawdzaja.
        // model_name_override: vLLM zna model pod HF repo name (np.
        // "Qwen/Qwen3.5-0.8B"), a router dispatchuje pod service name (np.
        // "tentaflow-vllm-metal-2izlb"). Override podmienia nazwe przed wyslaniem.
        let model_override = config["deployed_model"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from);
        let base_url = format!("http://127.0.0.1:{}/v1", host_port);
        let sb = ServiceBackend {
            connection: ConnectionType::OpenAIApi {
                url: base_url.clone(),
                api_key: Some("sk-tentaflow-local".to_string()),
                api_key_env: None,
                extra_headers: vec![],
                custom_endpoint: None,
                request_format: None,
                tts_config: None,
            },
            max_concurrent: 50,
            timeout_ms: 600_000,
            weight: 1,
            model_name_override: model_override,
            health_check_path: Some("/v1/models".to_string()),
        };
        let client = BackendClient::new(sb, None)
            .map_err(|e| anyhow::anyhow!("BackendClient::new: {}", e))?;
        self.service_manager
            .register_dynamic_http_backend(&svc.name, std::sync::Arc::new(client));

        // Rejestracja w MemoryGuard zeby ensure_loaded przed kazdym dispatch
        // moglo restartowac serwis gdy padnie. Env (HF_TOKEN, HF_HOME itd.)
        // odbudowywane z DB tak jak w pierwszym deploy.
        let model_repo = config["deployed_model"]
            .as_str()
            .map(String::from)
            .unwrap_or_default();
        let hf_token = crate::db::repository::get_setting_secure(db, "hf_token", settings_cipher)
            .unwrap_or_default()
            .unwrap_or_default();
        let _ = crate::paths::ensure_models_dirs();
        let hf_home = crate::paths::hf_home();
        let torch_home = crate::paths::torch_home();
        let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        env.insert("PORT".to_string(), host_port.to_string());
        if !model_repo.is_empty() {
            env.insert("MODEL".to_string(), model_repo.clone());
        }
        if !hf_token.is_empty() {
            env.insert("HF_TOKEN".to_string(), hf_token.clone());
            env.insert("HUGGING_FACE_HUB_TOKEN".to_string(), hf_token);
        }
        env.insert("HF_HOME".to_string(), hf_home.to_string_lossy().into_owned());
        env.insert(
            "HUGGINGFACE_HUB_CACHE".to_string(),
            hf_home.to_string_lossy().into_owned(),
        );
        env.insert(
            "TRANSFORMERS_CACHE".to_string(),
            hf_home.to_string_lossy().into_owned(),
        );
        env.insert(
            "TORCH_HOME".to_string(),
            torch_home.to_string_lossy().into_owned(),
        );

        let vram = crate::memory::estimate_vram_for_model(&model_repo);
        let guard_engine = std::sync::Arc::new(crate::memory::PythonBundleEngine::new(
            engine_id.clone(),
            svc.name.clone(),
            instance_name.clone(),
            model_repo.clone(),
            host_port,
            vram,
            env,
            pid_after_restore,
        ));
        let auto_pin = is_orchestrator_model_for_guard(&engine_id, &model_repo);
        let already_paused = svc.status == "stopped" || svc.paused;
        let affinity = parse_gpu_affinity_from_config(&config);
        crate::memory::guard_global().register(
            svc.name.clone(),
            guard_engine,
            vram,
            auto_pin,
            already_paused,
            affinity,
        );

        info!(
            service = %svc.name, pid = pid_after_restore, url = %base_url,
            "python-bundle: restore OK, HTTP backend zarejestrowany"
        );
        Ok(())
    }

    /// Rejestruje natywny serwis in-process w mesh registry,
    /// zeby inne nody widzialy go i mogly forwardowac requesty.
    pub fn register_native_service_in_mesh(
        &self,
        service_name: &str,
        service_type: &str,
        models: Vec<String>,
        engine_id: Option<String>,
        model_sizes_mb: Vec<u64>,
    ) {
        let mesh_guard = self.mesh_manager.read();
        let mesh = match mesh_guard.as_ref() {
            Some(m) => m,
            None => return,
        };

        let node_id = mesh.node_id().to_string();
        let quic_port = self.config.mesh.as_ref().map(|m| m.port).unwrap_or(8090);

        let service_info = tentaflow_protocol::MeshServiceInfo {
            service_id: format!("native-{}-{}", service_type, service_name),
            service_name: service_name.to_string(),
            service_type: service_type.to_string(),
            node_id,
            quic_port,
            quic_url: format!("quic://0.0.0.0:{}", quic_port),
            status: "running".to_string(),
            models,
            load_percent: 0,
            engine_id,
            model_sizes_mb,
        };

        mesh.service_registry().register_local(service_info);
        info!(
            "Zarejestrowano natywny serwis '{}' ({}) w mesh",
            service_name, service_type
        );
    }

    /// Wyrejestrowuje natywny serwis z mesh registry.
    pub fn unregister_native_service_from_mesh(&self, service_id: &str) {
        let mesh_guard = self.mesh_manager.read();
        if let Some(mesh) = mesh_guard.as_ref() {
            mesh.service_registry().unregister_local(service_id);
            info!("Wyrejestrowano natywny serwis '{}' z mesh", service_id);
        }
    }

    /// Pobierz backend clients dla serwisu (HTTP backends — statyczne lub dynamiczne)
    pub(crate) fn get_service_backends(
        &self,
        service_name: &str,
    ) -> Option<&Vec<Arc<BackendClient>>> {
        self.service_manager.get_service_backends(service_name)
    }

    /// Pobierz backend clients (statyczne lub dynamiczne) — klonuje Arc referencje
    pub(crate) fn get_service_backends_cloned(
        &self,
        service_name: &str,
    ) -> Option<Vec<Arc<BackendClient>>> {
        self.service_manager
            .get_service_backends_cloned(service_name)
    }

    /// Sprawdza czy serwis ma HTTP backends (statyczne lub dynamiczne)
    pub(crate) fn has_http_backends(&self, service_name: &str) -> bool {
        self.service_manager.has_http_backends(service_name)
    }

    /// Wybiera backend HTTP (statyczny lub dynamiczny) i wykonuje na nim operacje.
    /// Obsluguje oba typy rejestrow transparentnie.
    pub(crate) fn select_http_backend(&self, service_name: &str) -> Option<Arc<BackendClient>> {
        // Statyczny rejestr
        if let Some(backends) = self.service_manager.get_service_backends(service_name) {
            if !backends.is_empty() {
                if let Some(strategy) = self.service_manager.get_strategy(service_name) {
                    if let Ok(idx) = strategy.select_backend(backends) {
                        return Some(backends[idx].clone());
                    }
                }
                return Some(backends[0].clone());
            }
        }
        // Dynamiczny rejestr
        if let Some(entry) = self.service_manager.dynamic_backends.get(service_name) {
            let (backends, strategy) = entry.value();
            if !backends.is_empty() {
                if let Ok(idx) = strategy.select_backend(backends) {
                    return Some(backends[idx].clone());
                }
                return Some(backends[0].clone());
            }
        }
        None
    }

    /// Pobierz load balancing strategy dla serwisu
    pub(crate) fn get_strategy(
        &self,
        service_name: &str,
    ) -> Option<&Box<dyn LoadBalancingStrategy>> {
        self.service_manager.get_strategy(service_name)
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

    /// Probuje forwardowac ModelRequest do innego noda w mesh.
    /// Szuka noda ktory ma serwis danego typu i model, serializuje request
    /// przez rkyv i wysyla przez mesh forward. Zwraca zdeserializowana odpowiedz.
    pub(crate) async fn route_via_mesh(
        &self,
        service_type: &str,
        model_name: Option<&str>,
        request: &tentaflow_protocol::ModelRequest,
    ) -> Option<tentaflow_protocol::ModelResponse> {
        use crate::routing::service_manager::ServiceLocation;

        let target_node_id = if let Some(model) = model_name {
            match self.service_manager.find_service(service_type, model) {
                Some(ServiceLocation::MeshNode { node_id }) => node_id,
                _ => match self.service_manager.find_service_by_type(service_type) {
                    Some(ServiceLocation::MeshNode { node_id }) => node_id,
                    _ => return None,
                },
            }
        } else {
            match self.service_manager.find_service_by_type(service_type) {
                Some(ServiceLocation::MeshNode { node_id }) => node_id,
                _ => return None,
            }
        };

        let request_bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(request) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!("Mesh forward: blad serializacji ModelRequest: {}", e);
                return None;
            }
        };

        debug!(
            "Mesh forward: typ='{}' model={:?} -> node '{}'",
            service_type, model_name, target_node_id
        );

        match self
            .route_through_mesh(&target_node_id, &request_bytes)
            .await
        {
            Ok(response_bytes) => {
                match rkyv::access::<tentaflow_protocol::ArchivedModelResponse, rkyv::rancor::Error>(
                    &response_bytes,
                ) {
                    Ok(archived_resp) => {
                        match rkyv::deserialize::<
                            tentaflow_protocol::ModelResponse,
                            rkyv::rancor::Error,
                        >(archived_resp)
                        {
                            Ok(response) => {
                                debug!(
                                    "Mesh forward: odpowiedz z noda '{}' odebrana",
                                    target_node_id
                                );
                                Some(response)
                            }
                            Err(e) => {
                                warn!(
                                    "Mesh forward: blad deserializacji odpowiedzi z noda '{}': {}",
                                    target_node_id, e
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Mesh forward: blad dostepu do archived odpowiedzi z noda '{}': {}",
                            target_node_id, e
                        );
                        None
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Mesh forward: blad forwardowania do noda '{}': {}",
                    target_node_id, e
                );
                None
            }
        }
    }

    // ========================================================================
    // ALIAS RETENTAFLOWN
    // ========================================================================

    /// Rozwiazuje alias modelu na canonical name (config.toml aliasy).
    pub(crate) fn resolve_model_alias(&self, model: &str) -> String {
        for alias in &self.config.service_aliases {
            if alias.alias == model {
                debug!("Alias resolved: {} -> {}", model, alias.target);
                return alias.target.clone();
            }
        }
        model.to_string()
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

/// Wyciaga GpuAffinity z config_json restorowanego serwisu. Mirror
/// `runner::parse_gpu_affinity` ale dla serde_json::Value.
fn parse_gpu_affinity_from_config(config: &serde_json::Value) -> crate::memory::GpuAffinity {
    use crate::memory::GpuAffinity;
    let arr = match config.get("gpu_ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => return GpuAffinity::All,
    };
    let ids: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if ids.iter().any(|s| s.eq_ignore_ascii_case("all")) {
        return GpuAffinity::All;
    }
    if ids.iter().any(|s| s.eq_ignore_ascii_case("cpu")) {
        return GpuAffinity::Cpu;
    }
    let parsed: Vec<usize> = ids.iter().filter_map(|s| s.parse().ok()).collect();
    match parsed.len() {
        0 => GpuAffinity::All,
        1 => GpuAffinity::Single(parsed[0]),
        _ => GpuAffinity::Multi(parsed),
    }
}

/// Lokalna heurystyka — auto-pin orchestrator/STT/TTS w MemoryGuard przy
/// restore. Duplikat `runner::is_orchestrator_model` zeby uniknac cyklu
/// zaleznosci (router → deploy → router).
fn is_orchestrator_model_for_guard(engine_id: &str, model_repo: &str) -> bool {
    let m = model_repo.to_ascii_lowercase();
    let e = engine_id.to_ascii_lowercase();
    if m.contains("qwen3.5-0.8b") || m.contains("qwen3-5-0-8b") {
        return true;
    }
    if e == "whisper" || e == "sherpa-onnx" {
        return true;
    }
    false
}
