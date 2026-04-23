// =============================================================================
// Plik: message_body.rs
// Opis: Bootstrap 10 wariantow MessageBody (bootstrap). MessageBody to tresc
//       envelope'u — rkyv-serializowana osobno i trzymana jako Vec<u8> w polu
//       Envelope.body. Dzieki temu policy check dziala na envelope bez tykania
//       body, a dispatcher decoduje dopiero po przejsciu auth.
// Przyklad:
//   let body = MessageBody::ModelListRequest;
//   let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body)?.to_vec();
//   let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};

// =============================================================================
// Pomocnicze typy (bootstrap — docelowo rozpisane per-archetype)
// =============================================================================

/// Lekki widok modelu w katalogu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelSummary {
    /// Np. "llama-3.2-1b-instruct".
    pub id: String,
    /// Rodzina: "llm", "tts", "stt", "embedding", itd.
    pub category: String,
    /// Silnik ktory uruchamia model: "llama-cpp", "mlx", "vllm"...
    pub engine_id: String,
    /// `ready`, `downloading`, `not-installed`.
    pub availability: String,
}

// =============================================================================
// Kody bledu protokolu
// =============================================================================

/// Ustabilizowane kody bledu dla `ProtocolError.code`. Dodatkowe (numeryczne)
/// mozna zawsze dorzucic — klient powinien obslugiwac nieznane graceful.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    /// Malformed frame, failed bytecheck, wrong schema version.
    InvalidFrame = 1,
    /// Brak autoryzacji dla tego MessageBody variant.
    PolicyDenied = 2,
    /// SessionAuth nie odpowiada minimum dla tej operacji.
    AuthRequired = 3,
    /// Adresowany node_id nieznany lub offline.
    NodeUnreachable = 4,
    /// Stream anulowany przez klienta lub server timeout.
    StreamCancelled = 5,
    /// Rate limit przekroczony per sesja.
    RateLimited = 6,
    /// Nie zaimplementowany handler dla tego variantu.
    NotImplemented = 7,
    /// Wewnetrzny blad serwera (szczegoly w `message`).
    Internal = 8,
    /// Zasob nie znaleziony.
    NotFound = 9,
    /// Niepoprawne argumenty requestu (walidacja pol).
    BadRequest = 10,
}

/// Ujednolicony blad protokolu. Zwracany jako `MessageBody::Error(..)` z flagą
/// `EnvelopeFlags::IS_ERROR` ustawioną dla szybkiego branchowania.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    /// Kod ustabilizowany.
    pub code: ProtocolErrorCode,
    /// Human-readable message (en, dla klienta — lokalizacja po stronie GUI).
    pub message: String,
    /// Opcjonalny trace_id do korelacji z logami serwera.
    pub trace_id: Option<String>,
}

impl ProtocolError {
    /// Convenience: nowy blad z kodem + message, bez trace_id.
    pub fn new(code: ProtocolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            trace_id: None,
        }
    }

    /// Convenience: BadRequest z message.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ProtocolErrorCode::BadRequest, message)
    }

    /// Convenience: Internal z message.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ProtocolErrorCode::Internal, message)
    }

    /// Convenience: NotFound z message.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ProtocolErrorCode::NotFound, message)
    }

    /// Convenience: dodaj trace_id (builder-style).
    pub fn with_trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

// =============================================================================
// API Keys (R-LIST + W-CREATE + W-DELETE archetypes, migration-map #37-#39)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub key_id: String,
    pub name: String,
    pub created_at_epoch: u64,
    pub last_used_at_epoch: Option<u64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreateRequest {
    pub name: String,
    pub scopes: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreateResponse {
    pub key_id: String,
    /// Pelny token (widoczny TYLKO raz, w odpowiedzi na creation).
    pub token: String,
}

// =============================================================================
// Auth (W-ACTION + R-ONE archetypes, migration-map #40-#42)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthLoginResponse {
    pub jwt: String,
    pub user_id: [u8; 16],
    pub role: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthMeResponse {
    pub user_id: [u8; 16],
    pub username: String,
    pub role: String,
}

// =============================================================================
// Chat streaming (R-STREAM archetyp, migration-map #43)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// "system" / "user" / "assistant" / "tool".
    pub role: String,
    pub content: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct ChatStreamRequest {
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatStreamChunk {
    /// Partial token/fragment od modelu.
    pub delta: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ChatStreamEnd {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

// =============================================================================
// Models — szczegoly modelu (R-ONE), instalacja/odinstalacja (W-ACTION)
// migration-map #218-#227
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelDetail {
    pub id: String,
    pub category: String,
    pub engine_id: String,
    /// Sciezka pliku modelu na disku (jesli zainstalowany).
    pub local_path: Option<String>,
    /// Rozmiar w bajtach.
    pub size_bytes: u64,
    /// "ready" | "downloading" | "not-installed" | "error".
    pub availability: String,
    /// Opis (z manifest.toml).
    pub description: String,
    /// Hash SHA256 dla weryfikacji integralnosci.
    pub checksum_sha256: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelInstallRequest {
    pub model_id: String,
    /// Repozytorium HuggingFace (np. "Qwen/Qwen3.5-0.8B").
    pub source_repo: String,
}

// =============================================================================
// Hub — HuggingFace integration (R-LIST + R-STREAM dla download progress)
// migration-map #81-#86
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct HubEngineSummary {
    pub id: String,
    pub display_name: String,
    pub category: String,
    /// "docker" | "native" | "external".
    pub deploy_methods: Vec<String>,
    pub default_port: u16,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct HubModelSearchResult {
    pub repo_id: String,
    pub display_name: String,
    pub author: String,
    /// Liczba downloadow w HuggingFace (popularity signal).
    pub downloads: u64,
    pub likes: u64,
    pub last_modified_epoch: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct HubDownloadProgress {
    pub model_id: String,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
}

// =============================================================================
// Flows — workflow CRUD + executions (migration-map #65-#80)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at_epoch: u64,
    pub updated_at_epoch: u64,
    pub enabled: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowDetail {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// JSON DAG definition (zachowane jako string — parsowane przez flow_engine).
    pub graph_json: String,
    pub enabled: bool,
    /// Raw flow status column: "active" | "draft" | "archived" itp.
    pub status: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowCreateRequest {
    pub name: String,
    pub description: Option<String>,
    pub graph_json: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowExecutionSummary {
    pub id: String,
    pub flow_id: String,
    /// "pending" | "running" | "completed" | "failed" | "cancelled".
    pub status: String,
    pub started_at_epoch: u64,
    pub completed_at_epoch: Option<u64>,
}

// =============================================================================
// Services — runtime engine deployments (migration-map #295-#303)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceSummary {
    pub id: String,
    /// Nazwa serwisu nadana przez uzytkownika (np. `embeddings-bge`).
    pub name: String,
    /// Typ: "llm" | "embedding" | "stt" | "tts" | "rag" | "tools" | "memory" | "reranker".
    pub service_type: String,
    /// Strategia routingu (np. `single`).
    pub strategy: String,
    /// "active" | "inactive" | "running" | "starting" | "stopped" | "error".
    pub status: String,
    /// Serializowany JSON konfiguracji (quic_url, sni_domain, cluster_id).
    pub config_json: String,
    /// `None` gdy lokalny, hex enkodowany node_id mesh w innym wypadku.
    pub node_id: Option<String>,
    /// Czytelna nazwa wezla mesh (hostname) dla kolumny tabeli.
    pub node_hostname: Option<String>,
    /// ISO-8601 timestamp utworzenia.
    pub created_at: String,
    /// Metoda wdrozenia gdy serwis pochodzi z katalogu silnikow: "docker" | "native" | "external".
    pub deploy_method: Option<String>,
    /// Zewnetrzny URL endpointu silnika (jesli znany).
    pub endpoint_url: Option<String>,
    /// Unix epoch uruchomienia silnika.
    pub started_at_epoch: Option<u64>,
    /// Identyfikator silnika z katalogu (jesli wdrozony z katalogu).
    pub engine_id: Option<String>,
    /// Identyfikator modelu (jesli serwis obsluguje konkretny model).
    pub model_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceCreateRequest {
    pub name: String,
    pub service_type: String,
    pub strategy: String,
    pub config_json: String,
    /// Hex-enkodowany 32-bajtowy node_id lub `None` dla lokalnego.
    pub node_id: Option<String>,
    /// Id klastra do ktorego serwis nalezy.
    pub cluster_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceUpdateRequest {
    pub id: String,
    pub name: String,
    pub service_type: String,
    pub strategy: String,
    pub status: String,
    pub config_json: String,
    pub node_id: Option<String>,
    pub cluster_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceQuicStatus {
    pub name: String,
    /// "connected" | "connecting" | "disconnected" | "ready" | "config_error" | "none".
    pub status: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceDeployRequest {
    pub engine_id: String,
    pub model_id: String,
    /// "docker" | "native" | "external".
    pub deploy_method: String,
    pub node_id: [u8; 32],
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceDeployProgress {
    pub deploy_id: String,
    /// "pulling" | "building" | "starting" | "ready" | "failed".
    pub stage: String,
    pub progress_percent: u8,
    pub message: String,
}

// =============================================================================
// Prompts — prompt templates (migration-map #265-#269)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PromptSummary {
    pub id: String,
    pub name: String,
    pub category: String,
    pub updated_at_epoch: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PromptDetail {
    pub id: String,
    pub name: String,
    pub category: String,
    pub template: String,
    pub variables: Vec<String>,
    pub updated_at_epoch: u64,
}

// =============================================================================
// Registries — Docker/Conda registries (migration-map #275-#279)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct RegistrySummary {
    pub id: String,
    pub url: String,
    /// "docker" | "conda" | "huggingface".
    pub kind: String,
    pub auth_required: bool,
}

// =============================================================================
// Audit logs — read-only event stream (event-push archetype)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub ts_epoch: u64,
    pub user_id: Option<[u8; 16]>,
    /// "login" | "logout" | "deploy" | "delete" | "config-change" itp.
    pub event_kind: String,
    pub resource_id: Option<String>,
    pub message: String,
}

// ----- Audit log screen (Admin only) -----

/// Optional filters for audit log list/export — all fields nullable.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditLogFilters {
    pub user_id: Option<i64>,
    pub addon_id: Option<String>,
    pub action: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub search: Option<String>,
}

/// Single audit log row as returned to the Admin screen.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub action: String,
    pub user_id: Option<i64>,
    pub addon_id: Option<String>,
    pub resource: Option<String>,
    pub details: Option<String>,
    pub ip_address: Option<String>,
    pub node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogListRequest {
    pub filters: AuditLogFilters,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogListResponse {
    pub entries: Vec<AuditLogEntry>,
    pub total_count: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogExportRequest {
    pub filters: AuditLogFilters,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogExportResponse {
    pub csv: String,
    pub row_count: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogCleanupRequest {
    pub keep_days: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AuditLogCleanupResponse {
    pub deleted_count: u64,
}

// =============================================================================
// Portainer — Docker container ops (migration-map #248-#259)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    /// "running" | "stopped" | "paused" | "exited".
    pub state: String,
    pub created_at_epoch: u64,
    pub ports: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ContainerLogChunk {
    pub container_id: String,
    pub stream: String, // "stdout" | "stderr"
    pub line: String,
    pub ts_epoch: u64,
}

// =============================================================================
// Voice profiles — speaker enrollment (migration-map #325-#332)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VoiceProfileSummary {
    pub id: String,
    pub display_name: String,
    pub embedding_count: u32,
    pub created_at_epoch: u64,
}

// =============================================================================
// TTS rules — text→speech routing rules (migration-map #316-#319)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TtsRule {
    pub id: String,
    /// Regex pattern w treści wiadomości.
    pub pattern: String,
    /// Voice ID do uzycia gdy pattern matchuje.
    pub voice_id: String,
    pub priority: i32,
}

// =============================================================================
// PII rules — personally-identifiable-info redaction (migration-map #239-#242)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PiiRule {
    pub id: String,
    /// "email" | "phone" | "ssn" | "credit-card" | "custom".
    pub kind: String,
    pub regex: String,
    /// "redact" | "hash" | "tokenize".
    pub action: String,
}

// =============================================================================
// Fast-path patterns — bypass routing for known prompts (migration-map #61-#64)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FastPathPattern {
    pub id: String,
    pub pattern: String,
    pub response: String,
    pub priority: i32,
}

// =============================================================================
// Mesh trust events (W-ACTION + Event-push archetypy, mesh discriminants 0x23/0x24)
// =============================================================================

/// Broadcast: trust dla noda zostal cofniety (TrustRevoked, mesh discriminant 0x23).
/// Rozsylany do wszystkich peerow zeby usunac compromised key z trusted_keys.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustRevokedEvent {
    /// Node ktorego trust cofniety (Ed25519 public key, 32 bajty).
    pub revoked_node_id: [u8; 32],
    /// Powod cofniecia (audit trail).
    pub reason: String,
    /// Unix epoch — kiedy nastapilo cofniecie.
    pub revoked_at_epoch: u64,
}

/// Sync trusted_keys po pairing — node A wysyla swoja liste do noda B
/// zeby B widzial peerow A's mesh (mesh discriminant 0x24).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustedKeysSyncEvent {
    /// Lista trusted Ed25519 public keys (kazdy 32 bajty).
    pub trusted_keys: Vec<[u8; 32]>,
    /// Aktualny epoch sender'a (do replay protection).
    pub epoch: u32,
}

// =============================================================================
// Mesh peers (R-LIST + W-ACTION archetypy, migration-map #87-#92)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPeerSummary {
    pub node_id: [u8; 32],
    pub display_name: String,
    /// "trusted" / "pending" / "revoked" / "online".
    pub trust_state: String,
    /// Hostname lub ostatni znany IP.
    pub endpoint: Option<String>,
    pub last_seen_epoch: Option<u64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairInitRequest {
    pub node_id: [u8; 32],
    /// PIN wpisany przez administratora (6 cyfr).
    pub pin: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairInitResponse {
    pub pair_id: String,
    pub expires_at_epoch: u64,
}

// =============================================================================
// Mesh extended (FAZA 1a/1b: read-only + write actions for admin/dashboard).
// Helper structs are mirrored 1:1 by `mesh_node_info_to_js` and the
// per-variant encoders in `tentaflow-protocol-wasm`.
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeGpuInfo {
    pub vendor: String,
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: Option<u64>,
    pub temperature_c: Option<f32>,
    pub power_draw_w: Option<f32>,
    pub utilization_percent: Option<f32>,
    pub driver_version: Option<String>,
    pub cuda_version: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeNetworkInterface {
    pub name: String,
    pub link_up: bool,
    pub speed_mbps: Option<u32>,
    pub ipv4_address: Option<String>,
    pub interface_type: Option<String>,
    pub rdma_available: Option<bool>,
    pub roce_available: Option<bool>,
    pub numa_node: Option<i32>,
    pub rx_bytes_per_sec: Option<u64>,
    pub tx_bytes_per_sec: Option<u64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeModel {
    pub alias: String,
    pub kind: Option<String>,
    pub backend: Option<String>,
    pub size_mb: Option<u64>,
    pub loaded: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeContainer {
    pub name: String,
    pub image: String,
    pub status: String,
    pub cpu_percent: Option<f32>,
    pub memory_mb: Option<f32>,
    pub memory_limit_mb: Option<u64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeRoute {
    pub hops: u32,
    pub direct: bool,
    pub next_hop: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshConnectionPathInfo {
    pub transport: String,
    pub address: String,
    pub selected: bool,
    pub closed: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshConnectionInfo {
    pub transport: String,
    pub scope: Option<String>,
    pub address: Option<String>,
    pub relay_url: Option<String>,
    pub paths: Vec<MeshConnectionPathInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeInfo {
    pub node_id: String,
    pub hostname: String,
    pub ip: Option<String>,
    pub status: String,
    pub source: String,
    pub is_local: bool,
    pub uptime_secs: Option<u64>,
    pub gpus: Vec<MeshNodeGpuInfo>,
    pub network_interfaces: Vec<MeshNodeNetworkInterface>,
    pub cpu_count: Option<u32>,
    pub cpu_usage_percent: Option<f32>,
    pub ram_total_mb: Option<u64>,
    pub ram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub vram_used_mb: Option<u64>,
    pub gpu_load_percent: Option<f32>,
    pub models: Vec<MeshNodeModel>,
    pub containers: Vec<MeshNodeContainer>,
    pub last_seen_epoch: Option<i64>,
    pub route: Option<MeshNodeRoute>,
    pub platform: String,
    pub connection: Option<MeshConnectionInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeListResponse {
    pub nodes: Vec<MeshNodeInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeDetailRequest {
    pub node_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeDetailResponse {
    pub node: MeshNodeInfo,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPendingPair {
    pub pair_id: String,
    pub remote_node_id: String,
    pub remote_hostname: Option<String>,
    pub remote_ip: Option<String>,
    pub initiated_at: i64,
    pub state: String,
    pub pin: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPendingListResponse {
    pub pending: Vec<MeshPendingPair>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshIdentityResponse {
    pub node_id: String,
    pub hostname: String,
    pub public_key: String,
    pub addresses: Vec<String>,
    pub relay_url: String,
    pub version: String,
    /// Aktywny invite PIN dla QR. Empty string gdy disabled.
    /// Frontend odswieza co 50s (co kazdy re-fetch identity).
    pub invite_pin: String,
    /// Ile sekund do wygasniecia invite PIN (0 = brak).
    pub invite_pin_expires_sec: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshServicesEntry {
    pub service_name: String,
    pub node_id: String,
    pub status: String,
    pub endpoint: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshServicesListResponse {
    pub services: Vec<MeshServicesEntry>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustedNode {
    pub node_id: String,
    pub hostname: Option<String>,
    pub trusted_since_epoch: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustedListResponse {
    pub trusted: Vec<MeshTrustedNode>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingStartRequest {
    pub remote_address: String,
    /// Opcjonalny PIN z QR — gdy podany, initiate uzywa go zamiast generowac
    /// losowy. Pozwala nodowi B (skanujacemu) uzyc invite PIN-u nodu A, co
    /// triggeruje auto-confirm po stronie A bez user-interakcji.
    pub pin_hint: String,
    /// Publiczny klucz zdalnego noda (Ed25519 + X25519), jesli byl dostepny
    /// np. z QR. Nie jest wymagany do zestawienia polaczenia.
    pub remote_public_key: String,
    /// Lista adresow `ip:port` zdalnego noda z QR albo discovery.
    pub remote_addresses: Vec<String>,
    /// Relay URL zdalnego noda, jesli byl znany przy inicjacji.
    pub remote_relay_url: String,
    /// Hostname zdalnego noda — tylko hint diagnostyczny/UI.
    pub remote_hostname: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingStartResponse {
    pub pair_id: String,
    pub pin: String,
    pub completed: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingConfirmRequest {
    pub pair_id: String,
    pub pin: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingConfirmResponse {
    pub ok: bool,
    pub trusted_node_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingRejectRequest {
    pub pair_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingRejectResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustRevokeRequest {
    pub node_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustRevokeResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustRetrustRequest {
    pub node_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshTrustRetrustResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshConnectRequest {
    pub address: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshConnectResponse {
    pub ok: bool,
    pub remote_node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeCommandRequest {
    pub node_id: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeCommandResponse {
    pub ok: bool,
    pub output: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeNetworkConfigRequest {
    pub node_id: String,
    pub interface_name: String,
    pub config_json: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshNodeNetworkConfigResponse {
    pub ok: bool,
}

// =============================================================================
// Settings (R-LIST + W-UPDATE archetypy, migration-map #147-#148)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingEntry {
    pub key: String,
    pub value: String,
    /// Czy wartosc powinna byc zaszyfrowana (secret).
    pub is_secret: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingsUpdateRequest {
    pub entries: Vec<SettingEntry>,
}

// =============================================================================
// Dashboard metrics (R-LIST z subscription candidate, migration-map #60)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct DashboardSnapshot {
    pub cpu_usage_percent: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub active_requests: u64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub tokens_per_second: u64,
    pub active_services: u32,
}

// =============================================================================
// Clusters — full CRUD + member ops + probe streaming
// =============================================================================

/// Cluster summary returned by list/detail endpoints. Aggregates derived in
/// handler (members_count, members_online, status from online count).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub strategy: String,
    /// "active" | "inactive" — derived from members_online count.
    pub status: String,
    pub members_count: u32,
    pub members_online: u32,
    /// Unix epoch seconds (from SQLite timestamp parse).
    pub created_at: i64,
    pub updated_at: i64,
    pub failover_enabled: bool,
    pub failover_target: Option<String>,
    pub health_check_interval_ms: u32,
    pub timeout_ms: u32,
}

/// Single member of a cluster (node + interface info).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterMember {
    /// Hex-encoded 32-byte mesh node id.
    pub node_id: String,
    /// Peer hostname or node_id fallback.
    pub hostname: String,
    /// "online" | "offline" — from peer_store.
    pub status: String,
    pub interface_type: Option<String>,
    pub interface_speed_mbps: Option<u32>,
    /// Unix epoch seconds when member joined the cluster.
    pub joined_at: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterListResponse {
    pub clusters: Vec<ClusterInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterDetailRequest {
    pub cluster_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterDetailResponse {
    pub cluster: ClusterInfo,
    pub members: Vec<ClusterMember>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterCreateRequest {
    pub name: String,
    pub description: Option<String>,
    /// "distributed" | "replicated" | "primary_replica".
    pub strategy: String,
    pub failover_enabled: bool,
    pub failover_target: Option<String>,
    pub health_check_interval_ms: u32,
    pub timeout_ms: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterCreateResponse {
    pub cluster_id: String,
}

/// Partial-update request: `None` leaves the current value untouched server-side.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterUpdateRequest {
    pub cluster_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub strategy: Option<String>,
    pub failover_enabled: Option<bool>,
    pub failover_target: Option<String>,
    pub health_check_interval_ms: Option<u32>,
    pub timeout_ms: Option<u32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterUpdateResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterDeleteRequest {
    pub cluster_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterDeleteResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterAddMemberRequest {
    pub cluster_id: String,
    pub node_id: String,
    pub interface_type: Option<String>,
    pub interface_speed_mbps: Option<u32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterAddMemberResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterRemoveMemberRequest {
    pub cluster_id: String,
    pub node_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterRemoveMemberResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterProbeStreamRequest {
    pub node_ids: Vec<String>,
}

/// Single probe event. `event_type` is one of "started" | "probing_pair" |
/// "result" | "complete"; the populated optional fields depend on it.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterProbeStreamChunk {
    pub event_type: String,
    pub source_node: Option<String>,
    pub target_node: Option<String>,
    pub success: Option<bool>,
    pub latency_ms: Option<u32>,
    pub bandwidth_mbps: Option<u32>,
    pub interface_type: Option<String>,
    pub message: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ClusterProbeStreamEnd {
    pub total_pairs: u32,
    pub successful: u32,
    pub failed: u32,
}

// =============================================================================
// Flows phase 3 — partial update, node template palette, version history
// =============================================================================

/// Partial update — fields left `None` keep their existing server-side value.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowUpdateRequest {
    pub flow_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Full DAG JSON replacement when present.
    pub flow_json: Option<String>,
    /// Raw status column ("active" | "draft" | "archived" ...).
    pub status: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowUpdateResponse {
    pub ok: bool,
}

/// Single entry in the node-template palette shown by the flow builder.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowNodeTemplate {
    /// Database row id (palette template id).
    pub id: i64,
    pub node_type: String,
    pub category: String,
    pub label: String,
    pub description: Option<String>,
    /// Default config JSON shoved into a new node when dropped on the canvas.
    pub default_config: String,
    pub icon: Option<String>,
    /// Dostepne porty wejsciowe adaptera dla tego typu node'a. Pusta lista
    /// oznacza "nieznany adapter" — GUI powinno odradzac wiazania takich nodow.
    pub input_ports: Vec<String>,
    /// Dostepne porty wyjsciowe adaptera. LLM: ["stream","full"], wiekszosc
    /// innych: ["full"]. Pusta lista = nieznany adapter.
    pub output_ports: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowNodeTemplatesListResponse {
    pub templates: Vec<FlowNodeTemplate>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionListRequest {
    pub flow_id: String,
}

/// Lightweight view (no full flow_json) for the version-history list.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionSummary {
    pub id: String,
    pub flow_id: String,
    pub version_num: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub created_at_epoch: u64,
    pub created_by: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionListResponse {
    pub versions: Vec<FlowVersionSummary>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionGetRequest {
    pub flow_id: String,
    pub version_id: String,
}

/// Full version payload including embedded DAG JSON for diff/restore.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionFull {
    pub id: String,
    pub flow_id: String,
    pub version_num: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub flow_json: String,
    pub created_at_epoch: u64,
    pub created_by: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionGetResponse {
    pub version: FlowVersionFull,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionRestoreRequest {
    pub flow_id: String,
    pub version_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct FlowVersionRestoreResponse {
    pub ok: bool,
}

// ----- SSO / TLS / NGC -----

/// Pojedynczy wpis providera SSO dla listy admina. `client_secret` nie jest
/// zwracany do GUI — jedynie pola nie-sekretne. `default_group_id` jest opcjonalny
/// (Option) bo provider moze nie mapowac uzytkownikow do grupy domyslnej.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProviderEntry {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub discovery_url: String,
    pub enabled: bool,
    pub auto_create_users: bool,
    pub default_group_id: Option<i64>,
    pub created_at: String,
}

/// Response: lista wszystkich skonfigurowanych providerow SSO (Admin only).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProvidersListResponse {
    pub providers: Vec<SsoProviderEntry>,
}

/// Request: utworz nowego providera SSO/OIDC. `client_secret` jest szyfrowany
/// po stronie serwera przed zapisem do bazy (cipher w AppState).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProviderCreateRequest {
    pub name: String,
    pub provider_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub discovery_url: String,
    pub auto_create_users: bool,
    pub default_group_id: Option<i64>,
}

/// Response: potwierdzenie utworzenia providera SSO.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProviderCreateResponse {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
}

/// Request: usun providera SSO po id.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProviderDeleteRequest {
    pub id: i64,
}

/// Response: flagaczy provider istnial i zostal usuniety.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SsoProviderDeleteResponse {
    pub deleted: bool,
}

/// Response: status konfiguracji TLS (obecnosc cert/key w settings, bez ujawniania wartosci).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TlsStatusResponse {
    pub has_cert: bool,
    pub has_key: bool,
}

/// Response: status konfiguracji NGC (czy API key jest ustawiony, bez ujawniania wartosci).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NgcStatusResponse {
    pub configured: bool,
}

// =============================================================================
// Models / aliases / catalog (FAZA 2 + FAZA 5 — REST -> binary)
// =============================================================================

/// Instance of a unified model on a specific mesh node.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct UnifiedModelInstance {
    pub node_id: String,
    pub node_hostname: Option<String>,
    pub service_id: String,
    pub status: String,
    /// Engine serving the model (e.g. "llama-cpp", "vllm", "mlx", "whisper-rs").
    pub backend: Option<String>,
    /// Model weights size in MB when known.
    pub size_mb: Option<u64>,
    /// Convenience flag mirroring "status is running/ready".
    pub loaded: bool,
}

/// Unified model entry aggregating instances across mesh nodes.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct UnifiedModel {
    pub model_name: String,
    pub service_type: String,
    pub instances: Vec<UnifiedModelInstance>,
}

/// Response for `ModelsUnifiedListRequest`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelsUnifiedListResponse {
    pub models: Vec<UnifiedModel>,
}

/// Single model alias entry mapped from `DbModelAlias`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasEntry {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: bool,
    pub fallback_targets: Option<String>,
    pub strategy: Option<String>,
}

/// Response for `ModelAliasListRequest`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasListResponse {
    pub aliases: Vec<ModelAliasEntry>,
}

/// Request: create new model alias (Admin).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasCreateRequest {
    pub alias: String,
    pub target_model: String,
    pub strategy: Option<String>,
    pub fallback_targets: Option<String>,
}

/// Response: id of the newly created alias row.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasCreateResponse {
    pub id: i64,
}

/// Request: update existing model alias by id (Admin).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasUpdateRequest {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: Option<bool>,
    pub strategy: Option<String>,
    pub fallback_targets: Option<String>,
}

/// Response: whether update succeeded.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasUpdateResponse {
    pub ok: bool,
}

/// Request: delete alias by id (Admin).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasDeleteRequest {
    pub id: i64,
}

/// Response: whether delete succeeded.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelAliasDeleteResponse {
    pub ok: bool,
}

/// Single NIM catalog container entry mirrored from `api_nim::NimContainer`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NimContainerEntry {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub image: String,
    pub latest_tag: String,
    pub publisher: String,
    pub category: String,
    pub min_gpu_memory_gb: Option<u32>,
    pub updated_at: Option<String>,
    pub self_hostable: bool,
}

/// Response for `NimCatalogListRequest` (optional fetch error string).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NimCatalogListResponse {
    pub containers: Vec<NimContainerEntry>,
    pub error: Option<String>,
}

/// Request: deploy engine described by Service Manifest (Admin).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceManifestDeployRequest {
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub config_json: String,
}

/// Response: deploy descriptor plus websocket URL for progress stream.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceManifestDeployResponse {
    pub status: String,
    pub deploy_id: String,
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub websocket_url: String,
}

// =============================================================================
// MessageBody — wszystkie warianty
// =============================================================================

/// Enum wariantow tresci. Bootstrap (#29) zawieral 10; #36 dokladuje 10 kolejnych
/// pokrywajacych wszystkie 7 archetypow (R-ONE, R-LIST, R-STREAM, W-CREATE,
/// W-UPDATE, W-DELETE, W-ACTION). Dla kazdego variantu MUSI istniec wpis w
// =============================================================================
// Addons — list/detail/toggle/install/uninstall/reload + config + logs + tools
// + resources + network rules + visibility + permissions + OAuth (migration 38).
// =============================================================================

/// Summary wiersz dla listy addonow (kafelki w dashboard / catalog).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonInfo {
    pub addon_id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub is_enabled: bool,
    pub is_system: bool,
    pub runtime: String,
    pub oauth_mode: Option<String>,
    pub visibility_scope: String,
    pub declared_permissions_count: i32,
    pub users_with_oauth_count: i32,
    pub icon: Option<String>,
    pub category: Option<String>,
    pub file_size_bytes: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonsListResponse {
    pub addons: Vec<AddonInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonDetailRequest {
    pub addon_id: String,
}

/// Deklaracja uprawnienia (z manifestu addona).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionDecl {
    pub permission_id: String,
    pub display_name: String,
    pub description: String,
    pub risk: String,
    pub sort_order: i32,
}

/// Deklaracja providera OAuth (z manifestu).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthProviderDecl {
    pub addon_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub revoke_url: Option<String>,
    pub scopes: Vec<String>,
    pub mode: String,
    pub pkce: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonDetailResponse {
    pub addon_id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub is_enabled: bool,
    pub is_system: bool,
    pub admin_only: bool,
    pub category: String,
    pub permissions: Vec<AddonPermissionDecl>,
    pub oauth_providers: Vec<AddonOAuthProviderDecl>,
    pub license: String,
    pub file_size_bytes: i64,
    pub runtime: String,
    pub icon: Option<String>,
    pub oauth_mode: Option<String>,
    pub visibility_groups_visible: i32,
    pub visibility_groups_total: i32,
    pub tools_count: i32,
    pub linked_accounts_count: i32,
    pub show_in_catalog: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToggleRequest {
    pub addon_id: String,
    pub enabled: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToggleResponse {
    pub ok: bool,
    pub enabled: bool,
    pub message: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonInstallRequest {
    pub filename: String,
    pub content: Vec<u8>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonInstallResponse {
    pub ok: bool,
    pub addon_id: Option<String>,
    pub version: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonUninstallRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonUninstallResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonReloadRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonReloadResponse {
    pub ok: bool,
    pub message: Option<String>,
}

/// Pojedyncze pole konfiguracji addona (z manifestu).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonConfigField {
    pub id: String,
    pub label: String,
    pub field_type: String,
    pub description: String,
    pub default_value: String,
    pub options: Vec<String>,
    pub required: bool,
    pub secret: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonConfigGetRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonConfigGetResponse {
    pub schema: Vec<AddonConfigField>,
    pub values: Vec<(String, String)>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonConfigSetRequest {
    pub addon_id: String,
    pub values: Vec<(String, String)>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonConfigSetResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonLogsRequest {
    pub addon_id: String,
    pub limit: i64,
    pub offset: i64,
    pub level: Option<String>,
    pub search: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub level: String,
    pub action: String,
    pub message: String,
    pub user_id: Option<i64>,
    pub user_name: Option<String>,
    pub details: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonLogsResponse {
    pub entries: Vec<AddonLogEntry>,
    pub total: i64,
}

/// Parametr pojedynczego narzedzia deklarowanego przez addon.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToolParam {
    pub name: String,
    pub param_type: String,
    pub description: String,
    pub required: bool,
    pub default_value: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Vec<AddonToolParam>,
    pub return_type: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToolsRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonToolsResponse {
    pub tools: Vec<AddonToolDecl>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonResourcesGetRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonResourcesGetResponse {
    pub max_instances: i32,
    pub cpu_limit_pct: i32,
    pub ram_mb: i32,
    pub storage_mb: i32,
    pub http_requests_per_min: i32,
    pub llm_tokens_per_min: i32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonResourcesSetRequest {
    pub addon_id: String,
    pub max_instances: i32,
    pub cpu_limit_pct: i32,
    pub ram_mb: i32,
    pub storage_mb: i32,
    pub http_requests_per_min: i32,
    pub llm_tokens_per_min: i32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonResourcesSetResponse {
    pub ok: bool,
}

/// Zmergowana regula sieciowa zadeklarowana w manifescie + status pokrycia.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonNetworkRuleDecl {
    pub host: String,
    pub port: Option<i32>,
    pub mode: String,
    pub status: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonNetworkRulesGetRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonNetworkRulesGetResponse {
    pub allowed_hosts: Vec<String>,
    pub blocked_hosts: Vec<String>,
    pub mode: String,
    pub declared_rules: Vec<AddonNetworkRuleDecl>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonNetworkRulesSetRequest {
    pub addon_id: String,
    pub allowed_hosts: Vec<String>,
    pub blocked_hosts: Vec<String>,
    pub mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonNetworkRulesSetResponse {
    pub ok: bool,
}

/// Wiersz widocznosci addona per grupa.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonVisibilityRow {
    pub addon_id: String,
    pub group_id: i64,
    pub group_name: String,
    pub visible: bool,
    pub group_description: String,
    pub user_count: i32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonVisibilityListRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonVisibilityListResponse {
    pub addon_id: String,
    pub rows: Vec<AddonVisibilityRow>,
    pub show_in_catalog: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonVisibilitySetRequest {
    pub addon_id: String,
    pub group_id: i64,
    pub visible: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonVisibilitySetResponse {
    pub addon_id: String,
    pub group_id: i64,
    pub visible: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonAdminOnlySetRequest {
    pub addon_id: String,
    pub admin_only: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonAdminOnlySetResponse {
    pub addon_id: String,
    pub admin_only: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonShowInCatalogSetRequest {
    pub addon_id: String,
    pub show_in_catalog: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonShowInCatalogSetResponse {
    pub addon_id: String,
    pub show_in_catalog: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionCatalogRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionCatalogResponse {
    pub addon_id: String,
    pub entries: Vec<AddonPermissionDecl>,
}

/// Explicit allow/deny/inherit per subject (user|group) + permission.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionRow {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at_epoch: u64,
}

/// Default grant per addon + permission.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionDefault {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at_epoch: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionMatrixRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionMatrixResponse {
    pub addon_id: String,
    pub rows: Vec<AddonPermissionRow>,
    pub defaults: Vec<AddonPermissionDefault>,
    pub last_change_by: String,
    pub last_change_at_epoch: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionSetRequest {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionSetResponse {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionDefaultSetRequest {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionDefaultSetResponse {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionCheckRequest {
    pub addon_id: String,
    pub permission_id: String,
    pub user_id: Option<i64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionCheckResponse {
    pub addon_id: String,
    pub permission_id: String,
    pub allowed: bool,
    pub reason: String,
}

/// Server-push event wysylany gdy admin zmieni grant/visibility/default.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonPermissionChangedEvent {
    pub addon_id: String,
    pub subject_type: Option<String>,
    pub subject_id: Option<i64>,
    pub permission_id: Option<String>,
}

/// Konfiguracja OAuth per (addon, provider) — bez sekretow.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigRow {
    pub addon_id: String,
    pub provider_id: String,
    pub client_id: String,
    pub client_secret_set: bool,
    pub redirect_uri: String,
    pub enabled: bool,
    pub updated_at_epoch: u64,
    pub oauth_mode: String,
    pub linked_accounts_count: i32,
    pub shared_account_email: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigListRequest {
    pub addon_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigListResponse {
    pub addon_id: String,
    pub configs: Vec<AddonOAuthConfigRow>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigSetRequest {
    pub addon_id: String,
    pub provider_id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub enabled: bool,
    pub oauth_mode: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigSetResponse {
    pub addon_id: String,
    pub provider_id: String,
    pub client_secret_set: bool,
    pub enabled: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigClearSecretRequest {
    pub addon_id: String,
    pub provider_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthConfigClearSecretResponse {
    pub addon_id: String,
    pub provider_id: String,
    pub cleared: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthAuthorizeStartRequest {
    pub addon_id: String,
    pub provider_id: String,
    pub mode: String,
    pub redirect_after: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthAuthorizeStartResponse {
    pub authorize_url: String,
    pub state: String,
}

/// Metadane konta OAuth (tokeny nigdy nie wychodza poza core).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct UserOAuthAccountRow {
    pub id: i64,
    pub user_id: Option<i64>,
    pub addon_id: String,
    pub provider_id: String,
    pub external_account_id: String,
    pub display_name: String,
    pub token_type: String,
    pub scopes: Vec<String>,
    pub expires_at_epoch: Option<u64>,
    pub created_at_epoch: u64,
    pub last_used_at_epoch: Option<u64>,
    pub revoked: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthLinkedAccountsRequest {
    pub addon_id: String,
    pub scope: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthLinkedAccountsResponse {
    pub addon_id: String,
    pub accounts: Vec<UserOAuthAccountRow>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthRevokeRequest {
    pub account_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthRevokeResponse {
    pub account_id: i64,
    pub revoked: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthReauthorizeRequest {
    pub account_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthReauthorizeResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthTestConnectionRequest {
    pub addon_id: String,
    pub provider_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonOAuthTestConnectionResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub account_email: Option<String>,
}

/// Wpis widoku "Moje polaczone konta" (per uzytkownik).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MyOAuthEntry {
    pub addon_id: String,
    pub addon_name: String,
    pub addon_icon: Option<String>,
    pub addon_description: String,
    pub addon_version: String,
    pub provider_id: String,
    pub provider_display_name: String,
    pub status: String,
    pub account_id: Option<i64>,
    pub account_email: String,
    pub account_display_name: String,
    pub scopes: Vec<String>,
    pub connected_at_epoch: i64,
    pub last_used_at_epoch: i64,
    pub expires_at_epoch: i64,
}

/// Unit request (bez pol) — jawna struct aby trzymac Body(T) pattern.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MyOAuthAccountsListRequest;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MyOAuthAccountsListResponse {
    pub accounts: Vec<MyOAuthEntry>,
}

// =============================================================================
// Notes (per-user) — inner-enum multiplex zeby nie przekroczyc 256 variantow
// MessageBody. Payloady opakowane w strukty (nawet puste) dla spojnego wzorca.
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NotesListRequest;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteEntry {
    pub id: i64,
    pub title: String,
    pub body_preview: String,
    pub pinned: bool,
    pub created_at_epoch: i64,
    pub updated_at_epoch: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NotesListResponse {
    pub notes: Vec<NoteEntry>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteDetailRequest {
    pub note_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteDetailResponse {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub created_at_epoch: i64,
    pub updated_at_epoch: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteCreateRequest {
    pub title: String,
    pub body: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteCreateResponse {
    pub id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteUpdateRequest {
    pub note_id: i64,
    pub title: String,
    pub body: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteUpdateResponse {
    pub ok: bool,
    pub updated_at_epoch: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteSetPinnedRequest {
    pub note_id: i64,
    pub pinned: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteSetPinnedResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteDeleteRequest {
    pub note_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NoteDeleteResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum NotesRequest {
    List(NotesListRequest),
    Detail(NoteDetailRequest),
    Create(NoteCreateRequest),
    Update(NoteUpdateRequest),
    SetPinned(NoteSetPinnedRequest),
    Delete(NoteDeleteRequest),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum NotesResponse {
    List(NotesListResponse),
    Detail(NoteDetailResponse),
    Create(NoteCreateResponse),
    Update(NoteUpdateResponse),
    SetPinned(NoteSetPinnedResponse),
    Delete(NoteDeleteResponse),
}

// =============================================================================
// Deployments — real build/run pipeline with streaming progress + log tail.
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentSummary {
    pub deploy_id: String,
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub status: String,
    pub phase: String,
    pub progress_pct: i32,
    pub image_tag: String,
    pub container_name: String,
    pub started_at: String,
    pub finished_at: String,
    pub error_message: String,
    pub log_tail: String,
    pub user_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentStatusRequest {
    pub deploy_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentStatusResponse {
    pub deployment: DeploymentSummary,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentListRequest {
    /// "" = wszystkie engines; inaczej filtr exact match.
    pub engine_id: String,
    /// "" = wszystkie; inaczej: "queued"/"building"/"pulling"/"running"/"registering"/"success"/"failure"/"cancelled".
    pub status: String,
    /// true = tylko moje; false = wszystkie (wymaga admin).
    pub only_mine: bool,
    /// 0 = default 100.
    pub limit: i32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentListResponse {
    pub deployments: Vec<DeploymentSummary>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentLogStreamRequest {
    pub deploy_id: String,
    /// Czy emitować historyczne log_tail zanim stream zacznie live.
    pub replay_tail: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentStreamChunk {
    pub deploy_id: String,
    /// "log" = linia build output, "phase" = zmiana fazy, "progress" = update %.
    pub kind: String,
    pub line: String,
    pub phase: String,
    pub progress_pct: i32,
    /// Epoch ms wyemitowania chunka (do sort / debug).
    pub ts_ms: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DeploymentStreamEnd {
    pub deploy_id: String,
    /// "success" | "failure" | "cancelled".
    pub final_status: String,
    pub image_tag: String,
    pub container_name: String,
    pub error_message: String,
    pub duration_ms: i64,
}

/// System events — push-only, wysylane przez serwer jako unsolicited frames.
/// Jeden wariant `MessageBody::SystemEventBody` oszczedza sloty dla kazdego
/// typu eventu (service status, mesh peer status, cokolwiek dalej).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum SystemEventPayload {
    /// Zmiana stanu uslugi QUIC (LLM/TTS/STT/embeddings). Emitowany gdy
    /// ConnectionStatus transitions: Disconnected→Connected lub odwrotnie.
    /// Frontend moze pokazac toast + odswiezyc karty na Dashboard/Services.
    ServiceStatusChanged {
        service_name: String,
        service_type: String,
        status: String,
        message: String,
    },
    /// Zmiana stanu peer-a mesh. Emitowany gdy peer przechodzi w offline/degraded
    /// (liveness timer) albo wraca online po reconnect.
    MeshPeerStatusChanged {
        node_id: String,
        hostname: String,
        status: String,
        message: String,
    },
}

/// Zbiorczy payload deployment (req + res + stream chunks). Jeden wariant
/// `MessageBody::DeploymentBody` kosztuje 1 slot w 256-limicie — inner enum
/// rozgalezia sie lokalnie. Stream handler emituje `StreamChunk`/`StreamEnd`
/// przez SubscriptionEvent::Chunk/End tak samo jak ChatStream.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum DeploymentPayload {
    /// Start deploymentu — odpowiednik starego top-level
    /// `ServiceManifestDeployRequestBody`, przeniesiony tu żeby zmieścić się
    /// w 256-variant limicie rkyv (jedna top-level `DeploymentBody` zamiast
    /// dwóch osobnych Req/Res).
    ReqStart(ServiceManifestDeployRequest),
    ResStart(ServiceManifestDeployResponse),
    ReqStatus(DeploymentStatusRequest),
    ResStatus(DeploymentStatusResponse),
    ReqList(DeploymentListRequest),
    ResList(DeploymentListResponse),
    ReqLogStream(DeploymentLogStreamRequest),
    StreamChunk(DeploymentStreamChunk),
    StreamEnd(DeploymentStreamEnd),
}

// =============================================================================
// Meeting Bot (per-meeting container, live transcript, AI summary).
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionDescriptor {
    pub session_id: i64,
    pub meeting_key: String,
    pub meeting_url: String,
    pub title: String,
    pub status: String,
    pub started_at: String,
    pub last_activity_at: String,
    pub ended_at: String,
    pub platform: String,
    pub entry_count: i64,
    pub quic_port: i32,
    pub vnc_port: i32,
    pub novnc_port: i32,
    pub bot_endpoint_id: String,
    pub container_name: String,
    pub owner_user_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionStartRequest {
    pub meeting_url: String,
    pub title: String,
    pub platform: String,
    pub bot_name: String,
    pub stt_alias: String,
    pub tts_alias: String,
    pub llm_alias: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionStartResponse {
    pub session: MeetingSessionDescriptor,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionLeaveRequest {
    pub session_id: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionLeaveResponse {
    pub ok: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionListRequest {
    /// true = tylko moje sesje, false = wszystkie (admin)
    pub only_mine: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionListResponse {
    pub sessions: Vec<MeetingSessionDescriptor>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeetingTranscriptEntry {
    pub id: i64,
    pub session_id: i64,
    pub timestamp_ms: i64,
    pub speaker: String,
    pub profile_id: i64,
    pub confidence: f32,
    pub is_enrolled: bool,
    pub text: String,
    pub model: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSessionDetailRequest {
    pub session_id: i64,
    pub include_transcripts: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeetingSessionDetailResponse {
    pub session: MeetingSessionDescriptor,
    pub transcripts: Vec<MeetingTranscriptEntry>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingTranscriptsListRequest {
    pub session_id: i64,
    /// Zwroc tylko wpisy z timestamp_ms > since_ms. 0 = wszystko.
    pub since_ms: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeetingTranscriptsListResponse {
    pub entries: Vec<MeetingTranscriptEntry>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActiveSessionRequest;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActiveSessionResponse {
    /// session_id = 0 jesli brak aktywnej sesji.
    pub session: MeetingSessionDescriptor,
    pub has_active: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSettingKv {
    pub key: String,
    pub value: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSettingsGetRequest;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSettingsGetResponse {
    pub settings: Vec<MeetingSettingKv>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSettingsUpdateRequest {
    pub settings: Vec<MeetingSettingKv>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSettingsUpdateResponse {
    pub ok: bool,
}

/// Zbiorczy payload Meeting Bot (req + res w jednym enumie). Handler rozpoznaje
/// wariant i zwraca odpowiedni Res*. Pozwala na jeden wariant w MessageBody.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum MeetingPayload {
    ReqSessionStart(MeetingSessionStartRequest),
    ResSessionStart(MeetingSessionStartResponse),
    ReqSessionLeave(MeetingSessionLeaveRequest),
    ResSessionLeave(MeetingSessionLeaveResponse),
    ReqSessionList(MeetingSessionListRequest),
    ResSessionList(MeetingSessionListResponse),
    ReqSessionDetail(MeetingSessionDetailRequest),
    ResSessionDetail(MeetingSessionDetailResponse),
    ReqTranscriptsList(MeetingTranscriptsListRequest),
    ResTranscriptsList(MeetingTranscriptsListResponse),
    ReqActiveSession(MeetingActiveSessionRequest),
    ResActiveSession(MeetingActiveSessionResponse),
    ReqSettingsGet(MeetingSettingsGetRequest),
    ResSettingsGet(MeetingSettingsGetResponse),
    ReqSettingsUpdate(MeetingSettingsUpdateRequest),
    ResSettingsUpdate(MeetingSettingsUpdateResponse),
}

// =============================================================================
// Translate (LLM-backed translator w user app).
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TranslateRequest {
    pub source_text: String,
    pub source_lang: String,
    pub target_lang: String,
    pub tone: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TranslateResponse {
    pub translated_text: String,
    pub detected_source_lang: Option<String>,
    pub model_used: String,
    pub tokens_used: i32,
}

// =============================================================================
// Users list (Admin only) — rozszerzone metadane konta z last_login.
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub is_active: bool,
    pub is_admin: bool,
    pub sso_provider: Option<String>,
    pub last_login_at: Option<String>,
    pub created_at: String,
    /// "user" | "power_user" | "admin". Default "user" przy deserializacji
    /// starego payloadu.
    pub role: String,
    pub group_ids: Vec<i64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct UsersListResponse {
    pub users: Vec<UserInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub member_count: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PermissionEntry {
    pub resource_type: String,
    pub resource_id: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub access_level: String,
}

/// Inner-enum pack dla calego Identity & Access Management —
/// users + groups + resource permissions. Jeden slot w MessageBody (IamBody).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum IamPayload {
    // ---- Users ----
    ReqListUsers,
    ResListUsers { users: Vec<UserInfo> },
    ReqGetUser { user_id: i64 },
    ResGetUser { user: UserInfo },
    ReqCreateUser {
        username: String,
        password: String,
        display_name: String,
        email: String,
        role: String,
        group_ids: Vec<i64>,
    },
    ResCreateUser { user_id: i64 },
    ReqUpdateUser {
        user_id: i64,
        display_name: String,
        email: String,
        is_active: bool,
        role: String,
    },
    ReqDeleteUser { user_id: i64 },
    ReqSetUserGroups { user_id: i64, group_ids: Vec<i64> },
    ReqResetUserPassword { user_id: i64, new_password: String },

    // ---- Groups ----
    ReqListGroups,
    ResListGroups { groups: Vec<GroupInfo> },
    ReqCreateGroup { name: String, description: String },
    ResCreateGroup { group_id: i64 },
    ReqUpdateGroup { group_id: i64, name: String, description: String },
    ReqDeleteGroup { group_id: i64 },
    ReqGroupMembers { group_id: i64 },
    ResGroupMembers { members: Vec<UserInfo> },

    // ---- Resource permissions (generyczna ACL) ----
    /// resource_type: 'model' | 'flow' | 'addon' | ...
    /// subject_type: 'user' | 'group'
    /// access_level: 'allow' | 'deny'
    ReqSetPermission {
        resource_type: String,
        resource_id: String,
        subject_type: String,
        subject_id: i64,
        access_level: String,
    },
    ReqClearPermission {
        resource_type: String,
        resource_id: String,
        subject_type: String,
        subject_id: i64,
    },
    ReqListPermsForResource { resource_type: String, resource_id: String },
    ReqListPermsForSubject { subject_type: String, subject_id: i64 },
    ResListPermissions { entries: Vec<PermissionEntry> },

    // Generic OK dla mutacji (delete/update/set) bez specyficznego response.
    ResOk,
}

/// policy table (`#[policy]` proc-macro z #26).
///
/// Kazda zmiana layoutu wymaga bump `SCHEMA_VERSION`.
///
/// UWAGA: `Eq` NIE implementowane bo ChatStreamRequest ma `Option<f32>` (floaty
/// nie sa Eq przez NaN). Uzywamy `PartialEq` wszedzie.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum MessageBody {
    // ---- Meta (schema/handshake/keepalive) ----
    /// Klient -> serwer: sprawdz wersje protokolu przy handshake.
    MetaSchemaVersionCheck { client_version: u16 },
    /// Serwer -> klient: potwierdzenie (accepted=false => disconnect).
    MetaSchemaVersionAck { server_version: u16, accepted: bool },
    /// Dwukierunkowy keepalive (WSS ping substitute, liczy RTT).
    MetaHeartbeat { sent_at_epoch: u64 },
    /// Klient -> serwer: anuluj aktywny stream (match po correlation_id w envelope).
    MetaCancelStream,

    // ---- Read-list (R-LIST archetyp) ----
    /// Klient -> serwer: lista modeli (publiczne, Anonymous OK).
    ModelListRequest,
    /// Serwer -> klient: odpowiedz.
    ModelListResponse { models: Vec<ModelSummary> },

    // ---- API Keys (R-LIST + W-CREATE + W-DELETE) ----
    ApiKeyListRequest,
    ApiKeyListResponse { keys: Vec<ApiKeySummary> },
    ApiKeyCreateRequestBody(ApiKeyCreateRequest),
    ApiKeyCreateResponseBody(ApiKeyCreateResponse),
    ApiKeyRevokeRequest { key_id: String },
    ApiKeyRevokeResponse { deleted: bool },

    // ---- Auth (W-ACTION + R-ONE) ----
    AuthLoginRequestBody(AuthLoginRequest),
    AuthLoginResponseBody(AuthLoginResponse),
    AuthMeRequest,
    AuthMeResponseBody(AuthMeResponse),

    // ---- Chat streaming (R-STREAM) ----
    ChatStreamRequestBody(ChatStreamRequest),
    ChatStreamChunkBody(ChatStreamChunk),
    ChatStreamEndBody(ChatStreamEnd),

    // ---- Clusters (full CRUD + member ops + probe streaming) ----
    ClusterListRequest,
    ClusterListResponseBody(ClusterListResponse),
    ClusterDetailRequestBody(ClusterDetailRequest),
    ClusterDetailResponseBody(ClusterDetailResponse),
    ClusterCreateRequestBody(ClusterCreateRequest),
    ClusterCreateResponseBody(ClusterCreateResponse),
    ClusterUpdateRequestBody(ClusterUpdateRequest),
    ClusterUpdateResponseBody(ClusterUpdateResponse),
    ClusterDeleteRequestBody(ClusterDeleteRequest),
    ClusterDeleteResponseBody(ClusterDeleteResponse),
    ClusterAddMemberRequestBody(ClusterAddMemberRequest),
    ClusterAddMemberResponseBody(ClusterAddMemberResponse),
    ClusterRemoveMemberRequestBody(ClusterRemoveMemberRequest),
    ClusterRemoveMemberResponseBody(ClusterRemoveMemberResponse),
    ClusterProbeStreamRequestBody(ClusterProbeStreamRequest),
    ClusterProbeStreamChunkBody(ClusterProbeStreamChunk),
    ClusterProbeStreamEndBody(ClusterProbeStreamEnd),

    // ---- Mesh peers (R-LIST + W-ACTION) ----
    MeshPeersListRequest,
    MeshPeersListResponse { peers: Vec<MeshPeerSummary> },
    MeshPairInitRequestBody(MeshPairInitRequest),
    MeshPairInitResponseBody(MeshPairInitResponse),

    // ---- Mesh trust events (broadcast / sync) ----
    MeshTrustRevoked(MeshTrustRevokedEvent),
    MeshTrustedKeysSync(MeshTrustedKeysSyncEvent),

    // ---- Mesh extended (read-only + admin actions) ----
    MeshNodeListRequest,
    MeshNodeListResponseBody(MeshNodeListResponse),
    MeshNodeDetailRequestBody(MeshNodeDetailRequest),
    MeshNodeDetailResponseBody(MeshNodeDetailResponse),
    MeshPendingListRequest,
    MeshPendingListResponseBody(MeshPendingListResponse),
    MeshIdentityRequest,
    MeshIdentityResponseBody(MeshIdentityResponse),
    MeshServicesListRequest,
    MeshServicesListResponseBody(MeshServicesListResponse),
    MeshTrustedListRequest,
    MeshTrustedListResponseBody(MeshTrustedListResponse),
    MeshPairingStartRequestBody(MeshPairingStartRequest),
    MeshPairingStartResponseBody(MeshPairingStartResponse),
    MeshPairingConfirmRequestBody(MeshPairingConfirmRequest),
    MeshPairingConfirmResponseBody(MeshPairingConfirmResponse),
    MeshPairingRejectRequestBody(MeshPairingRejectRequest),
    MeshPairingRejectResponseBody(MeshPairingRejectResponse),
    MeshTrustRevokeRequestBody(MeshTrustRevokeRequest),
    MeshTrustRevokeResponseBody(MeshTrustRevokeResponse),
    MeshTrustRetrustRequestBody(MeshTrustRetrustRequest),
    MeshTrustRetrustResponseBody(MeshTrustRetrustResponse),
    MeshConnectRequestBody(MeshConnectRequest),
    MeshConnectResponseBody(MeshConnectResponse),
    MeshNodeCommandRequestBody(MeshNodeCommandRequest),
    MeshNodeCommandResponseBody(MeshNodeCommandResponse),
    MeshNodeNetworkConfigRequestBody(MeshNodeNetworkConfigRequest),
    MeshNodeNetworkConfigResponseBody(MeshNodeNetworkConfigResponse),

    // ---- Services (R-LIST + W-ACTION + R-STREAM dla deploy progress) ----
    ServiceListRequest,
    ServiceListResponse { services: Vec<ServiceSummary> },
    ServiceCreateRequestBody(ServiceCreateRequest),
    ServiceCreateResponse { id: String },
    ServiceUpdateRequestBody(ServiceUpdateRequest),
    ServiceUpdateResponse { updated: bool },
    ServiceDeployRequestBody(ServiceDeployRequest),
    ServiceDeployAccepted { deploy_id: String },
    ServiceDeployProgressBody(ServiceDeployProgress),
    ServiceStopRequest { service_id: String },
    ServiceStopResponse { stopped: bool },
    ServiceQuicStatusRequest,
    ServiceQuicStatusResponse { statuses: Vec<ServiceQuicStatus> },

    // ---- Prompts (R-LIST + R-ONE) ----
    PromptListRequest,
    PromptListResponse { prompts: Vec<PromptSummary> },
    PromptDetailRequest { prompt_id: String },
    PromptDetailResponse(PromptDetail),

    // ---- Registries (R-LIST) ----
    RegistryListRequest,
    RegistryListResponse { registries: Vec<RegistrySummary> },

    // ---- Audit (event push — server -> client) ----
    AuditEventBody(AuditEvent),

    // ----- Audit log -----
    AuditLogListRequestBody(AuditLogListRequest),
    AuditLogListResponseBody(AuditLogListResponse),
    AuditLogExportRequestBody(AuditLogExportRequest),
    AuditLogExportResponseBody(AuditLogExportResponse),
    AuditLogCleanupRequestBody(AuditLogCleanupRequest),
    AuditLogCleanupResponseBody(AuditLogCleanupResponse),

    // ---- Portainer (R-LIST + R-STREAM dla logs) ----
    ContainerListRequest,
    ContainerListResponse { containers: Vec<ContainerSummary> },
    ContainerStartRequest { container_id: String },
    ContainerStartResponse { started: bool },
    ContainerStopRequest { container_id: String },
    ContainerStopResponse { stopped: bool },
    ContainerLogStreamRequest { container_id: String, follow: bool },
    ContainerLogChunkBody(ContainerLogChunk),

    // ---- Voice profiles (R-LIST) ----
    VoiceProfileListRequest,
    VoiceProfileListResponse { profiles: Vec<VoiceProfileSummary> },

    // ---- TTS rules (R-LIST + W-CREATE/UPDATE/DELETE) ----
    TtsRuleListRequest,
    TtsRuleListResponse { rules: Vec<TtsRule> },
    TtsRuleCreateRequest(TtsRule),
    TtsRuleCreateResponse { rule_id: String },
    TtsRuleDeleteRequest { rule_id: String },
    TtsRuleDeleteResponse { deleted: bool },

    // ---- PII rules ----
    PiiRuleListRequest,
    PiiRuleListResponse { rules: Vec<PiiRule> },

    // ---- Fast-path patterns ----
    FastPathListRequest,
    FastPathListResponse { patterns: Vec<FastPathPattern> },

    // ---- Models (R-ONE + W-ACTION) ----
    ModelDetailRequest { model_id: String },
    ModelDetailResponse(ModelDetail),
    ModelInstallRequestBody(ModelInstallRequest),
    ModelInstallResponse { model_id: String, accepted: bool },
    ModelDeleteRequest { model_id: String },
    ModelDeleteResponse { deleted: bool },

    // ---- Hub (R-LIST + R-STREAM dla download) ----
    HubEngineListRequest,
    HubEngineListResponse { engines: Vec<HubEngineSummary> },
    HubModelSearchRequest { query: String },
    HubModelSearchResponse { results: Vec<HubModelSearchResult> },
    HubDownloadProgressBody(HubDownloadProgress),

    // ---- Flows (R-LIST + R-ONE + W-CREATE/UPDATE/DELETE + executions) ----
    FlowListRequest,
    FlowListResponse { flows: Vec<FlowSummary> },
    FlowDetailRequest { flow_id: String },
    FlowDetailResponse(FlowDetail),
    FlowCreateRequestBody(FlowCreateRequest),
    FlowCreateResponse { flow_id: String },
    FlowDeleteRequest { flow_id: String },
    FlowDeleteResponse { deleted: bool },
    FlowExecutionsListRequest { flow_id: String },
    FlowExecutionsListResponse { executions: Vec<FlowExecutionSummary> },

    // ---- Flows phase 3 (partial update, node templates, version history) ----
    FlowUpdateRequestBody(FlowUpdateRequest),
    FlowUpdateResponseBody(FlowUpdateResponse),
    FlowNodeTemplatesListRequest,
    FlowNodeTemplatesListResponseBody(FlowNodeTemplatesListResponse),
    FlowVersionListRequestBody(FlowVersionListRequest),
    FlowVersionListResponseBody(FlowVersionListResponse),
    FlowVersionGetRequestBody(FlowVersionGetRequest),
    FlowVersionGetResponseBody(FlowVersionGetResponse),
    FlowVersionRestoreRequestBody(FlowVersionRestoreRequest),
    FlowVersionRestoreResponseBody(FlowVersionRestoreResponse),

    // ---- SSO / TLS / NGC (FAZA 4 — REST -> binary) ----
    SsoProvidersListRequest,
    SsoProvidersListResponseBody(SsoProvidersListResponse),
    SsoProviderCreateRequestBody(SsoProviderCreateRequest),
    SsoProviderCreateResponseBody(SsoProviderCreateResponse),
    SsoProviderDeleteRequestBody(SsoProviderDeleteRequest),
    SsoProviderDeleteResponseBody(SsoProviderDeleteResponse),
    TlsStatusRequest,
    TlsStatusResponseBody(TlsStatusResponse),
    NgcStatusRequest,
    NgcStatusResponseBody(NgcStatusResponse),

    // ---- Subscription resume (client requests replay after reconnect) ----
    /// Klient -> serwer: zaresumuj subscription z tokenem ktory dostal w
    /// SubscribeResumeOffer przy ostatnim disconnect.
    SubscribeResumeRequest { resume_token: Vec<u8> },
    /// Serwer -> klient: ack/reject. Jesli accepted=true, subskrypcja jest
    /// odtworzona pod tym samym correlation_id i serwer zaraz wysle brakujace
    /// chunki z recorder buffer.
    SubscribeResumeAck { accepted: bool, error: Option<String> },
    /// Serwer -> klient: token ktory pozwoli na resume po disconnect.
    /// Wysylany RAZEM z IS_STREAM_END (envelope flag), opcjonalny.
    SubscribeResumeOffer { resume_token: Vec<u8> },

    // ---- Settings (R-LIST + W-UPDATE) ----
    SettingsListRequest,
    SettingsListResponse { entries: Vec<SettingEntry> },
    SettingsUpdateRequestBody(SettingsUpdateRequest),
    SettingsUpdateResponse { applied: u32 },

    // ---- Dashboard (R-LIST + subscription candidate) ----
    DashboardMetricsRequest,
    DashboardMetricsResponse(DashboardSnapshot),

    // ---- Models / aliases / catalog -----
    ModelsUnifiedListRequest,
    ModelsUnifiedListResponseBody(ModelsUnifiedListResponse),
    ModelAliasListRequest,
    ModelAliasListResponseBody(ModelAliasListResponse),
    ModelAliasCreateRequestBody(ModelAliasCreateRequest),
    ModelAliasCreateResponseBody(ModelAliasCreateResponse),
    ModelAliasUpdateRequestBody(ModelAliasUpdateRequest),
    ModelAliasUpdateResponseBody(ModelAliasUpdateResponse),
    ModelAliasDeleteRequestBody(ModelAliasDeleteRequest),
    ModelAliasDeleteResponseBody(ModelAliasDeleteResponse),
    NimCatalogListRequest,
    NimCatalogListResponseBody(NimCatalogListResponse),
    // ServiceManifestDeployRequest/Response przeniesione do DeploymentPayload
    // (ReqStart/ResStart). Oszczędza 1 slot w 256-variant limicie rkyv.

    // ---- Addons: list / detail / toggle / lifecycle ----
    AddonsListRequest,
    AddonsListResponseBody(AddonsListResponse),
    AddonDetailRequestBody(AddonDetailRequest),
    AddonDetailResponseBody(AddonDetailResponse),
    AddonToggleRequestBody(AddonToggleRequest),
    AddonToggleResponseBody(AddonToggleResponse),
    AddonInstallRequestBody(AddonInstallRequest),
    AddonInstallResponseBody(AddonInstallResponse),
    AddonUninstallRequestBody(AddonUninstallRequest),
    AddonUninstallResponseBody(AddonUninstallResponse),
    AddonReloadRequestBody(AddonReloadRequest),
    AddonReloadResponseBody(AddonReloadResponse),
    AddonConfigGetRequestBody(AddonConfigGetRequest),
    AddonConfigGetResponseBody(AddonConfigGetResponse),
    AddonConfigSetRequestBody(AddonConfigSetRequest),
    AddonConfigSetResponseBody(AddonConfigSetResponse),
    AddonLogsRequestBody(AddonLogsRequest),
    AddonLogsResponseBody(AddonLogsResponse),
    AddonToolsRequestBody(AddonToolsRequest),
    AddonToolsResponseBody(AddonToolsResponse),
    AddonResourcesGetRequestBody(AddonResourcesGetRequest),
    AddonResourcesGetResponseBody(AddonResourcesGetResponse),
    AddonResourcesSetRequestBody(AddonResourcesSetRequest),
    AddonResourcesSetResponseBody(AddonResourcesSetResponse),
    AddonNetworkRulesGetRequestBody(AddonNetworkRulesGetRequest),
    AddonNetworkRulesGetResponseBody(AddonNetworkRulesGetResponse),
    AddonNetworkRulesSetRequestBody(AddonNetworkRulesSetRequest),
    AddonNetworkRulesSetResponseBody(AddonNetworkRulesSetResponse),

    // ---- Addons: visibility ----
    AddonVisibilityListRequestBody(AddonVisibilityListRequest),
    AddonVisibilityListResponseBody(AddonVisibilityListResponse),
    AddonVisibilitySetRequestBody(AddonVisibilitySetRequest),
    AddonVisibilitySetResponseBody(AddonVisibilitySetResponse),
    AddonAdminOnlySetRequestBody(AddonAdminOnlySetRequest),
    AddonAdminOnlySetResponseBody(AddonAdminOnlySetResponse),
    AddonShowInCatalogSetRequestBody(AddonShowInCatalogSetRequest),
    AddonShowInCatalogSetResponseBody(AddonShowInCatalogSetResponse),

    // ---- Addons: permissions ----
    AddonPermissionCatalogRequestBody(AddonPermissionCatalogRequest),
    AddonPermissionCatalogResponseBody(AddonPermissionCatalogResponse),
    AddonPermissionMatrixRequestBody(AddonPermissionMatrixRequest),
    AddonPermissionMatrixResponseBody(AddonPermissionMatrixResponse),
    AddonPermissionSetRequestBody(AddonPermissionSetRequest),
    AddonPermissionSetResponseBody(AddonPermissionSetResponse),
    AddonPermissionDefaultSetRequestBody(AddonPermissionDefaultSetRequest),
    AddonPermissionDefaultSetResponseBody(AddonPermissionDefaultSetResponse),
    AddonPermissionCheckRequestBody(AddonPermissionCheckRequest),
    AddonPermissionCheckResponseBody(AddonPermissionCheckResponse),
    AddonPermissionChangedEventBody(AddonPermissionChangedEvent),

    // ---- Addons: OAuth ----
    AddonOAuthConfigListRequestBody(AddonOAuthConfigListRequest),
    AddonOAuthConfigListResponseBody(AddonOAuthConfigListResponse),
    AddonOAuthConfigSetRequestBody(AddonOAuthConfigSetRequest),
    AddonOAuthConfigSetResponseBody(AddonOAuthConfigSetResponse),
    AddonOAuthConfigClearSecretRequestBody(AddonOAuthConfigClearSecretRequest),
    AddonOAuthConfigClearSecretResponseBody(AddonOAuthConfigClearSecretResponse),
    AddonOAuthAuthorizeStartRequestBody(AddonOAuthAuthorizeStartRequest),
    AddonOAuthAuthorizeStartResponseBody(AddonOAuthAuthorizeStartResponse),
    AddonOAuthLinkedAccountsRequestBody(AddonOAuthLinkedAccountsRequest),
    AddonOAuthLinkedAccountsResponseBody(AddonOAuthLinkedAccountsResponse),
    AddonOAuthRevokeRequestBody(AddonOAuthRevokeRequest),
    AddonOAuthRevokeResponseBody(AddonOAuthRevokeResponse),
    AddonOAuthReauthorizeRequestBody(AddonOAuthReauthorizeRequest),
    AddonOAuthReauthorizeResponseBody(AddonOAuthReauthorizeResponse),
    AddonOAuthTestConnectionRequestBody(AddonOAuthTestConnectionRequest),
    AddonOAuthTestConnectionResponseBody(AddonOAuthTestConnectionResponse),

    // ---- My OAuth accounts (user-facing) ----
    MyOAuthAccountsListRequestBody(MyOAuthAccountsListRequest),
    MyOAuthAccountsListResponseBody(MyOAuthAccountsListResponse),

    // ---- Notes (inner-enum multiplex) ----
    NotesRequestBody(NotesRequest),
    NotesResponseBody(NotesResponse),

    // ---- Meeting Bot (single-variant, req+res w inner enum) ----
    MeetingBody(MeetingPayload),

    // ---- Meeting live broadcast (unsolicited push, correlation_id=0) ----
    // Pushowany z writer task w ws_binary po każdym sukcesie
    // `persist_meeting_event`. Filtr ownership (owner_user_id) stosowany
    // server-side — frame wychodzi tylko do właściciela sesji.
    MeetingLiveEventBody(crate::types::MeetingLiveEvent),

    // ---- Deployments (single-variant, req+res+stream w inner enum) ----
    DeploymentBody(DeploymentPayload),

    // ---- System events (single-variant, push-only unsolicited w inner enum) ----
    // Oszczedza sloty variantowe — dla wszystkich server-push eventow systemowych
    // (service status, mesh peer status, deployment progress summary itd.).
    SystemEventBody(SystemEventPayload),

    // ---- Translate (LLM-backed) ----
    TranslateRequestBody(TranslateRequest),
    TranslateResponseBody(TranslateResponse),

    // ---- Users list (Admin) ----
    // UsersList* consolidated into IamBody (below) jako ReqListUsers/ResListUsers.
    IamBody(IamPayload),

    // ---- Error ----
    /// Ujednolicony blad. Towarzyszy `EnvelopeFlags::IS_ERROR`.
    Error(ProtocolError),
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model() -> ModelSummary {
        ModelSummary {
            id: "llama-3.2-1b-instruct".to_string(),
            category: "llm".to_string(),
            engine_id: "llama-cpp".to_string(),
            availability: "ready".to_string(),
        }
    }

    fn round_trip(body: MessageBody) -> MessageBody {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body).expect("encode");
        rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&bytes).expect("decode")
    }

    #[test]
    fn meta_schema_version_check_round_trip() {
        let body = MessageBody::MetaSchemaVersionCheck { client_version: 2 };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_schema_version_ack_round_trip() {
        let body = MessageBody::MetaSchemaVersionAck {
            server_version: 2,
            accepted: true,
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_heartbeat_round_trip() {
        let body = MessageBody::MetaHeartbeat {
            sent_at_epoch: 1_700_000_000,
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn meta_cancel_stream_round_trip() {
        let body = MessageBody::MetaCancelStream;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn model_list_request_round_trip() {
        let body = MessageBody::ModelListRequest;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn model_list_response_round_trip() {
        let body = MessageBody::ModelListResponse {
            models: vec![sample_model()],
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn error_round_trip_with_trace() {
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::PolicyDenied,
            message: "requires UserSession".to_string(),
            trace_id: Some("trace-xyz".to_string()),
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn error_round_trip_without_trace() {
        let body = MessageBody::Error(ProtocolError {
            code: ProtocolErrorCode::NotFound,
            message: "node not in mesh".to_string(),
            trace_id: None,
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn all_error_codes_survive_round_trip() {
        for code in [
            ProtocolErrorCode::InvalidFrame,
            ProtocolErrorCode::PolicyDenied,
            ProtocolErrorCode::AuthRequired,
            ProtocolErrorCode::NodeUnreachable,
            ProtocolErrorCode::StreamCancelled,
            ProtocolErrorCode::RateLimited,
            ProtocolErrorCode::NotImplemented,
            ProtocolErrorCode::Internal,
            ProtocolErrorCode::NotFound,
            ProtocolErrorCode::BadRequest,
        ] {
            let body = MessageBody::Error(ProtocolError {
                code,
                message: "x".to_string(),
                trace_id: None,
            });
            assert_eq!(round_trip(body.clone()), body);
        }
    }

    #[test]
    fn truncated_body_bytes_rejected() {
        let body = MessageBody::ModelListResponse {
            models: vec![sample_model(), sample_model()],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body).expect("encode");
        let half = &bytes[..bytes.len() / 2];
        assert!(rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(half).is_err());
    }

    #[test]
    fn empty_body_bytes_rejected() {
        let result = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn protocol_error_constructors() {
        let e = ProtocolError::bad_request("missing field");
        assert_eq!(e.code, ProtocolErrorCode::BadRequest);
        assert_eq!(e.message, "missing field");
        assert!(e.trace_id.is_none());

        let e = ProtocolError::internal("oops").with_trace("tr-123");
        assert_eq!(e.code, ProtocolErrorCode::Internal);
        assert_eq!(e.trace_id.as_deref(), Some("tr-123"));

        let e = ProtocolError::not_found("user/42");
        assert_eq!(e.code, ProtocolErrorCode::NotFound);
        assert!(format!("{}", e).contains("NotFound"));
    }

    #[test]
    fn api_key_crud_round_trip() {
        let list = MessageBody::ApiKeyListResponse {
            keys: vec![ApiKeySummary {
                key_id: "k1".to_string(),
                name: "primary".to_string(),
                created_at_epoch: 1_700_000_000,
                last_used_at_epoch: Some(1_700_100_000),
            }],
        };
        assert_eq!(round_trip(list.clone()), list);

        let create = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
            name: "svc".to_string(),
            scopes: vec!["read".to_string(), "write".to_string()],
        });
        assert_eq!(round_trip(create.clone()), create);

        let created = MessageBody::ApiKeyCreateResponseBody(ApiKeyCreateResponse {
            key_id: "k2".to_string(),
            token: "secret-only-shown-once".to_string(),
        });
        assert_eq!(round_trip(created.clone()), created);

        let revoke = MessageBody::ApiKeyRevokeRequest {
            key_id: "k2".to_string(),
        };
        assert_eq!(round_trip(revoke.clone()), revoke);

        let revoked = MessageBody::ApiKeyRevokeResponse { deleted: true };
        assert_eq!(round_trip(revoked.clone()), revoked);
    }

    #[test]
    fn auth_login_flow_round_trip() {
        let login = MessageBody::AuthLoginRequestBody(AuthLoginRequest {
            username: "admin".to_string(),
            password: "s3cret".to_string(),
        });
        assert_eq!(round_trip(login.clone()), login);

        let logged = MessageBody::AuthLoginResponseBody(AuthLoginResponse {
            jwt: "eyJ...".to_string(),
            user_id: [9u8; 16],
            role: "admin".to_string(),
        });
        assert_eq!(round_trip(logged.clone()), logged);

        let me = MessageBody::AuthMeRequest;
        assert_eq!(round_trip(me.clone()), me);

        let me_resp = MessageBody::AuthMeResponseBody(AuthMeResponse {
            user_id: [9u8; 16],
            username: "admin".to_string(),
            role: "admin".to_string(),
        });
        assert_eq!(round_trip(me_resp.clone()), me_resp);
    }

    #[test]
    fn chat_stream_round_trip() {
        let req = MessageBody::ChatStreamRequestBody(ChatStreamRequest {
            model_id: "llama-3.2".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "You are helpful.".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "Hi".to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(256),
        });
        assert_eq!(round_trip(req.clone()), req);

        let chunk = MessageBody::ChatStreamChunkBody(ChatStreamChunk {
            delta: "Hello".to_string(),
        });
        assert_eq!(round_trip(chunk.clone()), chunk);

        let end = MessageBody::ChatStreamEndBody(ChatStreamEnd {
            prompt_tokens: 12,
            completion_tokens: 34,
        });
        assert_eq!(round_trip(end.clone()), end);
    }

    #[test]
    fn cluster_update_round_trip() {
        let req = MessageBody::ClusterUpdateRequestBody(ClusterUpdateRequest {
            cluster_id: "dev".to_string(),
            name: Some("Development".to_string()),
            description: Some("Internal cluster".to_string()),
            strategy: None,
            failover_enabled: Some(true),
            failover_target: None,
            health_check_interval_ms: Some(5000),
            timeout_ms: Some(30000),
        });
        assert_eq!(round_trip(req.clone()), req);

        let resp = MessageBody::ClusterUpdateResponseBody(ClusterUpdateResponse { ok: true });
        assert_eq!(round_trip(resp.clone()), resp);
    }

    #[test]
    fn mesh_peers_round_trip() {
        let list = MessageBody::MeshPeersListResponse {
            peers: vec![MeshPeerSummary {
                node_id: [7u8; 32],
                display_name: "peer-1".to_string(),
                trust_state: "trusted".to_string(),
                endpoint: Some("10.0.0.1:8090".to_string()),
                last_seen_epoch: Some(1_700_000_000),
            }],
        };
        assert_eq!(round_trip(list.clone()), list);

        let pair = MessageBody::MeshPairInitRequestBody(MeshPairInitRequest {
            node_id: [8u8; 32],
            pin: "123456".to_string(),
        });
        assert_eq!(round_trip(pair.clone()), pair);
    }

    #[test]
    fn settings_round_trip() {
        let list = MessageBody::SettingsListResponse {
            entries: vec![
                SettingEntry {
                    key: "theme".to_string(),
                    value: "dark".to_string(),
                    is_secret: false,
                },
                SettingEntry {
                    key: "api_key".to_string(),
                    value: "s3cret".to_string(),
                    is_secret: true,
                },
            ],
        };
        assert_eq!(round_trip(list.clone()), list);

        let update = MessageBody::SettingsUpdateRequestBody(SettingsUpdateRequest {
            entries: vec![SettingEntry {
                key: "theme".to_string(),
                value: "light".to_string(),
                is_secret: false,
            }],
        });
        assert_eq!(round_trip(update.clone()), update);
    }

    #[test]
    fn mesh_trust_revoked_round_trip() {
        let evt = MessageBody::MeshTrustRevoked(MeshTrustRevokedEvent {
            revoked_node_id: [0xAAu8; 32],
            reason: "key compromise detected".to_string(),
            revoked_at_epoch: 1_700_500_000,
        });
        assert_eq!(round_trip(evt.clone()), evt);
    }

    #[test]
    fn mesh_trusted_keys_sync_round_trip() {
        let evt = MessageBody::MeshTrustedKeysSync(MeshTrustedKeysSyncEvent {
            trusted_keys: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            epoch: 42,
        });
        assert_eq!(round_trip(evt.clone()), evt);
    }

    #[test]
    fn dashboard_metrics_round_trip() {
        let resp = MessageBody::DashboardMetricsResponse(DashboardSnapshot {
            cpu_usage_percent: 42.5,
            ram_used_mb: 1024,
            ram_total_mb: 8192,
            active_requests: 3,
            total_requests: 12345,
            total_errors: 7,
            tokens_per_second: 50,
            active_services: 4,
        });
        // DashboardSnapshot has f32 → MessageBody is PartialEq only.
        assert_eq!(round_trip(resp.clone()), resp);
    }

    #[test]
    fn body_nests_inside_envelope() {
        use crate::envelope::{message_kind, Envelope};
        let body = MessageBody::ModelListRequest;
        let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body)
            .expect("encode body")
            .to_vec();
        let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
        let env_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode env");
        let decoded_env: Envelope =
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&env_bytes).expect("decode env");
        let decoded_body: MessageBody =
            rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&decoded_env.body)
                .expect("decode body");
        assert_eq!(decoded_body, body);
    }
}
