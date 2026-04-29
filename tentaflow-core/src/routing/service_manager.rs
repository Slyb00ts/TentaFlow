// =============================================================================
// Plik: routing/service_manager.rs
// Opis: Asynchroniczny manager polaczen serwisow. Zarzadza cyklem zycia
//       polaczen QUIC (RAG, Embeddings, LLM, TTS, STT, Memory) oraz
//       backendami HTTP. Zapewnia rownolegle nawiazywanie polaczen,
//       background reconnect i lock-free odczyt stanu serwisow.
// =============================================================================

use crate::config::{ConnectionType, RouterConfig, ServiceType};
use crate::error::Result;
use crate::routing::backend::BackendClient;
use crate::routing::loadbalancer::{create_strategy, CircuitBreakerConfig, LoadBalancingStrategy};

/// Tworzy konfiguracje circuit breakera na podstawie ustawien routera
fn make_circuit_breaker_config(config: &RouterConfig) -> Option<CircuitBreakerConfig> {
    if config.load_balancing.circuit_breaker_enabled {
        Some(CircuitBreakerConfig {
            threshold: config.load_balancing.circuit_breaker_threshold,
            timeout_ms: config.load_balancing.circuit_breaker_timeout_ms,
            half_open_max_calls: 1,
        })
    } else {
        None
    }
}

// TODO: Przeniesc RAGClient i RAGEngineConfigCompat do crate::services::rag::client
use crate::services::rag::client::{RAGClient, RAGEngineConfigCompat};
// TODO: Przeniesc TTSClient i TTSConfigCompat do crate::services::tts::client
use crate::services::tts::client::{TTSClient, TTSConfigCompat};
// TODO: Przeniesc SharedPromptRegistry i create_shared_registry do crate::prompt_registry
use crate::prompt_registry::{create_shared_registry, SharedPromptRegistry};

use crate::mesh::service_registry::MeshServiceRegistry;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, info, warn};

const MAX_HISTORY_MESSAGES: usize = 50;
const SESSION_TTL_SECS: u64 = 3600;

/// Pojedyncza wiadomosc w historii konwersacji per sesja.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    pub timestamp: Instant,
}

struct SessionHistory {
    messages: VecDeque<ConversationMessage>,
    last_activity: Instant,
}

impl SessionHistory {
    fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            last_activity: Instant::now(),
        }
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push_back(ConversationMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Instant::now(),
        });
        self.last_activity = Instant::now();
        if self.messages.len() > MAX_HISTORY_MESSAGES {
            self.messages.pop_front();
        }
    }

    fn is_expired(&self) -> bool {
        self.last_activity.elapsed() > Duration::from_secs(SESSION_TTL_SECS)
    }
}

/// Cache historii konwersacji per session_id — uzywany wylacznie przez
/// flow_engine adapter `conversation_history`. Nie wstrzykuje historii
/// automatycznie w request; to robi user-defined flow.
pub struct ConversationCache {
    sessions: RwLock<HashMap<String, SessionHistory>>,
}

impl ConversationCache {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn add_message(&self, session_id: &str, role: &str, content: &str) {
        let mut sessions = self.sessions.write().await;
        let history = sessions
            .entry(session_id.to_string())
            .or_insert_with(SessionHistory::new);
        history.add_message(role, content);
        debug!(
            "ConversationCache: added {} message to session {}, total: {}",
            role,
            session_id,
            history.messages.len()
        );
    }

    pub async fn get_history(&self, session_id: &str) -> Vec<ConversationMessage> {
        let sessions = self.sessions.read().await;
        sessions
            .get(session_id)
            .map(|h| h.messages.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn cleanup_expired(&self) {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, h| !h.is_expired());
        let removed = before - sessions.len();
        if removed > 0 {
            debug!("ConversationCache: cleaned up {} expired sessions", removed);
        }
    }
}

impl Default for ConversationCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// LOKALIZACJA SERWISU
// ============================================================================

/// Lokalizacja serwisu — lokalny lub na zdalnym nodzie mesh
#[derive(Debug, Clone)]
pub enum ServiceLocation {
    /// Serwis dostepny lokalnie
    Local,
    /// Serwis na zdalnym nodzie w mesh
    MeshNode { node_id: String },
}

// ============================================================================
// TYPY STANOW SERWISOW
// ============================================================================

/// Stan polaczenia serwisu QUIC (RAG, Embeddings)
#[derive(Debug, Clone)]
pub enum QuicServiceState {
    /// Trwa laczenie (background task dziala)
    Connecting,
    /// Polaczony i gotowy do uzycia
    Connected,
    /// Rozlaczony (bedzie proba reconnect)
    Disconnected { reason: String },
    /// Blad konfiguracji (nie bedzie prob reconnect)
    ConfigError { message: String },
}

impl QuicServiceState {
    pub fn is_available(&self) -> bool {
        matches!(self, QuicServiceState::Connected)
    }
}

/// Wrapper dla RAG client z stanem
pub struct RAGServiceHandle {
    pub state: RwLock<QuicServiceState>,
    pub client: RwLock<Option<Arc<RAGClient>>>,
    pub config: RAGEngineConfigCompat,
    /// Sygnal shutdown per serwis
    shutdown_tx: watch::Sender<bool>,
    pub shutdown_rx: watch::Receiver<bool>,
}

impl RAGServiceHandle {
    pub fn new(config: RAGEngineConfigCompat) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            state: RwLock::new(QuicServiceState::Connecting),
            client: RwLock::new(None),
            config,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Wyslij sygnal shutdown do tego serwisu
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Sprawdz czy serwis jest dostepny (non-blocking read)
    pub async fn is_available(&self) -> bool {
        self.state.read().await.is_available()
    }

    /// Pobierz klienta jesli dostepny (non-blocking read)
    pub async fn get_client(&self) -> Option<Arc<RAGClient>> {
        self.client.read().await.clone()
    }

    /// Ustaw stan connected z klientem
    pub async fn set_connected(&self, client: Arc<RAGClient>) {
        *self.client.write().await = Some(client);
        *self.state.write().await = QuicServiceState::Connected;
    }

    /// Ustaw stan disconnected
    pub async fn set_disconnected(&self, reason: String) {
        *self.client.write().await = None;
        *self.state.write().await = QuicServiceState::Disconnected { reason };
    }
}

/// Uniwersalny wrapper dla QUIC client z stanem.
/// Uzywany dla: Embeddings, TTS, i innych serwisow QUIC.
pub struct QuicServiceHandle {
    pub state: RwLock<QuicServiceState>,
    pub client: RwLock<Option<Arc<crate::net::quic::QuicClient>>>,
    pub config: crate::net::quic::QuicConfig,
    /// Sygnal shutdown per serwis
    shutdown_tx: watch::Sender<bool>,
    pub shutdown_rx: watch::Receiver<bool>,
}

impl QuicServiceHandle {
    pub fn new(config: crate::net::quic::QuicConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            state: RwLock::new(QuicServiceState::Connecting),
            client: RwLock::new(None),
            config,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Wyslij sygnal shutdown do tego serwisu
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub async fn is_available(&self) -> bool {
        self.state.read().await.is_available()
    }

    pub async fn get_client(&self) -> Option<Arc<crate::net::quic::QuicClient>> {
        self.client.read().await.clone()
    }

    pub async fn set_connected(&self, client: Arc<crate::net::quic::QuicClient>) {
        let prev = self.state.read().await.clone();
        *self.client.write().await = Some(client);
        *self.state.write().await = QuicServiceState::Connected;
        // Emituj tylko na realna zmiane stanu (Connecting/Disconnected → Connected).
        if !matches!(prev, QuicServiceState::Connected) {
            crate::dispatch::system_event_broadcast::publish_service_status(
                &self.config.name,
                &service_type_from_alpn(&self.config.alpn),
                "connected",
                "",
            );
        }
    }

    pub async fn set_disconnected(&self, reason: String) {
        let prev = self.state.read().await.clone();
        *self.client.write().await = None;
        *self.state.write().await = QuicServiceState::Disconnected {
            reason: reason.clone(),
        };
        if !matches!(prev, QuicServiceState::Disconnected { .. }) {
            crate::dispatch::system_event_broadcast::publish_service_status(
                &self.config.name,
                &service_type_from_alpn(&self.config.alpn),
                "disconnected",
                &reason,
            );
        }
    }

    pub async fn shutdown_client_and_mark_disconnected(&self, reason: &str) {
        let client = self.client.write().await.take();
        if let Some(client) = client {
            client.shutdown().await;
        }
        self.set_disconnected(reason.to_string()).await;
    }
}

/// Z ALPN typu "tentaflow-llm" / "tentaflow-tts" wyjmij czysta kategorie.
fn service_type_from_alpn(alpn: &str) -> String {
    alpn.strip_prefix("tentaflow-").unwrap_or(alpn).to_string()
}

// ============================================================================
// MODEL POOL - mapowanie model_name -> lista serwisow
// ============================================================================

/// Strategia wyboru serwisu z puli modelu
#[derive(Debug, Clone, Copy)]
pub enum PoolStrategy {
    FirstAvailable,
    RoundRobin,
    LeastLoaded,
}

impl std::fmt::Display for PoolStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolStrategy::FirstAvailable => write!(f, "first_available"),
            PoolStrategy::RoundRobin => write!(f, "round_robin"),
            PoolStrategy::LeastLoaded => write!(f, "least_loaded"),
        }
    }
}

impl PoolStrategy {
    /// Parsuje string na strategie. Domyslnie: FirstAvailable.
    pub fn parse(s: &str) -> PoolStrategy {
        match s {
            "round_robin" => PoolStrategy::RoundRobin,
            "least_loaded" => PoolStrategy::LeastLoaded,
            "first_available" => PoolStrategy::FirstAvailable,
            _ => PoolStrategy::FirstAvailable,
        }
    }
}

/// Wpis puli serwisow obslugujacych dany model
pub struct ModelPoolEntry {
    pub service_names: Vec<String>,
    pub strategy: PoolStrategy,
    pub service_type: String,
    counter: std::sync::atomic::AtomicUsize,
}

impl ModelPoolEntry {
    pub fn new() -> Self {
        Self {
            service_names: Vec::new(),
            strategy: PoolStrategy::RoundRobin,
            service_type: "llm".to_string(),
            counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Wybierz nastepny serwis (round-robin)
    pub fn next_service(&self) -> Option<&str> {
        if self.service_names.is_empty() {
            return None;
        }
        let idx = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.service_names.len();
        Some(&self.service_names[idx])
    }
}

// ============================================================================
// SERVICE MANAGER
// ============================================================================

/// Manager wszystkich serwisow z asynchronicznym lifecycle
pub struct ServiceManager {
    /// Konfiguracja
    pub config: Arc<RouterConfig>,

    /// RAG serwisy (QUIC) - nazwa -> handle. DashMap: lock-free per-shard.
    pub rag_services: dashmap::DashMap<String, Arc<RAGServiceHandle>>,

    /// QUIC Embedding serwisy - nazwa -> handle
    pub quic_embedding_services: dashmap::DashMap<String, Arc<QuicServiceHandle>>,

    /// OpenAI API backends (LLM, Vision, STT, Embedding) - synchroniczne, nie wymagaja polaczenia
    pub service_backends: HashMap<String, Vec<Arc<BackendClient>>>,

    /// Dynamicznie rejestrowane HTTP backends (po deploy kontenera). Zawiera
    /// tuple z Box<dyn LoadBalancingStrategy> — LoadBalancingStrategy musi byc
    /// Send+Sync dla DashMap.
    pub dynamic_backends:
        dashmap::DashMap<String, (Vec<Arc<BackendClient>>, Box<dyn LoadBalancingStrategy>)>,

    /// Load balancing strategies
    pub load_balancing_strategies: HashMap<String, Box<dyn LoadBalancingStrategy>>,

    /// TTS clients (HTTP, nie wymagaja polaczenia)
    pub tts_clients: HashMap<String, Arc<TTSClient>>,

    /// QUIC TTS serwisy - nazwa -> handle
    pub quic_tts_services: dashmap::DashMap<String, Arc<QuicServiceHandle>>,

    /// QUIC LLM serwisy - nazwa -> handle
    pub quic_llm_services: dashmap::DashMap<String, Arc<QuicServiceHandle>>,

    /// Kategorie modeli LLM (dla KV Cache) - nazwa -> kategoria
    pub llm_model_categories: dashmap::DashMap<String, crate::config::LlmModelCategory>,

    /// QUIC STT serwisy - nazwa -> handle
    pub quic_stt_services: dashmap::DashMap<String, Arc<QuicServiceHandle>>,

    /// QUIC Memory serwisy - nazwa -> handle
    pub quic_memory_services: dashmap::DashMap<String, Arc<QuicServiceHandle>>,

    /// Pula serwisow per model: model_name -> ModelPoolEntry
    pub model_pool: dashmap::DashMap<String, ModelPoolEntry>,

    /// Modele obslugiwane przez lokalna inferencje in-process (MLX, llama.cpp).
    /// DashMap<K, ()> zamiast HashSet — lock-free contains_key.
    pub local_inference_models: dashmap::DashMap<String, ()>,

    /// Callback channel dla RAG
    callback_tx: mpsc::UnboundedSender<(
        tentaflow_protocol::ModelRequest,
        mpsc::Sender<tentaflow_protocol::ModelResponse>,
    )>,
    callback_rx: Arc<
        tokio::sync::Mutex<
            mpsc::UnboundedReceiver<(
                tentaflow_protocol::ModelRequest,
                mpsc::Sender<tentaflow_protocol::ModelResponse>,
            )>,
        >,
    >,

    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,
    /// Publiczny zeby zewnetrzne serwery (unified_server) mogly zasubskrybowac
    /// shutdown i zamknac swoje loopy bez zostawiania otwartych portow.
    pub shutdown_rx: watch::Receiver<bool>,

    /// Prompt Registry dla KV cache
    pub prompt_registry: SharedPromptRegistry,

    /// Shared cache historii konwersacji
    pub conversation_cache: Arc<ConversationCache>,

    /// Rejestr serwisow mesh — do wyszukiwania serwisow na zdalnych nodach
    pub mesh_registry: parking_lot::RwLock<Option<Arc<MeshServiceRegistry>>>,

    /// Router dla odwrotnych requestow od kontenerow (ustawiany po konstrukcji Routera)
    pub(crate) reverse_router: parking_lot::RwLock<Option<crate::routing::Router>>,

    /// EventBus addonow — ustawiany po konstrukcji AddonManager (jak reverse_router)
    pub(crate) event_bus: parking_lot::RwLock<Option<Arc<crate::addon::event_bus::EventBus>>>,

    /// Snapshot receiver wired in by `Router::set_services_snapshot_rx`. Read
    /// path consults `current_snapshot()` which falls back to an empty
    /// `ServicesSnapshot::default()` when unwired (legacy startup, tests).
    snapshot_rx: parking_lot::RwLock<
        Option<tokio::sync::watch::Receiver<Arc<crate::services::supervisor::ServicesSnapshot>>>,
    >,
}

impl ServiceManager {
    /// Tworzy ServiceManager i NATYCHMIAST zwraca.
    /// Wszystkie polaczenia QUIC sa uruchamiane w background taskach.
    pub fn new(config: Arc<RouterConfig>, db_pool: Option<crate::db::DbPool>) -> Result<Self> {
        let (callback_tx, callback_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut service_backends: HashMap<String, Vec<Arc<BackendClient>>> = HashMap::new();
        let mut load_balancing_strategies: HashMap<String, Box<dyn LoadBalancingStrategy>> =
            HashMap::new();
        let mut rag_services: HashMap<String, Arc<RAGServiceHandle>> = HashMap::new();
        let mut quic_embedding_services: HashMap<String, Arc<QuicServiceHandle>> = HashMap::new();
        let mut tts_clients: HashMap<String, Arc<TTSClient>> = HashMap::new();
        let mut quic_tts_services: HashMap<String, Arc<QuicServiceHandle>> = HashMap::new();
        let mut quic_llm_services: HashMap<String, Arc<QuicServiceHandle>> = HashMap::new();
        let mut llm_model_categories: HashMap<String, crate::config::LlmModelCategory> =
            HashMap::new();
        let mut quic_stt_services: HashMap<String, Arc<QuicServiceHandle>> = HashMap::new();
        let mut quic_memory_services: HashMap<String, Arc<QuicServiceHandle>> = HashMap::new();
        let model_pool: HashMap<String, ModelPoolEntry> = HashMap::new();

        // Inicjalizacja Prompt Registry dla KV cache (z fallbackiem do DB)
        let prompt_registry = create_shared_registry(db_pool);
        info!(
            "PromptRegistry: Zaladowano {} promptow",
            prompt_registry.all_ids().len()
        );

        let conversation_cache = Arc::new(ConversationCache::new());

        info!("ServiceManager: Inicjalizacja (non-blocking)...");

        for service in &config.services {
            match service.service_type {
                // ===== Vision - OpenAI API only (synchroniczne, nie wymaga polaczenia) =====
                ServiceType::Vision => {
                    let mut clients = Vec::new();

                    for backend in &service.backends {
                        if let ConnectionType::OpenAIApi { .. } = &backend.connection {
                            let client = BackendClient::new(
                                backend.clone(),
                                make_circuit_breaker_config(&config),
                            )?;
                            clients.push(Arc::new(client));
                        }
                    }

                    if !clients.is_empty() {
                        let weights: Vec<u32> = service.backends.iter().map(|b| b.weight).collect();
                        let strategy = create_strategy(&service.strategy, &clients, weights)?;

                        service_backends.insert(service.name.clone(), clients);
                        load_balancing_strategies.insert(service.name.clone(), strategy);

                        info!(
                            "  {} (Vision) - {} backends ready",
                            service.name,
                            service.backends.len()
                        );
                    }
                }

                // ===== LLM - moze byc OpenAI API lub QUIC =====
                ServiceType::LLM => {
                    let mut clients = Vec::new();

                    for backend in &service.backends {
                        match &backend.connection {
                            ConnectionType::OpenAIApi { .. } => {
                                let client = BackendClient::new(
                                    backend.clone(),
                                    make_circuit_breaker_config(&config),
                                )?;
                                clients.push(Arc::new(client));
                            }
                            ConnectionType::QUIC {
                                quic_url,
                                tls_ca,
                                auto_reconnect,
                                reconnect_interval_ms,
                                keepalive_interval_ms,
                                ..
                            } => {
                                let quic_config = crate::net::quic::QuicConfig {
                                    name: service.name.clone(),
                                    url: quic_url.clone(),
                                    tls_ca: tls_ca.clone(),
                                    server_name: None,
                                    alpn: "h3".to_string(),
                                    timeout_ms: backend.timeout_ms,
                                    auto_reconnect: *auto_reconnect,
                                    reconnect_interval_ms: *reconnect_interval_ms,
                                    keepalive_interval_ms: *keepalive_interval_ms,
                                    skip_tls_verify: false,
                                    direct_addrs: Vec::new(),
                                };

                                let handle = Arc::new(QuicServiceHandle::new(quic_config));
                                quic_llm_services.insert(service.name.clone(), handle);
                                llm_model_categories
                                    .insert(service.name.clone(), service.model_category);

                                info!(
                                    "  {} (LLM QUIC, category: {:?}) - connecting in background...",
                                    service.name, service.model_category
                                );
                                break;
                            }
                        }
                    }

                    if !clients.is_empty() {
                        let weights: Vec<u32> = service.backends.iter().map(|b| b.weight).collect();
                        let strategy = create_strategy(&service.strategy, &clients, weights)?;
                        let clients_count = clients.len();

                        service_backends.insert(service.name.clone(), clients);
                        load_balancing_strategies.insert(service.name.clone(), strategy);

                        info!(
                            "  {} (LLM OpenAI) - {} backends ready",
                            service.name, clients_count
                        );
                    }
                }

                // ===== STT - moze byc OpenAI API lub QUIC =====
                ServiceType::STT => {
                    let mut clients = Vec::new();

                    for backend in &service.backends {
                        match &backend.connection {
                            ConnectionType::OpenAIApi { .. } => {
                                let client = BackendClient::new(
                                    backend.clone(),
                                    make_circuit_breaker_config(&config),
                                )?;
                                clients.push(Arc::new(client));
                            }
                            ConnectionType::QUIC {
                                quic_url,
                                tls_ca,
                                auto_reconnect,
                                reconnect_interval_ms,
                                keepalive_interval_ms,
                                ..
                            } => {
                                let quic_config = crate::net::quic::QuicConfig {
                                    name: service.name.clone(),
                                    url: quic_url.clone(),
                                    tls_ca: tls_ca.clone(),
                                    server_name: None,
                                    alpn: "h3".to_string(),
                                    timeout_ms: backend.timeout_ms,
                                    auto_reconnect: *auto_reconnect,
                                    reconnect_interval_ms: *reconnect_interval_ms,
                                    keepalive_interval_ms: *keepalive_interval_ms,
                                    skip_tls_verify: false,
                                    direct_addrs: Vec::new(),
                                };

                                let handle = Arc::new(QuicServiceHandle::new(quic_config));
                                quic_stt_services.insert(service.name.clone(), handle);

                                info!(
                                    "  {} (STT QUIC) - connecting in background...",
                                    service.name
                                );
                                break;
                            }
                        }
                    }

                    if !clients.is_empty() {
                        let weights: Vec<u32> = service.backends.iter().map(|b| b.weight).collect();
                        let strategy = create_strategy(&service.strategy, &clients, weights)?;
                        let clients_count = clients.len();

                        service_backends.insert(service.name.clone(), clients);
                        load_balancing_strategies.insert(service.name.clone(), strategy);

                        info!(
                            "  {} (STT OpenAI) - {} backends ready",
                            service.name, clients_count
                        );
                    }
                }

                // ===== RAG - QUIC (asynchroniczne, background connection) =====
                ServiceType::RAG => {
                    for backend in &service.backends {
                        if let ConnectionType::QUIC {
                            quic_url,
                            tls_ca,
                            auto_reconnect,
                            reconnect_interval_ms,
                            keepalive_interval_ms,
                            ..
                        } = &backend.connection
                        {
                            let rag_config = RAGEngineConfigCompat {
                                name: service.name.clone(),
                                quic_url: quic_url.clone(),
                                tls_ca: tls_ca.clone(),
                                max_concurrent: backend.max_concurrent,
                                timeout_ms: backend.timeout_ms,
                                auto_reconnect: *auto_reconnect,
                                reconnect_interval_ms: *reconnect_interval_ms,
                                keepalive_interval_ms: *keepalive_interval_ms,
                            };

                            let handle = Arc::new(RAGServiceHandle::new(rag_config));
                            rag_services.insert(service.name.clone(), handle);

                            info!("  {} (RAG) - connecting in background...", service.name);
                            break;
                        }
                    }
                }

                // ===== Embedding - moze byc OpenAI API lub QUIC =====
                ServiceType::Embedding => {
                    let mut clients = Vec::new();

                    for backend in &service.backends {
                        match &backend.connection {
                            ConnectionType::OpenAIApi { .. } => {
                                let client = BackendClient::new(
                                    backend.clone(),
                                    make_circuit_breaker_config(&config),
                                )?;
                                clients.push(Arc::new(client));
                            }
                            ConnectionType::QUIC {
                                quic_url,
                                tls_ca,
                                auto_reconnect,
                                reconnect_interval_ms,
                                keepalive_interval_ms,
                                ..
                            } => {
                                let quic_config = crate::net::quic::QuicConfig {
                                    name: service.name.clone(),
                                    url: quic_url.clone(),
                                    tls_ca: tls_ca.clone(),
                                    server_name: None,
                                    alpn: "h3".to_string(),
                                    timeout_ms: backend.timeout_ms,
                                    auto_reconnect: *auto_reconnect,
                                    reconnect_interval_ms: *reconnect_interval_ms,
                                    keepalive_interval_ms: *keepalive_interval_ms,
                                    skip_tls_verify: false,
                                    direct_addrs: Vec::new(),
                                };

                                let handle = Arc::new(QuicServiceHandle::new(quic_config));
                                quic_embedding_services.insert(service.name.clone(), handle);

                                info!(
                                    "  {} (Embedding QUIC) - connecting in background...",
                                    service.name
                                );
                                break;
                            }
                        }
                    }

                    if !clients.is_empty() {
                        let weights: Vec<u32> = service.backends.iter().map(|b| b.weight).collect();
                        let strategy = create_strategy(&service.strategy, &clients, weights)?;
                        let clients_count = clients.len();

                        service_backends.insert(service.name.clone(), clients);
                        load_balancing_strategies.insert(service.name.clone(), strategy);

                        info!(
                            "  {} (Embedding OpenAI) - {} backends ready",
                            service.name, clients_count
                        );
                    }
                }

                // ===== TTS - moze byc OpenAI API (HTTP) lub QUIC =====
                ServiceType::TTS => {
                    for backend in &service.backends {
                        match &backend.connection {
                            // HTTP backend (OpenAI TTS API)
                            ConnectionType::OpenAIApi {
                                url,
                                api_key,
                                api_key_env,
                                tts_config,
                                ..
                            } => {
                                let tts_cfg = TTSConfigCompat {
                                    url: url.clone(),
                                    api_key: api_key.clone(),
                                    api_key_env: api_key_env.clone(),
                                    model: tts_config
                                        .as_ref()
                                        .map(|c| c.model.clone())
                                        .unwrap_or_else(|| "tts-1".to_string()),
                                    voice: tts_config
                                        .as_ref()
                                        .map(|c| c.voice.clone())
                                        .unwrap_or_else(|| "alloy".to_string()),
                                    response_format: tts_config
                                        .as_ref()
                                        .map(|c| c.response_format.clone())
                                        .unwrap_or_else(|| "opus".to_string()),
                                    speed: tts_config.as_ref().map(|c| c.speed).unwrap_or(1.0),
                                    timeout_ms: backend.timeout_ms,
                                };

                                let client = TTSClient::new(tts_cfg)?;
                                tts_clients.insert(service.name.clone(), Arc::new(client));

                                info!("  {} (TTS HTTP) - ready", service.name);
                                break;
                            }
                            // QUIC backend (TentaFlow.TTS z rkyv)
                            ConnectionType::QUIC {
                                quic_url,
                                tls_ca,
                                auto_reconnect,
                                reconnect_interval_ms,
                                keepalive_interval_ms,
                                ..
                            } => {
                                let quic_config = crate::net::quic::QuicConfig {
                                    name: service.name.clone(),
                                    url: quic_url.clone(),
                                    tls_ca: tls_ca.clone(),
                                    server_name: None,
                                    alpn: "h3".to_string(),
                                    timeout_ms: backend.timeout_ms,
                                    auto_reconnect: *auto_reconnect,
                                    reconnect_interval_ms: *reconnect_interval_ms,
                                    keepalive_interval_ms: *keepalive_interval_ms,
                                    skip_tls_verify: false,
                                    direct_addrs: Vec::new(),
                                };

                                let handle = Arc::new(QuicServiceHandle::new(quic_config));
                                quic_tts_services.insert(service.name.clone(), handle);

                                info!(
                                    "  {} (TTS QUIC) - connecting in background...",
                                    service.name
                                );
                                break;
                            }
                        }
                    }
                }

                // ===== Memory - QUIC only (graf wiedzy, entity storage) =====
                ServiceType::Memory => {
                    for backend in &service.backends {
                        if let ConnectionType::QUIC {
                            quic_url,
                            tls_ca,
                            auto_reconnect,
                            reconnect_interval_ms,
                            keepalive_interval_ms,
                            ..
                        } = &backend.connection
                        {
                            let quic_config = crate::net::quic::QuicConfig {
                                name: service.name.clone(),
                                url: quic_url.clone(),
                                tls_ca: tls_ca.clone(),
                                server_name: None,
                                alpn: "h3".to_string(),
                                timeout_ms: backend.timeout_ms,
                                auto_reconnect: *auto_reconnect,
                                reconnect_interval_ms: *reconnect_interval_ms,
                                keepalive_interval_ms: *keepalive_interval_ms,
                                skip_tls_verify: false,
                                direct_addrs: Vec::new(),
                            };

                            let handle = Arc::new(QuicServiceHandle::new(quic_config));
                            quic_memory_services.insert(service.name.clone(), handle);

                            info!(
                                "  {} (Memory QUIC) - connecting in background...",
                                service.name
                            );
                            break;
                        }
                    }
                }

                // ===== Meeting Bot - QUIC only (sidecar do spotkan) =====
                ServiceType::MeetingBot => {
                    for backend in &service.backends {
                        if let ConnectionType::QUIC {
                            quic_url,
                            tls_ca,
                            auto_reconnect,
                            reconnect_interval_ms,
                            keepalive_interval_ms,
                            ..
                        } = &backend.connection
                        {
                            let quic_config = crate::net::quic::QuicConfig {
                                name: service.name.clone(),
                                url: quic_url.clone(),
                                tls_ca: tls_ca.clone(),
                                server_name: None,
                                alpn: "tentaflow-service/v1".to_string(),
                                timeout_ms: backend.timeout_ms,
                                auto_reconnect: *auto_reconnect,
                                reconnect_interval_ms: *reconnect_interval_ms,
                                keepalive_interval_ms: *keepalive_interval_ms,
                                skip_tls_verify: tls_ca.is_none(),
                                direct_addrs: Vec::new(),
                            };

                            let handle = Arc::new(QuicServiceHandle::new(quic_config));
                            quic_llm_services.insert(service.name.clone(), handle);

                            info!(
                                "  {} (Meeting Bot QUIC) - connecting in background...",
                                service.name
                            );
                            break;
                        }
                    }
                }

                // ===== Reranker =====
                ServiceType::Reranker => {
                    for backend in &service.backends {
                        if let ConnectionType::QUIC {
                            quic_url,
                            tls_ca,
                            auto_reconnect,
                            reconnect_interval_ms,
                            keepalive_interval_ms,
                            ..
                        } = &backend.connection
                        {
                            let quic_config = crate::net::quic::QuicConfig {
                                name: service.name.clone(),
                                url: quic_url.clone(),
                                tls_ca: tls_ca.clone(),
                                server_name: None,
                                alpn: "h3".to_string(),
                                timeout_ms: backend.timeout_ms,
                                auto_reconnect: *auto_reconnect,
                                reconnect_interval_ms: *reconnect_interval_ms,
                                keepalive_interval_ms: *keepalive_interval_ms,
                                skip_tls_verify: false,
                                direct_addrs: Vec::new(),
                            };

                            let handle = Arc::new(QuicServiceHandle::new(quic_config));
                            quic_embedding_services.insert(service.name.clone(), handle);

                            info!(
                                "  {} (Reranker QUIC) - connecting in background...",
                                service.name
                            );
                            break;
                        }
                    }
                }
            }
        }

        info!("ServiceManager: Inicjalizacja zakonczona (QUIC connections spawning in background)");

        Ok(Self {
            config,
            rag_services: rag_services.into_iter().collect(),
            quic_embedding_services: quic_embedding_services.into_iter().collect(),
            service_backends,
            dynamic_backends: dashmap::DashMap::new(),
            load_balancing_strategies,
            tts_clients,
            quic_tts_services: quic_tts_services.into_iter().collect(),
            quic_llm_services: quic_llm_services.into_iter().collect(),
            llm_model_categories: llm_model_categories.into_iter().collect(),
            quic_stt_services: quic_stt_services.into_iter().collect(),
            quic_memory_services: quic_memory_services.into_iter().collect(),
            model_pool: model_pool.into_iter().collect(),
            local_inference_models: dashmap::DashMap::new(),
            callback_tx,
            callback_rx: Arc::new(tokio::sync::Mutex::new(callback_rx)),
            shutdown_tx,
            shutdown_rx,
            prompt_registry,
            conversation_cache,
            mesh_registry: parking_lot::RwLock::new(None),
            reverse_router: parking_lot::RwLock::new(None),
            event_bus: parking_lot::RwLock::new(None),
            snapshot_rx: parking_lot::RwLock::new(None),
        })
    }

    /// Wires the supervisor's services snapshot receiver. Called by
    /// `Router::set_services_snapshot_rx` so the manager can resolve models
    /// against the V2 snapshot without re-borrowing through the router.
    ///
    /// Spawning a background task that re-runs `hydrate_from_snapshot` on every
    /// snapshot update is left to the caller (`Router::set_services_snapshot_rx`)
    /// so unit tests that wire a snapshot directly do not pull in a Tokio
    /// runtime requirement.
    pub fn set_snapshot_rx(
        &self,
        rx: tokio::sync::watch::Receiver<Arc<crate::services::supervisor::ServicesSnapshot>>,
    ) {
        *self.snapshot_rx.write() = Some(rx);
    }

    /// Returns the current snapshot. When the receiver has not been wired
    /// (legacy startup paths, unit tests using `Router::default`-style setup)
    /// returns an empty snapshot rather than `None` — callers should treat an
    /// empty snapshot as "no services known".
    pub fn current_snapshot(&self) -> Arc<crate::services::supervisor::ServicesSnapshot> {
        self.snapshot_rx
            .read()
            .as_ref()
            .map(|rx| rx.borrow().clone())
            .unwrap_or_else(|| Arc::new(crate::services::supervisor::ServicesSnapshot::default()))
    }

    /// Uruchamia wszystkie background taski dla polaczen QUIC.
    /// Wywolaj to PO utworzeniu ServiceManager.
    /// Laduje serwisy QUIC z bazy danych i rejestruje je w service_manager.
    /// Wywolywane przy starcie routera — uzupelnia serwisy deployowane z GUI.
    pub fn load_quic_services_from_db(&self, db: &crate::db::DbPool) {
        let services = match crate::db::repository::list_services(db) {
            Ok(s) => s,
            Err(e) => {
                warn!("Nie udalo sie zaladowac serwisow z DB: {}", e);
                return;
            }
        };

        for service in &services {
            // Pomijaj serwisy ktore nie sa QUIC
            let backends = match crate::db::repository::list_backends_for_service(db, service.id) {
                Ok(b) => b,
                Err(_) => continue,
            };

            for backend in &backends {
                if backend.connection_type != "quic" {
                    continue;
                }

                // Parsuj config_json backendu
                let config: serde_json::Value =
                    serde_json::from_str(&backend.config_json).unwrap_or_default();
                let quic_url = match config.get("quic_url").and_then(|v| v.as_str()) {
                    Some(u) => u.to_string(),
                    None => continue,
                };

                // Sprawdz czy juz zarejestrowany (z config.toml)
                if self.quic_llm_services.contains_key(&service.name) {
                    continue;
                }

                info!(
                    "Ladowanie serwisu QUIC z DB: '{}' (typ={}, url={})",
                    service.name, service.service_type, quic_url
                );

                self.register_quic_service(
                    service.name.clone(),
                    &service.service_type,
                    quic_url,
                    None,
                    None,
                );
            }
        }
    }

    pub fn spawn_connection_tasks(&self) {
        info!("Spawning background connection tasks...");

        // Spawn RAG connection tasks
        let rag_entries: Vec<_> = self
            .rag_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in rag_entries {
            let callback_tx = self.callback_tx.clone();
            let shutdown_rx = self.shutdown_rx.clone();

            tokio::spawn(async move {
                Self::rag_connection_loop(name, handle, callback_tx, shutdown_rx).await;
            });
        }

        // Spawn QUIC Embedding connection tasks
        let embedding_entries: Vec<_> = self
            .quic_embedding_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in embedding_entries {
            let shutdown_rx = self.shutdown_rx.clone();
            let reverse_router = self.reverse_router.read().clone();

            tokio::spawn(async move {
                Self::quic_service_connection_loop(
                    name,
                    handle,
                    shutdown_rx,
                    "Embedding",
                    reverse_router,
                )
                .await;
            });
        }

        // Spawn QUIC TTS connection tasks
        let tts_entries: Vec<_> = self
            .quic_tts_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in tts_entries {
            let shutdown_rx = self.shutdown_rx.clone();
            let reverse_router = self.reverse_router.read().clone();

            tokio::spawn(async move {
                Self::quic_service_connection_loop(
                    name,
                    handle,
                    shutdown_rx,
                    "TTS",
                    reverse_router,
                )
                .await;
            });
        }

        // Spawn QUIC LLM + Meeting Bot connection tasks
        let llm_entries: Vec<_> = self
            .quic_llm_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in llm_entries {
            let shutdown_rx = self.shutdown_rx.clone();

            // Meeting bot ma dedykowany loop z reverse listenerem + transcript subscriberem
            if name.contains("meeting") || name.contains("teams-bot") {
                let event_bus = self.event_bus.read().clone();
                let reverse_router = self.reverse_router.read().clone();
                tokio::spawn(async move {
                    Self::meeting_bot_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        event_bus,
                        reverse_router,
                    )
                    .await;
                });
            } else {
                let prompt_registry = self.prompt_registry.clone();
                let model_category = self
                    .llm_model_categories
                    .get(&name)
                    .map(|r| *r.value())
                    .unwrap_or_default();
                tokio::spawn(async move {
                    Self::quic_llm_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        prompt_registry,
                        model_category,
                    )
                    .await;
                });
            }
        }

        // Spawn QUIC STT connection tasks
        let stt_entries: Vec<_> = self
            .quic_stt_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in stt_entries {
            let shutdown_rx = self.shutdown_rx.clone();
            let reverse_router = self.reverse_router.read().clone();

            tokio::spawn(async move {
                Self::quic_service_connection_loop(
                    name,
                    handle,
                    shutdown_rx,
                    "STT",
                    reverse_router,
                )
                .await;
            });
        }

        // Spawn QUIC Memory connection tasks (z obsluga callbacks)
        let memory_entries: Vec<_> = self
            .quic_memory_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in memory_entries {
            let callback_tx = self.callback_tx.clone();
            let shutdown_rx = self.shutdown_rx.clone();

            tokio::spawn(async move {
                Self::memory_connection_loop(name, handle, callback_tx, shutdown_rx).await;
            });
        }
    }

    /// Ustawia router dla odwrotnych requestow od kontenerow.
    /// Wywolaj po utworzeniu Routera, zeby kontenery mogly wysylac requesty do routera.
    pub fn set_reverse_router(&self, router: crate::routing::Router) {
        *self.reverse_router.write() = Some(router.clone());

        // Uruchom reverse listenery na istniejacych polaczeniach
        let shutdown_rx = self.shutdown_rx.clone();

        let all_services: Vec<(String, Arc<QuicServiceHandle>)> = {
            let mut entries = Vec::new();
            for r in self.quic_tts_services.iter() {
                entries.push((r.key().clone(), r.value().clone()));
            }
            for r in self.quic_stt_services.iter() {
                entries.push((r.key().clone(), r.value().clone()));
            }
            for r in self.quic_llm_services.iter() {
                entries.push((r.key().clone(), r.value().clone()));
            }
            for r in self.quic_embedding_services.iter() {
                entries.push((r.key().clone(), r.value().clone()));
            }
            entries
        };

        for (name, handle) in all_services {
            if let Ok(guard) = handle.client.try_read() {
                if let Some(ref client) = *guard {
                    let srv_shutdown = shutdown_rx.clone();
                    let router_clone = router.clone();
                    crate::routing::reverse_request::spawn_reverse_listener(
                        client.clone(),
                        router_clone,
                        name.clone(),
                        srv_shutdown,
                    );
                    info!(
                        "Reverse listener uruchomiony dla istniejacego serwisu: {}",
                        name
                    );
                }
            }
        }
    }

    /// Ustawia EventBus addonow — wywolaj po utworzeniu AddonManager.
    /// Potrzebny do uruchomienia subskrypcji transkrypcji z meeting bot.
    pub fn set_event_bus(&self, event_bus: Arc<crate::addon::event_bus::EventBus>) {
        *self.event_bus.write() = Some(event_bus);
        info!("ServiceManager: EventBus ustawiony");
    }

    /// Background loop dla RAG connection z auto-reconnect
    async fn rag_connection_loop(
        name: String,
        handle: Arc<RAGServiceHandle>,
        callback_tx: mpsc::UnboundedSender<(
            tentaflow_protocol::ModelRequest,
            mpsc::Sender<tentaflow_protocol::ModelResponse>,
        )>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        let reconnect_interval =
            std::time::Duration::from_millis(handle.config.reconnect_interval_ms);
        let mut per_service_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() || *per_service_rx.borrow() {
                info!(
                    "RAG '{}': Shutdown signal received, stopping connection loop",
                    name
                );
                break;
            }

            info!(
                "RAG '{}': Attempting connection to {}...",
                name, handle.config.quic_url
            );

            let config = handle.config.clone();
            let callback_tx_clone = callback_tx.clone();
            let shutdown_rx_clone = shutdown_rx.clone();

            match RAGClient::connect(config, callback_tx_clone, shutdown_rx_clone).await {
                Ok(client) => {
                    info!("RAG '{}': Connected successfully!", name);
                    handle.set_connected(Arc::new(client)).await;

                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() {
                                    info!("RAG '{}': Shutdown signal, disconnecting", name);
                                    handle.set_disconnected("shutdown".to_string()).await;
                                    return;
                                }
                            }
                            _ = per_service_rx.changed() => {
                                if *per_service_rx.borrow() {
                                    info!("RAG '{}': Per-service shutdown signal", name);
                                    handle.set_disconnected("removed".to_string()).await;
                                    return;
                                }
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                if !handle.is_available().await {
                                    warn!("RAG '{}': Connection lost, will reconnect", name);
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "RAG '{}': Connection failed: {}. Retrying in {:?}...",
                        name, e, reconnect_interval
                    );
                    handle.set_disconnected(e.to_string()).await;
                }
            }

            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("RAG '{}': Shutdown during reconnect wait", name);
                        return;
                    }
                }
                _ = per_service_rx.changed() => {
                    if *per_service_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(reconnect_interval) => {}
            }
        }
    }

    /// Background loop dla QUIC service connection z auto-reconnect.
    /// Uzywany dla: Embeddings, TTS, i innych serwisow QUIC.
    /// Opcjonalny `reverse_router` uruchamia nasluchiwanie na odwrotne requesty od kontenera.
    async fn quic_service_connection_loop(
        name: String,
        handle: Arc<QuicServiceHandle>,
        mut shutdown_rx: watch::Receiver<bool>,
        service_type: &'static str,
        reverse_router: Option<crate::routing::Router>,
    ) {
        let reconnect_interval =
            std::time::Duration::from_millis(handle.config.reconnect_interval_ms);
        let mut per_service_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() || *per_service_rx.borrow() {
                info!("{} QUIC '{}': Shutdown signal received", service_type, name);
                break;
            }

            info!(
                "{} QUIC '{}': Attempting connection to {}...",
                service_type, name, handle.config.url
            );

            let config = handle.config.clone();
            let shutdown_rx_clone = shutdown_rx.clone();

            match crate::net::quic::QuicClient::connect(config, shutdown_rx_clone).await {
                Ok(client) => {
                    info!("{} QUIC '{}': Connected successfully!", service_type, name);
                    let client = Arc::new(client);
                    handle.set_connected(client.clone()).await;

                    // Uruchom reverse listener jesli router jest dostepny
                    let reverse_task = reverse_router.as_ref().map(|router| {
                        info!(
                            "{} QUIC '{}': Uruchamiam reverse listener",
                            service_type, name
                        );
                        crate::routing::reverse_request::spawn_reverse_listener(
                            client,
                            router.clone(),
                            name.clone(),
                            shutdown_rx.clone(),
                        )
                    });

                    let should_return = loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() {
                                    info!("{} QUIC '{}': Shutdown signal", service_type, name);
                                    handle.shutdown_client_and_mark_disconnected("shutdown").await;
                                    break true;
                                }
                            }
                            _ = per_service_rx.changed() => {
                                if *per_service_rx.borrow() {
                                    info!("{} QUIC '{}': Per-service shutdown signal", service_type, name);
                                    handle.shutdown_client_and_mark_disconnected("removed").await;
                                    break true;
                                }
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                if !handle.is_available().await {
                                    warn!("{} QUIC '{}': Connection lost, will reconnect", service_type, name);
                                    break false;
                                }
                            }
                        }
                    };

                    if let Some(task) = reverse_task {
                        task.abort();
                    }
                    if should_return {
                        return;
                    }
                }
                Err(e) => {
                    warn!(
                        "{} QUIC '{}': Connection failed: {}. Retrying in {:?}...",
                        service_type, name, e, reconnect_interval
                    );
                    handle.set_disconnected(e.to_string()).await;
                }
            }

            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = per_service_rx.changed() => {
                    if *per_service_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(reconnect_interval) => {}
            }
        }
    }

    /// Background loop dla meeting bot QUIC — po polaczeniu uruchamia reverse listener
    /// (bot wysyla requesty STT/TTS) i (opcjonalnie) subskrypcje transkrypcji.
    async fn meeting_bot_connection_loop(
        name: String,
        handle: Arc<QuicServiceHandle>,
        mut shutdown_rx: watch::Receiver<bool>,
        event_bus: Option<Arc<crate::addon::event_bus::EventBus>>,
        reverse_router: Option<crate::routing::Router>,
    ) {
        let _ = event_bus;
        let reconnect_interval =
            std::time::Duration::from_millis(handle.config.reconnect_interval_ms);
        let mut per_service_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() || *per_service_rx.borrow() {
                info!("MeetingBot QUIC '{}': Shutdown signal received", name);
                break;
            }

            info!(
                "MeetingBot QUIC '{}': Attempting connection to {}...",
                name, handle.config.url
            );

            let config = handle.config.clone();
            let shutdown_rx_clone = shutdown_rx.clone();

            match crate::net::quic::QuicClient::connect(config, shutdown_rx_clone).await {
                Ok(client) => {
                    info!("MeetingBot QUIC '{}': Connected successfully!", name);
                    let client = Arc::new(client);
                    handle.set_connected(client.clone()).await;

                    // Uruchom reverse listener — bot otwiera bidi streams do STT/TTS przez router.
                    // Bez tego ModelRequest od bota wisi bez accept_bi po stronie routera.
                    let reverse_task = reverse_router.as_ref().map(|router| {
                        info!("MeetingBot QUIC '{}': Uruchamiam reverse listener", name);
                        crate::routing::reverse_request::spawn_reverse_listener(
                            client.clone(),
                            router.clone(),
                            name.clone(),
                            shutdown_rx.clone(),
                        )
                    });

                    // TODO: subskrypcja transkrypcji — wylaczona do czasu stabilizacji QUIC
                    // Transcript subscriber otwiera streaming request ktory destabilizuje polaczenie
                    let transcript_task: Option<tokio::task::JoinHandle<()>> = None;

                    let should_return = loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() {
                                    info!("MeetingBot QUIC '{}': Shutdown signal", name);
                                    handle.shutdown_client_and_mark_disconnected("shutdown").await;
                                    break true;
                                }
                            }
                            _ = per_service_rx.changed() => {
                                if *per_service_rx.borrow() {
                                    info!("MeetingBot QUIC '{}': Per-service shutdown signal", name);
                                    handle.shutdown_client_and_mark_disconnected("removed").await;
                                    break true;
                                }
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                if !handle.is_available().await {
                                    warn!("MeetingBot QUIC '{}': Connection lost, will reconnect", name);
                                    break false;
                                }
                            }
                        }
                    };

                    if let Some(task) = transcript_task {
                        task.abort();
                    }
                    if let Some(task) = reverse_task {
                        task.abort();
                    }
                    if should_return {
                        return;
                    }
                }
                Err(e) => {
                    warn!(
                        "MeetingBot QUIC '{}': Connection failed: {}. Retrying in {:?}...",
                        name, e, reconnect_interval
                    );
                    handle.set_disconnected(e.to_string()).await;
                }
            }

            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = per_service_rx.changed() => {
                    if *per_service_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(reconnect_interval) => {}
            }
        }
    }

    /// Background loop dla LLM QUIC connection z wysylaniem prefix cache.
    ///
    /// Po nawiazaniu polaczenia z LLM Engine:
    /// 1. Wysyla PrefixCacheInitRequest z promptami dla odpowiedniej kategorii modelu
    /// 2. LLM Engine cachuje KV (Key-Value) dla tych promptow
    /// 3. Utrzymuje polaczenie z auto-reconnect
    async fn quic_llm_connection_loop(
        name: String,
        handle: Arc<QuicServiceHandle>,
        mut shutdown_rx: watch::Receiver<bool>,
        prompt_registry: SharedPromptRegistry,
        config_category: crate::config::LlmModelCategory,
    ) {
        use tentaflow_protocol::{PrefixCacheInitRequest, PrefixCacheModelCategory};
        // TODO: Przeniesc ModelCategory do crate::prompt_registry
        use crate::prompt_registry::ModelCategory;

        let (registry_category, protocol_category) = match config_category {
            crate::config::LlmModelCategory::Main => {
                (ModelCategory::MainLlm, PrefixCacheModelCategory::MainLlm)
            }
            crate::config::LlmModelCategory::Analyzer => (
                ModelCategory::AnalyzerLlm,
                PrefixCacheModelCategory::AnalyzerLlm,
            ),
        };

        let reconnect_interval =
            std::time::Duration::from_millis(handle.config.reconnect_interval_ms);
        let mut per_service_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() || *per_service_rx.borrow() {
                info!("LLM QUIC '{}': Shutdown signal received", name);
                break;
            }

            info!(
                "LLM QUIC '{}': Attempting connection to {}...",
                name, handle.config.url
            );

            let config = handle.config.clone();
            let shutdown_rx_clone = shutdown_rx.clone();

            match crate::net::quic::QuicClient::connect(config, shutdown_rx_clone).await {
                Ok(client) => {
                    info!("LLM QUIC '{}': Connected successfully!", name);
                    let client = Arc::new(client);
                    handle.set_connected(client.clone()).await;

                    let prompt_set = prompt_registry.get_prompt_set(registry_category);
                    let prompts: Vec<_> =
                        prompt_set.prompts.iter().map(|p| p.to_protocol()).collect();

                    if !prompts.is_empty() {
                        let init_request = PrefixCacheInitRequest {
                            request_id: uuid::Uuid::new_v4().to_string(),
                            model_name: name.clone(),
                            category: protocol_category,
                            prompts,
                        };

                        info!(
                            "LLM '{}': Wysylam {} promptow do prefix cache...",
                            name,
                            init_request.prompts.len()
                        );

                        match client.send_prefix_cache_init(init_request).await {
                            Ok(response) => {
                                if response.success {
                                    info!(
                                        "LLM '{}': Prefix cache zainicjalizowany - {} promptow zacheowanych{}",
                                        name,
                                        response.cached_count,
                                        response.cache_memory_mb.map(|mb| format!(", {:.2} MB", mb)).unwrap_or_default()
                                    );
                                } else {
                                    warn!(
                                        "LLM '{}': Prefix cache czesciowo nieudany - {} bledow: {:?}",
                                        name,
                                        response.errors.len(),
                                        response.errors
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("LLM '{}': Nie udalo sie zainicjalizowac prefix cache: {}. Kontynuuje bez cache.", name, e);
                            }
                        }
                    }

                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() {
                                    info!("LLM QUIC '{}': Shutdown signal", name);
                                    handle.shutdown_client_and_mark_disconnected("shutdown").await;
                                    return;
                                }
                            }
                            _ = per_service_rx.changed() => {
                                if *per_service_rx.borrow() {
                                    info!("LLM QUIC '{}': Per-service shutdown signal", name);
                                    handle.shutdown_client_and_mark_disconnected("removed").await;
                                    return;
                                }
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                if !handle.is_available().await {
                                    warn!("LLM QUIC '{}': Connection lost, will reconnect", name);
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "LLM QUIC '{}': Connection failed: {}. Retrying in {:?}...",
                        name, e, reconnect_interval
                    );
                    handle.set_disconnected(e.to_string()).await;
                }
            }

            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = per_service_rx.changed() => {
                    if *per_service_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(reconnect_interval) => {}
            }
        }
    }

    /// Background loop dla Memory QUIC connection z obsluga callbacks.
    ///
    /// Memory wymaga dwukierunkowej komunikacji - moze wysylac callback requests
    /// do Router (np. dla embeddings). Ta funkcja:
    /// 1. Laczy sie z Memory przez QUIC
    /// 2. Spawnuje callback listener ktory akceptuje incoming streams
    /// 3. Przekazuje callback requests do glownego callback handlera
    async fn memory_connection_loop(
        name: String,
        handle: Arc<QuicServiceHandle>,
        callback_tx: mpsc::UnboundedSender<(
            tentaflow_protocol::ModelRequest,
            mpsc::Sender<tentaflow_protocol::ModelResponse>,
        )>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        use anyhow::Context;
        use tracing::{debug, error};

        let reconnect_interval =
            std::time::Duration::from_millis(handle.config.reconnect_interval_ms);
        let mut per_service_rx = handle.shutdown_rx.clone();

        loop {
            if *shutdown_rx.borrow() || *per_service_rx.borrow() {
                info!("Memory QUIC '{}': Shutdown signal received", name);
                break;
            }

            info!(
                "Memory QUIC '{}': Attempting connection to {}...",
                name, handle.config.url
            );

            let config = handle.config.clone();
            let shutdown_rx_clone = shutdown_rx.clone();

            match crate::net::quic::QuicClient::connect(config.clone(), shutdown_rx_clone).await {
                Ok(client) => {
                    info!("Memory QUIC '{}': Connected successfully!", name);
                    let client = Arc::new(client);
                    handle.set_connected(client.clone()).await;

                    // Spawn callback listener dla tego polaczenia
                    let callback_tx_clone = callback_tx.clone();
                    let name_clone = name.clone();
                    let mut callback_shutdown_rx = shutdown_rx.clone();
                    let client_for_callback = client.clone();

                    let callback_task = tokio::spawn(async move {
                        info!("Memory '{}': Callback listener started", name_clone);

                        loop {
                            // Pobierz aktywne polaczenie iroh (z auto-reconnect).
                            let conn = match client_for_callback.iroh_connection().await {
                                Ok(c) => c,
                                Err(_) => {
                                    tokio::select! {
                                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                                            continue;
                                        }
                                        _ = callback_shutdown_rx.changed() => {
                                            info!("Memory '{}': Callback listener shutdown signal", name_clone);
                                            break;
                                        }
                                    }
                                }
                            };

                            // Accept incoming bi-directional stream (callback od Memory)
                            tokio::select! {
                                result = conn.accept_bi() => {
                                    match result {
                                        Ok((mut send, mut recv)) => {
                                            let callback_tx = callback_tx_clone.clone();
                                            let name_inner = name_clone.clone();

                                            tokio::spawn(async move {
                                                // Read callback request
                                                match recv.read_to_end(10_000_000).await {
                                                    Ok(data) => {
                                                        // Deserialize ModelRequest (callback)
                                                        match rkyv::access::<tentaflow_protocol::ArchivedModelRequest, rkyv::rancor::Error>(&data)
                                                            .context("Failed to access archived ModelRequest")
                                                        {
                                                            Ok(archived) => {
                                                                match rkyv::deserialize::<tentaflow_protocol::ModelRequest, rkyv::rancor::Error>(archived) {
                                                                    Ok(callback_req) => {
                                                                        debug!("Memory '{}' callback request: {}", name_inner, callback_req.request_id);

                                                                        // Kanaly odpowiedzi
                                                                        let (resp_tx, mut resp_rx) = mpsc::channel(1);

                                                                        // Wyslij do callback handlera
                                                                        if callback_tx.send((callback_req, resp_tx)).is_err() {
                                                                            error!("Memory '{}': Failed to send callback to handler", name_inner);
                                                                            return;
                                                                        }

                                                                        // Czekaj na odpowiedz od handlera
                                                                        if let Some(callback_resp) = resp_rx.recv().await {
                                                                            // Serializacja i wyslanie odpowiedzi
                                                                            match rkyv::to_bytes::<rkyv::rancor::Error>(&callback_resp) {
                                                                                Ok(resp_data) => {
                                                                                    if let Err(e) = send.write_all(&resp_data).await {
                                                                                        error!("Memory '{}': Failed to send callback response: {}", name_inner, e);
                                                                                    }
                                                                                    let _ = send.finish();
                                                                                    debug!("Memory '{}' callback response sent", name_inner);
                                                                                }
                                                                                Err(e) => {
                                                                                    error!("Memory '{}': Failed to serialize callback response: {}", name_inner, e);
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        error!("Memory '{}': Failed to deserialize callback request: {}", name_inner, e);
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                error!("Memory '{}': Failed to access callback request: {}", name_inner, e);
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!("Memory '{}': Failed to read callback request: {}", name_inner, e);
                                                    }
                                                }
                                            });
                                        }
                                        Err(iroh::endpoint::ConnectionError::ApplicationClosed { .. }) => {
                                            info!("Memory '{}': Connection closed by server", name_clone);
                                            break;
                                        }
                                        Err(e) => {
                                            warn!("Memory '{}': Failed to accept callback stream: {}", name_clone, e);
                                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                        }
                                    }
                                }
                                _ = callback_shutdown_rx.changed() => {
                                    info!("Memory '{}': Callback listener shutdown signal", name_clone);
                                    break;
                                }
                            }
                        }

                        info!("Memory '{}': Callback listener ended", name_clone);
                    });

                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() {
                                    info!("Memory QUIC '{}': Shutdown signal", name);
                                    callback_task.abort();
                                    handle.shutdown_client_and_mark_disconnected("shutdown").await;
                                    return;
                                }
                            }
                            _ = per_service_rx.changed() => {
                                if *per_service_rx.borrow() {
                                    info!("Memory QUIC '{}': Per-service shutdown signal", name);
                                    callback_task.abort();
                                    handle.shutdown_client_and_mark_disconnected("removed").await;
                                    return;
                                }
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                                if !handle.is_available().await {
                                    warn!("Memory QUIC '{}': Connection lost, will reconnect", name);
                                    callback_task.abort();
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Memory QUIC '{}': Connection failed: {}. Retrying in {:?}...",
                        name, e, reconnect_interval
                    );
                    handle.set_disconnected(e.to_string()).await;
                }
            }

            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = per_service_rx.changed() => {
                    if *per_service_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(reconnect_interval) => {}
            }
        }
    }

    // ========================================================================
    // ACCESSORS (non-blocking)
    // ========================================================================

    /// Pobierz RAG client jesli dostepny (non-blocking)
    pub async fn get_rag_client(&self, service_name: &str) -> Option<Arc<RAGClient>> {
        let handle = self
            .rag_services
            .get(service_name)
            .map(|r| r.value().clone())?;
        handle.get_client().await
    }

    /// Pobierz QUIC Embedding client jesli dostepny (non-blocking)
    pub async fn get_quic_embedding_client(
        &self,
        service_name: &str,
    ) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handle = self
            .quic_embedding_services
            .get(service_name)
            .map(|r| r.value().clone())?;
        handle.get_client().await
    }

    /// Pobierz QUIC TTS client jesli dostepny (non-blocking)
    pub async fn get_quic_tts_client(
        &self,
        service_name: &str,
    ) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handle = self
            .quic_tts_services
            .get(service_name)
            .map(|r| r.value().clone())?;
        handle.get_client().await
    }

    /// Pobierz QUIC LLM client jesli dostepny (non-blocking)
    pub async fn get_quic_llm_client(
        &self,
        service_name: &str,
    ) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handle = self
            .quic_llm_services
            .get(service_name)
            .map(|r| r.value().clone())?;
        handle.get_client().await
    }

    /// Pobierz QUIC STT client jesli dostepny (non-blocking)
    pub async fn get_quic_stt_client(
        &self,
        service_name: &str,
    ) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handle = self
            .quic_stt_services
            .get(service_name)
            .map(|r| r.value().clone())?;
        handle.get_client().await
    }

    /// Pobierz backend clients dla serwisu (zawsze dostepne - HTTP)
    pub fn get_service_backends(&self, service_name: &str) -> Option<&Vec<Arc<BackendClient>>> {
        if let Some(v) = self.service_backends.get(service_name) {
            return Some(v);
        }
        None
    }

    /// Sprawdza czy serwis ma HTTP backends (statyczne lub dynamiczne)
    pub fn has_http_backends(&self, service_name: &str) -> bool {
        self.service_backends.contains_key(service_name)
            || self.dynamic_backends.contains_key(service_name)
    }

    /// Pobierz backend clients (statyczne lub dynamiczne) — klonuje Arc referencje
    pub fn get_service_backends_cloned(
        &self,
        service_name: &str,
    ) -> Option<Vec<Arc<BackendClient>>> {
        if let Some(v) = self.service_backends.get(service_name) {
            return Some(v.clone());
        }
        self.dynamic_backends
            .get(service_name)
            .map(|r| r.value().0.clone())
    }

    /// Pobierz load balancing strategy
    pub fn get_strategy(&self, service_name: &str) -> Option<&Box<dyn LoadBalancingStrategy>> {
        self.load_balancing_strategies.get(service_name)
    }

    /// Pobierz TTS client
    pub fn get_tts_client(&self, service_name: &str) -> Option<Arc<TTSClient>> {
        self.tts_clients.get(service_name).cloned()
    }

    /// Pobierz callback receiver
    pub fn get_callback_rx(
        &self,
    ) -> Arc<
        tokio::sync::Mutex<
            mpsc::UnboundedReceiver<(
                tentaflow_protocol::ModelRequest,
                mpsc::Sender<tentaflow_protocol::ModelResponse>,
            )>,
        >,
    > {
        self.callback_rx.clone()
    }

    /// Sprawdz czy RAG jest dostepny
    pub async fn is_rag_available(&self, service_name: &str) -> bool {
        let handle = self
            .rag_services
            .get(service_name)
            .map(|r| r.value().clone());
        match handle {
            Some(h) => h.is_available().await,
            None => false,
        }
    }

    /// Wyslij shutdown signal
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    // ========================================================================
    // ADDITIONAL ACCESSORS (for Router compatibility)
    // ========================================================================

    /// Sprawdz czy serwis RAG istnieje (niezaleznie od stanu polaczenia)
    pub fn has_rag_service(&self, service_name: &str) -> bool {
        self.rag_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis QUIC embedding istnieje
    pub fn has_quic_embedding_service(&self, service_name: &str) -> bool {
        self.quic_embedding_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis TTS istnieje (HTTP lub QUIC)
    pub fn has_tts_service(&self, service_name: &str) -> bool {
        self.tts_clients.contains_key(service_name)
            || self.quic_tts_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis QUIC TTS istnieje
    pub fn has_quic_tts_service(&self, service_name: &str) -> bool {
        self.quic_tts_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis QUIC LLM istnieje
    pub fn has_quic_llm_service(&self, service_name: &str) -> bool {
        self.quic_llm_services.contains_key(service_name)
    }

    /// Sprawdz czy model jest obslugiwany przez lokalna inferencje in-process
    pub fn has_local_inference_service(&self, model_name: &str) -> bool {
        self.local_inference_models.contains_key(model_name)
    }

    /// Rejestruje model jako obslugiwany lokalnie (in-process MLX/llama.cpp)
    pub fn register_local_inference_model(&self, model_name: &str) {
        self.local_inference_models
            .insert(model_name.to_string(), ());
        info!(
            "LocalInference: zarejestrowano model '{}' do obslugi in-process",
            model_name
        );
    }

    /// Snapshot-first resolution of HTTP backends for a given model. Walks the
    /// supervisor snapshot, materialises a `BackendClient` for every HTTP /
    /// ExternalHTTP entry that hosts `model_name`, and returns the freshly built
    /// list. Returns `None` when the snapshot has no candidates so the caller
    /// can fall back to the legacy DashMap path.
    ///
    /// FAZA-8c: this becomes the only path; legacy fallback in chat.rs /
    /// adapters disappears.
    pub fn resolve_http_backends_via_snapshot(
        &self,
        model_name: &str,
    ) -> Option<Vec<Arc<BackendClient>>> {
        use crate::routing::transport_client::entry_to_backend_client;
        use crate::services::transport::Transport;

        let snap = self.current_snapshot();
        let entries = snap.find_services_for_model(model_name);
        if entries.is_empty() {
            return None;
        }

        let mut clients: Vec<Arc<BackendClient>> = Vec::new();
        for entry in entries {
            match entry.transport {
                Transport::HttpDirect | Transport::ExternalHttp => {
                    match entry_to_backend_client(entry) {
                        Ok(client) => clients.push(Arc::new(client)),
                        Err(e) => {
                            warn!(
                                "snapshot path: backend client init for svc-{} failed: {}",
                                entry.id, e
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        if clients.is_empty() {
            None
        } else {
            Some(clients)
        }
    }

    /// Reconciles legacy DashMap stores (`local_inference_models`,
    /// `dynamic_backends`) with the current supervisor snapshot. Called after
    /// every successful deploy commit so callers that still depend on the
    /// legacy stores observe the new service immediately, without waiting for
    /// the next supervisor tick to push the snapshot.
    ///
    /// Embedded entries register their model names into `local_inference_models`.
    /// HTTP entries materialise a `BackendClient` and push it into
    /// `dynamic_backends` keyed by every model name the entry hosts. SidecarQuic
    /// entries are owned by `register_quic_service` and are not touched here.
    pub fn hydrate_from_snapshot(&self) {
        use crate::services::transport::Transport;

        let snap = self.current_snapshot();
        for entry in &snap.services {
            if matches!(entry.transport, Transport::Embedded) {
                for m in &entry.models {
                    if !self.local_inference_models.contains_key(&m.model_name) {
                        self.register_local_inference_model(&m.model_name);
                    }
                }
            }
        }
    }

    /// Sprawdz czy serwis QUIC STT istnieje
    pub fn has_quic_stt_service(&self, service_name: &str) -> bool {
        self.quic_stt_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis LLM istnieje (HTTP lub QUIC)
    pub fn has_llm_service(&self, service_name: &str) -> bool {
        self.service_backends.contains_key(service_name)
            || self.quic_llm_services.contains_key(service_name)
    }

    /// Sprawdz czy serwis STT istnieje (HTTP lub QUIC)
    pub fn has_stt_service(&self, service_name: &str) -> bool {
        self.service_backends.contains_key(service_name)
            || self.quic_stt_services.contains_key(service_name)
    }

    /// Pobierz nazwe pierwszego serwisu TTS (dla fallback) - preferuje QUIC
    pub fn get_first_tts_service_name(&self) -> Option<String> {
        self.quic_tts_services
            .iter()
            .next()
            .map(|r| r.key().clone())
            .or_else(|| self.tts_clients.keys().next().cloned())
    }

    /// Pobierz pierwszy dostepny TTS HTTP client (dla fallback)
    pub fn get_first_tts_client(&self) -> Option<Arc<TTSClient>> {
        self.tts_clients.values().next().cloned()
    }

    /// Pobierz pierwszy dostepny QUIC TTS client (async, dla fallback)
    pub async fn get_first_quic_tts_client(&self) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handles: Vec<Arc<QuicServiceHandle>> = self
            .quic_tts_services
            .iter()
            .map(|r| r.value().clone())
            .collect();
        for handle in handles {
            if let Some(client) = handle.get_client().await {
                return Some(client);
            }
        }
        None
    }

    /// Pobierz nazwe pierwszego serwisu STT (dla fallback) - preferuje QUIC
    pub fn get_first_stt_service_name(&self) -> Option<String> {
        self.quic_stt_services
            .iter()
            .next()
            .map(|r| r.key().clone())
    }

    /// Pobierz pierwszy dostepny QUIC STT client (async, dla fallback)
    pub async fn get_first_quic_stt_client(&self) -> Option<Arc<crate::net::quic::QuicClient>> {
        let handles: Vec<Arc<QuicServiceHandle>> = self
            .quic_stt_services
            .iter()
            .map(|r| r.value().clone())
            .collect();
        for handle in handles {
            if let Some(client) = handle.get_client().await {
                return Some(client);
            }
        }
        None
    }

    /// Sprawdz czy sa jakiekolwiek service backends
    pub fn has_service_backends(&self) -> bool {
        !self.service_backends.is_empty()
    }

    /// Sprawdz czy sa jakiekolwiek RAG serwisy
    pub fn has_rag_services(&self) -> bool {
        !self.rag_services.is_empty()
    }

    /// Pobierz nazwy wszystkich service backends
    pub fn service_backend_names(&self) -> Vec<String> {
        self.service_backends.keys().cloned().collect()
    }

    /// Pobierz nazwy wszystkich RAG serwisow
    pub fn rag_service_names(&self) -> Vec<String> {
        self.rag_services.iter().map(|r| r.key().clone()).collect()
    }

    /// Clone service backends (dla callback handler)
    pub fn clone_service_backends(&self) -> HashMap<String, Vec<Arc<BackendClient>>> {
        self.service_backends.clone()
    }

    /// Clone QUIC embedding services handles (dla callback handler - zwraca nazwy)
    pub fn quic_embedding_service_names(&self) -> Vec<String> {
        self.quic_embedding_services
            .iter()
            .map(|r| r.key().clone())
            .collect()
    }

    /// Clone load balancing strategies (dla callback handler)
    pub fn clone_strategies(&self) -> &HashMap<String, Box<dyn LoadBalancingStrategy>> {
        &self.load_balancing_strategies
    }

    /// Pobierz konfiguracje
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// Pobierz RAG handle (dla bezposredniego dostepu)
    pub fn get_rag_handle(&self, service_name: &str) -> Option<Arc<RAGServiceHandle>> {
        self.rag_services
            .get(service_name)
            .map(|r| r.value().clone())
    }

    /// Pobierz status wszystkich serwisow (do diagnostyki)
    pub async fn get_service_status(&self) -> HashMap<String, String> {
        let mut status = HashMap::new();

        for (name, _) in &self.service_backends {
            status.insert(name.clone(), "ready (HTTP)".to_string());
        }

        let rag_entries: Vec<_> = self
            .rag_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in rag_entries {
            let state = handle.state.read().await;
            let state_str = match &*state {
                QuicServiceState::Connecting => "connecting...".to_string(),
                QuicServiceState::Connected => "connected".to_string(),
                QuicServiceState::Disconnected { reason } => format!("disconnected: {}", reason),
                QuicServiceState::ConfigError { message } => format!("config error: {}", message),
            };
            status.insert(name, state_str);
        }

        let embedding_entries: Vec<_> = self
            .quic_embedding_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in embedding_entries {
            let state = handle.state.read().await;
            let state_str = match &*state {
                QuicServiceState::Connecting => "connecting...".to_string(),
                QuicServiceState::Connected => "connected".to_string(),
                QuicServiceState::Disconnected { reason } => format!("disconnected: {}", reason),
                QuicServiceState::ConfigError { message } => format!("config error: {}", message),
            };
            status.insert(name, state_str);
        }

        for (name, _) in &self.tts_clients {
            status.insert(name.clone(), "ready (TTS HTTP)".to_string());
        }

        let tts_entries: Vec<_> = self
            .quic_tts_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in tts_entries {
            let state = handle.state.read().await;
            let state_str = match &*state {
                QuicServiceState::Connecting => "connecting... (TTS QUIC)".to_string(),
                QuicServiceState::Connected => "connected (TTS QUIC)".to_string(),
                QuicServiceState::Disconnected { reason } => {
                    format!("disconnected (TTS QUIC): {}", reason)
                }
                QuicServiceState::ConfigError { message } => {
                    format!("config error (TTS QUIC): {}", message)
                }
            };
            status.insert(name, state_str);
        }

        let llm_entries: Vec<_> = self
            .quic_llm_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in llm_entries {
            let state = handle.state.read().await;
            let state_str = match &*state {
                QuicServiceState::Connecting => "connecting... (LLM QUIC)".to_string(),
                QuicServiceState::Connected => "connected (LLM QUIC)".to_string(),
                QuicServiceState::Disconnected { reason } => {
                    format!("disconnected (LLM QUIC): {}", reason)
                }
                QuicServiceState::ConfigError { message } => {
                    format!("config error (LLM QUIC): {}", message)
                }
            };
            status.insert(name, state_str);
        }

        let stt_entries: Vec<_> = self
            .quic_stt_services
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        for (name, handle) in stt_entries {
            let state = handle.state.read().await;
            let state_str = match &*state {
                QuicServiceState::Connecting => "connecting... (STT QUIC)".to_string(),
                QuicServiceState::Connected => "connected (STT QUIC)".to_string(),
                QuicServiceState::Disconnected { reason } => {
                    format!("disconnected (STT QUIC): {}", reason)
                }
                QuicServiceState::ConfigError { message } => {
                    format!("config error (STT QUIC): {}", message)
                }
            };
            status.insert(name, state_str);
        }

        status
    }

    // ========================================================================
    // MESH ROUTING
    // ========================================================================

    /// Ustawia referencje do mesh registry (wywolane po utworzeniu mesh managera)
    pub fn set_mesh_registry(&self, registry: Arc<MeshServiceRegistry>) {
        *self.mesh_registry.write() = Some(registry);
    }

    /// Szuka serwisu — najpierw lokalnie, potem w mesh.
    /// Zwraca ServiceLocation::Local jesli serwis dostepny lokalnie,
    /// ServiceLocation::MeshNode jesli znaleziony na zdalnym nodzie.
    pub fn find_service(&self, service_type: &str, model_name: &str) -> Option<ServiceLocation> {
        let has_local = match service_type {
            "llm" => {
                self.has_quic_llm_service(model_name)
                    || self.service_backends.contains_key(model_name)
                    || self.has_local_inference_service(model_name)
            }
            "embedding" => self.has_quic_embedding_service(model_name),
            "tts" => {
                self.has_quic_tts_service(model_name) || self.tts_clients.contains_key(model_name)
            }
            "stt" => self.has_quic_stt_service(model_name),
            "rag" => self.has_rag_service(model_name),
            "memory" => self.quic_memory_services.contains_key(model_name),
            _ => false,
        };

        if has_local {
            return Some(ServiceLocation::Local);
        }

        let registry = self.mesh_registry.read();
        if let Some(ref reg) = *registry {
            if let Some(node_id) = reg.find_service_node(service_type, model_name) {
                return Some(ServiceLocation::MeshNode { node_id });
            }
        }

        None
    }

    /// Szuka dowolnego noda w mesh z serwisem danego typu (bez konkretnego modelu).
    pub fn find_service_by_type(&self, service_type: &str) -> Option<ServiceLocation> {
        let registry = self.mesh_registry.read();
        if let Some(ref reg) = *registry {
            if let Some(node_id) = reg.find_service_by_type(service_type) {
                return Some(ServiceLocation::MeshNode { node_id });
            }
        }
        None
    }

    /// Dynamicznie rejestruje serwis QUIC i uruchamia background connection task.
    /// Wywolywane po deploy uslugi lub przy starcie Routera (z DB).
    pub fn register_quic_service(
        &self,
        name: String,
        service_type: &str,
        quic_url: String,
        tls_ca: Option<String>,
        server_name: Option<String>,
    ) {
        self.register_quic_service_with_addrs(
            name,
            service_type,
            quic_url,
            tls_ca,
            server_name,
            Vec::new(),
        );
    }

    /// Wariant `register_quic_service` z jawnymi direct addresses. Używany przez
    /// MeetingManager gdy spawnuje ephemeralny kontener w docker bridge network:
    /// host podaje `127.0.0.1:<mapped_quic_port>` obok EndpointId, bo kontener
    /// nie jest discoverable przez LAN mDNS/DHT hosta. Pusty `direct_addrs` =
    /// polegaj na discovery (równoważne starej sygnaturze).
    pub fn register_quic_service_with_addrs(
        &self,
        name: String,
        service_type: &str,
        quic_url: String,
        tls_ca: Option<String>,
        server_name: Option<String>,
        direct_addrs: Vec<String>,
    ) {
        let is_self_signed = tls_ca.is_none();
        let quic_config = crate::net::quic::QuicConfig {
            name: name.clone(),
            url: quic_url,
            tls_ca,
            server_name,
            alpn: "tentaflow-service/v1".to_string(),
            timeout_ms: 120000,
            auto_reconnect: true,
            reconnect_interval_ms: 5000,
            keepalive_interval_ms: 30000,
            skip_tls_verify: is_self_signed,
            direct_addrs,
        };

        info!(
            "Zarejestrowano dynamiczny serwis QUIC: {} (typ={}, SNI={:?})",
            name, service_type, quic_config.server_name
        );

        let handle = Arc::new(QuicServiceHandle::new(quic_config));
        let shutdown_rx = self.shutdown_rx.clone();

        match service_type {
            "llm" => {
                if let Some(old) = self.quic_llm_services.insert(name.clone(), handle.clone()) {
                    old.shutdown();
                }
                self.llm_model_categories
                    .insert(name.clone(), crate::config::LlmModelCategory::Main);
                let prompt_registry = self.prompt_registry.clone();
                tokio::spawn(async move {
                    Self::quic_llm_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        prompt_registry,
                        crate::config::LlmModelCategory::Main,
                    )
                    .await;
                });
            }
            "tts" => {
                if let Some(old) = self.quic_tts_services.insert(name.clone(), handle.clone()) {
                    old.shutdown();
                }
                let reverse_router = self.reverse_router.read().clone();
                tokio::spawn(async move {
                    Self::quic_service_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        "TTS",
                        reverse_router,
                    )
                    .await;
                });
            }
            "stt" => {
                if let Some(old) = self.quic_stt_services.insert(name.clone(), handle.clone()) {
                    old.shutdown();
                }
                let reverse_router = self.reverse_router.read().clone();
                tokio::spawn(async move {
                    Self::quic_service_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        "STT",
                        reverse_router,
                    )
                    .await;
                });
            }
            "embedding" => {
                if let Some(old) = self
                    .quic_embedding_services
                    .insert(name.clone(), handle.clone())
                {
                    old.shutdown();
                }
                let reverse_router = self.reverse_router.read().clone();
                tokio::spawn(async move {
                    Self::quic_service_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        "Embedding",
                        reverse_router,
                    )
                    .await;
                });
            }
            "memory" => {
                if let Some(old) = self
                    .quic_memory_services
                    .insert(name.clone(), handle.clone())
                {
                    old.shutdown();
                }
                let callback_tx = self.callback_tx.clone();
                tokio::spawn(async move {
                    Self::memory_connection_loop(name, handle, callback_tx, shutdown_rx).await;
                });
            }
            "meeting-bot" => {
                if let Some(old) = self.quic_llm_services.insert(name.clone(), handle.clone()) {
                    old.shutdown();
                }
                let event_bus = self.event_bus.read().clone();
                let reverse_router = self.reverse_router.read().clone();
                tokio::spawn(async move {
                    Self::meeting_bot_connection_loop(
                        name,
                        handle,
                        shutdown_rx,
                        event_bus,
                        reverse_router,
                    )
                    .await;
                });
            }
            _ => {
                warn!("Nieznany typ serwisu QUIC: {} ({})", service_type, name);
            }
        }
    }

    /// Usuwa serwis QUIC z mapy i wysyla sygnal shutdown do background tasku.
    pub fn remove_quic_service(&self, name: &str, service_type: &str) {
        match service_type {
            "llm" => {
                self.llm_model_categories.remove(name);
                if let Some((_, h)) = self.quic_llm_services.remove(name) {
                    h.shutdown();
                }
            }
            "tts" => {
                if let Some((_, h)) = self.quic_tts_services.remove(name) {
                    h.shutdown();
                }
            }
            "stt" => {
                if let Some((_, h)) = self.quic_stt_services.remove(name) {
                    h.shutdown();
                }
            }
            "embedding" => {
                if let Some((_, h)) = self.quic_embedding_services.remove(name) {
                    h.shutdown();
                }
            }
            "rag" => {
                if let Some((_, h)) = self.rag_services.remove(name) {
                    h.shutdown();
                }
            }
            "memory" => {
                if let Some((_, h)) = self.quic_memory_services.remove(name) {
                    h.shutdown();
                }
            }
            "meeting-bot" => {
                if let Some((_, h)) = self.quic_llm_services.remove(name) {
                    h.shutdown();
                }
            }
            _ => {}
        }
        info!(
            "Usunieto dynamiczny serwis QUIC: {} (typ={})",
            name, service_type
        );
    }

    // ========================================================================
    // MODEL POOL - mapowanie model_name -> serwisy
    // ========================================================================

    /// Zwraca liste serwisow obslugujacych dany model
    pub fn resolve_model_services(&self, model_name: &str) -> Option<Vec<String>> {
        self.model_pool
            .get(model_name)
            .map(|e| e.service_names.clone())
    }

    /// Wybiera najlepszy serwis dla modelu (round-robin)
    pub fn select_service_for_model(&self, model_name: &str) -> Option<String> {
        self.model_pool
            .get(model_name)
            .and_then(|e| e.next_service().map(|s| s.to_string()))
    }

    /// Sprawdza czy model istnieje w puli
    pub fn has_model(&self, model_name: &str) -> bool {
        self.model_pool.contains_key(model_name)
    }

    /// Usuwa mapowanie serwisu z puli modelu
    pub fn remove_model_mapping(&self, model_name: &str, service_name: &str) {
        let remaining = if let Some(mut entry) = self.model_pool.get_mut(model_name) {
            entry.service_names.retain(|s| s != service_name);
            entry.service_names.len()
        } else {
            return;
        };
        if remaining == 0 {
            self.model_pool.remove(model_name);
            info!("ModelPool: '{}' -> usunieto (brak serwisow)", model_name);
        } else {
            info!(
                "ModelPool: '{}' -> usunieto serwis '{}' (pozostalo: {})",
                model_name, service_name, remaining
            );
        }
    }

    /// Zmienia strategie load-balancing dla modelu w puli
    pub fn set_model_strategy(&self, model_name: &str, strategy: PoolStrategy) -> bool {
        if let Some(mut entry) = self.model_pool.get_mut(model_name) {
            entry.strategy = strategy;
            info!(
                "ModelPool: '{}' -> strategia zmieniona na {:?}",
                model_name, strategy
            );
            true
        } else {
            false
        }
    }

    /// Ustawia liste serwisow dla modelu w puli (zastepuje istniejace)
    pub fn set_model_services(&self, model_name: &str, service_names: Vec<String>) {
        let mut entry = self
            .model_pool
            .entry(model_name.to_string())
            .or_insert_with(ModelPoolEntry::new);
        entry.service_names = service_names;
        info!(
            "ModelPool: '{}' -> ustawiono {} serwisow",
            model_name,
            entry.service_names.len()
        );
    }

    /// Zwraca informacje o model_pool (do diagnostyki/API)
    pub fn get_model_pool_info(&self) -> Vec<(String, Vec<String>, String, String)> {
        self.model_pool
            .iter()
            .map(|kv| {
                let name = kv.key().clone();
                let entry = kv.value();
                (
                    name,
                    entry.service_names.clone(),
                    entry.strategy.to_string(),
                    entry.service_type.clone(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod strategy_tests {
    use super::PoolStrategy;

    #[test]
    fn parse_first_available() {
        assert!(matches!(
            PoolStrategy::parse("first_available"),
            PoolStrategy::FirstAvailable
        ));
    }

    #[test]
    fn parse_round_robin() {
        assert!(matches!(
            PoolStrategy::parse("round_robin"),
            PoolStrategy::RoundRobin
        ));
    }

    #[test]
    fn parse_least_loaded() {
        assert!(matches!(
            PoolStrategy::parse("least_loaded"),
            PoolStrategy::LeastLoaded
        ));
    }

    #[test]
    fn parse_unknown_defaults_to_first_available() {
        assert!(matches!(
            PoolStrategy::parse("garbage"),
            PoolStrategy::FirstAvailable
        ));
    }

    #[test]
    fn parse_empty_defaults_to_first_available() {
        assert!(matches!(
            PoolStrategy::parse(""),
            PoolStrategy::FirstAvailable
        ));
    }

    #[test]
    fn display_round_trip() {
        // Sprawdza ze Display i parse sa sprojne
        let strategies = [
            PoolStrategy::FirstAvailable,
            PoolStrategy::RoundRobin,
            PoolStrategy::LeastLoaded,
        ];
        for s in &strategies {
            let text = s.to_string();
            let parsed = PoolStrategy::parse(&text);
            assert_eq!(
                std::mem::discriminant(s),
                std::mem::discriminant(&parsed),
                "Round-trip nie powiodl sie dla {:?}",
                s
            );
        }
    }
}

#[cfg(test)]
mod snapshot_helpers_tests {
    //! FAZA-8b-3: tests for `resolve_http_backends_via_snapshot` and
    //! `hydrate_from_snapshot`. The legacy ad-hoc DashMap stores must be
    //! reconciled from a fresh snapshot so chat.rs callbacks and adapters
    //! observe newly-deployed services.

    use super::*;
    use crate::config::RouterConfig;
    use crate::services::supervisor::{ModelEntry, ServiceEntry, ServicesSnapshot};
    use crate::services::transport::Transport;
    use crate::services_repo::services::{DeployMethod, ServiceStatus};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::watch;

    fn fixture_entry(
        id: i64,
        engine_id: &str,
        transport: Transport,
        models: Vec<&str>,
    ) -> ServiceEntry {
        // BackendClient::new requires an API key (direct or via env). The
        // unified deploy pipeline always populates one of those fields for
        // HTTP services; the fixture mirrors that contract so the helper
        // can materialise the client without env wiring.
        let mut extra_config = HashMap::new();
        extra_config.insert("api_key".into(), "test-key".into());
        ServiceEntry {
            id,
            engine_id: engine_id.into(),
            deploy_method: DeployMethod::NativePythonBundle,
            transport,
            status: ServiceStatus::Running,
            endpoint_url: Some(format!("http://127.0.0.1:50{:02}", id % 100)),
            runtime_pid: None,
            runtime_port: Some(5050 + id as u16),
            sidecar_quic_port: Some(5060 + id as u16),
            models: models
                .into_iter()
                .enumerate()
                .map(|(i, name)| ModelEntry {
                    id: (id * 100) + i as i64,
                    model_name: name.into(),
                    display_name: None,
                    is_default: i == 0,
                })
                .collect(),
            timeout_ms: 30_000,
            max_concurrent: 16,
            weight: 100,
            model_name_override: None,
            extra_config,
        }
    }

    fn build_snapshot(services: Vec<ServiceEntry>) -> ServicesSnapshot {
        let mut models_by_name = HashMap::new();
        let mut services_by_id = HashMap::new();
        for (idx, svc) in services.iter().enumerate() {
            services_by_id.insert(svc.id, idx);
            for m in &svc.models {
                models_by_name.insert(m.model_name.clone(), svc.id);
            }
        }
        ServicesSnapshot {
            services,
            models_by_name,
            services_by_id,
            generated_at_unix_ms: 0,
        }
    }

    fn make_manager_with_snapshot(snap: ServicesSnapshot) -> Arc<ServiceManager> {
        let mgr = Arc::new(
            ServiceManager::new(Arc::new(RouterConfig::default()), None)
                .expect("ServiceManager construction"),
        );
        let (_tx, rx) = watch::channel(Arc::new(snap));
        mgr.set_snapshot_rx(rx);
        mgr
    }

    #[test]
    fn resolve_http_backends_via_snapshot_returns_clients_for_http() {
        let svc = fixture_entry(1, "vllm", Transport::HttpDirect, vec!["llama-x"]);
        let mgr = make_manager_with_snapshot(build_snapshot(vec![svc]));

        let backends = mgr
            .resolve_http_backends_via_snapshot("llama-x")
            .expect("snapshot HTTP backends");
        assert_eq!(backends.len(), 1);
    }

    #[test]
    fn resolve_http_backends_via_snapshot_skips_quic_and_embedded() {
        let q = fixture_entry(2, "vllm-q", Transport::SidecarQuic, vec!["llama-q"]);
        let e = fixture_entry(3, "llama-cpp", Transport::Embedded, vec!["llama-emb"]);
        let mgr = make_manager_with_snapshot(build_snapshot(vec![q, e]));

        // SidecarQuic and Embedded must yield None — they are not HTTP.
        assert!(mgr.resolve_http_backends_via_snapshot("llama-q").is_none());
        assert!(mgr
            .resolve_http_backends_via_snapshot("llama-emb")
            .is_none());
    }

    #[test]
    fn resolve_http_backends_via_snapshot_unknown_model_returns_none() {
        let mgr = make_manager_with_snapshot(ServicesSnapshot::default());
        assert!(mgr
            .resolve_http_backends_via_snapshot("nope-not-here")
            .is_none());
    }

    #[test]
    fn hydrate_from_snapshot_registers_local_inference_for_embedded() {
        let svc = fixture_entry(4, "llama-cpp", Transport::Embedded, vec!["qwen-mini"]);
        let mgr = make_manager_with_snapshot(build_snapshot(vec![svc]));

        assert!(!mgr.has_local_inference_service("qwen-mini"));
        mgr.hydrate_from_snapshot();
        assert!(mgr.has_local_inference_service("qwen-mini"));
    }
}
