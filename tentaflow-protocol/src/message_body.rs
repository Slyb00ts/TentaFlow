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
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// Pomocnicze typy (bootstrap — docelowo rozpisane per-archetype)
// =============================================================================

/// Wide model view sourced from `model_registry` joined with the parent
/// `services` row. Returned by `ModelListRequest` so the chat picker can
/// disambiguate duplicates and the catalog can show transport/endpoint.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelSummary {
    /// Stable model identifier used for dispatch (alias key). Equal to the
    /// row's `model_name` — keeps backward compat with existing call sites
    /// that match by `id`.
    pub id: String,
    /// `model_registry.model_name` — the canonical model handle.
    pub model_name: String,
    /// User-friendly label (defaults to `model_name` when null in DB).
    pub display_name: String,
    /// Coarse bucket derived from `capabilities` ("llm" / "tts" / "stt" /
    /// "embedding" / ...). Kept for chat-side filtering.
    pub category: String,
    /// `services.engine_id` — engine implementation (vllm / mlx / llama-cpp).
    pub engine_id: String,
    /// `services.id` — disambiguates the same `model_name` across instances.
    pub service_id: i64,
    /// Owning mesh node — endpoint-id hex of the node that hosts this model's
    /// service. Equal to the local node when the model is hosted here; for
    /// rows aggregated from `MeshServicesRegistry` carries the remote node id.
    pub node_id: String,
    /// Mirrors `services.status` ("running" / "degraded" / ...).
    pub availability: String,
    /// `services.transport` (embedded / http_direct / sidecar_quic /
    /// external_http).
    pub transport: String,
    /// `services.endpoint_url` when known.
    pub endpoint_url: Option<String>,
    /// Capabilities array carried verbatim from the DB JSON column.
    pub capabilities: Vec<String>,
    /// Optional context window length advertised by the engine.
    pub context_length: Option<u32>,
    /// Optional quantization tag (e.g. "Q4_K_M").
    pub quantization: Option<String>,
    /// Whether this row is the default model for its parent service.
    pub is_default: bool,
}

// =============================================================================
// Services — runtime view of deployed services + grouped models. The whole
// surface is packed into `ServicePayload` to keep the 256-variant rkyv limit
// on `MessageBody` (same trick as `DeploymentPayload` / `MeetingPayload`).
// =============================================================================

/// Single model row attached to a `ServiceInfo`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ServiceModelEntry {
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: Vec<String>,
    pub context_length: Option<u32>,
    pub quantization: Option<String>,
    pub is_default: bool,
}

/// Runtime view of one deployed service. Aggregates the `services` row with
/// its attached `model_registry` rows. Niesie tez `request_time_parameters`
/// — typed mape wartosci ktore BackendClient materializuje przy kazdym
/// requestcie (Ollama options, python wrapper extra fields, whisper/mlx
/// deploy defaults z opcjonalnym per-request override).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ServiceInfo {
    pub id: i64,
    /// Owning mesh node — endpoint-id hex. Same as the local node when the row
    /// originates from this process; populated from the announcement payload
    /// for snapshots received over mesh sync (see `MeshServicesRegistry`).
    pub node_id: String,
    pub engine_id: String,
    /// llm / stt / tts / embeddings / image-gen / agents / ...
    pub category: String,
    pub display_name: String,
    /// docker / native_embedded / native_binary / native_python_bundle / external.
    pub deploy_method: String,
    /// embedded / http_direct / sidecar_quic / external_http.
    pub transport: String,
    /// starting / running / degraded / failed / stopped.
    pub status: String,
    pub pinned: bool,
    pub paused: bool,
    pub runtime_pid: Option<i64>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub endpoint_url: Option<String>,
    pub restart_count: u32,
    pub health_last_err: Option<String>,
    /// Krótki user-friendly opis aktualnej fazy startu (np.
    /// "warming up — alive 30s, waiting for /v1/models"). Aktualizowany
    /// przez supervisor heartbeat co 5s podczas Starting. Frontend
    /// pokazuje obok status chipa, zeby user widzial PROGRES (vLLM
    /// cold start ~3 min). NULL gdy serwis Running albo nic do
    /// raportowania.
    pub progress_message: Option<String>,
    pub models: Vec<ServiceModelEntry>,
    pub created_at: String,
    pub updated_at: String,
    /// Typed request-time parameters z `services.config_json.parameters`,
    /// propagowane do BackendClient przez handles_cache. Puste mapy gdy
    /// service nie ma konfigurowalnych parametrow.
    pub request_time_parameters: RequestTimeParameters,
}

/// Wartosci parametrow konsumowane przy kazdym requestcie do silnika.
/// Per-target storage:
///   * `ollama_options` → klucz=wartosc dla Ollama API `options` mapy w
///     POST `/api/generate`/`/api/chat`.
///   * `python_request` → pola POST body dla generic Python wrapperow
///     (qwen-asr, kyutai-tts, xtts, voxcpm, chatterbox).
///   * `whisper_overridable` → deploy defaults dla whisper z
///     `request_override = true`; backend uzywa jako baseline, klient API
///     moze nadpisac per request.
///   * `mlx_overridable` → analogicznie dla MLX engine.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[rkyv(derive(Debug))]
pub struct RequestTimeParameters {
    pub ollama_options: Vec<KeyValue>,
    pub python_request: Vec<KeyValue>,
    pub whisper_overridable: Vec<KeyValue>,
    pub mlx_overridable: Vec<KeyValue>,
}

/// Generic key-value pair dla typed parametrow propagowanych przez wire.
/// Wartosc jako serialized JSON string (rkyv nie obsluguje natywnie
/// `serde_json::Value`).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct KeyValue {
    pub key: String,
    /// JSON-serialized value. Konsument deserializuje przez `serde_json::from_str`.
    pub value_json: String,
}

/// Incremental change applied to one entry in the mesh services registry. Used
/// by `MeshServicesUpdate` push messages so peers do not have to re-broadcast
/// the full snapshot on every deploy / stop / pin / pause / rename / delete.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum ServiceChange {
    Added(ServiceInfo),
    Updated(ServiceInfo),
    Removed { service_id: i64 },
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceListRequest {
    /// Reserved for future filtering (engine / category). Empty vec = no filter.
    pub engine_id_filter: Option<String>,
    pub category_filter: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceListResponse {
    pub services: Vec<ServiceInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceDeleteRequest {
    pub service_id: i64,
    /// Target mesh node. `None` (or local node id) = run locally; otherwise
    /// the dispatcher forwards the action to the named peer over mesh and
    /// waits for the response. `service_id` always lives in the target node's
    /// SQLite namespace.
    pub node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceDeleteResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServicePinRequest {
    pub service_id: i64,
    pub pinned: bool,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServicePinResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceStartRequest {
    pub service_id: i64,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceStartResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServicePauseRequest {
    pub service_id: i64,
    pub paused: bool,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServicePauseResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// Edycja istniejącego serwisu (po deploy). Pola opcjonalne — backend
/// aktualizuje tylko te które są `Some(_)`. `restart_after_save=true`
/// wymusza stop+respawn z nowym configiem (vLLM model reload ~30–180s).
///
/// Typed parameters (max_model_len, max_num_seqs, kv_cache_dtype itd.)
/// są materializowane do `services.config_json` jako manifest schema
/// parameters — backend regeneruje `vllm_args` ze schema bindings, więc
/// klient może wysłać albo typed pola albo `vllm_args` raw (power user).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ServiceUpdateRequest {
    pub service_id: i64,
    /// See `ServiceDeleteRequest::node_id` — None = local node.
    pub node_id: Option<String>,
    /// HF repo — switch model bez delete+create. `model_preset_id` ma
    /// wyższy priorytet gdy oba podane.
    pub model_repo: Option<String>,
    pub model_preset_id: Option<String>,
    /// vLLM-specific parametry runtime. Backend mapuje na `config_json`
    /// keys i dorzuca do regenerated `vllm_args` jeśli engine to vLLM.
    pub gpu_memory_utilization: Option<f32>,
    pub max_model_len: Option<u32>,
    pub max_num_seqs: Option<u32>,
    pub max_num_batched_tokens: Option<u32>,
    pub kv_cache_dtype: Option<String>,
    pub chunked_prefill: Option<bool>,
    /// Power user: surowe `vllm_args`. Gdy ustawione, nadpisuje typed
    /// pola powyżej (backend honoruje 1:1, brak walidacji).
    pub vllm_args_override: Option<String>,
    /// Pinned/paused flagi — pomija jeśli `None`.
    pub pinned: Option<bool>,
    pub paused: Option<bool>,
    /// `true` = stop running service + respawn z nowym configiem.
    /// `false` = tylko zapisz do DB (zmiany aktywne po następnym restarcie).
    pub restart_after_save: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceUpdateResponse {
    pub success: bool,
    pub error: Option<String>,
    /// `true` jeśli serwis został restartowany w ramach tej operacji.
    pub restarted: bool,
}

/// Snapshot aktualnego zajęcia VRAM per GPU + lista zewnętrznych procesów.
/// Klient wywołuje co 2s podczas modal Edit / wizard Advanced step żeby
/// pokazać user'owi "co już używa GPU" + zalecony `gpu_memory_utilization`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceVramHintRequest {
    /// `None` = wszystkie GPU. Zawęź do indeksu jeśli wizard już wybrał GPU.
    pub gpu_index: Option<u32>,
    /// `None` = local node. Mesh forward gdy wybrano peer.
    pub node_id: Option<String>,
    /// Service ID dla którego liczymy hint (excluded z external — własne
    /// procesy serwisu nie liczą się jako "external"). `None` = nowy
    /// deploy, brak wykluczeń.
    pub exclude_service_id: Option<i64>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct ServiceVramHintResponse {
    pub gpus: Vec<GpuVramSnapshot>,
    /// Sugerowane `gpu_memory_utilization` z uwzględnieniem external
    /// processes. Wzór: `(free_mib - desktop_reserve_mib) / total_mib`,
    /// clamp [0.10..0.95]. Desktop reserve = 1024 MiB (bezpieczne dla
    /// X11/Wayland compositor + headroom).
    pub recommended_utilization: Option<f32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GpuVramSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    pub total_mib: u64,
    pub free_mib: u64,
    pub used_mib: u64,
    pub external_processes: Vec<GpuProcessInfo>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct GpuProcessInfo {
    pub pid: u32,
    pub process_name: String,
    pub used_mib: u64,
}

/// Lista presetów modelu z manifestu silnika. Edit modal wywołuje to po
/// zmianie dropdown'a "Preset z manifestu" — backend zwraca dokładnie te
/// `[[model_preset]]` które są zadeklarowane w `<engine>.toml` (single
/// source of truth, build.rs generuje z TOML do `services_generated.rs`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceEnginePresetsRequest {
    pub engine_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceEnginePresetsResponse {
    pub presets: Vec<ServicePresetInfo>,
}

/// Pojedynczy preset z manifestu — frontend renderuje jako preset-card
/// w Edit modal lub deploy wizard. `repo` to HF repository, `quantization`
/// pochodzi z manifestu (auto/awq/gptq/nvfp4/...). Pełen VRAM estimate
/// liczony jest osobno przez `DeployVllmRecommendRequest` po wyborze.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ServicePresetInfo {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    pub quantization: Option<String>,
    pub recommended: bool,
}

/// Inner enum bundling every services-screen RPC pair into a single MessageBody
/// slot — `MessageBody::ServiceBody`. Pattern mirrors `DeploymentPayload`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum ServicePayload {
    ReqList(ServiceListRequest),
    ResList(ServiceListResponse),
    ReqDelete(ServiceDeleteRequest),
    ResDelete(ServiceDeleteResponse),
    ReqPin(ServicePinRequest),
    ResPin(ServicePinResponse),
    ReqPause(ServicePauseRequest),
    ResPause(ServicePauseResponse),
    ReqStart(ServiceStartRequest),
    ResStart(ServiceStartResponse),
    ReqUpdate(ServiceUpdateRequest),
    ResUpdate(ServiceUpdateResponse),
    ReqVramHint(ServiceVramHintRequest),
    ResVramHint(ServiceVramHintResponse),
    ReqEnginePresets(ServiceEnginePresetsRequest),
    ResEnginePresets(ServiceEnginePresetsResponse),
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
    /// Stan zasobu wyklucza wykonanie operacji (np. inna sesja juz trwa).
    Conflict = 11,
    /// Funkcjonalnosc niedostepna na tym nodzie (brak narzedzia/feature flagi).
    NotAvailable = 12,
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
// Me / User preferences (preferowany jezyk dla TTS itd.)
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MePreferencesGetRequest {}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MePreferencesGetResponse {
    pub language: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MePreferencesUpdateRequest {
    pub language: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MePreferencesUpdateResponse {
    pub language: Option<String>,
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
    /// When `Some`, expose this flow as a model with the given id through
    /// the catalog (`/v1/models`, mesh `catalog.list`, GUI). The handler
    /// validates the name against active aliases and other published flows
    /// before writing — collisions return a domain error instead of being
    /// silently accepted (D.19).
    pub published_model_name: Option<String>,
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
// Teams-bot wake words — slowa aktywujace odpowiedz bota
// =============================================================================

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct WakeWord {
    pub id: i64,
    pub word: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Sub-action `WakeWordRequest` — list/create/toggle/delete.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum WakeWordOp {
    List,
    Create { word: String },
    Toggle { id: i64, enabled: bool },
    Delete { id: i64 },
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

/// Inner-enum pack — wszystkie trust eventy w jednym slocie MessageBody.
/// Konsolidacja zwalnia slot pod nowe warianty (rkyv 0.8 ma twardy limit 256).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum MeshTrustEventPayload {
    /// Broadcast cofniecia trust (mesh discriminant 0x23).
    Revoked(MeshTrustRevokedEvent),
    /// Post-pairing sync listy zaufanych kluczy (mesh discriminant 0x24).
    KeysSync(MeshTrustedKeysSyncEvent),
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
pub enum MeshConnState {
    Disconnected,
    Connecting,
    Connected,
    Degraded,
    Reconnecting,
    Offline,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeshConnectionInfo {
    pub state: MeshConnState,
    pub transport: String,
    pub scope: Option<String>,
    pub address: Option<String>,
    pub relay_url: Option<String>,
    pub paths: Vec<MeshConnectionPathInfo>,
    /// Unix epoch ms — moment ostatniej zmiany stanu (`state`).
    pub since_ms: i64,
    /// Unix epoch ms — ostatni heartbeat aplikacyjny od peera. 0 gdy brak.
    pub last_app_heartbeat_ms: i64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeshNodeInfo {
    pub node_id: String,
    pub hostname: String,
    pub ip: Option<String>,
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
    /// Czy `nsys` (NVIDIA Nsight Systems) jest dostepny na nodzie — wymagany do
    /// uruchomienia sesji profilowania GPU.
    pub nsys_available: bool,
    /// Wykryta wersja `nsys` (np. "2024.5.1"); pusty string gdy niedostepny.
    pub nsys_version: String,
    /// Multi-source profiling: lista identyfikatorow kolektorow (np.
    /// `linux.proc.cpu_util`, `nvidia.nsys.gpu`) ktore peer moze uruchomic.
    /// Pusta lista = peer nie obsluguje multi-source profiling V2.
    pub profiling_collectors_available: Vec<String>,
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
// Mesh & Network settings (IPv4-only enumeracja NIC + reguly bind/advertise)
// =============================================================================

/// Pojedynczy interfejs sieciowy hosta z adresami IPv4 (v6 odrzucane).
/// `kind` jest znormalizowaną kategorią dla GUI (nie surowy `InterfaceType`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub mac: String,
    pub ipv4_addrs: Vec<String>,
    pub mtu: u32,
    /// "ethernet" | "wifi" | "loopback" | "docker" | "tunnel" | "virtual" | "unknown"
    pub kind: String,
    pub is_up: bool,
    pub description: String,
}

/// Perzistowana konfiguracja mesh networking. `bind_mode="auto"` pozwala iroh
/// bindowac 0.0.0.0, `"custom"` wymusza `bind_ipv4`. Flagi `hide_*` filtruja
/// adresy wysylane peerom. `iroh_relay_url` pusty = default N0 preset.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    /// "auto" | "custom"
    pub bind_mode: String,
    pub bind_ipv4: String,
    pub hide_docker: bool,
    pub hide_link_local: bool,
    pub hide_loopback: bool,
    pub hide_cgnat: bool,
    pub prefer_same_subnet: bool,
    pub iroh_relay_url: String,
}

/// Snapshot zdrowia relay iroh — co backend wie o aktualnym stanie polaczenia
/// z konfigurowanym serwerem relay + faktyczny adres bind iroh endpointu.
/// `rtt_ms == 0` gdy relay unreachable; `last_success_unix_secs == 0` gdy nigdy
/// nie udalo sie zpingowac. `status` jest jedna z czterech wartosci:
/// `"connected" | "degraded" | "unreachable" | "disabled"`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct RelayHealthInfo {
    pub url: String,
    pub reachable: bool,
    pub rtt_ms: u32,
    pub last_check_unix_secs: i64,
    pub last_success_unix_secs: i64,
    pub status: String,
    /// Realny adres bind iroh endpointu (np. "192.168.0.93:8090" lub
    /// "0.0.0.0:8090" gdy fallback z custom IP). To jest to co iroh REALNIE
    /// zbindowal, nie zadanie z DB — dzieki temu GUI moze pokazac fallback.
    pub bind_addr_actual: String,
}

/// Skonsolidowany payload dla Mesh & Network settings — 6 logicznych variantow
/// (interfaces list req/res, config get req/res, config update req/res) zajmuje
/// 1 slot w `MessageBody` zeby zmiescic sie w 256-variant limicie rkyv.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum NetworkPayload {
    ReqInterfacesList,
    ResInterfacesList {
        interfaces: Vec<NetworkInterfaceInfo>,
    },
    ReqConfigGet,
    ResConfigGet(NetworkConfig),
    ReqConfigUpdate(NetworkConfig),
    ResConfigUpdate {
        restart_required: bool,
    },
    ReqRelayStatus,
    ResRelayStatus(RelayHealthInfo),
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
    /// Update or clear the catalog publish name. `Some(Some("..."))`
    /// publishes / re-publishes; `Some(None)` un-publishes; `None` leaves
    /// the existing value untouched. Validated against the catalog before
    /// the row is written.
    pub published_model_name: Option<Option<String>>,
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
    /// Typ danych per port wejsciowy w tej samej kolejnosci co `input_ports`.
    /// Wartosci jako lowercase string FlowDataType: "any" / "text" / "audio"
    /// / "image" / "video" / "embedding" / "json". GUI uzywa do kolorowania
    /// portu i blokowania niekompatybilnych polaczen (lustrzana walidacja R8).
    pub input_port_types: Vec<String>,
    /// Analogicznie do `input_port_types`, dla portow wyjsciowych.
    pub output_port_types: Vec<String>,
    /// JSON-Schema-like opis pol konfiguracyjnych. Pusty string = brak
    /// schemy (config tab w builderze pokazuje "Brak parametrow"). Format:
    /// `{"properties":{<key>:{type, title, description, default, enum?,
    /// minimum?, maximum?, format?, dynamic_enum?}}, "required":[...],
    /// "order":[...]}`. `dynamic_enum` (rozszerzenie tentaflow): mowi GUI
    /// zeby wczytac liste z runtime registry zamiast statycznego enum
    /// — `{"source":"models","category":"stt"|"tts"|"llm"|"embeddings"}`.
    pub params_schema: String,
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

/// One node hosting a service model. Reused inside `CatalogEntryKind::ServiceModel`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CatalogModelInstance {
    pub node_id: String,
    pub node_hostname: Option<String>,
    pub service_id: i64,
    pub status: String,
    /// Engine serving the model (e.g. "llama-cpp", "vllm", "mlx", "whisper-rs").
    pub backend: Option<String>,
    /// Model weights size in MB when known.
    pub size_mb: Option<u64>,
    /// Convenience flag mirroring `status in ('running', 'ready')`.
    pub loaded: bool,
}

/// What a single catalog entry represents on the wire. Mirrors
/// `services::catalog::CatalogEntryKind` from `tentaflow-core` but expresses
/// enums as plain strings so that adding a new surface or modality on the
/// service side does not require a protocol bump.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum CatalogEntryKindWire {
    ServiceModel {
        instances: Vec<CatalogModelInstance>,
    },
    Flow {
        flow_id: i64,
        published_name: String,
    },
    Alias {
        target: String,
        fallback_targets: Vec<String>,
        /// "first_available" | "round_robin" — open string per D.11.
        strategy: String,
    },
}

/// Diagnostic flag attached to an entry. Strings instead of typed enums for
/// the same forward-compatibility reason as `CatalogEntryKindWire`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum CatalogDiagnosticWire {
    RemoteShadowed {
        local_owner: String,
    },
    LocalOverride {
        conflicting_remote_node: String,
    },
    IncompatibleAliasTargets {
        alias: String,
        /// Lower-snake-case modality names ("text", "image", "audio").
        missing_modalities: Vec<String>,
    },
}

/// One advertised model in the unified catalog. Surface and modality lists
/// stay as strings so protocol can absorb new values without a schema bump.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntryWire {
    pub id: String,
    pub kind: CatalogEntryKindWire,
    pub service_surfaces: Vec<String>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub diagnostic: Option<CatalogDiagnosticWire>,
    /// `tentaflow-service` | `tentaflow-flow` | `tentaflow-alias`.
    pub owned_by: String,
}

/// Catalog list request. The wire form lets callers narrow by surface and
/// admin tooling opt into seeing entries hidden from `/v1/models`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogListRequest {
    /// When set, return only entries whose `service_surfaces` contain the
    /// given surface string (e.g. "chat", "stt"). `None` = no filter.
    pub surface_filter: Option<String>,
    /// When `true`, include entries blocked by RemoteShadowed / LocalOverride
    /// diagnostics. Used by GUI admin views; the OpenAI `/v1/models` path
    /// always passes `false`.
    pub include_blocking_diagnostics: bool,
}

/// Catalog list response. `version` is monotonic and lets clients cheaply
/// detect "anything changed since my last poll" without diffing entries.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CatalogListResponse {
    pub entries: Vec<CatalogEntryWire>,
    pub version: u64,
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

// =============================================================================
// vLLM deploy recommend (TP/PP/ctx_len/max_seqs/kv_dtype calculator).
// f64 fields drop Eq; PartialEq only.
// =============================================================================

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmGpuInfo {
    pub index: u32,
    pub name: String,
    pub memory_gb: f64,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmRecommendRequest {
    pub model: String,
    pub gpus: Vec<DeployVllmGpuInfo>,
    pub hf_token: Option<String>,
    pub tensor_parallel: Option<u32>,
    pub pipeline_parallel: Option<u32>,
    pub max_model_len: Option<u64>,
    pub max_num_seqs: Option<u64>,
    pub kv_cache_dtype: Option<String>,
    pub gpu_memory_utilization: Option<f64>,
    pub quantization_override: Option<String>,
    pub lock_max_model_len: Option<bool>,
    pub lock_max_num_seqs: Option<bool>,
    pub lock_tensor_parallel: Option<bool>,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmConfig {
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    pub kv_cache_dtype: String,
    pub gpu_memory_utilization: f64,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmModelSpecSummary {
    pub model_type: String,
    pub architectures: Vec<String>,
    pub dtype: String,
    pub quantization: Option<String>,
    pub hidden_size: u64,
    pub num_attention_heads: u64,
    pub num_key_value_heads: u64,
    pub num_hidden_layers: u64,
    pub max_position_embeddings: u64,
    pub has_vision: bool,
    pub has_audio: bool,
    pub estimated_params_billions: f64,
    pub bytes_per_param: f64,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmVramEstimate {
    pub model_weights_gb: f64,
    pub kv_cache_gb: f64,
    pub activations_gb: f64,
    pub overhead_gb: f64,
    pub total_gb: f64,
    pub per_gpu_gb: f64,
    pub fits_per_gpu: bool,
    pub fits_total: bool,
    pub warnings: Vec<String>,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmGpuCompatibility {
    pub used_tp: u32,
    pub used_pp: u32,
    pub uses_all_gpus: bool,
    pub clean_partition: bool,
    pub better_gpu_counts: Vec<u32>,
    pub warning: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmRecommendResponse {
    pub model_spec: DeployVllmModelSpecSummary,
    pub vram_estimate: DeployVllmVramEstimate,
    pub recommended: DeployVllmConfig,
    pub max_supported_model_len: u64,
    pub max_supported_num_seqs: u64,
    pub recommended_vllm_args: String,
    pub warnings: Vec<String>,
    pub gpu_compatibility: DeployVllmGpuCompatibility,
    pub applied: DeployVllmConfig,
    pub auto_adjusted: Vec<String>,
    pub at_limit: bool,
}

/// Generyczne wywolanie auto-tunera dla dowolnego silnika z `[[parameter]]`
/// schema w manifescie. Backend dispatchuje per `engine_id` (vllm/sglang/
/// tensorrt-llm uzywaja `auto_fit_config` z mapowaniem do typed pol; inne
/// silniki maja proste defaulty per kategoria). Zwraca typed mape
/// `parameter.key → JSON value` ktora wizard pre-filluje do formularza.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EngineRecommendRequest {
    pub engine_id: String,
    pub model_repo: String,
    pub gpus: Vec<DeployVllmGpuInfo>,
    pub hf_token: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EngineRecommendResponse {
    /// JSON-serialized values per parameter key. Wizard JS deserializuje
    /// zgodnie z `parameter.kind` z manifestu.
    pub parameters: Vec<KeyValue>,
    pub warnings: Vec<String>,
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

// =============================================================================
// SCHEMA v14: Apps menu + UI v2 endpointy
// =============================================================================

/// Aplikacja addonu widoczna w głównym menu launcher. Źródło:
/// manifest `[application]` sekcja po install.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonApplicationInfo {
    pub addon_id: String,
    pub title: String,
    /// ID panelu UI startowego (frontend ładuje przez
    /// `AddonUiPayload::ReqPanelGet`).
    pub entry_panel: String,
    /// Identyfikator ikony sprite. None = fallback na `addon.icon`.
    pub icon: Option<String>,
    /// Kolejność w menu (mniejsze = wyżej). Default 100 jeżeli manifest pominie.
    pub sort_order: i32,
}

/// Multiplex 6 endpointów Apps menu + UI v2 w jednym slocie `MessageBody`,
/// zeby zmiescic sie w 256-variant limicie rkyv. Tag jest na poziomie tego
/// enum'a, nie zewnetrznego `MessageBody`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum AddonUiPayload {
    // ---- Apps menu ----
    ReqApplicationsList,
    ResApplicationsList {
        applications: Vec<AddonApplicationInfo>,
    },

    // ---- UI panel get (read last rendered tree from cache) ----
    ReqPanelGet {
        addon_id: String,
        panel_id: String,
    },
    /// `tree_json` empty = brak panelu w cache (addon nie wywolał ui_render).
    ResPanelGet {
        addon_id: String,
        panel_id: String,
        tree_json: String,
    },

    // ---- UI action (button click / form submit) ----
    /// Host woła addon on_request z tool_name = "ui.{panel_id}.{action_id}".
    /// `params_json` to JSON payload (form values, event metadata).
    ReqAction {
        addon_id: String,
        panel_id: String,
        action_id: String,
        params_json: String,
    },
    /// `result_json` empty = brak wyniku.
    ResAction {
        result_json: String,
    },
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
    /// Aktualny etap lifecycle bota (patrz `LIFECYCLE_*` w `types.rs`).
    /// Pusty string gdy sesja jeszcze nie dotknęła żadnego etapu.
    pub lifecycle_stage: String,
    /// Opcjonalne szczegóły ostatniego etapu (np. treść błędu przy `failed`).
    /// Pusty string = brak dodatkowych informacji.
    pub lifecycle_details: String,
    /// Backend models reported by the bot via BackendUpdate. Empty string
    /// when the bot has not reported the field yet (live view shows a
    /// placeholder). Numeric counters use `-1` as the same sentinel.
    pub backend_stt_model: String,
    pub backend_tts_model: String,
    pub backend_summarization_model: String,
    pub backend_diarization_model: String,
    pub backend_streaming_latency_ms: i64,
    pub backend_enrolled_speakers: i64,
    pub backend_total_participants: i64,
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

// -----------------------------------------------------------------------------
// Summaries / action items / transcript export (post-Etap 2.1).
// -----------------------------------------------------------------------------

/// Jedno podsumowanie sesji z `meeting_summaries`. Protokolowa forma bez
/// content_hash — dedup jest szczegolem DB i nie jedzie po wire.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSummaryItem {
    pub id: i64,
    pub created_at: String,
    pub decisions_text: String,
    pub summary_text: String,
    pub model: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSummariesListRequest {
    pub meeting_key: String,
    /// Limit najnowszych rekordow. `None` = domyslnie 20.
    pub limit: Option<u32>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingSummariesListResponse {
    pub items: Vec<MeetingSummaryItem>,
}

/// Action item wyekstrahowany przez LLM z transkryptu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemItem {
    pub id: i64,
    pub owner: String,
    pub task: String,
    pub deadline: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemsListRequest {
    pub meeting_key: String,
    /// `None` = wszystkie; `Some("pending"|"done"|"cancelled")` = filtr po statusie.
    pub status_filter: Option<String>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemsListResponse {
    pub items: Vec<MeetingActionItemItem>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemStatusUpdateRequest {
    pub item_id: i64,
    pub status: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemStatusUpdateResponse {
    pub success: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingTranscriptExportRequest {
    pub meeting_key: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingTranscriptExportResponse {
    /// Sformatowany plain text gotowy do zapisu jako .txt (naglowek + linie).
    pub content: String,
}

// =============================================================================
// Meeting VNC tunnel — same-node websockify bridge through dashboard WSS.
// =============================================================================
//
// Phase A: frontend opens `VncTunnelOpenRequest{session_id}` as a subscription.
// Handler bridges a TCP connection to the container's novnc port (websockify)
// and streams RFB bytes back as `VncTunnelChunk`. Reverse direction (keyboard/
// mouse events) uses one-shot `VncTunnelSendRequest{tunnel_id, bytes}`. On TCP
// end a `VncTunnelStreamEnd` is emitted and the tunnel entry is cleaned up.
// Cross-node forwarding over iroh is reserved for phase B (remote_node status).

pub const VNC_TUNNEL_OPEN_OK: &str = "ok";
pub const VNC_TUNNEL_OPEN_NOT_FOUND: &str = "not_found";
pub const VNC_TUNNEL_OPEN_FORBIDDEN: &str = "forbidden";
pub const VNC_TUNNEL_OPEN_NO_PORT: &str = "no_port";
pub const VNC_TUNNEL_OPEN_REMOTE_NODE: &str = "remote_node";
pub const VNC_TUNNEL_OPEN_FAILED: &str = "failed";

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelOpenRequest {
    pub session_id: i64,
}

/// First frame on the subscription stream. When `status != "ok"`, the stream
/// also ends immediately and `tunnel_id` is empty.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelOpenResponse {
    pub status: String,
    pub tunnel_id: String,
    pub error: String,
}

/// RFB bytes read from the container TCP socket, pushed to the browser.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelChunk {
    pub tunnel_id: String,
    pub bytes: Vec<u8>,
}

/// Browser → container RFB bytes (keyboard/mouse, client init). One-shot.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelSendRequest {
    pub tunnel_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelSendResponse {
    pub ok: bool,
    pub error: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelCloseRequest {
    pub tunnel_id: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelCloseResponse {
    pub ok: bool,
}

/// Emitted as the terminal stream chunk when the container-side TCP socket
/// closes (either EOF, I/O error, or handler-initiated shutdown).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct VncTunnelStreamEnd {
    pub tunnel_id: String,
    pub reason: String,
}

/// Single inner enum carrying every VNC tunnel message so the top-level
/// `MessageBody` spends only one variant slot on the feature.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum VncTunnelPayload {
    ReqOpen(VncTunnelOpenRequest),
    ResOpen(VncTunnelOpenResponse),
    Chunk(VncTunnelChunk),
    ReqSend(VncTunnelSendRequest),
    ResSend(VncTunnelSendResponse),
    ReqClose(VncTunnelCloseRequest),
    ResClose(VncTunnelCloseResponse),
    StreamEnd(VncTunnelStreamEnd),
}

// =============================================================================
// Meeting Browser Capture — jednorazowe zapytania do teams-bot po screenshot
// albo snapshot DOM aktywnej strony Chromium. Dashboard pyta przez WSS,
// handler otwiera bistream do bota i dostaje `BrowserResult` w `ModelResponse`.
// =============================================================================

pub const BROWSER_CAPTURE_OK: &str = "ok";
pub const BROWSER_CAPTURE_NOT_FOUND: &str = "not_found";
pub const BROWSER_CAPTURE_FORBIDDEN: &str = "forbidden";
pub const BROWSER_CAPTURE_REMOTE_NODE: &str = "remote_node";
pub const BROWSER_CAPTURE_FAILED: &str = "failed";

pub const BROWSER_CAPTURE_KIND_SCREENSHOT: &str = "screenshot";
pub const BROWSER_CAPTURE_KIND_DOM: &str = "dom";

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BrowserCaptureRequest {
    pub session_id: i64,
    /// `"screenshot"` albo `"dom"`. Inna wartość => `status="failed"`.
    pub kind: String,
    /// Ignorowane gdy `kind="dom"`. Dla screenshota: true => cała strona ze
    /// scrollowaniem, false => tylko viewport.
    pub full_page: bool,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BrowserCaptureResponse {
    pub status: String,
    pub kind: String,
    /// Populated gdy `kind="screenshot"` i `status="ok"`.
    pub png: Vec<u8>,
    /// Populated gdy `kind="dom"` i `status="ok"`.
    pub html: String,
    /// Opis błędu gdy `status != "ok"`.
    pub error: String,
}

/// Single inner enum carrying both browser capture messages so the top-level
/// `MessageBody` spends only one variant slot on the feature.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum BrowserCapturePayload {
    Request(BrowserCaptureRequest),
    Response(BrowserCaptureResponse),
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
    ReqSummariesList(MeetingSummariesListRequest),
    ResSummariesList(MeetingSummariesListResponse),
    ReqActionItemsList(MeetingActionItemsListRequest),
    ResActionItemsList(MeetingActionItemsListResponse),
    ReqActionItemStatusUpdate(MeetingActionItemStatusUpdateRequest),
    ResActionItemStatusUpdate(MeetingActionItemStatusUpdateResponse),
    ReqTranscriptExport(MeetingTranscriptExportRequest),
    ResTranscriptExport(MeetingTranscriptExportResponse),
    /// Wake-words CRUD: list/create/toggle/delete (1 sub-action)
    ReqWakeWord(MeetingWakeWordRequest),
    ResWakeWord(MeetingWakeWordResponse),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingWakeWordRequest {
    pub op: WakeWordOp,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingWakeWordResponse {
    pub words: Vec<WakeWord>,
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

// Skonsolidowane w `TranslatePayload` — 1 slot w `MessageBody` zamiast 2,
// zeby zmiescic sie w limicie 256 wariantow rkyv 0.8.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum TranslatePayload {
    Req(TranslateRequest),
    Res(TranslateResponse),
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
    ResListUsers {
        users: Vec<UserInfo>,
    },
    ReqGetUser {
        user_id: i64,
    },
    ResGetUser {
        user: UserInfo,
    },
    ReqCreateUser {
        username: String,
        password: String,
        display_name: String,
        email: String,
        role: String,
        group_ids: Vec<i64>,
    },
    ResCreateUser {
        user_id: i64,
    },
    ReqUpdateUser {
        user_id: i64,
        display_name: String,
        email: String,
        is_active: bool,
        role: String,
    },
    ReqDeleteUser {
        user_id: i64,
    },
    ReqSetUserGroups {
        user_id: i64,
        group_ids: Vec<i64>,
    },
    ReqResetUserPassword {
        user_id: i64,
        new_password: String,
    },

    // ---- Groups ----
    ReqListGroups,
    ResListGroups {
        groups: Vec<GroupInfo>,
    },
    ReqCreateGroup {
        name: String,
        description: String,
    },
    ResCreateGroup {
        group_id: i64,
    },
    ReqUpdateGroup {
        group_id: i64,
        name: String,
        description: String,
    },
    ReqDeleteGroup {
        group_id: i64,
    },
    ReqGroupMembers {
        group_id: i64,
    },
    ResGroupMembers {
        members: Vec<UserInfo>,
    },

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
    ReqListPermsForResource {
        resource_type: String,
        resource_id: String,
    },
    ReqListPermsForSubject {
        subject_type: String,
        subject_id: i64,
    },
    ResListPermissions {
        entries: Vec<PermissionEntry>,
    },

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
    MetaSchemaVersionCheck {
        client_version: u16,
    },
    /// Serwer -> klient: potwierdzenie (accepted=false => disconnect).
    MetaSchemaVersionAck {
        server_version: u16,
        accepted: bool,
    },
    /// Dwukierunkowy keepalive (WSS ping substitute, liczy RTT).
    MetaHeartbeat {
        sent_at_epoch: u64,
    },
    /// Klient -> serwer: anuluj aktywny stream (match po correlation_id w envelope).
    MetaCancelStream,

    // ---- Read-list (R-LIST archetyp) ----
    /// Klient -> serwer: lista modeli (publiczne, Anonymous OK).
    ModelListRequest,
    /// Serwer -> klient: odpowiedz.
    ModelListResponse {
        models: Vec<ModelSummary>,
    },

    // ---- API Keys (R-LIST + W-CREATE + W-DELETE) ----
    ApiKeyListRequest,
    ApiKeyListResponse {
        keys: Vec<ApiKeySummary>,
    },
    ApiKeyCreateRequestBody(ApiKeyCreateRequest),
    ApiKeyCreateResponseBody(ApiKeyCreateResponse),
    ApiKeyRevokeRequest {
        key_id: String,
    },
    ApiKeyRevokeResponse {
        deleted: bool,
    },

    // ---- Auth (W-ACTION + R-ONE) ----
    AuthLoginRequestBody(AuthLoginRequest),
    AuthLoginResponseBody(AuthLoginResponse),
    AuthMeRequest,
    AuthMeResponseBody(AuthMeResponse),

    // ---- Me / User preferences ----
    MePreferencesGetRequestBody(MePreferencesGetRequest),
    MePreferencesGetResponseBody(MePreferencesGetResponse),
    MePreferencesUpdateRequestBody(MePreferencesUpdateRequest),
    MePreferencesUpdateResponseBody(MePreferencesUpdateResponse),

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
    MeshPeersListResponse {
        peers: Vec<MeshPeerSummary>,
    },
    MeshPairInitRequestBody(MeshPairInitRequest),
    MeshPairInitResponseBody(MeshPairInitResponse),

    // ---- Mesh trust events (broadcast / sync) — skonsolidowane w jeden slot ----
    MeshTrustEventBody(MeshTrustEventPayload),

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

    // ---- Prompts (R-LIST + R-ONE) ----
    PromptListRequest,
    PromptListResponse {
        prompts: Vec<PromptSummary>,
    },
    PromptDetailRequest {
        prompt_id: String,
    },
    PromptDetailResponse(PromptDetail),

    // ---- Registries (R-LIST) ----
    RegistryListRequest,
    RegistryListResponse {
        registries: Vec<RegistrySummary>,
    },

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
    ContainerListResponse {
        containers: Vec<ContainerSummary>,
    },
    ContainerStartRequest {
        container_id: String,
    },
    ContainerStartResponse {
        started: bool,
    },
    ContainerStopRequest {
        container_id: String,
    },
    ContainerStopResponse {
        stopped: bool,
    },
    ContainerLogStreamRequest {
        container_id: String,
        follow: bool,
    },
    ContainerLogChunkBody(ContainerLogChunk),

    // ---- Voice profiles (R-LIST) ----
    VoiceProfileListRequest,
    VoiceProfileListResponse {
        profiles: Vec<VoiceProfileSummary>,
    },

    // ---- TTS rules (R-LIST + W-CREATE/UPDATE/DELETE) ----
    TtsRuleListRequest,
    TtsRuleListResponse {
        rules: Vec<TtsRule>,
    },
    TtsRuleCreateRequest(TtsRule),
    TtsRuleCreateResponse {
        rule_id: String,
    },
    TtsRuleDeleteRequest {
        rule_id: String,
    },
    TtsRuleDeleteResponse {
        deleted: bool,
    },

    // ---- PII rules (spakowane w inner enum dla oszczednosci slotu) ----
    // Patrz ProfilingBody i VisionBody — limit 256 wariantow w MessageBody.
    PiiRuleBody(crate::pii::PiiRulePayload),

    // ---- Fast-path patterns ----
    FastPathListRequest,
    FastPathListResponse {
        patterns: Vec<FastPathPattern>,
    },

    // ---- Models (R-ONE + W-ACTION) ----
    ModelDetailRequest {
        model_id: String,
    },
    ModelDetailResponse(ModelDetail),
    ModelInstallRequestBody(ModelInstallRequest),
    ModelInstallResponse {
        model_id: String,
        accepted: bool,
    },
    ModelDeleteRequest {
        model_id: String,
    },
    ModelDeleteResponse {
        deleted: bool,
    },

    // ---- Hub (R-LIST + R-STREAM dla download) ----
    HubEngineListRequest,
    HubEngineListResponse {
        engines: Vec<HubEngineSummary>,
    },
    HubModelSearchRequest {
        query: String,
    },
    HubModelSearchResponse {
        results: Vec<HubModelSearchResult>,
    },
    HubDownloadProgressBody(HubDownloadProgress),

    // ---- Flows (R-LIST + R-ONE + W-CREATE/UPDATE/DELETE + executions) ----
    FlowListRequest,
    FlowListResponse {
        flows: Vec<FlowSummary>,
    },
    FlowDetailRequest {
        flow_id: String,
    },
    FlowDetailResponse(FlowDetail),
    FlowCreateRequestBody(FlowCreateRequest),
    FlowCreateResponse {
        flow_id: String,
    },
    FlowDeleteRequest {
        flow_id: String,
    },
    FlowDeleteResponse {
        deleted: bool,
    },
    FlowExecutionsListRequest {
        flow_id: String,
    },
    FlowExecutionsListResponse {
        executions: Vec<FlowExecutionSummary>,
    },

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
    SubscribeResumeRequest {
        resume_token: Vec<u8>,
    },
    /// Serwer -> klient: ack/reject. Jesli accepted=true, subskrypcja jest
    /// odtworzona pod tym samym correlation_id i serwer zaraz wysle brakujace
    /// chunki z recorder buffer.
    SubscribeResumeAck {
        accepted: bool,
        error: Option<String>,
    },
    /// Serwer -> klient: token ktory pozwoli na resume po disconnect.
    /// Wysylany RAZEM z IS_STREAM_END (envelope flag), opcjonalny.
    SubscribeResumeOffer {
        resume_token: Vec<u8>,
    },

    // ---- Settings (R-LIST + W-UPDATE) ----
    SettingsListRequest,
    SettingsListResponse {
        entries: Vec<SettingEntry>,
    },
    SettingsUpdateRequestBody(SettingsUpdateRequest),
    SettingsUpdateResponse {
        applied: u32,
    },

    // ---- Mesh & Network settings (enumeracja NIC + bind/advertise rules) ----
    // Skonsolidowane w `NetworkPayload` — 1 slot w enum (256-variant limit rkyv).
    NetworkBody(NetworkPayload),

    // ---- Dashboard (R-LIST + subscription candidate) ----
    DashboardMetricsRequest,
    DashboardMetricsResponse(DashboardSnapshot),

    // ---- Models / aliases / catalog -----
    CatalogListRequestBody(CatalogListRequest),
    CatalogListResponseBody(CatalogListResponse),
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
    DeployVllmRecommendRequestBody(DeployVllmRecommendRequest),
    DeployVllmRecommendResponseBody(DeployVllmRecommendResponse),
    EngineRecommendRequestBody(EngineRecommendRequest),
    EngineRecommendResponseBody(EngineRecommendResponse),
    // ServiceManifestDeployRequest/Response przeniesione do DeploymentPayload
    // (ReqStart/ResStart). Oszczędza 1 slot w 256-variant limicie rkyv.

    // ---- Addons: list / detail / toggle / lifecycle ----
    AddonsListRequest,
    AddonsListResponseBody(AddonsListResponse),
    // v14: Apps menu + UI v2 — multiplex w 1 slocie zeby zmiescic sie w 256
    // wariantach rkyv (vide IamBody/ServicePayload).
    AddonUiBody(AddonUiPayload),
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

    // ---- Meeting VNC tunnel (one slot for entire R-STREAM + two one-shot RPCs) ----
    VncTunnelBody(VncTunnelPayload),

    // ---- Meeting browser capture (one-shot RPC: screenshot / DOM snapshot) ----
    BrowserCaptureBody(BrowserCapturePayload),

    // ---- Meeting live broadcast (unsolicited push, correlation_id=0) ----
    // Pushowany z writer task w ws_binary po każdym sukcesie
    // `persist_meeting_event`. Filtr ownership (owner_user_id) stosowany
    // server-side — frame wychodzi tylko do właściciela sesji.
    MeetingLiveEventBody(crate::types::MeetingLiveEvent),

    // ---- Deployments (single-variant, req+res+stream w inner enum) ----
    DeploymentBody(DeploymentPayload),

    // ---- Services view (single-slot, every req+res packed into ServicePayload).
    // Powers the GUI Services tab + chat model picker. Multi-node aggregation
    // is handled in a later step (Krok N5) — N2 returns local-only data.
    ServiceBody(ServicePayload),

    // ---- System events (single-variant, push-only unsolicited w inner enum) ----
    // Oszczedza sloty variantowe — dla wszystkich server-push eventow systemowych
    // (service status, mesh peer status, deployment progress summary itd.).
    SystemEventBody(SystemEventPayload),

    // ---- Translate (LLM-backed) ----
    TranslateBody(TranslatePayload),

    // ---- Users list (Admin) ----
    // UsersList* consolidated into IamBody (below) jako ReqListUsers/ResListUsers.
    IamBody(IamPayload),

    // ---- Multi-source profiling (single-variant, req+res w inner enum) ----
    // 9 par request/response w jednym slocie — rkyv 0.8 ma twardy limit 256
    // wariantow MessageBody, wiec wszystkie wiadomosci profiling pakujemy do
    // jednego `ProfilingPayload`.
    ProfilingBody(crate::profiling::ProfilingPayload),

    // ---- Vision inference (single-slot, req+res w inner enum) ----
    // Slot odzyskany przez konsolidacje PiiRuleListRequest/Response do
    // PiiRuleBody. Patrz ProfilingBody jako wzor inner-enum pack.
    VisionBody(crate::vision::VisionInferPayload),

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
            model_name: "llama-3.2-1b-instruct".to_string(),
            display_name: "meta-llama/Llama-3.2-1B-Instruct".to_string(),
            category: "llm".to_string(),
            engine_id: "llama-cpp".to_string(),
            service_id: 1,
            node_id: "test-local-node".to_string(),
            availability: "ready".to_string(),
            transport: "http_direct".to_string(),
            endpoint_url: Some("http://127.0.0.1:8080".to_string()),
            capabilities: vec!["chat".to_string()],
            context_length: Some(4096),
            quantization: None,
            is_default: true,
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
        // Truncate aggressively (first quarter) zeby na pewno odciac rkyv
        // root pointer — half-bytes po RAG-removal cleanup'ie jest na tyle
        // krotki ze przypadkowo parsuje sie jako valid prefix dla maléjszego
        // payloadu. 1/4 jest gwarantowanie nizej niz pointer table.
        let quarter = &bytes[..bytes.len() / 4];
        assert!(rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(quarter).is_err());
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
        let evt = MessageBody::MeshTrustEventBody(MeshTrustEventPayload::Revoked(
            MeshTrustRevokedEvent {
                revoked_node_id: [0xAAu8; 32],
                reason: "key compromise detected".to_string(),
                revoked_at_epoch: 1_700_500_000,
            },
        ));
        assert_eq!(round_trip(evt.clone()), evt);
    }

    #[test]
    fn mesh_trusted_keys_sync_round_trip() {
        let evt = MessageBody::MeshTrustEventBody(MeshTrustEventPayload::KeysSync(
            MeshTrustedKeysSyncEvent {
                trusted_keys: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
                epoch: 42,
            },
        ));
        assert_eq!(round_trip(evt.clone()), evt);
    }

    #[test]
    fn profiling_body_round_trip() {
        use crate::profiling::{
            GpuTargets, ProfileScope, ProfileSourceFlags, ProfileTarget, ProfilingPayload,
            ProfilingStartRequest,
        };
        let body = MessageBody::ProfilingBody(ProfilingPayload::StartRequest(
            ProfilingStartRequest {
                node_id: "node-x".into(),
                scope: ProfileScope {
                    sources: ProfileSourceFlags(
                        ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::GPU,
                    ),
                    gpu_targets: GpuTargets::All,
                    cpu_sampling_hz: 99,
                    target: ProfileTarget::SystemWide,
                    duration_seconds: 30,
                    label: "deep-profile".into(),
                },
                label: "deep-profile".into(),
                elevation_password: String::new(),
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn consolidated_trust_event_payload_round_trip() {
        let revoked = MeshTrustEventPayload::Revoked(MeshTrustRevokedEvent {
            revoked_node_id: [0x11u8; 32],
            reason: "replay attack".into(),
            revoked_at_epoch: 1_700_600_000,
        });
        let sync = MeshTrustEventPayload::KeysSync(MeshTrustedKeysSyncEvent {
            trusted_keys: vec![[7u8; 32]],
            epoch: 9,
        });
        for payload in [revoked, sync] {
            let body = MessageBody::MeshTrustEventBody(payload);
            assert_eq!(round_trip(body.clone()), body);
        }
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
