// =============================================================================
// Plik: state/mod.rs
// Opis: Stan wspoldzielony miedzy Core a UI (Arc<RwLock>).
//       Odwzorowuje wszystkie dane z web dashboard routera.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Mesh / Peers
// ---------------------------------------------------------------------------

/// Informacje o peerze w mesh
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub node_id: String,
    pub hostname: String,
    pub address: String,
    pub ip_addresses: Vec<String>,
    pub role: String,
    pub status: String,
    pub quic_connected: bool,
    pub services: Vec<String>,
    pub cpu_usage: f64,
    pub ram_usage: f64,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub gpu_info: Option<String>,
    pub gpus: Vec<GpuInfo>,
    pub models: Vec<String>,
    pub containers: Vec<PeerContainerInfo>,
    pub labels: Vec<(String, String)>,
    pub network_rx_bytes_sec: f64,
    pub network_tx_bytes_sec: f64,
}

/// Informacja o GPU na peerze
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub usage_percent: f64,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
}

/// Informacja o kontenerze Docker na peerze
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PeerContainerInfo {
    pub name: String,
    pub image: String,
    pub status: String,
    pub cpu_percent: f32,
    pub memory_mb: u64,
}

// ---------------------------------------------------------------------------
// Models / Inference
// ---------------------------------------------------------------------------

/// Informacja o lokalnym modelu inference
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LocalModelInfo {
    pub name: String,
    /// Sciezka do katalogu/pliku modelu na dysku
    pub path: String,
    /// Rozmiar modelu w MB
    pub size_mb: u64,
    /// Format modelu: "mlx", "gguf", "safetensors", "unknown"
    pub format: String,
    pub loaded: bool,
    pub tokens_per_second: f64,
    pub vram_used_mb: u64,
}

/// Informacja o modelu w rejestrze
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: i64,
    pub name: String,
    pub display_name: String,
    pub service_type: ServiceType,
    pub strategy: String,
    pub service_count: usize,
    pub flow_id: Option<String>,
    pub is_public: bool,
    pub is_active: bool,
    pub loaded: bool,
    pub backend: String,
    pub tokens_per_second: f64,
}

/// Alias modelu
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelAlias {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

/// Typ serwisu AI
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ServiceType {
    Llm,
    Tts,
    Stt,
    Rag,
    Embedding,
    Vision,
    Router,
    Memory,
    Reranker,
}

impl Default for ServiceType {
    fn default() -> Self {
        Self::Llm
    }
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm => write!(f, "LLM"),
            Self::Tts => write!(f, "TTS"),
            Self::Stt => write!(f, "STT"),
            Self::Rag => write!(f, "RAG"),
            Self::Embedding => write!(f, "Embedding"),
            Self::Vision => write!(f, "Vision"),
            Self::Router => write!(f, "Router"),
            Self::Memory => write!(f, "Memory"),
            Self::Reranker => write!(f, "Reranker"),
        }
    }
}

/// Status serwisu
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Error,
    Starting,
}

impl Default for ServiceStatus {
    fn default() -> Self {
        Self::Stopped
    }
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Dziala"),
            Self::Stopped => write!(f, "Zatrzymany"),
            Self::Error => write!(f, "Blad"),
            Self::Starting => write!(f, "Uruchamianie"),
        }
    }
}

/// Status polaczenia QUIC
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QuicStatus {
    Connected,
    Connecting,
    Disconnected,
    ConfigError,
}

impl Default for QuicStatus {
    fn default() -> Self {
        Self::Disconnected
    }
}

impl std::fmt::Display for QuicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected => write!(f, "Connected"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Disconnected => write!(f, "Disconnected"),
            Self::ConfigError => write!(f, "Config Error"),
        }
    }
}

/// Informacja o serwisie AI
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceInfo {
    pub id: i64,
    pub name: String,
    pub service_type: ServiceType,
    pub status: ServiceStatus,
    pub quic_status: QuicStatus,
    pub quic_address: String,
    pub backends: Vec<String>,
    pub strategy: String,
    pub avg_latency_ms: f64,
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// API Keys
// ---------------------------------------------------------------------------

/// Klucz API
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKeyInfo {
    pub id: i64,
    pub key_prefix: String,
    pub name: String,
    pub rate_limit_rps: u32,
    pub is_active: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

/// Typ promptu
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromptType {
    System,
    Suffix,
    Template,
    User,
}

impl Default for PromptType {
    fn default() -> Self {
        Self::System
    }
}

impl std::fmt::Display for PromptType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => write!(f, "System"),
            Self::Suffix => write!(f, "Suffix"),
            Self::Template => write!(f, "Template"),
            Self::User => write!(f, "User"),
        }
    }
}

/// Informacja o prompcie
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptInfo {
    pub id: i64,
    pub name: String,
    pub prompt_id: String,
    pub prompt_type: PromptType,
    pub content: String,
    pub default_model: String,
    pub version: u32,
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

/// Status flow
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FlowStatus {
    Active,
    Inactive,
    Failed,
    Draft,
    Archived,
}

impl Default for FlowStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl std::fmt::Display for FlowStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Aktywny"),
            Self::Inactive => write!(f, "Nieaktywny"),
            Self::Failed => write!(f, "Blad"),
            Self::Draft => write!(f, "Szkic"),
            Self::Archived => write!(f, "Zarchiwizowany"),
        }
    }
}

/// Informacja o flow
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlowInfo {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub service_type: String,
    pub status: FlowStatus,
    pub last_run: Option<DateTime<Utc>>,
    pub flow_json: String,
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// Regula PII
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PiiRule {
    pub id: i64,
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
    pub priority: i32,
    pub is_active: bool,
}

/// Regula czyszczenia TTS
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TtsCleaningRule {
    pub id: i64,
    pub name: String,
    pub pattern: String,
    pub replacement: String,
    pub priority: i32,
    pub is_active: bool,
}

/// Wzorzec Fast Path
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FastPathPattern {
    pub id: i64,
    pub name: String,
    pub pattern: String,
    pub response: String,
    pub priority: i32,
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Pojedyncze ustawienie klucz-wartosc
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SettingEntry {
    pub key: String,
    pub value: String,
    pub updated_at: Option<String>,
}

/// Instancja Portainer
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PortainerInstance {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub auth_type: String,
}

// ---------------------------------------------------------------------------
// Chat / Playground
// ---------------------------------------------------------------------------

/// Rola w wiadomosci czatu
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

/// Wiadomosc czatu
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub duration_secs: Option<f64>,
    pub token_count: Option<u32>,
    pub tokens_per_sec: Option<f64>,
}

/// Konwersacja
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    pub model: String,
    pub system_prompt: String,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Dashboard metrics (real-time)
// ---------------------------------------------------------------------------

/// Metryki w czasie rzeczywistym
#[derive(Debug, Clone, Default)]
pub struct DashboardMetrics {
    pub tokens_in_per_sec: f64,
    pub tokens_out_per_sec: f64,
    pub active_requests: u64,
    pub avg_latency_ms: f64,
    pub active_services: u64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Historia tokens/sec (ostatnie 60 pomiarow)
    pub tokens_history: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Typ powiadomienia
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NotificationType {
    Info,
    Success,
    Warning,
    Error,
}

/// Powiadomienie systemowe
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    pub notification_type: NotificationType,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// UI Commands — komunikacja UI → Core (CRUD)
// ---------------------------------------------------------------------------

/// Komendy wysylane z UI do Core w celu wykonania operacji na bazie
#[derive(Debug, Clone)]
pub enum UiCommand {
    // --- Prompts ---
    CreatePrompt { prompt_id: String, name: String, content: String, prompt_type: String, default_model: String },
    UpdatePrompt { id: i64, name: String, content: String, prompt_type: String, default_model: String, is_active: bool },
    DeletePrompt(i64),

    // --- Services ---
    CreateService { name: String, service_type: String, strategy: String, config_json: String },
    DeleteService(i64),

    // --- Models ---
    CreateModelEntry { model_name: String, display_name: String, service_type: String, connection_type: String, is_public: bool, config_json: String },
    DeleteModelEntry(i64),

    // --- Model Aliases ---
    CreateModelAlias { alias: String, target_model: String },
    DeleteModelAlias(i64),

    // --- API Keys ---
    CreateApiKey { name: String, rate_limit_rps: i64 },
    DeleteApiKey(i64),

    // --- Flows ---
    CreateFlow { name: String, description: String, service_type: String, flow_json: String },
    DeleteFlow(i64),

    // --- PII Rules ---
    CreatePiiRule { name: String, category: String, pattern: String, replacement: String, priority: i64 },
    UpdatePiiRule { id: i64, name: String, category: String, pattern: String, replacement: String, is_active: bool, priority: i64 },
    DeletePiiRule(i64),

    // --- TTS Cleaning Rules ---
    CreateTtsRule { rule_type: String, pattern: String, replacement: String, language: String, priority: i64 },
    DeleteTtsRule(i64),

    // --- Fast Path Patterns ---
    CreateFastPath { module: String, pattern_type: String, pattern: String, match_type: String, result_json: String, priority: i64 },
    DeleteFastPath(i64),

    // --- Settings ---
    SetSetting { key: String, value: String },

    // --- Portainer ---
    CreatePortainerInstance { name: String, url: String, api_key: String, username: String, password: String },
    DeletePortainerInstance(i64),

    // --- Lokalne modele (pobieranie/ladowanie/wyladowywanie) ---
    DownloadModel { model_id: String },
    LoadModel { path: String },
    UnloadModel,
    DeleteLocalModel { path: String },

    // --- Force refresh ---
    RefreshAll,

    // --- Hub / Deploy ---
    FetchEngines { os_info: String },
    SearchHfModels { query: String, engine_id: String },
    FetchDefaultModels { engine_id: String },
    DeployLlm { peer_id: String, engine_id: String, model_id: String, port: u16, deploy_mode: String },
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Stan aplikacji wspoldzielony miedzy Core (tokio) a UI (egui)
#[derive(Debug, Clone, Default)]
pub struct AppState {
    // Node identity
    pub node_id: String,
    pub node_role: String,

    // Mesh
    pub peers: Vec<PeerInfo>,
    pub mesh_connected: bool,

    // Router
    pub router_running: bool,

    // Real-time metrics
    pub metrics: DashboardMetrics,
    pub total_requests: u64,
    pub avg_latency_ms: f64,

    // Services
    pub services: Vec<ServiceInfo>,

    // Models
    pub models: Vec<ModelInfo>,
    pub model_aliases: Vec<ModelAlias>,
    pub local_models: Vec<LocalModelInfo>,
    pub download_progress: HashMap<String, f32>,

    // API Keys
    pub api_keys: Vec<ApiKeyInfo>,

    // Prompts
    pub prompts: Vec<PromptInfo>,

    // Flows
    pub flows: Vec<FlowInfo>,

    // Rules
    pub pii_rules: Vec<PiiRule>,
    pub tts_cleaning_rules: Vec<TtsCleaningRule>,
    pub fast_path_patterns: Vec<FastPathPattern>,

    // Settings
    pub settings: Vec<SettingEntry>,
    pub portainer_instances: Vec<PortainerInstance>,

    // Chat
    pub chat_messages: Vec<ChatMessage>,
    pub conversations: Vec<Conversation>,

    // Notifications
    pub notifications: Vec<Notification>,

    // Command channel (UI → Core)
    #[allow(dead_code)]
    cmd_tx: Option<mpsc::UnboundedSender<UiCommand>>,
}

impl AppState {
    /// Wysyla komende do Core (CRUD na bazie)
    pub fn send_command(&self, cmd: UiCommand) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Ustawia nadawce komend
    pub fn set_command_sender(&mut self, tx: mpsc::UnboundedSender<UiCommand>) {
        self.cmd_tx = Some(tx);
    }

    pub fn add_notification(&mut self, notification_type: NotificationType, msg: impl Into<String>) {
        self.notifications.push(Notification {
            notification_type,
            message: msg.into(),
            timestamp: Utc::now(),
        });
    }

    pub fn add_chat_message(&mut self, role: ChatRole, content: impl Into<String>) {
        self.chat_messages.push(ChatMessage {
            role,
            content: content.into(),
            reasoning_content: None,
            timestamp: Utc::now(),
            duration_secs: None,
            token_count: None,
            tokens_per_sec: None,
        });
    }

    pub fn update_peer(&mut self, peer: PeerInfo) {
        if let Some(existing) = self.peers.iter_mut().find(|p| p.node_id == peer.node_id) {
            *existing = peer;
        } else {
            self.peers.push(peer);
        }
    }

    pub fn online_peer_count(&self) -> usize {
        self.peers.iter().filter(|p| p.status == "online" || p.status == "connected").count()
    }

    /// Aktualizuj historie metryk (dodaj punkt, zachowaj max 60)
    pub fn push_metrics_point(&mut self) {
        let val = self.metrics.tokens_in_per_sec + self.metrics.tokens_out_per_sec;
        self.metrics.tokens_history.push(val);
        if self.metrics.tokens_history.len() > 60 {
            self.metrics.tokens_history.remove(0);
        }
    }
}

/// Thread-safe wrapper
pub type SharedAppState = Arc<RwLock<AppState>>;

/// Tworzy nowy wspoldzielony stan
pub fn new_shared_state() -> SharedAppState {
    Arc::new(RwLock::new(AppState::default()))
}
