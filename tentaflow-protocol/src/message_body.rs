// =============================================================================
// Plik: message_body.rs
// Opis: Bootstrap 10 wariantow MessageBody (bootstrap). MessageBody to tresc
//       envelope'u — rkyv-serializowana osobno i trzymana jako Vec<u8> w polu
//       Envelope.body. Dzieki temu policy check dziala na envelope bez tykania
//       body, a dispatcher decoduje dopiero po przejsciu auth.
// Przyklad:
//   let body = MessageBody::NodeListRequest;
//   let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body)?.to_vec();
//   let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};

// =============================================================================
// Pomocnicze typy (bootstrap — docelowo rozpisane per-archetype)
// =============================================================================

/// Lekki widok noda mesh dla list/overview. Pelne dane idą przez osobny NodeInfo.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NodeSummary {
    /// Ed25519 public key (32 bajty).
    pub node_id: [u8; 32],
    /// Hostname / display label.
    pub display_name: String,
    /// `online` / `offline` / `degraded`. String dla elastycznosci.
    pub status: String,
    /// Tier: `leader`, `worker`, itp.
    pub role: String,
    /// Czy to lokalny node (self-view).
    pub is_self: bool,
}

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
    /// Zasoba nie znaleziono (np. NodeInfoRequest z nieznanym id).
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

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeInfo {
    pub node_id: String,
    pub hostname: String,
    pub ip: Option<String>,
    pub status: String,
    pub source: String,
    pub is_local: bool,
    pub uptime_secs: Option<u64>,
    pub gpu_info: Option<MeshNodeGpuInfo>,
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
    pub version: String,
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
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingStartResponse {
    pub pair_id: String,
    pub pin: String,
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
// MessageBody — wszystkie warianty
// =============================================================================

/// Enum wariantow tresci. Bootstrap (#29) zawieral 10; #36 dokladuje 10 kolejnych
/// pokrywajacych wszystkie 7 archetypow (R-ONE, R-LIST, R-STREAM, W-CREATE,
/// W-UPDATE, W-DELETE, W-ACTION). Dla kazdego variantu MUSI istniec wpis w
/// policy table (`#[policy]` proc-macro z #26).
///
/// Kazda nowa pozycja = additive change i bump `SCHEMA_VERSION`.
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
    /// Klient -> serwer: lista nodow mesh. Anonymous / UserSession / MeshTrust.
    NodeListRequest,
    /// Serwer -> klient: odpowiedz (summary, pelne info przez NodeInfoRequest).
    NodeListResponse { nodes: Vec<NodeSummary> },
    /// Klient -> serwer: lista modeli (publiczne, Anonymous OK).
    ModelListRequest,
    /// Serwer -> klient: odpowiedz.
    ModelListResponse { models: Vec<ModelSummary> },

    // ---- Read-one (R-ONE archetyp) ----
    /// Klient -> serwer: szczegoly konkretnego noda.
    NodeInfoRequest { node_id: [u8; 32] },

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

    fn sample_node() -> NodeSummary {
        NodeSummary {
            node_id: [5u8; 32],
            display_name: "alpha".to_string(),
            status: "online".to_string(),
            role: "leader".to_string(),
            is_self: true,
        }
    }

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
    fn node_list_request_unit_variant() {
        let body = MessageBody::NodeListRequest;
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn node_list_response_with_multiple_nodes() {
        let body = MessageBody::NodeListResponse {
            nodes: vec![
                sample_node(),
                NodeSummary {
                    node_id: [6u8; 32],
                    display_name: "beta".to_string(),
                    status: "degraded".to_string(),
                    role: "worker".to_string(),
                    is_self: false,
                },
            ],
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn node_info_request_round_trip() {
        let body = MessageBody::NodeInfoRequest {
            node_id: [0xAAu8; 32],
        };
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
        let body = MessageBody::NodeListResponse {
            nodes: vec![sample_node(), sample_node()],
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
        let body = MessageBody::NodeListRequest;
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
