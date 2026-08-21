// =============================================================================
// Plik: message_body.rs
// Opis: Bootstrap 10 wariantow MessageBody (bootstrap). MessageBody to tresc
//       envelope'u — CBOR-serializowana osobno i trzymana jako Vec<u8> w polu
//       Envelope.body. Dzieki temu policy check dziala na envelope bez tykania
//       body, a dispatcher decoduje dopiero po przejsciu auth.
// Przyklad:
//   let body = MessageBody::ModelListRequest;
//   let body_bytes = CBOR::to_bytes::<CBOR::rancor::Error>(&body)?.to_vec();
//   let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// Pomocnicze typy (bootstrap — docelowo rozpisane per-archetype)
// =============================================================================

/// Wide model view sourced from `model_registry` joined with the parent
/// `services` row. Returned by `ModelListRequest` so the chat picker can
/// disambiguate duplicates and the catalog can show transport/endpoint.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
// surface is packed into `ServicePayload` to keep the 256-variant CBOR limit
// on `MessageBody` (same trick as `DeploymentPayload` / `MeetingPayload`).
// =============================================================================

/// Single model row attached to a `ServiceInfo`.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceModelEntry {
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: Vec<String>,
    pub context_length: Option<u32>,
    pub quantization: Option<String>,
    pub is_default: bool,
    /// Powierzchnie usługi (`chat`/`documents`/`embeddings`/…) policzone przez
    /// anonsujący node z JEGO manifestu (`effective_service_surfaces`). Peer
    /// może NIE mieć manifestu tego silnika (np. zdalny model na innym zestawie
    /// kontenerów), a `category` typu `vision` nie mapuje się na ServiceSurface —
    /// dlatego anonsujemy surfaces WPROST, by zdalny model był resolwowalny
    /// (np. nemotron-parse: category=vision, surface=chat). `#[serde(default)]`
    /// zachowuje kompat ze starszymi peerami, którzy tego pola nie wysyłają.
    #[serde(default)]
    pub service_surfaces: Vec<String>,
}

/// Runtime view of one deployed service. Aggregates the `services` row with
/// its attached `model_registry` rows. Niesie tez `request_time_parameters`
/// — typed mape wartosci ktore BackendClient materializuje przy kazdym
/// requestcie (Ollama options, python wrapper extra fields, whisper/mlx
/// deploy defaults z opcjonalnym per-request override).
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
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
    /// docker / native_embedded / native_binary / native_python_bundle / native_managed_cli / external.
    pub deploy_method: String,
    /// embedded / http_direct / sidecar_quic / external_http / agent_rpc.
    pub transport: String,
    /// deploying / starting / running / degraded / failed / stopped / interrupted.
    pub status: String,
    pub pinned: bool,
    pub paused: bool,
    pub runtime_pid: Option<i64>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub endpoint_url: Option<String>,
    pub restart_count: u32,
    pub health_last_err: Option<String>,
    pub active_deploy_id: String,
    pub last_deploy_id: String,
    pub deployment_progress_pct: i32,
    /// Krótki user-friendly opis aktualnej fazy startu (np.
    /// "warming up — alive 30s, waiting for /v1/models"). Aktualizowany
    /// przez supervisor heartbeat co 5s podczas Starting. Frontend
    /// pokazuje obok status chipa, zeby user widzial PROGRES (vLLM
    /// cold start ~3 min). NULL gdy serwis Running albo nic do
    /// raportowania.
    pub progress_message: Option<String>,
    #[serde(default)]
    pub usage_json: Option<String>,
    #[serde(default)]
    pub usage_updated_at: Option<String>,
    pub models: Vec<ServiceModelEntry>,
    /// True gdy hash drzewa źródeł bundla zapisany przy deployu różni się od
    /// aktualnego hashu z manifestu — Core wykrył nowszą wersję wbudowanego
    /// bundla (docker/native). `#[serde(default)]` zachowuje kompatybilnosc ze
    /// starszymi peerami mesh, ktorzy tego pola nie wysylaja.
    #[serde(default)]
    pub update_available: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Typed request-time parameters z `services.config_json.parameters`,
    /// propagowane do BackendClient przez handles_cache. Puste mapy gdy
    /// service nie ma konfigurowalnych parametrow.
    pub request_time_parameters: RequestTimeParameters,
    /// Karty GPU, na ktorych dziala serwis (z deploy configu `gpu_select_mode` +
    /// `gpu_ids`): `"all"` = wszystkie widoczne, `"0,1"` = konkretne indeksy,
    /// `"CPU"` = bez GPU, `""` = nieznane/nie dotyczy. `#[serde(default)]` dla
    /// kompatybilnosci ze starszymi peerami mesh.
    #[serde(default)]
    pub gpu_selection: String,
    /// Gdy niepuste: ten wiersz jest czlonkiem distributed-deploymentu klastra
    /// (head/worker kontenera TP) i niesie `deployment_cluster_id` calego
    /// klastra. GUI uzywa go do skierowania akcji stop/usun na CALY klaster
    /// (`ClusterDeployStopRequest`) zamiast kasowac pojedynczy rank. Puste dla
    /// zwyklych serwisow. `#[serde(default)]` dla kompatybilnosci mesh.
    #[serde(default)]
    pub cluster_deployment_id: String,
}

/// Wartosci parametrow konsumowane przy kazdym requestcie do silnika.
/// Per-target storage:
///   * `ollama_options` → klucz=wartosc dla Ollama API `options` mapy w
///     POST `/api/generate`/`/api/chat`.
///   * `python_request` → pola POST body dla generic Python wrapperow
///     (qwen-asr, kyutai-tts, xtts, voxcpm).
///   * `whisper_overridable` → deploy defaults dla whisper z
///     `request_override = true`; backend uzywa jako baseline, klient API
///     moze nadpisac per request.
///   * `mlx_overridable` → analogicznie dla MLX engine.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestTimeParameters {
    pub ollama_options: Vec<KeyValue>,
    pub python_request: Vec<KeyValue>,
    pub whisper_overridable: Vec<KeyValue>,
    pub mlx_overridable: Vec<KeyValue>,
}

/// Generic key-value pair dla typed parametrow propagowanych przez wire.
/// Wartosc jako serialized JSON string (CBOR nie obsluguje natywnie
/// `serde_json::Value`).
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct KeyValue {
    pub key: String,
    /// JSON-serialized value. Konsument deserializuje przez `serde_json::from_str`.
    pub value_json: String,
}

/// Incremental change applied to one entry in the mesh services registry. Used
/// by `MeshServicesUpdate` push messages so peers do not have to re-broadcast
/// the full snapshot on every deploy / stop / pin / pause / rename / delete.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum ServiceChange {
    Added(ServiceInfo),
    Updated(ServiceInfo),
    Removed { service_id: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceListRequest {
    /// Reserved for future filtering (engine / category). Empty vec = no filter.
    pub engine_id_filter: Option<String>,
    pub category_filter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceListResponse {
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceDeleteRequest {
    pub service_id: i64,
    /// Target mesh node. `None` (or local node id) = run locally; otherwise
    /// the dispatcher forwards the action to the named peer over mesh and
    /// waits for the response. `service_id` always lives in the target node's
    /// SQLite namespace.
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceDeleteResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServicePinRequest {
    pub service_id: i64,
    pub pinned: bool,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServicePinResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceStartRequest {
    pub service_id: i64,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceStartResponse {
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServicePauseRequest {
    pub service_id: i64,
    pub paused: bool,
    /// See `ServiceDeleteRequest::node_id`.
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceUpdateResponse {
    pub success: bool,
    pub error: Option<String>,
    /// `true` jeśli serwis został restartowany w ramach tej operacji.
    pub restarted: bool,
}

/// Snapshot aktualnego zajęcia VRAM per GPU + lista zewnętrznych procesów.
/// Klient wywołuje co 2s podczas modal Edit / wizard Advanced step żeby
/// pokazać user'owi "co już używa GPU" + zalecony `gpu_memory_utilization`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceVramHintResponse {
    pub gpus: Vec<GpuVramSnapshot>,
    /// Sugerowane `gpu_memory_utilization` z uwzględnieniem external
    /// processes. Wzór: `(free_mib - desktop_reserve_mib) / total_mib`,
    /// clamp [0.10..0.95]. Desktop reserve = 1024 MiB (bezpieczne dla
    /// X11/Wayland compositor + headroom).
    pub recommended_utilization: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct GpuVramSnapshot {
    pub gpu_index: u32,
    pub gpu_name: String,
    pub total_mib: u64,
    pub free_mib: u64,
    pub used_mib: u64,
    pub external_processes: Vec<GpuProcessInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct GpuProcessInfo {
    pub pid: u32,
    pub process_name: String,
    pub used_mib: u64,
}

/// Lista presetów modelu z manifestu silnika. Edit modal wywołuje to po
/// zmianie dropdown'a "Preset z manifestu" — backend zwraca dokładnie te
/// `[[model_preset]]` które są zadeklarowane w `<engine>.toml` (single
/// source of truth, build.rs generuje z TOML do `services_generated.rs`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceEnginePresetsRequest {
    pub engine_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceEnginePresetsResponse {
    pub presets: Vec<ServicePresetInfo>,
}

/// Pojedynczy preset z manifestu — frontend renderuje jako preset-card
/// w Edit modal lub deploy wizard. `repo` to HF repository, `quantization`
/// pochodzi z manifestu (auto/awq/gptq/nvfp4/...). Pełen VRAM estimate
/// liczony jest osobno przez `DeployVllmRecommendRequest` po wyborze.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServicePresetInfo {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    pub quantization: Option<String>,
    pub recommended: bool,
}

/// Request: list the LIVE model catalog of a deployed external provider service
/// (fetched from the provider API). `node_id` None/local = run here, otherwise
/// the dispatcher forwards to the owning peer.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceModelCatalogRequest {
    pub service_id: i64,
    pub node_id: Option<String>,
}

/// One model offered by the provider, with whether it is already selected
/// (present in this service's model_registry).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceModelCatalogEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub modality: String,
    pub context_length: Option<u32>,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceModelCatalogResponse {
    pub models: Vec<ServiceModelCatalogEntry>,
    pub error: Option<String>,
}

/// Request: persist the admin's model selection for a service — model_registry
/// is upserted to exactly this set (rows inserted/removed to match).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceModelSelectionRequest {
    pub service_id: i64,
    pub node_id: Option<String>,
    pub selected_model_ids: Vec<String>,
    /// Optional per-model pricing for the selected external models. Each entry
    /// is matched to a selected model by `model_id`; entries for models not in
    /// `selected_model_ids` are ignored by the handler.
    #[serde(default)]
    pub pricing: Vec<ModelPricingInput>,
}

/// Per-model pricing supplied when selecting external models. `model_id` MUST
/// equal the selected model id so metrics (`requested_model()`) line up.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelPricingInput {
    pub model_id: String,
    #[serde(default)]
    pub prompt_per_1k: Option<f64>,
    #[serde(default)]
    pub completion_per_1k: Option<f64>,
    #[serde(default)]
    pub audio_per_min: Option<f64>,
    #[serde(default)]
    pub image_each: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceModelSelectionResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// Request: begin a subscription OAuth login (browser PKCE) on the named node.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceOauthStartRequest {
    pub provider: String,
    pub node_id: Option<String>,
}

/// Response: the URL to open in the browser plus a flow id to poll.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceOauthStartResponse {
    pub flow_id: String,
    /// Page the user opens in any browser to authorise the device-code login.
    pub authorize_url: String,
    /// One-time code the user enters on `authorize_url`.
    pub user_code: String,
    pub error: Option<String>,
}

/// Request: poll a login flow's status.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceOauthPollRequest {
    pub flow_id: String,
    pub node_id: Option<String>,
}

/// Response: `status` is "pending" | "done" | "error".
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceOauthPollResponse {
    pub status: String,
    pub account_label: Option<String>,
    pub error: Option<String>,
}

/// Request routed to a deployed Codex or Claude Code CLI bridge.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceAgentRequest {
    pub service_id: i64,
    pub node_id: Option<String>,
    /// Stable bridge operation name, for example `auth.status` or `session.turn`.
    pub operation: String,
    /// Operation-specific JSON. The owner validates it before contacting the bridge.
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceAgentResponse {
    pub success: bool,
    pub result_json: String,
    pub error: Option<String>,
}

/// Inner enum bundling every services-screen RPC pair into a single MessageBody
/// slot — `MessageBody::ServiceBody`. Pattern mirrors `DeploymentPayload`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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
    ReqModelCatalog(ServiceModelCatalogRequest),
    ResModelCatalog(ServiceModelCatalogResponse),
    ReqModelSelection(ServiceModelSelectionRequest),
    ResModelSelection(ServiceModelSelectionResponse),
    ReqOauthStart(ServiceOauthStartRequest),
    ResOauthStart(ServiceOauthStartResponse),
    ReqOauthPoll(ServiceOauthPollRequest),
    ResOauthPoll(ServiceOauthPollResponse),
    ReqAgent(ServiceAgentRequest),
    ResAgent(ServiceAgentResponse),
}

// =============================================================================
// Kody bledu protokolu
// =============================================================================

/// Ustabilizowane kody bledu dla `ProtocolError.code`. Dodatkowe (numeryczne)
/// mozna zawsze dorzucic — klient powinien obslugiwac nieznane graceful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ApiKeySummary {
    pub key_id: String,
    pub name: String,
    pub created_at_epoch: u64,
    pub last_used_at_epoch: Option<u64>,
    /// 'user' | 'group' | 'general'.
    pub key_type: String,
    /// user_id (user) / group_id (group) / None (general).
    pub subject_id: Option<String>,
    /// Human label for the subject (user display name / group name), if any.
    pub subject_label: Option<String>,
    /// Count of `resource_permissions` rows scoped to this key (general keys).
    pub scope_count: u32,
    pub is_active: bool,
}

/// One resource entry on a general key's explicit allowlist at creation time.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ResourceRef {
    /// 'model' | 'flow' | 'alias'.
    pub resource_type: String,
    pub resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ApiKeyCreateRequest {
    pub name: String,
    /// 'user' | 'group' | 'general'.
    pub key_type: String,
    /// Required for 'user'/'group' (the user/group id); None for 'general'.
    pub subject_id: Option<String>,
    /// Explicit allowlist seeded for 'general' keys; ignored for user/group.
    pub scope_resources: Vec<ResourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ApiKeyCreateResponse {
    pub key_id: String,
    /// Pelny token (widoczny TYLKO raz, w odpowiedzi na creation).
    pub token: String,
}

// =============================================================================
// Auth (W-ACTION + R-ONE archetypes, migration-map #40-#42)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuthLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuthLoginResponse {
    pub jwt: String,
    pub user_id: [u8; 16],
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuthMeResponse {
    pub user_id: [u8; 16],
    pub username: String,
    pub role: String,
}

// =============================================================================
// Me / User preferences (preferowany jezyk dla TTS itd.)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MePreferencesGetRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MePreferencesGetResponse {
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MePreferencesUpdateRequest {
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MePreferencesUpdateResponse {
    pub language: Option<String>,
}

// =============================================================================
// Chat streaming (R-STREAM archetyp, migration-map #43)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ChatMessage {
    /// "system" / "user" / "assistant" / "tool".
    pub role: String,
    pub content: String,
    /// Reasoning content of a reasoning model, carried through the chat path so
    /// a replayed assistant turn keeps it. Optional and skipped when absent, so
    /// a peer that predates the field still decodes our frames and we still
    /// decode theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ChatStreamRequest {
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Flow wybrany przez usera w UI czatu — gdy ustawiony, backend odpala
    /// KONKRETNY flow po ID zamiast syntetycznego. Brak = syntetyczny
    /// "Default Chat". `#[serde(default)]` zachowuje kompatybilnosc ze
    /// starszymi peerami.
    #[serde(default)]
    pub flow_id: Option<String>,
    /// Konwersacja UI = sesja flow. Bez tego węzły `conversation_history` /
    /// `memory` w wybranym flow nie mają klucza sesji i twardo failują
    /// ("no session_id"). `#[serde(default)]` — starsi peerzy wysyłają None.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ChatStreamChunk {
    /// Partial token/fragment od modelu.
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ChatStreamEnd {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Pelny zakumulowany tekst odpowiedzi (suma wszystkich delt). Front
    /// uzywa go gdy zlozone delty sa puste (np. zgubione chunki).
    /// `#[serde(default)]` zachowuje kompatybilnosc ze starszymi peerami.
    #[serde(default)]
    pub text: Option<String>,
    /// Per-message metryki wydajnosci inferencji. `#[serde(default)]` zachowuje
    /// kompatybilnosc ze starszymi peerami (0 gdy nieznane).
    #[serde(default)]
    pub ttft_ms: u32,
    #[serde(default)]
    pub prefill_tps: f32,
    #[serde(default)]
    pub decode_tps: f32,
    #[serde(default)]
    pub total_ms: u32,
}

// =============================================================================
// Universal multimodal flow invoke — most binarny klient → flow engine. Niesie
// ZBIÓR typowanych wejść (audio, tekst, pliki…); flow odpowiada strumieniem
// przeplatanych chunków (tekst + audio + …). Zastępuje REST /v1/audio/* dla
// dashboardu. Bajty inline (hybryda: małe inline, duże przez blob upload — TODO).
// =============================================================================

/// Jedno typowane wejście do flow. Pierwsze niepuste staje się payloadem
/// FlowEnvelope, kolejne artefaktami `input_{n}`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum FlowInputValue {
    Text(String),
    /// Serializowany JSON (string).
    Json(String),
    Audio {
        mime: String,
        sample_rate: Option<u32>,
        bytes: Vec<u8>,
    },
    Image {
        mime: String,
        bytes: Vec<u8>,
    },
    Video {
        mime: String,
        bytes: Vec<u8>,
    },
    File {
        mime: String,
        filename: Option<String>,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowInvokeRequest {
    /// Gdy ustawione — odpal KONKRETNY flow po ID (wybrany przez usera). Ma
    /// priorytet nad model/service_type (np. audio chat uruchamia wybrany flow).
    pub flow_id: Option<String>,
    /// Nazwa modelu — używana do rozwiązania flow przez model/service_type gdy
    /// `flow_id` nie jest podany.
    pub model: String,
    /// Service type dla rozwiązania flow gdy brak `flow_id`: "chat"/"tts"/"stt".
    pub service_type: String,
    pub inputs: Vec<FlowInputValue>,
    /// Język (transkrypcja/TTS) → envelope.meta.
    pub language: Option<String>,
    pub session_id: Option<String>,
    /// `envelope.meta["output_audio"]`: `true` = text + synthesized audio,
    /// `false` (default) = text only, the flow's `tts` node passes text through.
    #[serde(default)]
    pub output_audio: bool,
    /// Seeds `envelope.meta["stt_model"]` when the flow's `stt` node has no
    /// pinned model.
    #[serde(default)]
    pub stt_model: Option<String>,
    /// Seeds `envelope.meta["tts_model"]` when the flow's `tts` node has no
    /// pinned model.
    #[serde(default)]
    pub tts_model: Option<String>,
}

/// Pojedynczy chunk odpowiedzi flow — odwzorowanie `EnvelopeDelta`. Klient
/// składa tekst do bąbla, audio do kolejki odtwarzania, media do renderu.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum FlowInvokeChunk {
    Text {
        choice_index: u32,
        delta: String,
    },
    Audio {
        choice_index: u32,
        mime: String,
        sample_rate: Option<u32>,
        bytes: Vec<u8>,
    },
    Image {
        mime: String,
        bytes: Vec<u8>,
    },
    Video {
        mime: String,
        bytes: Vec<u8>,
    },
    File {
        mime: String,
        filename: Option<String>,
        bytes: Vec<u8>,
    },
    /// Transcript of the audio input (`stt` node), emitted once before the
    /// first text/audio chunk so the client can render the user's utterance.
    Transcript {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowInvokeEnd {
    pub finish_reason: String,
    pub error: Option<String>,
    /// Pelny zakumulowany tekst odpowiedzi. Front nadpisuje nim wiadomosc, bo
    /// delty streamu bywaja ucinane gdy audio leci dluzej niz tekst.
    /// `#[serde(default)]` zachowuje kompatybilnosc ze starszymi peerami.
    #[serde(default)]
    pub text: Option<String>,
}

// =============================================================================
// Models — szczegoly modelu (R-ONE), instalacja/odinstalacja (W-ACTION)
// migration-map #218-#227
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelInstallRequest {
    pub model_id: String,
    /// Repozytorium HuggingFace (np. "Qwen/Qwen3.5-0.8B").
    pub source_repo: String,
}

// =============================================================================
// Hub — HuggingFace integration (R-LIST + R-STREAM dla download progress)
// migration-map #81-#86
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct HubEngineSummary {
    pub id: String,
    pub display_name: String,
    pub category: String,
    /// "docker" | "native" | "external".
    pub deploy_methods: Vec<String>,
    pub default_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct HubModelSearchResult {
    pub repo_id: String,
    pub display_name: String,
    pub author: String,
    /// Liczba downloadow w HuggingFace (popularity signal).
    pub downloads: u64,
    pub likes: u64,
    pub last_modified_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at_epoch: u64,
    pub updated_at_epoch: u64,
    pub enabled: bool,
    /// `flows.is_default` — the chat UI preselects this flow on first load
    /// instead of the synthetic no-flow chat.
    #[serde(default)]
    pub is_default: bool,
    /// When set, the flow is exposed as a model under this id (`/v1/models`,
    /// catalog). It is the name external clients call and the resource id the
    /// access-key wizard must grant — the flow's own UUID is not callable.
    #[serde(default)]
    pub published_model_name: Option<String>,
    /// `flows.is_system` — platform-seeded flow; the server rejects user
    /// edit/delete/status changes, so the UI can hide those actions.
    #[serde(default)]
    pub is_system: bool,
    /// Factory flow (`db::seed::FACTORY_FLOW_IDS`): editable but never
    /// deletable, restorable to its canonical graph via
    /// `FlowFactoryRestoreRequest`.
    #[serde(default)]
    pub is_factory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowDetail {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// JSON DAG definition (zachowane jako string — parsowane przez flow_engine).
    pub graph_json: String,
    pub enabled: bool,
    /// Raw flow status column: "active" | "draft" | "decoded" itp.
    pub status: String,
    /// `flows.is_system` — platform-seeded flow; the server rejects user
    /// edit/delete/status changes, so the UI can hide those actions.
    #[serde(default)]
    pub is_system: bool,
    /// Factory flow (`db::seed::FACTORY_FLOW_IDS`): editable but never
    /// deletable, restorable to its canonical graph via
    /// `FlowFactoryRestoreRequest`.
    #[serde(default)]
    pub is_factory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct PromptSummary {
    pub id: String,
    pub name: String,
    pub category: String,
    pub updated_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogFilters {
    pub user_id: Option<String>,
    pub addon_id: Option<String>,
    pub action: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub search: Option<String>,
}

/// Single audit log row as returned to the Admin screen.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub action: String,
    pub user_id: Option<String>,
    pub addon_id: Option<String>,
    pub resource: Option<String>,
    pub details: Option<String>,
    pub ip_address: Option<String>,
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogListRequest {
    pub filters: AuditLogFilters,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogListResponse {
    pub entries: Vec<AuditLogEntry>,
    pub total_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogExportRequest {
    pub filters: AuditLogFilters,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogExportResponse {
    pub csv: String,
    pub row_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogCleanupRequest {
    pub keep_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AuditLogCleanupResponse {
    pub deleted_count: u64,
}

// ----- Scheduler screen (Admin only) -----

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobsListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobsListResponse {
    pub jobs_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerActionsListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerActionsListResponse {
    pub actions_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerRunsListRequest {
    pub job_id: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerRunsListResponse {
    pub runs_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobUpsertRequest {
    pub job_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobUpsertResponse {
    pub job_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobDeleteRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobDeleteResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobRunNowRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SchedulerJobRunNowResponse {
    pub run_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SchedulerPayload {
    JobsListRequest(SchedulerJobsListRequest),
    JobsListResponse(SchedulerJobsListResponse),
    ActionsListRequest(SchedulerActionsListRequest),
    ActionsListResponse(SchedulerActionsListResponse),
    RunsListRequest(SchedulerRunsListRequest),
    RunsListResponse(SchedulerRunsListResponse),
    JobUpsertRequest(SchedulerJobUpsertRequest),
    JobUpsertResponse(SchedulerJobUpsertResponse),
    JobDeleteRequest(SchedulerJobDeleteRequest),
    JobDeleteResponse(SchedulerJobDeleteResponse),
    JobRunNowRequest(SchedulerJobRunNowRequest),
    JobRunNowResponse(SchedulerJobRunNowResponse),
}

// ----- ML Studio screen (UserSession) -----

/// One ML Studio project type with a stable machine slug and a Polish UI label.
/// The slug is what flows/handlers branch on; the label is what the wizard
/// shows. The types are fixed by the product (recognition, ft_llm,
/// ft_vision_audio, tabular_anomaly, distillation).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectTypeInfo {
    pub slug: String,
    pub label: String,
    pub description: String,
}

/// Compact project row for the projects list screen (`p00-projekty.html`):
/// identity, type/status badges and the model-count KPI.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectSummary {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub project_type: String,
    pub status: String,
    pub dataset_count: u32,
    pub model_count: u32,
    pub training_count: u32,
    /// Role of the requesting user in this project (`owner`/`editor`/`viewer`).
    pub role: String,
    /// Convenience flag for the UI: the requesting user owns this project.
    pub is_owner: bool,
    pub created_at: String,
    pub updated_at: String,
    // Live training KPIs (progress/loss/ETA) come from training_runs in later slices
}

/// Full project record returned by the detail screen.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectDetail {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub project_type: String,
    pub status: String,
    pub owner_user_id: String,
    pub org_id: String,
    pub dataset_count: u32,
    pub model_count: u32,
    pub training_count: u32,
    /// Role of the requesting user in this project (`owner`/`editor`/`viewer`).
    pub role: String,
    /// Convenience flag for the UI: the requesting user owns this project.
    pub is_owner: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectsListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectsListResponse {
    pub projects: Vec<MlStudioProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectCreateRequest {
    pub name: String,
    pub description: String,
    pub project_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectCreateResponse {
    pub project: MlStudioProjectDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectDetailRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectDetailResponse {
    pub project: MlStudioProjectDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectTypesListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectTypesListResponse {
    pub types: Vec<MlStudioProjectTypeInfo>,
}

/// One project membership row for the sharing screen (`p02-udostepnianie.html`):
/// who is a member, with what role and whether their invitation is still pending.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMember {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub status: String,
    pub invited_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMembersListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMembersListResponse {
    pub members: Vec<MlStudioProjectMember>,
}

/// One training-run row for the project overview tab (`Przegląd`). Mirrors the
/// `training_runs` table; `model_id`/`started_at`/`finished_at` are NULL until the
/// run produces a model or transitions state.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTrainingRunSummary {
    pub run_id: String,
    pub model_id: Option<String>,
    pub status: String,
    pub config_json: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTrainingRunsListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTrainingRunsListResponse {
    pub runs: Vec<MlStudioTrainingRunSummary>,
}

/// Statystyki GPU odczytane z `nvidia-smi` (pierwsza karta). Gdy `nvidia-smi`
/// niedostępny — wszystkie pola zerowe, `name` puste (bez błędu handlera).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct GpuStats {
    pub name: String,
    pub mem_used_mb: i32,
    pub mem_total_mb: i32,
    pub util_pct: i32,
}

/// Jeden aktywny (running/pending) job treningowy do panelu jobów ML Studio.
/// Łączy dane runu z bazy (run_id/project/kind/variant/status) z polami live-view
/// z serwisu treningowego (epoch/total_epochs/eta_s/elapsed_s/gpu_mem_mb/stage).
/// Pola live-view są tolerancyjne: gdy serwis ich nie zwraca → 0/"".
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct TrainingJobInfo {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub kind: String,
    pub variant: String,
    pub status: String,
    pub epoch: i32,
    pub total_epochs: i32,
    pub eta_s: f32,
    pub elapsed_s: f32,
    pub gpu_mem_mb: f32,
    pub stage: String,
    pub started_at: String,
}

/// Żądanie przeglądu wszystkich aktywnych jobów treningowych widocznych dla
/// użytkownika (projekty, których jest członkiem). Bez parametrów.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioJobsOverviewRequest {}

/// Odpowiedź panelu jobów: lista aktywnych jobów + zbiorcze statystyki GPU węzła.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioJobsOverviewResponse {
    pub jobs: Vec<TrainingJobInfo>,
    pub gpu: GpuStats,
}

/// One model row for the project overview tab. Mirrors the `models` table;
/// `metrics_json` carries the serialized metric snapshot for the model card.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioModelSummary {
    pub model_id: String,
    pub name: String,
    pub framework: String,
    pub base_model: String,
    pub status: String,
    pub metrics_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioModelsListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioModelsListResponse {
    pub models: Vec<MlStudioModelSummary>,
}

/// Member-accessible view of the resource grants allocated to one project,
/// reusing `MlStudioResourceGrant`. The admin-wide `ResourceGrantsList` stays
/// Admin-only; this one is gated by project membership.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectGrantsListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectGrantsListResponse {
    pub grants: Vec<MlStudioResourceGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectInviteRequest {
    pub project_id: String,
    pub invitee_user_id: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectInviteResponse {
    pub member: MlStudioProjectMember,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMemberRemoveRequest {
    pub project_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMemberRemoveResponse {
    pub project_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMemberRoleSetRequest {
    pub project_id: String,
    pub user_id: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectMemberRoleSetResponse {
    pub member: MlStudioProjectMember,
}

/// One distinct value of a categorical column with its row count. Mirrors
/// `ml_studio::profile::ClassCount`; feeds the "wykryto N klas" UI list.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClassCount {
    pub value: String,
    pub count: u64,
}

/// Profile of one dataset column. `column_type` is a stable slug
/// (`categorical`/`integer`/`float`/`date`/`text`). `classes` is non-empty only
/// for small categorical columns. Mirrors `ml_studio::profile::ColumnProfile`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ColumnProfile {
    pub name: String,
    pub column_type: String,
    pub unique_count: u64,
    pub missing_ratio: f64,
    pub examples: Vec<String>,
    pub classes: Vec<ClassCount>,
    pub unique_capped: bool,
}

/// Full profile of an uploaded table. Mirrors `ml_studio::profile::TableProfile`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct TableProfile {
    pub format: String,
    pub row_count: u64,
    pub scanned_rows: u64,
    pub column_count: u32,
    pub columns: Vec<ColumnProfile>,
    pub truncated: bool,
}

/// Compact dataset row for the project data screen (`t-dane`): identity, source
/// kind and the row/column KPIs. The full per-column profile is fetched
/// separately via `DatasetProfileRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DatasetSummary {
    pub dataset_id: String,
    pub project_id: String,
    pub name: String,
    pub kind: String,
    pub row_count: u64,
    pub column_count: u32,
    pub created_at: String,
    /// Profil datasetu (JSON): dla COCO niesie `classes`/`splits`/`image_count`,
    /// dla tabel `TableProfile`. UI recognition czyta z niego listę klas.
    #[serde(default)]
    pub profile_json: String,
}

/// Upload a tabular file (CSV/XLSX) into a project for profiling. `bytes` is the
/// raw file content carried inline in the CBOR body (no multipart for the
/// dashboard); `filename` selects the parser by extension.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetUploadRequest {
    pub project_id: String,
    pub name: String,
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetUploadResponse {
    pub dataset: DatasetSummary,
}

/// Jeden fragment przesyłanego datasetu. Duże pliki (np. ZIP COCO, dataset SFT)
/// przekraczają limit pojedynczej ramki WS, więc klient dzieli plik na części o
/// numerach `seq` (0..total_chunks). Serwer akumuluje fragmenty po `upload_id` i
/// tworzy dataset dopiero po odebraniu ostatniego fragmentu.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetUploadChunkRequest {
    pub project_id: String,
    pub name: String,
    pub filename: String,
    pub upload_id: String,
    pub seq: u32,
    pub total_chunks: u32,
    pub bytes: Vec<u8>,
}

/// Odpowiedź na fragment uploadu. Dla fragmentów pośrednich `dataset` jest `None`
/// i zwracamy postęp odebranych bajtów; po ostatnim fragmencie `dataset` zawiera
/// utworzony rekord.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetUploadChunkResponse {
    pub upload_id: String,
    pub received_chunks: u32,
    pub received_bytes: u64,
    pub dataset: Option<DatasetSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetsListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetsListResponse {
    pub datasets: Vec<DatasetSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetProfileRequest {
    pub dataset_id: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetProfileResponse {
    pub dataset: DatasetSummary,
    pub profile: TableProfile,
}

/// Podglad/edycja zawartosci datasetu. Wiersze to surowe linie JSONL (generyczne —
/// dziala dla {question,answer}, {prompt,chosen,rejected} i innych ksztaltow).
/// GUI parsuje/buduje JSON per wiersz; zapis nadpisuje raw_data datasetu.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetRowsRequest {
    pub dataset_id: String,
    /// Limit wierszy (0 = wszystkie); GUI moze paginowac dla duzych datasetow.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetRowsResponse {
    pub dataset_id: String,
    pub kind: String,
    pub total: u32,
    /// Surowe linie JSONL (do `total` albo do limitu).
    pub rows: Vec<String>,
    /// Pochodzenie (JSON `distill_meta` z profile_json): czym/jak wygenerowano —
    /// teacher, wariant, prompt, źródło pytań. None dla datasetów spoza destylacji.
    #[serde(default)]
    pub meta: Option<String>,
    /// Dataset w trakcie generacji (distill_status=pending) — GUI blokuje edycję/zapis.
    #[serde(default)]
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetRowsSaveRequest {
    pub dataset_id: String,
    /// Pelny zestaw wierszy (linie JSONL) — nadpisuje raw_data datasetu.
    pub rows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDatasetRowsSaveResponse {
    pub dataset_id: String,
    pub row_count: u32,
}

/// One admin-managed mesh resource grant (§11.3). A record of an allocation of
/// a node resource to a subject, not live usage. `subject_kind` ∈
/// {user, group, project}; `resource_kind` ∈ {gpu, cpu, ram}. `resource_ref`
/// names the card (e.g. GPU name/index) and is empty for cpu/ram; `quota` is
/// free-form text (GPU count, hours, or empty).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrant {
    pub grant_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub node_id: String,
    pub resource_kind: String,
    pub resource_ref: String,
    pub quota: String,
    pub granted_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantCreateRequest {
    pub subject_kind: String,
    pub subject_id: String,
    pub node_id: String,
    pub resource_kind: String,
    pub resource_ref: String,
    pub quota: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantCreateResponse {
    pub grant: MlStudioResourceGrant,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantsListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantsListResponse {
    pub grants: Vec<MlStudioResourceGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantRevokeRequest {
    pub grant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioResourceGrantRevokeResponse {
    pub grant_id: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectResourcesRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectResourcesResponse {
    pub grants: Vec<MlStudioResourceGrant>,
}

/// Request to train the tabular baseline: pick a `target_column` in a dataset
/// and a `task` (`classification`/`regression`); Core re-parses the dataset's
/// stored raw bytes and trains several pure-Rust models, returning a ranked
/// leaderboard. `project_id` scopes authorization (owner/editor membership).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTabularTrainRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub target_column: String,
    pub task: String,
    /// Wybór silnika treningu: `None`/`""`/`"rust"` → wbudowany silnik Rust
    /// (domyślny, kompatybilny wstecz); `"autogluon"` → zewnętrzny serwis HTTP
    /// AutoGluon. Pole na końcu structu, żeby starsi klienci dekodowali bez zmian.
    pub engine: Option<String>,
}

/// One leaderboard row returned by a tabular training run. Classification fills
/// `accuracy`/`f1_macro`; regression fills `rmse`/`r2`. `train_secs` is the
/// model's wall-clock training time.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTabularLeaderboardEntry {
    pub model_name: String,
    pub framework: String,
    pub accuracy: Option<f64>,
    pub f1_macro: Option<f64>,
    pub rmse: Option<f64>,
    pub r2: Option<f64>,
    pub train_secs: f64,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTabularTrainResponse {
    pub run_id: String,
    pub best_model_id: String,
    pub best_model_name: String,
    pub task: String,
    pub target_column: String,
    pub train_rows: u64,
    pub holdout_rows: u64,
    pub leaderboard: Vec<MlStudioTabularLeaderboardEntry>,
}

/// Hiperparametry asynchronicznego fine-tuningu LLM. Lustro pól, których
/// oczekuje serwis ml-training (`hyperparams{...}` w `POST /train`). Wartości
/// idą wprost do serwisu; Core ich nie waliduje poza zakresem typu.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtHyperparams {
    pub learning_rate: f64,
    pub batch_size: u32,
    pub grad_accum_steps: u32,
    pub epochs: u32,
    pub lora_r: u32,
    pub lora_alpha: u32,
    pub lora_dropout: f64,
    pub max_seq_len: u32,
}

/// Żądanie startu fine-tuningu LLM. Trening biegnie ASYNCHRONICZNIE w tle Core
/// (zob. `train_llm.rs`), więc odpowiedź wraca natychmiast z `run_id`, a UI
/// odpytuje postęp przez `MlStudioFtTrainStatusRequest`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtTrainStartRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub base_model: String,
    pub method: String,
    pub objective: String,
    /// Model-nauczyciel dla KD (objective=="kd"); None dla sft/dpo.
    #[serde(default)]
    pub teacher_model: Option<String>,
    pub hyperparams: MlStudioFtHyperparams,
    pub merge_adapter: bool,
    /// Węzeł docelowy treningu (mesh). Pusty/None → trening lokalny na tym węźle;
    /// inny node_id → zlecenie przez mesh (komenda MlTrainStart kind="llm").
    #[serde(default)]
    pub target_node_id: Option<String>,
    /// Liczba GPU na węźle treningowym (None → wszystkie dostępne). Multi-GPU DDP.
    #[serde(default)]
    pub num_gpus: Option<u32>,
    /// Konfiguracja treningu rozproszonego między węzłami (multi-rig). None →
    /// single-node (num_gpus decyduje o liczbie kart).
    #[serde(default)]
    pub dist: Option<MlStudioDistConfig>,
}

/// Konfiguracja treningu rozproszonego multi-node (multi-rig). Mapuje wprost na
/// argumenty `torchrun --nnodes/--node-rank/--rdzv-endpoint` po stronie serwisu.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistConfig {
    pub nnodes: u32,
    pub node_rank: u32,
    pub master_addr: String,
    pub master_port: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtTrainStartResponse {
    pub run_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtTrainStatusRequest {
    pub run_id: String,
}

/// Pojedynczy punkt krzywej straty (krok treningu) do wykresu w UI (f02).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLossPoint {
    pub step: u64,
    pub train_loss: Option<f64>,
    pub eval_loss: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtTrainStatusResponse {
    pub run_id: String,
    pub status: String,
    pub step: u64,
    pub total_steps: u64,
    pub train_loss: Option<f64>,
    pub eval_loss: Option<f64>,
    pub error: Option<String>,
    pub loss_curve: Vec<MlStudioLossPoint>,
    /// Faza transferu datasetu przez mesh (trening zdalny): "zipping"|"syncing"|
    /// "starting"; None gdy lokalnie lub po zmaterializowaniu. UI: pasek B/s.
    #[serde(default)]
    pub sync_phase: Option<String>,
    #[serde(default)]
    pub sync_bytes_sent: u64,
    #[serde(default)]
    pub sync_bytes_total: u64,
    #[serde(default)]
    pub sync_rate_bps: u64,
}

// ----- Recognition (RF-DETR detekcja obiektów) — trening na COCO -----

/// Rejestracja datasetu COCO przez ŚCIEŻKĘ do katalogu na serwerze (a nie
/// upload bajtów) — zbiory detekcji to dziesiątki/setki MB obrazów, ponad limit
/// ramki WS (~0.9 MB). Katalog musi mieć splity z `_annotations.coco.json`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogDatasetRegisterRequest {
    pub project_id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogDatasetRegisterResponse {
    pub dataset: DatasetSummary,
}

/// Stages ONE raw media file (image/video) server-side for a recognition
/// project, chunked over the WS frame limit (~0.9 MB). Reuses the chunked-upload
/// metadata shape (`upload_id`/`seq`/`total_chunks`); on the final chunk the
/// reassembled bytes are written to the project staging dir (NOT a dataset).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogStageMediaRequest {
    pub project_id: String,
    pub filename: String,
    pub upload_id: String,
    pub seq: u32,
    pub total_chunks: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogStageMediaResponse {
    pub upload_id: String,
    pub received_chunks: u32,
    pub received_bytes: u64,
    /// True once the whole file was reassembled and written to staging.
    pub staged: bool,
}

/// Builds a COCO `coco_path` dataset from every staged media file of the project
/// (copy images, decode HEIC, extract video frames at `fps`). Building runs
/// ASYNCHRONICZNIE w tle (HEIC/ffmpeg per plik trwają minuty); odpowiedź wraca
/// natychmiast z `build_id`, a UI odpytuje postęp przez
/// `MlStudioRecogBuildStatusRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogBuildDatasetRequest {
    pub project_id: String,
    pub dataset_name: String,
    pub fps: u32,
    /// Opcjonalna ścieżka katalogu na serwerze przeszukiwana REKURENCYJNIE jako
    /// źródło mediów (zamiast wgranych plików staging). Pusta/None → staging.
    pub source_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogBuildDatasetResponse {
    /// Identyfikator zadania budowy do pollingu statusu. Pusty gdy start odrzucono.
    pub build_id: String,
    /// "running" gdy zadanie wystartowało, "failed" gdy odrzucono (np. brak plików,
    /// inna budowa w toku, zły fps).
    pub status: String,
    pub error: Option<String>,
}

/// Polling postępu asynchronicznej budowy datasetu (po `build_id`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogBuildStatusRequest {
    pub build_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogBuildStatusResponse {
    pub build_id: String,
    /// "running" | "succeeded" | "failed".
    pub status: String,
    pub files_total: u64,
    pub files_done: u64,
    pub frames_extracted: u64,
    /// Zarejestrowany dataset (tylko gdy `status == "succeeded"`).
    pub dataset: Option<DatasetSummary>,
    pub image_count: u64,
    pub category_count: u32,
    pub error: Option<String>,
}

/// Auto-etykietowanie całego datasetu COCO wbudowanym detektorem RF-DETR (ADR):
/// dla każdego obrazu uruchamia detektor i zapisuje detekcje jako adnotacje COCO
/// (edytowalny punkt startowy). Praca jest CIĘŻKA (dekodowanie + inferencja per
/// obraz), więc job leci ASYNCHRONICZNIE: odpowiedź wraca natychmiast z `job_id`,
/// a UI odpytuje postęp przez `MlStudioRecogAutolabelStatusRequest`. `mode`:
/// "only_empty" (tylko obrazy bez adnotacji — ręczne poprawki nietknięte) lub
/// "overwrite" (zastąp wszystkie adnotacje).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogAutolabelRequest {
    pub dataset_id: String,
    pub threshold: f64,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogAutolabelResponse {
    /// Identyfikator zadania do pollingu statusu. Pusty gdy start odrzucono.
    pub job_id: String,
    /// "running" gdy zadanie wystartowało, "failed" gdy odrzucono.
    pub status: String,
    pub error: Option<String>,
}

/// Polling postępu asynchronicznego auto-etykietowania (po `job_id`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogAutolabelStatusRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogAutolabelStatusResponse {
    /// "running" | "succeeded" | "failed".
    pub status: String,
    pub images_total: u64,
    pub images_done: u64,
    /// Łączna liczba zapisanych detekcji.
    pub detections: u64,
    /// Detekcje pominięte, bo ich klasa nie występuje w kategoriach datasetu.
    /// Niezerowa wartość przy `detections == 0` sygnalizuje niedopasowany model.
    pub skipped_unknown: u64,
    pub error: Option<String>,
}

/// Detekcja na obrazie wytrenowanym modelem recognition. `image_b64` to małe
/// zdjęcie (limit ramki WS ~0.9MB). Odpowiedź niesie detekcje jako JSON
/// (`detections_json`: [{class_id,class_name,score,bbox_xyxy}]) — bez osobnych
/// struktur per-detekcja w warstwie wasm.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogDetectRequest {
    pub model_id: String,
    pub threshold: f64,
    pub image_b64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogDetectResponse {
    pub detections_json: String,
    pub width: u32,
    pub height: u32,
    pub error: Option<String>,
}

// ----- Recognition: edytor anotacji (galeria + edycja bboxów COCO) -----

/// Lista obrazów datasetu COCO do galerii anotacji. `images_json` =
/// [{image_id,file_name,split,width,height,ann_count}] (image_id syntetyczny
/// "split|coco_id"), `categories_json` = [{id,name}].
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImagesListRequest {
    pub dataset_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImagesListResponse {
    pub images_json: String,
    pub categories_json: String,
}

/// Pobranie jednego obrazu (przeskalowanego do wyświetlenia) + jego anotacji.
/// `annotations_json` = [{id,category_id,bbox:[x,y,w,h]}] w ORYGINALNYCH
/// współrzędnych; UI mapuje na przeskalowany obraz przez orig_width/height.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImageRequest {
    pub dataset_id: String,
    pub image_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImageResponse {
    pub image_b64: String,
    pub mime: String,
    pub orig_width: u32,
    pub orig_height: u32,
    pub annotations_json: String,
    pub error: Option<String>,
}

/// Zapis anotacji jednego obrazu z powrotem do `_annotations.coco.json` splitu.
/// `annotations_json` = [{category_id,bbox:[x,y,w,h]}] w ORYGINALNYCH współrz.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogSaveAnnotationsRequest {
    pub dataset_id: String,
    pub image_id: String,
    pub annotations_json: String,
    /// `true` = zapis + oznaczenie obrazu jako zatwierdzony; `false` = sam zapis
    /// (istniejący stan zatwierdzenia pozostaje nietknięty).
    pub approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogSaveAnnotationsResponse {
    pub ok: bool,
    pub error: Option<String>,
}

// ----- Vision model registry (dynamic camera-CV ONNX models) -----

/// Publishes a trained ML Studio model into the core `vision_models`
/// registry: locates/exports the ONNX, copies it into the vision models dir,
/// hashes it and inserts the registry row so camera pipelines can reference
/// it (directly or through an optional alias) without recompiling.
/// `op`: "detect" (RF-DETR) or "classify" (softmax classifier); it must match
/// the model's framework. `threshold` becomes the registry default score
/// threshold for detect models. `alias` (optional) creates/updates a
/// `model_aliases` row pointing at the new model name.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelPublishRequest {
    pub model_id: String,
    pub model_name: String,
    pub op: String,
    pub threshold: Option<f64>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelPublishResponse {
    pub ok: bool,
    pub error: Option<String>,
}

/// One `vision_models` registry row as shown in the dashboard list.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelInfo {
    pub model_name: String,
    pub op: String,
    pub file_name: String,
    pub sha256: String,
    pub classes: Vec<String>,
    pub source: String,
    pub default_threshold: Option<f64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelsListRequest {}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelsListResponse {
    pub models: Vec<MlStudioVisionModelInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelDeleteRequest {
    pub model_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioVisionModelDeleteResponse {
    pub ok: bool,
    pub error: Option<String>,
}

// ----- Custom vision-model import (unpaired instance → HTTPS + API key) -----

/// One file entry from a remote `/models/manifest/<ref>` response, surfaced to
/// the deploy wizard's "Custom" tab so the admin sees what will be pulled.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportManifestFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// Registry model metadata carried by a single-model remote manifest — enough
/// for the wizard to preview the model before importing it.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportManifestModel {
    pub model_name: String,
    pub op: String,
    pub file_name: String,
    pub classes: Vec<String>,
    pub output_contract: String,
    pub default_threshold: Option<f64>,
}

/// Fetch a remote model-bundle manifest through the Core (server-side, no-
/// redirect, query-redacting HTTP client) using an API key. `manifest_url` is
/// the admin-pasted `https://<host>/models/manifest/<ref>` URL; `api_key` is
/// sent as `Authorization: Bearer` and never persisted by this request.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportFetchManifestRequest {
    pub manifest_url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportFetchManifestResponse {
    pub bundle: String,
    pub files: Vec<VisionImportManifestFile>,
    /// Present only for a single-model registry bundle (importable). A fixed
    /// engine bundle (`vision-all`, camera-CV) has no registry row → `None`.
    pub model: Option<VisionImportManifestModel>,
    pub error: Option<String>,
}

/// Import a single registry model from a remote instance: Core re-fetches the
/// manifest with the key, downloads the ONNX (+ sidecars), verifies sha256,
/// places files in `vision_models_dir()` and registers a `vision_models` row
/// (`source='imported'`). `alias` optionally creates/retargets a model alias.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportModelRequest {
    pub manifest_url: String,
    pub api_key: String,
    /// The registry model name (== the single-model bundle_ref) to import.
    pub model_name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VisionImportModelResponse {
    pub ok: bool,
    pub imported_model_name: Option<String>,
    pub error: Option<String>,
}

/// One variant carrying the whole custom-import family (fetch + import). New
/// variants ALWAYS appended at the END — ciborium encodes by index.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum VisionImportPayload {
    FetchManifestRequest(VisionImportFetchManifestRequest),
    FetchManifestResponse(VisionImportFetchManifestResponse),
    ImportRequest(VisionImportModelRequest),
    ImportResponse(VisionImportModelResponse),
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogHyperparams {
    pub epochs: u32,
    pub batch_size: u32,
    pub grad_accum: u32,
    pub learning_rate: f64,
    pub resolution: u32,
    pub early_stopping: bool,
}

/// Start treningu detekcji RF-DETR. Dataset to COCO (zip) wgrany wcześniej.
/// Biegnie ASYNCHRONICZNIE (zob. `train_recognition.rs`); UI pyta o postęp przez
/// `MlStudioRecogTrainStatusRequest`. `variant` = nano|small|medium|base|large.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogTrainStartRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub variant: String,
    pub hyperparams: MlStudioRecogHyperparams,
    /// Mesh-distributed: węzeł docelowy treningu. None/pusty/local → trening
    /// lokalny; inny node_id → trening uruchamiany na zdalnym węźle (Node B)
    /// przez komendę mesh, status proxowany z powrotem.
    #[serde(default)]
    pub target_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogTrainStartResponse {
    pub run_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogTrainStatusRequest {
    pub run_id: String,
}

/// Punkt krzywej treningu detekcji (epoka): train loss + mAP@50 do wykresu.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogMetricPoint {
    pub epoch: u64,
    pub train_loss: Option<f64>,
    pub map50: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogTrainStatusResponse {
    pub run_id: String,
    pub status: String,
    pub epoch: u64,
    pub total_epochs: u64,
    pub train_loss: Option<f64>,
    pub map50: Option<f64>,
    pub map50_95: Option<f64>,
    pub error: Option<String>,
    pub curve: Vec<MlStudioRecogMetricPoint>,
    /// Faza transferu datasetu przez mesh (trening zdalny): "zipping" | "syncing"
    /// | "starting"; None gdy lokalnie lub gdy transfer zakończony i trening leci
    /// na węźle B. UI pokazuje pasek postępu z prędkością B/s w fazie "syncing".
    pub sync_phase: Option<String>,
    pub sync_bytes_sent: u64,
    pub sync_bytes_total: u64,
    pub sync_rate_bps: u64,
    /// Pola live-view z serwisu treningowego (`/status`). Serde default = wsteczna
    /// kompatybilność ze starszymi nadawcami. `eta_s`/`elapsed_s` w sekundach,
    /// `gpu_mem_mb` w MB, `stage` = etap serwisu (np. "warmup"|"train"|"eval").
    #[serde(default)]
    pub eta_s: f32,
    #[serde(default)]
    pub elapsed_s: f32,
    #[serde(default)]
    pub gpu_mem_mb: f32,
    #[serde(default)]
    pub stage: String,
}

/// Hiperparametry treningu klasyfikatora atrybutu na wycinkach (timm). Kontrakt
/// jest sztywny (inne zespoły piszą pod te same nazwy/typy), stąd `i32`/`f32`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioClassifierHyperparams {
    pub epochs: i32,
    pub batch_size: i32,
    pub learning_rate: f32,
    pub image_size: i32,
    pub freeze_backbone: bool,
}

/// Start treningu KLASYFIKATORA ATRYBUTU na wycinkach (np. atrybut "stan" o
/// wartościach czysta/brudna). Cropy z obrazów źródłowych buduje SERWIS Python
/// (`classifier-training`); Core przekazuje tylko dataset + specyfikację atrybutu.
/// Biegnie ASYNCHRONICZNIE (zob. `train_classifier.rs`); UI pyta o postęp przez
/// `MlStudioGenericTrainStatusRequest`. `variant` = mobilenetv4|efficientnet_b0|
/// resnet50. `source_class` = nazwa kategorii COCO definiującej atrybut ("" =
/// wszystkie klasy). `values` = etykiety atrybutu (kolejność = indeks etykiety).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioClassifierTrainStartRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub attribute: String,
    pub source_class: String,
    pub variant: String,
    pub values: Vec<String>,
    pub hyperparams: MlStudioClassifierHyperparams,
    /// Węzeł docelowy treningu: "" = trening lokalny; inny node_id → trening na
    /// zdalnym węźle (Node B) przez komendę mesh, status proxowany z powrotem.
    #[serde(default)]
    pub target_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioClassifierTrainStartResponse {
    pub run_id: String,
    pub status: String,
}

/// Generyczne żądanie statusu treningu (klasyfikator i inne torry nie-detekcyjne).
/// Detekcja RF-DETR nadal używa własnego `MlStudioRecogTrainStatusRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioGenericTrainStatusRequest {
    pub run_id: String,
}

/// Punkt generycznej krzywej treningu: (epoka, nazwa metryki, wartość). Pozwala
/// serwować dowolny zestaw metryk (np. train_loss, val_acc, val_macro_f1) bez
/// sztywnej struktury per-tor.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct GenericMetricPoint {
    pub epoch: i32,
    pub metric_name: String,
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioGenericTrainStatusResponse {
    pub run_id: String,
    pub status: String,
    pub epoch: i32,
    pub total_epochs: i32,
    pub curve: Vec<GenericMetricPoint>,
    pub error: String,
    /// Faza transferu datasetu przez mesh (trening zdalny): "zipping" | "syncing"
    /// | "starting"; None gdy lokalnie lub gdy transfer zakończony i trening leci
    /// na węźle B. Analogiczne do `MlStudioRecogTrainStatusResponse`.
    pub sync_phase: Option<String>,
    pub sync_bytes_sent: u64,
    pub sync_bytes_total: u64,
    pub sync_rate_bps: u64,
    /// Pola live-view z serwisu treningowego (`/status`). Serde default = wsteczna
    /// kompatybilność ze starszymi nadawcami. `eta_s`/`elapsed_s` w sekundach,
    /// `gpu_mem_mb` w MB, `stage` = etap serwisu (np. "warmup"|"train"|"eval").
    #[serde(default)]
    pub eta_s: f32,
    #[serde(default)]
    pub elapsed_s: f32,
    #[serde(default)]
    pub gpu_mem_mb: f32,
    #[serde(default)]
    pub stage: String,
}

/// Anulowanie TRWAJĄCEGO treningu ML Studio — jeden wariant dla wszystkich torów
/// (detekcja, klasyfikator, OCR, LLM), bo run zna swój tor po `config_json`.
/// Handler woła `/cancel` serwisu treningowego (lokalnie albo przez mesh na węźle
/// treningowym) i podnosi flagę anulowania, którą widzą pętle nadzoru Core.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTrainCancelRequest {
    pub run_id: String,
}

/// `cancelled = false` znaczy „nie było co anulować" (run już się zakończył);
/// `status` to stan runu po żądaniu.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioTrainCancelResponse {
    pub run_id: String,
    pub status: String,
    pub cancelled: bool,
}

/// Hiperparametry treningu czytnika OCR (CRNN + CTC) na wierszach tablic.
/// `synthetic_per_epoch` to liczba próbek syntetycznych generowanych na epokę
/// (0 = trening wyłącznie na realnych wierszach), `real_repeat` ile razy realne
/// wiersze są powtarzane w epoce — realnych etykiet jest z natury mało, więc bez
/// powtórzeń syntetyk by je zdominował.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioOcrHyperparams {
    pub epochs: i32,
    pub batch_size: i32,
    pub learning_rate: f32,
    pub synthetic_per_epoch: i32,
    pub real_repeat: i32,
}

/// Start treningu CZYTNIKA OCR na wierszach wycinków (np. atrybut "kod" klasy
/// `tablica_adr` o wartościach w formacie `<kemler>/<UN>`). Wycinki, podział na
/// wiersze i etykiety buduje SERWIS `ocr-training`; Core przekazuje dataset +
/// specyfikację atrybutu. Biegnie ASYNCHRONICZNIE (zob. `train_ocr.rs`); UI pyta
/// o postęp przez `MlStudioGenericTrainStatusRequest`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioOcrTrainStartRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub attribute: String,
    pub source_class: String,
    pub hyperparams: MlStudioOcrHyperparams,
    /// Węzeł docelowy treningu: "" = trening lokalny; inny node_id → trening na
    /// zdalnym węźle (Node B) przez komendę mesh, status proxowany z powrotem.
    #[serde(default)]
    pub target_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioOcrTrainStartResponse {
    pub run_id: String,
    pub status: String,
}

/// Żądanie eksportu wytrenowanego modelu FT do GGUF. Eksport (merge adaptera +
/// konwersja) trwa, więc biegnie ASYNCHRONICZNIE w tle Core (zob.
/// `export_llm.rs`); odpowiedź wraca natychmiast, a UI odpytuje przez
/// `MlStudioFtExportStatusRequest`. `outtype` to format kwantyzacji: "f16"|"q8_0".
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtExportRequest {
    pub model_id: String,
    pub outtype: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtExportResponse {
    pub model_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtExportStatusRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtExportStatusResponse {
    pub model_id: String,
    pub status: String,
    pub gguf_path: Option<String>,
    pub size_bytes: Option<u64>,
    pub error: Option<String>,
}

/// Żądanie DEPLOY wytrenowanego modelu FT (lokalny GGUF po eksporcie) jako
/// embedded serwisu inferencji llama.cpp. Domyka cykl FT: trenuj→eksportuj→
/// DEPLOY→używaj. Deploy biegnie przez istniejący `service_manifest_deploy`
/// (engine `llama-cpp`, `native` embedded); model staje się dostępny pod
/// aliasem `model_name` w routingu `/v1`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtDeployRequest {
    pub model_id: String,
    /// Węzeł docelowy deployu. Pusty = węzeł, na którym żyje artefakt (domyślne).
    /// Inny niż węzeł artefaktu → Core przenosi artefakt przez mesh przed deployem.
    #[serde(default)]
    pub target_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtDeployResponse {
    pub model_id: String,
    pub model_name: String,
    pub status: String,
    pub error: Option<String>,
}

/// Zapytanie do wdrożonego modelu FT (test/„użyj"). Dashboard używa protokołu
/// binarnego (nie REST /v1), a gdy model żyje na innym węźle mesh, Core proxuje
/// zapytanie do węzła-właściciela komendą `MlChat`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtChatRequest {
    pub model_id: String,
    pub message: String,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioFtChatResponse {
    pub answer: String,
    #[serde(default)]
    pub error: Option<String>,
}

// ----- Recognition: schema editor (opaque JSON owned by the frontend) -----

/// Reads the project's recognition schema as an OPAQUE JSON string. Core never
/// parses the internal shape — it persists and returns whatever the frontend
/// stored. Empty/none-stored projects get `"{}"`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioSchemaGetRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioSchemaGetResponse {
    pub schema_json: String,
}

/// Upserts the project's recognition schema (one schema row per project). Core
/// stores `schema_json` verbatim without validating its internal shape.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioSchemaSaveRequest {
    pub project_id: String,
    pub schema_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioSchemaSaveResponse {
    pub ok: bool,
}

/// Lists the project's lookup dictionaries. `dicts_json` is a JSON array string
/// of `{dictId,name,rowsJson}`; `rowsJson` is the opaque per-dict rows blob the
/// frontend stored. Mirrors the JSON-as-string convention used by recog images.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictsListRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictsListResponse {
    pub dicts_json: String,
}

/// Upserts one lookup dictionary. Empty `dict_id` → INSERT with a fresh uuid
/// (returned); non-empty → UPDATE that dict's name + rows. `rows_json` is stored
/// opaquely.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictSaveRequest {
    pub project_id: String,
    pub dict_id: String,
    pub name: String,
    pub rows_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictSaveResponse {
    pub dict_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictDeleteRequest {
    pub dict_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioLookupDictDeleteResponse {
    pub ok: bool,
}

/// Lists models a recognition schema field can bind to. `models_json` is a JSON
/// array string of `{id,name,capability,source}` merged from the `service_models`
/// table plus the in-core built-in CV models. Empty `capability` = all.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioServiceModelsListRequest {
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioServiceModelsListResponse {
    pub models_json: String,
}

// ---------------------------------------------------------------------------
// Destylacja — generowanie datasetu par (question, answer). Zrodlo pytan:
// import wgranego datasetu ALBO generacja modelem z promptu usera; teacher
// (dowolny wybrany model) generuje odpowiedzi referencyjne. Wynik -> dataset.
// ---------------------------------------------------------------------------

/// Start generowania datasetu destylacji. Job w tle: zbiera pytania, odpytuje
/// teachera po odpowiedzi, zapisuje pary do nowego datasetu (kind="distill_qa").
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistillGenerateRequest {
    pub project_id: String,
    pub dataset_name: String,
    /// Zrodlo pytan: "import" (kolumna z istniejacego datasetu) lub "generate"
    /// (model generuje pytania z `generate_prompt`).
    pub question_source: String,
    /// "import": id wgranego datasetu + nazwa pola/kolumny z pytaniami.
    #[serde(default)]
    pub source_dataset_id: Option<String>,
    #[serde(default)]
    pub question_field: Option<String>,
    /// "generate": prompt usera (co wygenerowac), model generujacy pytania, ile pytan.
    #[serde(default)]
    pub generate_prompt: Option<String>,
    #[serde(default)]
    pub question_model: Option<String>,
    #[serde(default)]
    pub num_questions: Option<u32>,
    /// Teacher — model generujacy ODPOWIEDZI (alias/model wybrany w GUI; dowolny
    /// tentaflow lub external). Odpowiedzi to etykiety treningowe ucznia.
    pub teacher_model: String,
    /// Instrukcja dla teachera (jak ma odpowiadac); doklejana przed pytaniem.
    #[serde(default)]
    pub answer_instruction: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Wariant treningu pod ktory generujemy dane: "sft"/"kd" -> pary
    /// (question, answer); "dpo" -> trojki (prompt, chosen, rejected). Domyslnie sft.
    #[serde(default)]
    pub objective: Option<String>,
    /// DPO: model generujacy ODRZUCONA (gorsza) odpowiedz — zwykle slabszy/bazowy
    /// albo teacher z instrukcja "odpowiedz gorzej". Wymagany dla objective=dpo.
    #[serde(default)]
    pub rejected_model: Option<String>,
    /// DPO: instrukcja dla modelu generujacego odrzucona odpowiedz.
    #[serde(default)]
    pub rejected_instruction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistillGenerateResponse {
    pub dataset_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistillGenerateStatusRequest {
    pub dataset_id: String,
}

/// Podglad wygenerowanej probki. SFT/KD: `question`+`answer`. DPO: `question`=prompt,
/// `answer`=chosen (lepsza), `rejected`=Some(gorsza).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistillQaPair {
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub rejected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioDistillGenerateStatusResponse {
    /// pending | generating_questions | answering | completed | failed
    pub status: String,
    pub total: u32,
    pub done: u32,
    #[serde(default)]
    pub error: Option<String>,
    /// Kilka pierwszych par do podgladu w UI.
    pub samples: Vec<MlStudioDistillQaPair>,
}

// ---------------------------------------------------------------------------
// Project export/import — pakowanie całego projektu ML Studio (datasety, klasy,
// opcjonalnie modele i historia) do archiwum i odtwarzanie po stronie klienta.
// ---------------------------------------------------------------------------

/// Podsumowanie datasetu wykryte w podglądzie importowanego archiwum.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioImportDatasetInfo {
    pub dataset_id: String,
    pub name: String,
    pub image_count: u64,
    pub annotation_count: u64,
}

/// Artefakt zadeklarowany w manifeście archiwum, którego brakuje w paczce.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioImportMissingArtifact {
    pub path: String,
    pub reason: String,
}

/// Pojedyncze nagranie dostępne do zaimportowania jako źródło klatek datasetu.
/// `created_at` to unix w MILISEKUNDACH.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecordingItem {
    pub recording_ref: String,
    pub kind: String,
    pub camera_id: String,
    pub created_at: i64,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    pub file_size_bytes: i64,
    #[serde(default)]
    pub plate_text: Option<String>,
    #[serde(default)]
    pub adr_text: Option<String>,
    /// Signed `/frames/<ref>` URL of a representative full frame (the scene at
    /// the best OCR read of the event), used as a clip preview. `None` when the
    /// recording carries no thumb ref or lives on a remote node.
    #[serde(default)]
    pub thumb_url: Option<String>,
}

/// Start pakowania projektu. Job w tle buduje archiwum; postęp przez
/// `MlStudioProjectExportStatusRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectExportStartRequest {
    pub project_id: String,
    pub include_models: bool,
    pub include_history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectExportStartResponse {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectExportStatusRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectExportStatusResponse {
    pub status: String,
    pub phase: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub export_ref: Option<String>,
    #[serde(default)]
    pub signed_url: Option<String>,
    #[serde(default)]
    pub archive_bytes: Option<u64>,
}

/// Jeden fragment przesyłanego archiwum projektu. Duże paczki przekraczają limit
/// pojedynczej ramki WS, więc klient dzieli plik na części `seq` (0..total_chunks).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportUploadChunkRequest {
    pub upload_id: String,
    pub seq: u32,
    pub total_chunks: u32,
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportUploadChunkResponse {
    pub upload_id: String,
    pub received_chunks: u32,
    pub received_bytes: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportUploadStatusRequest {
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportUploadStatusResponse {
    pub upload_id: String,
    pub received_chunks: u32,
    pub received_bytes: u64,
    pub total_chunks: u32,
    pub complete: bool,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportPreviewRequest {
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportPreviewResponse {
    pub project_name: String,
    pub project_type: String,
    pub datasets: Vec<MlStudioImportDatasetInfo>,
    pub classes: Vec<String>,
    pub has_models: bool,
    pub has_history: bool,
    pub total_uncompressed_bytes: u64,
    pub missing_artifacts: Vec<MlStudioImportMissingArtifact>,
    pub archive_version: u32,
}

/// Zatwierdzenie importu. `mode`: "new_project" | "merge".
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportApplyRequest {
    pub upload_id: String,
    pub mode: String,
    #[serde(default)]
    pub name_override: Option<String>,
    #[serde(default)]
    pub target_project_id: Option<String>,
    #[serde(default)]
    pub target_dataset_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportApplyResponse {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportStatusRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportStatusResponse {
    pub status: String,
    pub phase: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportCancelRequest {
    pub upload_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioProjectImportCancelResponse {
    pub cancelled: bool,
}

/// Lista nagrań filtrowana po kamerze i zakresie czasu (unix w milisekundach).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecordingsListRequest {
    #[serde(default)]
    pub camera_id: Option<String>,
    #[serde(default)]
    pub date_from_ms: Option<i64>,
    #[serde(default)]
    pub date_to_ms: Option<i64>,
    pub limit: u32,
    /// Hex node_id of a PAIRED node to list recordings from. `None`/absent =
    /// local node (unchanged behaviour).
    #[serde(default)]
    pub source_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecordingsListResponse {
    pub items: Vec<MlStudioRecordingItem>,
}

/// Import nagrań do datasetu rozpoznawania: ekstrakcja klatek `fps`, opcjonalny
/// autolabel. `collision`: "suffix" | "skip".
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImportRecordingsRequest {
    pub project_id: String,
    pub dataset_id: String,
    pub recording_refs: Vec<String>,
    pub fps: u32,
    pub autolabel: bool,
    pub collision: String,
    /// Hex node_id of a PAIRED node to pull recordings from. `None`/absent =
    /// local node (unchanged behaviour).
    #[serde(default)]
    pub source_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImportRecordingsResponse {
    pub job_id: String,
}

/// Wynik importu pojedynczego nagrania. `skipped` = Some(powód) gdy pominięto.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecordingOutcome {
    pub recording_ref: String,
    pub frames: u64,
    pub detections: u64,
    pub skipped_frames: u64,
    #[serde(default)]
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImportRecordingsStatusRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRecogImportRecordingsStatusResponse {
    pub status: String,
    pub phase: String,
    pub recordings_total: u32,
    pub recordings_done: u32,
    pub frames_extracted: u64,
    pub frames_labeled: u64,
    pub images_added: u64,
    pub detections: u64,
    #[serde(default)]
    pub error: Option<String>,
    pub outcomes: Vec<MlStudioRecordingOutcome>,
}

/// Podgląd projektu udostępnionego przez ZDALNĄ, niesparowaną instancję. `url` to
/// link „share" (albo bezpośredni `/ml-studio/share/<id>/manifest`, albo baza),
/// `api_key` to klucz Bearer. Handler pobiera TYLKO manifest (tani podgląd, bez
/// pobierania archiwum) przez utwardzony klient HTTPS.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportPreviewRequest {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportPreviewResponse {
    pub project_name: String,
    pub project_type: String,
    pub datasets: Vec<MlStudioImportDatasetInfo>,
    pub classes: Vec<String>,
    pub archive_bytes: u64,
    pub archive_version: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// Start zdalnego importu: pobranie archiwum ZIP z instancji źródłowej i import
/// jako NOWY projekt lokalny. Job w tle; postęp przez `RemoteImportStatusRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportStartRequest {
    pub url: String,
    pub api_key: String,
    #[serde(default)]
    pub name_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportStartResponse {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportStatusRequest {
    pub job_id: String,
}

/// Postęp zdalnego importu. `phase` obejmuje etap „downloading" przed fazami
/// importu archiwum („extracting" | „registering").
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MlStudioRemoteImportStatusResponse {
    pub status: String,
    pub phase: String,
    pub bytes_total: u64,
    pub bytes_done: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum MlStudioPayload {
    ProjectsListRequest(MlStudioProjectsListRequest),
    ProjectsListResponse(MlStudioProjectsListResponse),
    ProjectCreateRequest(MlStudioProjectCreateRequest),
    ProjectCreateResponse(MlStudioProjectCreateResponse),
    ProjectDetailRequest(MlStudioProjectDetailRequest),
    ProjectDetailResponse(MlStudioProjectDetailResponse),
    ProjectTypesListRequest(MlStudioProjectTypesListRequest),
    ProjectTypesListResponse(MlStudioProjectTypesListResponse),
    ProjectMembersListRequest(MlStudioProjectMembersListRequest),
    ProjectMembersListResponse(MlStudioProjectMembersListResponse),
    ProjectInviteRequest(MlStudioProjectInviteRequest),
    ProjectInviteResponse(MlStudioProjectInviteResponse),
    ProjectMemberRemoveRequest(MlStudioProjectMemberRemoveRequest),
    ProjectMemberRemoveResponse(MlStudioProjectMemberRemoveResponse),
    ProjectMemberRoleSetRequest(MlStudioProjectMemberRoleSetRequest),
    ProjectMemberRoleSetResponse(MlStudioProjectMemberRoleSetResponse),
    DatasetUploadRequest(MlStudioDatasetUploadRequest),
    DatasetUploadResponse(MlStudioDatasetUploadResponse),
    DatasetsListRequest(MlStudioDatasetsListRequest),
    DatasetsListResponse(MlStudioDatasetsListResponse),
    DatasetProfileRequest(MlStudioDatasetProfileRequest),
    DatasetProfileResponse(MlStudioDatasetProfileResponse),
    TabularTrainRequest(MlStudioTabularTrainRequest),
    TabularTrainResponse(MlStudioTabularTrainResponse),
    ResourceGrantCreateRequest(MlStudioResourceGrantCreateRequest),
    ResourceGrantCreateResponse(MlStudioResourceGrantCreateResponse),
    ResourceGrantsListRequest(MlStudioResourceGrantsListRequest),
    ResourceGrantsListResponse(MlStudioResourceGrantsListResponse),
    ResourceGrantRevokeRequest(MlStudioResourceGrantRevokeRequest),
    ResourceGrantRevokeResponse(MlStudioResourceGrantRevokeResponse),
    ProjectResourcesRequest(MlStudioProjectResourcesRequest),
    ProjectResourcesResponse(MlStudioProjectResourcesResponse),
    TrainingRunsListRequest(MlStudioTrainingRunsListRequest),
    TrainingRunsListResponse(MlStudioTrainingRunsListResponse),
    JobsOverviewRequest(MlStudioJobsOverviewRequest),
    JobsOverviewResponse(MlStudioJobsOverviewResponse),
    ModelsListRequest(MlStudioModelsListRequest),
    ModelsListResponse(MlStudioModelsListResponse),
    ProjectGrantsListRequest(MlStudioProjectGrantsListRequest),
    ProjectGrantsListResponse(MlStudioProjectGrantsListResponse),
    FtTrainStartRequest(MlStudioFtTrainStartRequest),
    FtTrainStartResponse(MlStudioFtTrainStartResponse),
    FtTrainStatusRequest(MlStudioFtTrainStatusRequest),
    FtTrainStatusResponse(MlStudioFtTrainStatusResponse),
    FtExportRequest(MlStudioFtExportRequest),
    FtExportResponse(MlStudioFtExportResponse),
    FtExportStatusRequest(MlStudioFtExportStatusRequest),
    FtExportStatusResponse(MlStudioFtExportStatusResponse),
    FtDeployRequest(MlStudioFtDeployRequest),
    FtDeployResponse(MlStudioFtDeployResponse),
    DistillGenerateRequest(MlStudioDistillGenerateRequest),
    DistillGenerateResponse(MlStudioDistillGenerateResponse),
    DistillGenerateStatusRequest(MlStudioDistillGenerateStatusRequest),
    DistillGenerateStatusResponse(MlStudioDistillGenerateStatusResponse),
    RecogTrainStartRequest(MlStudioRecogTrainStartRequest),
    RecogTrainStartResponse(MlStudioRecogTrainStartResponse),
    RecogTrainStatusRequest(MlStudioRecogTrainStatusRequest),
    RecogTrainStatusResponse(MlStudioRecogTrainStatusResponse),
    ClassifierTrainStartRequest(MlStudioClassifierTrainStartRequest),
    ClassifierTrainStartResponse(MlStudioClassifierTrainStartResponse),
    GenericTrainStatusRequest(MlStudioGenericTrainStatusRequest),
    GenericTrainStatusResponse(MlStudioGenericTrainStatusResponse),
    RecogDatasetRegisterRequest(MlStudioRecogDatasetRegisterRequest),
    RecogDatasetRegisterResponse(MlStudioRecogDatasetRegisterResponse),
    RecogStageMediaRequest(MlStudioRecogStageMediaRequest),
    RecogStageMediaResponse(MlStudioRecogStageMediaResponse),
    RecogBuildDatasetRequest(MlStudioRecogBuildDatasetRequest),
    RecogBuildDatasetResponse(MlStudioRecogBuildDatasetResponse),
    RecogBuildStatusRequest(MlStudioRecogBuildStatusRequest),
    RecogBuildStatusResponse(MlStudioRecogBuildStatusResponse),
    RecogAutolabelRequest(MlStudioRecogAutolabelRequest),
    RecogAutolabelResponse(MlStudioRecogAutolabelResponse),
    RecogAutolabelStatusRequest(MlStudioRecogAutolabelStatusRequest),
    RecogAutolabelStatusResponse(MlStudioRecogAutolabelStatusResponse),
    RecogDetectRequest(MlStudioRecogDetectRequest),
    RecogDetectResponse(MlStudioRecogDetectResponse),
    RecogImagesListRequest(MlStudioRecogImagesListRequest),
    RecogImagesListResponse(MlStudioRecogImagesListResponse),
    RecogImageRequest(MlStudioRecogImageRequest),
    RecogImageResponse(MlStudioRecogImageResponse),
    RecogSaveAnnotationsRequest(MlStudioRecogSaveAnnotationsRequest),
    RecogSaveAnnotationsResponse(MlStudioRecogSaveAnnotationsResponse),
    DatasetUploadChunkRequest(MlStudioDatasetUploadChunkRequest),
    DatasetUploadChunkResponse(MlStudioDatasetUploadChunkResponse),
    FtChatRequest(MlStudioFtChatRequest),
    FtChatResponse(MlStudioFtChatResponse),
    SchemaGetRequest(MlStudioSchemaGetRequest),
    SchemaGetResponse(MlStudioSchemaGetResponse),
    SchemaSaveRequest(MlStudioSchemaSaveRequest),
    SchemaSaveResponse(MlStudioSchemaSaveResponse),
    LookupDictsListRequest(MlStudioLookupDictsListRequest),
    LookupDictsListResponse(MlStudioLookupDictsListResponse),
    LookupDictSaveRequest(MlStudioLookupDictSaveRequest),
    LookupDictSaveResponse(MlStudioLookupDictSaveResponse),
    LookupDictDeleteRequest(MlStudioLookupDictDeleteRequest),
    LookupDictDeleteResponse(MlStudioLookupDictDeleteResponse),
    ServiceModelsListRequest(MlStudioServiceModelsListRequest),
    ServiceModelsListResponse(MlStudioServiceModelsListResponse),
    // NOWE warianty ZAWSZE na końcu — ciborium serializuje indeks wariantu, więc
    // wstawienie w środku przesunęłoby dyskryminatory i zepsuło wire compat.
    DatasetRowsRequest(MlStudioDatasetRowsRequest),
    DatasetRowsResponse(MlStudioDatasetRowsResponse),
    DatasetRowsSaveRequest(MlStudioDatasetRowsSaveRequest),
    DatasetRowsSaveResponse(MlStudioDatasetRowsSaveResponse),
    VisionModelPublishRequest(MlStudioVisionModelPublishRequest),
    VisionModelPublishResponse(MlStudioVisionModelPublishResponse),
    VisionModelsListRequest(MlStudioVisionModelsListRequest),
    VisionModelsListResponse(MlStudioVisionModelsListResponse),
    VisionModelDeleteRequest(MlStudioVisionModelDeleteRequest),
    VisionModelDeleteResponse(MlStudioVisionModelDeleteResponse),
    ProjectExportStartRequest(MlStudioProjectExportStartRequest),
    ProjectExportStartResponse(MlStudioProjectExportStartResponse),
    ProjectExportStatusRequest(MlStudioProjectExportStatusRequest),
    ProjectExportStatusResponse(MlStudioProjectExportStatusResponse),
    ProjectImportUploadChunkRequest(MlStudioProjectImportUploadChunkRequest),
    ProjectImportUploadChunkResponse(MlStudioProjectImportUploadChunkResponse),
    ProjectImportUploadStatusRequest(MlStudioProjectImportUploadStatusRequest),
    ProjectImportUploadStatusResponse(MlStudioProjectImportUploadStatusResponse),
    ProjectImportPreviewRequest(MlStudioProjectImportPreviewRequest),
    ProjectImportPreviewResponse(MlStudioProjectImportPreviewResponse),
    ProjectImportApplyRequest(MlStudioProjectImportApplyRequest),
    ProjectImportApplyResponse(MlStudioProjectImportApplyResponse),
    ProjectImportStatusRequest(MlStudioProjectImportStatusRequest),
    ProjectImportStatusResponse(MlStudioProjectImportStatusResponse),
    ProjectImportCancelRequest(MlStudioProjectImportCancelRequest),
    ProjectImportCancelResponse(MlStudioProjectImportCancelResponse),
    RecordingsListRequest(MlStudioRecordingsListRequest),
    RecordingsListResponse(MlStudioRecordingsListResponse),
    RecogImportRecordingsRequest(MlStudioRecogImportRecordingsRequest),
    RecogImportRecordingsResponse(MlStudioRecogImportRecordingsResponse),
    RecogImportRecordingsStatusRequest(MlStudioRecogImportRecordingsStatusRequest),
    RecogImportRecordingsStatusResponse(MlStudioRecogImportRecordingsStatusResponse),
    RemoteImportPreviewRequest(MlStudioRemoteImportPreviewRequest),
    RemoteImportPreviewResponse(MlStudioRemoteImportPreviewResponse),
    RemoteImportStartRequest(MlStudioRemoteImportStartRequest),
    RemoteImportStartResponse(MlStudioRemoteImportStartResponse),
    RemoteImportStatusRequest(MlStudioRemoteImportStatusRequest),
    RemoteImportStatusResponse(MlStudioRemoteImportStatusResponse),
    TrainCancelRequest(MlStudioTrainCancelRequest),
    TrainCancelResponse(MlStudioTrainCancelResponse),
    OcrTrainStartRequest(MlStudioOcrTrainStartRequest),
    OcrTrainStartResponse(MlStudioOcrTrainStartResponse),
}

// ----- Robots screen (UserSession) -----

/// Typed, allowlisted robot control action carried over the binary protocol. A
/// flat `kind` discriminant plus the optional `Move` axes mirrors the addon SDK
/// `RobotActionWire`, so the Robots app, the addon SDK and the host all share ONE
/// action wire shape and the same closed allowlist (Core rejects unknown kinds).
/// `kind` is one of: "move", "stop", "estop", "reset_estop", "recovery_stand",
/// "stand_up", "stand_down", "sit", "hello", "stretch", "status".
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotActionWire {
    pub kind: String,
    /// `Move` body velocity (normalized -1..1). Ignored for non-move kinds; the
    /// owner clamps again to its safety cap.
    pub vx: f64,
    pub vy: f64,
    pub vyaw: f64,
    /// Generic numeric params for parametered poses/levels, keyed by `kind` (see
    /// the SDK `RobotActionWire` and core `RobotAction::from_kind_params`).
    /// Defaulted to 0 for parameterless kinds and older senders. The owner clamps
    /// each to the documented Go2 range.
    #[serde(default)]
    pub p1: f64,
    #[serde(default)]
    pub p2: f64,
    #[serde(default)]
    pub p3: f64,
    #[serde(default)]
    pub p4: f64,
}

/// One numeric parameter of a parametered robot action, with the inclusive range
/// the UI must bound its input to (the owner re-clamps on receipt regardless).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotActionParam {
    pub name: String,
    pub min: f64,
    pub max: f64,
}

/// Rich descriptor of ONE advertised robot control: enough for a capability-driven
/// UI to render the right widget (button / bounded inputs / dpad), gate high-risk
/// acrobatics behind a confirmation, and label it — without hardcoding any action
/// list. Projected verbatim from the owning addon's `actions_meta`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotActionMeta {
    pub kind: String,
    pub label: String,
    /// Risk tier: "low" / "medium" / "high". "high" (or `acrobatic`) requires the
    /// UI to confirm before sending.
    pub risk: String,
    #[serde(default)]
    pub acrobatic: bool,
    /// A read-only action (e.g. status) the UI must NOT render as a control.
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub params: Vec<RobotActionParam>,
}

/// Inertial-measurement snapshot the robot reports (orientation + temperature).
/// Every field is optional so a robot that omits one (or a whole IMU block) is
/// representable without inventing a reading.
#[derive(Debug, Clone, PartialEq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct RobotImuSnapshot {
    #[serde(default)]
    pub roll: Option<f64>,
    #[serde(default)]
    pub pitch: Option<f64>,
    #[serde(default)]
    pub yaw: Option<f64>,
    /// Orientation quaternion [w, x, y, z] (empty when the robot omits it).
    #[serde(default)]
    pub quaternion: Vec<f64>,
    /// IMU board temperature in °C.
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Battery detail beyond the flat percentage: voltage / current / cell SOC /
/// pack temperature. All optional (capability-absent → `None`).
#[derive(Debug, Clone, PartialEq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct RobotBatterySnapshot {
    #[serde(default)]
    pub soc: Option<f64>,
    #[serde(default)]
    pub voltage: Option<f64>,
    #[serde(default)]
    pub current: Option<f64>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Structured runtime telemetry snapshot of a robot, projected from the owning
/// addon's `status.telemetry` object. This is a SNAPSHOT read at the existing
/// advertisement cadence, NOT a high-rate stream. Every field is optional / a
/// possibly-empty vector so a robot that does not report a value simply omits it
/// (capability-absent, never a fabricated reading).
#[derive(Debug, Clone, PartialEq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct RobotTelemetrySnapshot {
    /// Sport/gait mode integer the robot reports (firmware-specific).
    #[serde(default)]
    pub mode: Option<i64>,
    /// Gait type integer (trot / run / climb …).
    #[serde(default)]
    pub gait_type: Option<i64>,
    /// Current body height in metres.
    #[serde(default)]
    pub body_height: Option<f64>,
    /// Forward velocity (m/s).
    #[serde(default)]
    pub vx: Option<f64>,
    /// Lateral velocity (m/s).
    #[serde(default)]
    pub vy: Option<f64>,
    /// Yaw rate (rad/s).
    #[serde(default)]
    pub vyaw: Option<f64>,
    /// Odometry position [x, y, z] (empty when absent).
    #[serde(default)]
    pub position: Vec<f64>,
    /// Per-foot contact force (empty when absent).
    #[serde(default)]
    pub foot_force: Vec<f64>,
    #[serde(default)]
    pub imu: Option<RobotImuSnapshot>,
    #[serde(default)]
    pub battery: Option<RobotBatterySnapshot>,
    /// Leg joint angles in radians, Go2 order FR/FL/RR/RL × hip/thigh/calf
    /// (empty when absent). Drives the dashboard robot animation. APPENDED LAST for
    /// wire back-compat (ciborium positional fields — new fields go at the end).
    #[serde(default)]
    pub joints: Vec<f64>,
    /// World pose (odom frame) from lidar odometry: position [x,y,z] meters.
    /// APPENDED for wire back-compat — keep new fields after this.
    #[serde(default)]
    pub pose_position: Vec<f64>,
    /// World orientation quaternion [x,y,z,w] paired with `pose_position`.
    #[serde(default)]
    pub pose_orientation: Vec<f64>,
}

/// SMALL LiDAR availability snapshot — NEVER the point cloud (which would be far
/// too large to advertise every ~10 s). It carries only enough for the UI to show
/// "LiDAR active, N points" and for a future renderer to know a fresh frame exists
/// (then pull it on demand via the `lidar_frame` action). Every field is plain so
/// an addon that reports no LiDAR simply advertises `None` for the whole block.
#[derive(Debug, Clone, PartialEq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct RobotLidarStatus {
    /// Operator intent: the LiDAR sensor has been switched on.
    #[serde(default)]
    pub enabled: bool,
    /// At least one voxel frame has decoded this session (a renderer can fetch it).
    #[serde(default)]
    pub available: bool,
    /// Number of decoded points in the latest frame.
    #[serde(default)]
    pub point_count: u32,
    /// Voxel resolution in metres (cube edge), when known.
    #[serde(default)]
    pub resolution: Option<f32>,
    /// Grid origin [x, y, z] in metres (empty when unknown).
    #[serde(default)]
    pub origin: Vec<f64>,
    /// Monotonic frame counter this session (0 = no frame yet).
    #[serde(default)]
    pub frame_seq: u64,
    /// Wall-clock seconds of the last decoded frame (0 = none).
    #[serde(default)]
    pub last_update_ts: i64,
}

/// One robot row for the Robots list screen: a projection of the mesh registry's
/// `AdvertisedRobot`, scoped to the caller's org. `is_local` lets the UI label a
/// robot this node physically owns vs one controlled over the mesh.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotEntry {
    pub robot_id: String,
    /// Endpoint-id hex of the node that owns (physically controls) this robot.
    pub owner_node_id: String,
    pub is_local: bool,
    pub kind: Option<String>,
    pub status: String,
    pub battery_percent: Option<f32>,
    pub rtt_ms: Option<u32>,
    pub camera_id: Option<String>,
    pub capabilities: Vec<String>,
    /// Rich capability descriptors driving the capability-based control UI.
    /// Appended last for CBOR back-compat: an older owner that advertises no
    /// `actions_meta` decodes with an empty vec (`#[serde(default)]`,
    /// ciborium APPEND-AT-END rule) — the UI then falls back to plain chips.
    #[serde(default)]
    pub actions_meta: Vec<RobotActionMeta>,
    /// Structured runtime telemetry snapshot (gait / velocity / IMU / battery
    /// detail). Appended last for CBOR back-compat: an older owner that reports
    /// no telemetry decodes with `None` (`#[serde(default)]`, ciborium
    /// APPEND-AT-END rule) — the UI then renders no telemetry panel.
    #[serde(default)]
    pub telemetry: Option<RobotTelemetrySnapshot>,
    /// SMALL LiDAR availability snapshot (no point cloud). Appended last for CBOR
    /// back-compat: an older owner without LiDAR decodes with `None`
    /// (`#[serde(default)]`, ciborium APPEND-AT-END rule).
    #[serde(default)]
    pub lidar: Option<RobotLidarStatus>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotsListRequest;

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotsListResponse {
    pub robots: Vec<RobotEntry>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotControlRequest {
    pub robot_id: String,
    pub action: RobotActionWire,
}

/// Result of a control action. A robot-level refusal is still a successful call
/// carrying `rejected` (a stable tag, e.g. "permission_denied", "unknown_robot");
/// `error` holds an execution failure message.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotControlResponse {
    pub ok: bool,
    pub rejected: Option<String>,
    pub error: Option<String>,
    /// Optional JSON result payload for read-only actions that return data (e.g.
    /// `lidar_frame` returns small availability metadata — enabled/available/
    /// point_count/frame_seq — never the cloud, which flows as binary L1 frames
    /// through the host LidarStreamHub). Appended last for CBOR back-compat: an
    /// older peer decodes it as `None`
    /// (`#[serde(default)]`, ciborium APPEND-AT-END rule). Action-class commands
    /// (move/pose/…) leave it `None`.
    #[serde(default)]
    pub result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotCameraShareRequest {
    pub robot_id: String,
    pub camera_id: String,
}

/// Result of exposing a robot's camera to TentaVision. For a LOCAL robot this
/// persists a cross-addon read grant; for a REMOTE robot no local grant exists
/// (camera rows are node-local) so `note` explains the remote-view path.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotCameraShareResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

/// Manual georeference for a robot's scene (the "set map origin" operation): pins the
/// scene origin to a real-world WGS84 position + heading. `lat/lon/alt/heading` all
/// `Some` = set; all `None` = clear the anchor. Heading is the compass bearing
/// (degrees clockwise from true North) of the scene's +X axis.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotGeoAnchorSetRequest {
    pub robot_id: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub heading: Option<f64>,
}

/// Read a robot's current geo anchor + live real-world position.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotGeoAnchorGetRequest {
    pub robot_id: String,
}

/// The robot's geo anchor + (when anchored and a pose is known) its current WGS84
/// position. `anchored` is the applied-anchor flag; `*_deg`/`alt`/`heading` describe
/// it; `pose_*` carry the live global position (None until the robot has a pose).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct RobotGeoAnchorResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub anchored: bool,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub heading: Option<f64>,
    pub pose_lat: Option<f64>,
    pub pose_lon: Option<f64>,
    pub pose_alt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum RobotsPayload {
    ListRequest(RobotsListRequest),
    ListResponse(RobotsListResponse),
    ControlRequest(RobotControlRequest),
    ControlResponse(RobotControlResponse),
    CameraShareRequest(RobotCameraShareRequest),
    CameraShareResponse(RobotCameraShareResponse),
    GeoAnchorSetRequest(RobotGeoAnchorSetRequest),
    GeoAnchorGetRequest(RobotGeoAnchorGetRequest),
    GeoAnchorResponse(RobotGeoAnchorResponse),
}

// ----- Skills registry (Harness plan §3.2) -----

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsListRequest {
    pub tag: Option<String>,
    pub source: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsListResponse {
    pub skills_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsDetailRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsDetailResponse {
    pub skill_json: String,
    pub files_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsUpsertRequest {
    pub skill_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsUpsertResponse {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsDeleteRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsForkRequest {
    pub skill_id: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsForkResponse {
    pub skill_id: String,
}

// ----- Skills Hub: runtime fetch/install (Harness plan §3.2 source `hub`) -----
//
// Import a skill on the fly from a public source (a GitHub repo path resolved
// through the Contents API, or a direct HTTPS URL to a SKILL.md). The import
// lands in `quarantine` and an injection-pattern scan produces a verdict; an
// admin approves (→ `active`) or rejects (→ delete). All fetches go through the
// existing public-URL SSRF guard. Handlers are Admin-only.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubSearchRequest {
    pub query: String,
    /// Optional single tap (`owner/repo`) or `https://` URL to scope the search.
    /// Absent = search the configured taps.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubSearchResponse {
    /// JSON array of `{name, description, source, path, tags}` candidate skills.
    pub results_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubImportRequest {
    /// `owner/repo[/path]` (GitHub tap form) or an `https://` URL to a SKILL.md.
    pub source: String,
    /// Branch/tag/sha for the GitHub form; ignored for URL imports.
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubImportResponse {
    pub skill_id: String,
    /// JSON `{clean: bool, findings: [...]}` injection-scan verdict.
    pub verdict_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubApproveRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubApproveResponse {
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubRejectRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsHubRejectResponse {
    pub rejected: bool,
}

// Skills curator (Harness plan §3.2 — grouping/umbrella). A review pass proposes
// merge/umbrella/archive actions (no autonomous mutation); the response carries a
// structured proposal JSON + a snapshot id. Apply executes an admin-approved subset
// against the live snapshot; rollback restores the captured pre-apply rows. All
// handlers Admin-only.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorRunRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorRunResponse {
    /// JSON `{actions: [{action, skill_ids, target_name?, rationale}]}`.
    pub proposal_json: String,
    /// Handle for a subsequent apply / rollback against this proposal.
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorApplyRequest {
    pub snapshot_id: String,
    /// JSON array of approved action indices (into the proposal's `actions`).
    pub approved_actions_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorApplyResponse {
    /// Number of skills mutated (archived + umbrellas created).
    pub mutated: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorRollbackRequest {
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SkillsCuratorRollbackResponse {
    /// Number of skills restored.
    pub restored: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SkillsPayload {
    ListRequest(SkillsListRequest),
    ListResponse(SkillsListResponse),
    DetailRequest(SkillsDetailRequest),
    DetailResponse(SkillsDetailResponse),
    UpsertRequest(SkillsUpsertRequest),
    UpsertResponse(SkillsUpsertResponse),
    DeleteRequest(SkillsDeleteRequest),
    DeleteResponse(SkillsDeleteResponse),
    ForkRequest(SkillsForkRequest),
    ForkResponse(SkillsForkResponse),
    HubSearchRequest(SkillsHubSearchRequest),
    HubSearchResponse(SkillsHubSearchResponse),
    HubImportRequest(SkillsHubImportRequest),
    HubImportResponse(SkillsHubImportResponse),
    HubApproveRequest(SkillsHubApproveRequest),
    HubApproveResponse(SkillsHubApproveResponse),
    HubRejectRequest(SkillsHubRejectRequest),
    HubRejectResponse(SkillsHubRejectResponse),
    CuratorRunRequest(SkillsCuratorRunRequest),
    CuratorRunResponse(SkillsCuratorRunResponse),
    CuratorApplyRequest(SkillsCuratorApplyRequest),
    CuratorApplyResponse(SkillsCuratorApplyResponse),
    CuratorRollbackRequest(SkillsCuratorRollbackRequest),
    CuratorRollbackResponse(SkillsCuratorRollbackResponse),
}

// ----- Agents registry (Harness plan §3.3) -----
//
// CRUD over the `agents` table plus read-only views over runtime `agent_runs`
// and a pickable tool catalog. All collection payloads carry pre-serialized
// JSON (`*_json`) just like SkillsBody — the DB rows are too wide and too
// evolution-prone to mirror field-by-field on the wire, and the dashboard
// parses them straight into its editor model. Run control (spawn/wait/cancel)
// and the RunEvents push channel are phase 6 and deliberately absent here.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsListRequest {
    pub enabled: Option<bool>,
    pub routable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsListResponse {
    pub agents_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsDetailRequest {
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsDetailResponse {
    pub agent_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsUpsertRequest {
    pub agent_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsUpsertResponse {
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsDeleteRequest {
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentsDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunsListRequest {
    pub agent_id: Option<String>,
    pub status: Option<String>,
    pub parent_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunsListResponse {
    pub runs_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunDetailRequest {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunDetailResponse {
    pub run_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ToolsCatalogRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ToolsCatalogResponse {
    pub tools_json: String,
}

// ----- Run interaction replies (Harness plan §3.13) -----
//
// A run that asks the operator a question (`core.ask_user` / the `ask_user`
// block) or needs a permission grant parks in `waiting_user`; the dashboard
// answers it over these requests. `question_id` / `request_id` is the pending
// interaction id (server-minted, surfaced in the progress event). ACL: the run's
// principal or an admin (enforced by the handler). The subscribe/event-push side
// of the RunEvents channel is stage D and deliberately absent here.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunReplyRequest {
    pub run_id: String,
    pub question_id: String,
    /// Free-text answer, or the chosen option's label.
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunReplyResponse {
    /// True when a pending question with that id was waiting and got the reply.
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentPermissionReplyRequest {
    pub run_id: String,
    pub request_id: String,
    /// One of: "deny" | "allow_once" | "allow_for_run" | "always".
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentPermissionReplyResponse {
    pub delivered: bool,
}

// ----- Run control: cancel (§3.6) -----
//
// Cancel one in-flight run. The handler signals the run's cancel token through
// the process-global `AgentRunManager`. ACL is the run principal or an admin.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunCancelRequest {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunCancelResponse {
    /// True when a live run was signalled (false = run already terminal/unknown).
    pub cancelled: bool,
}

// ----- Run events: subscribe + push (§3.11 C) -----
//
// The dashboard subscribes to a scope (the chat session id, or one run id) and
// receives ephemeral `AgentRunEvent` frames over the existing WS/WT stream
// channel — the same mechanism chat streaming already uses. Events are NOT
// persisted (durable record is `run_log`); on reconnect the UI reconciles from
// `RunDetail` and re-subscribes. ACL: a session scope is always the caller's
// own session; a run scope must resolve to the caller's principal or an admin.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AgentRunEventScope {
    /// All runs published under the caller's chat session id.
    Session { session_id: String },
    /// One specific run (and its own scope only).
    Run { run_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunEventsSubscribeRequest {
    pub scope: AgentRunEventScope,
}

/// One progress event pushed to a subscriber. Mirrors the engine
/// `ProgressEvent` enum (flow_engine::dispatchers::progress) flattened onto the
/// wire: `kind` is the event discriminant, the rest are kind-specific fields
/// (absent fields stay empty / zero). `scope` echoes the broadcast key the
/// event arrived under so a multiplexed subscriber can route it.
#[derive(Debug, Clone, Default, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunEvent {
    pub scope: String,
    /// One of: node_started | node_finished | iteration_started |
    /// iteration_finished | map_element | tool_call_started | tool_call_finished
    /// | compaction | child_spawned | child_finished | router_decision |
    /// user_question | permission_request | interaction_resolved.
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// Identyfikator wywolania narzedzia — paruje `tool_call_started` z
    /// `tool_call_finished`, gdy kilka wywolan tej samej nazwy leci rownolegle.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    #[serde(default)]
    pub n: u32,
    #[serde(default)]
    pub max: u32,
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub total: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub selected: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub interaction_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub addon_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permission: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
}

// ----- Run start + builder assist -----
//
// RunStart spawns one attended background run for the calling admin session
// (the dashboard "run now" button). BuilderAssist is a short LLM-backed
// conversation that drafts an agent definition; the transcript and the result
// travel as pre-serialized JSON (`*_json`), matching the rest of this domain.

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunStartRequest {
    pub agent_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentRunStartResponse {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentBuilderAssistRequest {
    /// JSON array of `{"role":"user"|"assistant","content":"..."}` turns.
    pub messages_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AgentBuilderAssistResponse {
    /// JSON object `{"reply":String,"proposal":null|{...}}`.
    pub result_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AgentsPayload {
    ListRequest(AgentsListRequest),
    ListResponse(AgentsListResponse),
    DetailRequest(AgentsDetailRequest),
    DetailResponse(AgentsDetailResponse),
    UpsertRequest(AgentsUpsertRequest),
    UpsertResponse(AgentsUpsertResponse),
    DeleteRequest(AgentsDeleteRequest),
    DeleteResponse(AgentsDeleteResponse),
    RunsListRequest(AgentRunsListRequest),
    RunsListResponse(AgentRunsListResponse),
    RunDetailRequest(AgentRunDetailRequest),
    RunDetailResponse(AgentRunDetailResponse),
    ToolsCatalogRequest(ToolsCatalogRequest),
    ToolsCatalogResponse(ToolsCatalogResponse),
    RunReplyRequest(AgentRunReplyRequest),
    RunReplyResponse(AgentRunReplyResponse),
    PermissionReplyRequest(AgentPermissionReplyRequest),
    PermissionReplyResponse(AgentPermissionReplyResponse),
    RunCancelRequest(AgentRunCancelRequest),
    RunCancelResponse(AgentRunCancelResponse),
    RunEventsSubscribeRequest(AgentRunEventsSubscribeRequest),
    RunEvent(AgentRunEvent),
    // Append-only past this point: ciborium encodes variants by index, so
    // inserting or reordering above breaks older peers on the wire.
    RunStartRequest(AgentRunStartRequest),
    RunStartResponse(AgentRunStartResponse),
    BuilderAssistRequest(AgentBuilderAssistRequest),
    BuilderAssistResponse(AgentBuilderAssistResponse),
}

// =============================================================================
// Sync conflict manager — admin-only conflict review and resolution.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SyncConflictResolution {
    KeepLocal,
    Ignore,
    AcceptRemote,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncConflictRow {
    pub operation_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub table_name: String,
    pub resource_type: String,
    pub resource_id: String,
    pub action: String,
    pub source_node_id: String,
    pub error_kind: String,
    pub error_message: String,
    pub status: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncConflictsListRequest {
    pub org_id: String,
    pub addon_id: String,
    pub status: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncConflictsListResponse {
    pub conflicts: Vec<SyncConflictRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncConflictResolveRequest {
    pub org_id: String,
    pub addon_id: String,
    pub operation_id: String,
    pub resolution: SyncConflictResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncConflictResolveResponse {
    pub operation_id: String,
    pub status: String,
    pub resolution: String,
    pub rows_affected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SyncConflictPayload {
    ListRequest(SyncConflictsListRequest),
    ListResponse(SyncConflictsListResponse),
    ResolveRequest(SyncConflictResolveRequest),
    ResolveResponse(SyncConflictResolveResponse),
}

// =============================================================================
// Sync storage pressure — admin-only disk and ledger storage report.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SyncStoragePressureLevel {
    Ok,
    Info,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncStoragePathUsage {
    pub label: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncStorageReportRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SyncStorageReportResponse {
    pub root: String,
    pub level: SyncStoragePressureLevel,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub free_percent_bps: Option<u32>,
    pub sqlite_bytes: u64,
    pub fjall_ledger_bytes: u64,
    pub snapshot_blob_bytes: u64,
    pub final_blob_bytes: u64,
    pub pending_blob_chunk_bytes: u64,
    pub large_blob_block_bytes: u64,
    pub paths: Vec<SyncStoragePathUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum SyncStoragePayload {
    ReportRequest(SyncStorageReportRequest),
    ReportResponse(SyncStorageReportResponse),
}

// =============================================================================
// Portainer — Docker container ops (migration-map #248-#259)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    /// "running" | "stopped" | "paused" | "exited".
    pub state: String,
    pub created_at_epoch: u64,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ContainerLogChunk {
    pub container_id: String,
    pub stream: String, // "stdout" | "stderr"
    pub line: String,
    pub ts_epoch: u64,
}

/// Wszystkie operacje Portainer/Docker spakowane w jeden slot `MessageBody`.
/// Wzorzec „1 slot per feature" — odciaza globalny limit 256 wariantow CBOR 0.8
/// i utrzymuje wszystkie req/res/stream-chunk pod jedna dyskryminanta.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum ContainerPayload {
    /// Klient -> serwer: lista kontenerow widzianych przez node.
    ListRequest,
    /// Serwer -> klient: odpowiedz z lista podsumowan.
    ListResponse { containers: Vec<ContainerSummary> },
    /// Klient -> serwer: start danego kontenera (admin only).
    StartRequest { container_id: String },
    /// Serwer -> klient: ack startu (zmiana stanu obserwowana przez ListResponse).
    StartResponse { started: bool },
    /// Klient -> serwer: stop danego kontenera (admin only).
    StopRequest { container_id: String },
    /// Serwer -> klient: ack stopa.
    StopResponse { stopped: bool },
    /// Klient -> serwer: otworz stream logow (R-STREAM); `follow=true` => tail.
    LogStreamRequest { container_id: String, follow: bool },
    /// Serwer -> klient: pojedynczy chunk logu (stdout/stderr).
    LogChunkBody(ContainerLogChunk),
}

// =============================================================================
// Voice profiles — speaker enrollment (migration-map #325-#332)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VoiceProfileSummary {
    pub id: String,
    pub display_name: String,
    pub embedding_count: u32,
    pub created_at_epoch: u64,
}

// =============================================================================
// TTS rules — text→speech routing rules (migration-map #316-#319)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TtsRule {
    pub id: String,
    /// Wzorzec do dopasowania w tekscie (substring).
    pub pattern: String,
    /// Tekst zamiennika — `pattern` zamieniany na to przed TTS. Historyczna
    /// nazwa pola (`voice_id`); funkcjonalnie to ZAMIENNIK substytucji TTS, nie
    /// glos. UI pokazuje jako "Zamiennik".
    pub voice_id: String,
    pub priority: i32,
}

// =============================================================================
// PII rules — personally-identifiable-info redaction (migration-map #239-#242)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct WakeWord {
    pub id: i64,
    pub word: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Sub-action `WakeWordRequest` — list/create/toggle/delete.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum WakeWordOp {
    List,
    Create { word: String },
    Toggle { id: i64, enabled: bool },
    Delete { id: i64 },
}

// =============================================================================
// Fast-path patterns — bypass routing for known prompts (migration-map #61-#64)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustedKeysSyncEvent {
    /// Lista trusted Ed25519 public keys (kazdy 32 bajty).
    pub trusted_keys: Vec<[u8; 32]>,
    /// Aktualny epoch sender'a (do replay protection).
    pub epoch: u32,
}

/// Inner-enum pack — wszystkie trust eventy w jednym slocie MessageBody.
/// Konsolidacja zwalnia slot pod nowe warianty (CBOR 0.8 ma twardy limit 256).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum MeshTrustEventPayload {
    /// Broadcast cofniecia trust (mesh discriminant 0x23).
    Revoked(MeshTrustRevokedEvent),
    /// Post-pairing sync listy zaufanych kluczy (mesh discriminant 0x24).
    KeysSync(MeshTrustedKeysSyncEvent),
}

// =============================================================================
// Mesh peers (R-LIST + W-ACTION archetypy, migration-map #87-#92)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPeerSummary {
    pub node_id: [u8; 32],
    pub display_name: String,
    /// "trusted" / "pending" / "revoked" / "online".
    pub trust_state: String,
    /// Hostname lub ostatni znany IP.
    pub endpoint: Option<String>,
    pub last_seen_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairInitRequest {
    pub node_id: [u8; 32],
    /// PIN wpisany przez administratora (6 cyfr).
    pub pin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairInitResponse {
    pub pair_id: String,
    pub expires_at_epoch: u64,
}

// =============================================================================
// Mesh extended (FAZA 1a/1b: read-only + write actions for admin/dashboard).
// Helper structs are mirrored 1:1 by `mesh_node_info_to_js` and the
// per-variant encoders in `tentaflow-protocol-wasm`.
// =============================================================================

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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
    /// PCI bus id as nvidia-smi prints it, e.g. "00000000:82:00.0" (32-bit domain).
    #[serde(default)]
    pub pci_bus_id: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub power_limit_w: Option<f32>,
    #[serde(default)]
    pub fan_speed_percent: Option<u8>,
    /// Maximum PCIe link generation / lane width — the slot/board capability.
    #[serde(default)]
    pub pcie_link_gen: Option<u8>,
    #[serde(default)]
    pub pcie_link_width: Option<u8>,
    /// Momentarily negotiated link; an idle card drops to Gen1, so it reflects
    /// the power state, not the hardware.
    #[serde(default)]
    pub pcie_link_gen_current: Option<u8>,
    #[serde(default)]
    pub pcie_link_width_current: Option<u8>,
}

/// One inter-GPU link from `nvidia-smi topo -m`; `a`/`b` are the node's GPU
/// indices (same order as `MeshNodeInfo.gpus`), always `a < b`. `link` is
/// "NVL" | "PIX" | "PXB" | "PHB" | "NODE" | "SYS" | "UNKNOWN".
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshGpuLink {
    pub a: u32,
    pub b: u32,
    pub link: String,
    pub p2p_ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeModel {
    pub alias: String,
    pub kind: Option<String>,
    pub backend: Option<String>,
    pub size_mb: Option<u64>,
    pub loaded: bool,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeContainer {
    pub name: String,
    pub image: String,
    pub status: String,
    pub cpu_percent: Option<f32>,
    pub memory_mb: Option<f32>,
    pub memory_limit_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeRoute {
    pub hops: u32,
    pub direct: bool,
    pub next_hop: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshConnectionPathInfo {
    pub transport: String,
    pub address: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum MeshConnState {
    Disconnected,
    Connecting,
    Connected,
    Degraded,
    Reconnecting,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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
    /// Inter-GPU PCIe/NVLink topology of the node; empty when unknown.
    #[serde(default)]
    pub gpu_links: Vec<MeshGpuLink>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeListResponse {
    pub nodes: Vec<MeshNodeInfo>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeDetailRequest {
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeDetailResponse {
    pub node: MeshNodeInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPendingPair {
    pub pair_id: String,
    pub remote_node_id: String,
    pub remote_hostname: Option<String>,
    pub remote_ip: Option<String>,
    pub initiated_at: i64,
    pub state: String,
    pub pin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPendingListResponse {
    pub pending: Vec<MeshPendingPair>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesEntry {
    pub service_name: String,
    pub node_id: String,
    pub status: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesListResponse {
    pub services: Vec<MeshServicesEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustedNode {
    pub node_id: String,
    pub hostname: Option<String>,
    pub trusted_since_epoch: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustedListResponse {
    pub trusted: Vec<MeshTrustedNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingStartResponse {
    pub pair_id: String,
    pub pin: String,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingConfirmRequest {
    pub pair_id: String,
    pub pin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingConfirmResponse {
    pub ok: bool,
    pub trusted_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRejectRequest {
    pub pair_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRejectResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustRevokeRequest {
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustRevokeResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustRetrustRequest {
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshTrustRetrustResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshConnectRequest {
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshConnectResponse {
    pub ok: bool,
    pub remote_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeCommandRequest {
    pub node_id: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeCommandResponse {
    pub ok: bool,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeNetworkConfigRequest {
    pub node_id: String,
    pub interface_name: String,
    pub config_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeshNodeNetworkConfigResponse {
    pub ok: bool,
}

// =============================================================================
// Sync baseline-adopt admin (R-LIST + W-ACTION). Admin wskazuje dawce baseline'u
// i steruje pojedyncza adopcja single-flight: lista kandydatow, start, status,
// odblokowanie zawieszonego stanu. Donorow widac tylko sposrod zaufanych peerow.
// =============================================================================

/// Lokalnie znane podsumowanie baseline'u kandydata. Pelne liczby dawcy poznaje
/// sie dopiero z naglowka transferu (`BaselineHeader`), wiec dla listy to pole
/// jest opcjonalne — `None` gdy lokalnie nic nie wiadomo o zawartosci dawcy.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineDonorSummary {
    pub org_name: String,
    pub users: u64,
    pub flows: u64,
    pub roles: u64,
}

/// Kandydat na dawce baseline'u: zaufany sparowany peer. `trusted` jest zawsze
/// `true` na wyjsciu (filtrujemy nie-zaufanych po stronie hosta) — pole zostaje,
/// by frontend mogl jawnie pokazac status zaufania bez zgadywania.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineDonorCandidate {
    pub node_id: String,
    pub display_name: String,
    pub trusted: bool,
    pub summary: Option<BaselineDonorSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineDonorListResponse {
    pub candidates: Vec<BaselineDonorCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAdoptStartRequest {
    /// node_id (hex) wskazanego dawcy. Musi byc zaufanym sparowanym peerem.
    pub donor_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAdoptStartResponse {
    pub ok: bool,
    /// `true` gdy adopcja faktycznie wystartowala (rola joiner, pull w tle).
    pub started: bool,
    /// Komunikat diagnostyczny (np. powod odmowy single-flight).
    pub message: String,
}

/// Faza adopcji widziana przez admina. `None` = brak trwajacej/zakonczonej
/// adopcji. Pozostale warianty mapuja `core_baseline::BaselinePhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum BaselineAdoptPhaseTag {
    None,
    Elected,
    Receiving,
    Importing,
    Imported,
    Completed,
}

/// Raport importu baseline'u (dostepny dopiero po `Completed`). Lustro
/// `core_baseline::BaselineImportReport` w ksztalcie wire.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAdoptReport {
    pub donor_org_id: String,
    pub users_merged_by_email: u64,
    pub users_joined_donor_org: u64,
    pub collisions_suffixed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAdoptStatusResponse {
    pub phase: BaselineAdoptPhaseTag,
    /// node_id (hex) drugiej strony adopcji, jesli stan istnieje.
    pub peer: Option<String>,
    /// Czy lokalny nod jest joinerem (`true`) czy dawca (`false`); `None` gdy
    /// brak stanu.
    pub is_joiner: Option<bool>,
    /// Raport dostepny tylko gdy faza == Completed; inaczej `None`.
    pub report: Option<BaselineAdoptReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAdoptClearResponse {
    pub ok: bool,
    /// `true` gdy stan zostal wyczyszczony; `false` gdy nic nie bylo do
    /// wyczyszczenia albo czyszczenie bylo niedozwolone (aktywny import).
    pub cleared: bool,
    pub message: String,
}

// =============================================================================
// Settings (R-LIST + W-UPDATE archetypy, migration-map #147-#148)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SettingEntry {
    pub key: String,
    pub value: String,
    /// Czy wartosc powinna byc zaszyfrowana (secret).
    pub is_secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SettingsUpdateRequest {
    pub entries: Vec<SettingEntry>,
}

// =============================================================================
// Mesh & Network settings (IPv4-only enumeracja NIC + reguly bind/advertise)
// =============================================================================

/// Pojedynczy interfejs sieciowy hosta z adresami IPv4 (v6 odrzucane).
/// `kind` jest znormalizowaną kategorią dla GUI (nie surowy `InterfaceType`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    /// Nazwy interfejsow wykluczonych z advertise mesh per-karta (np. "eth3").
    /// `#[serde(default)]` zachowuje kompatybilnosc ze starszymi peerami ktorzy
    /// tego pola nie wysylaja. Pusta lista = nic nie wykluczone.
    #[serde(default)]
    pub excluded_interfaces: Vec<String>,
}

/// Snapshot zdrowia relay iroh — co backend wie o aktualnym stanie polaczenia
/// z konfigurowanym serwerem relay + faktyczny adres bind iroh endpointu.
/// `rtt_ms == 0` gdy relay unreachable; `last_success_unix_secs == 0` gdy nigdy
/// nie udalo sie zpingowac. `status` jest jedna z czterech wartosci:
/// `"connected" | "degraded" | "unreachable" | "disabled"`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
/// 1 slot w `MessageBody` zeby zmiescic sie w 256-variant limicie CBOR.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    /// Full member list (node + interface info). Populated for both list and
    /// detail responses so the UI can render members without a second request.
    #[serde(default)]
    pub members: Vec<ClusterMember>,
}

/// Single member of a cluster (node + interface info).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    /// Comma-separated RoCE device list for distributed deploy (`NCCL_IB_HCA`),
    /// e.g. "rocep1s0f0,roceP2p1s0f0" — BOTH twins of the QSFP port. Empty until
    /// the cluster RDMA auto-config has run on this member.
    #[serde(default)]
    pub rdma_devices: Option<String>,
    /// Primary RDMA IPv4 the distributed deploy binds to (QSFP socket ifname IP).
    #[serde(default)]
    pub rdma_ip: Option<String>,
    /// Netdev name of the QSFP socket interface carrying `rdma_ip` (the
    /// `NCCL_SOCKET_IFNAME`/`GLOO_SOCKET_IFNAME` bootstrap interface).
    #[serde(default)]
    pub rdma_socket_ifname: Option<String>,
    /// Netdev chosen as the cluster interconnect for this member, and its IPv4.
    /// Without them the UI cannot show WHICH card is currently in use, so a NIC
    /// picker would have nothing to preselect.
    #[serde(default)]
    pub interface_name: Option<String>,
    #[serde(default)]
    pub interface_ip: Option<String>,
    /// RoCE v2 GID index handed to `NCCL_IB_GID_INDEX`, read from the member's
    /// own GID table by the RDMA auto-config.
    #[serde(default)]
    pub rdma_gid_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterListResponse {
    pub clusters: Vec<ClusterInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDetailRequest {
    pub cluster_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDetailResponse {
    pub cluster: ClusterInfo,
    pub members: Vec<ClusterMember>,
    /// Live distributed deployment of this cluster, if any. Without it the
    /// dashboard only knew about a deployment it had started itself in the
    /// current page session, so a refresh made a running cluster look idle and
    /// offered "deploy" again. `default` keeps peers that predate the field
    /// decodable.
    #[serde(default)]
    pub deployment: Option<ClusterDeploymentInfo>,
}

/// A distributed deployment as the dashboard needs to render it: identity,
/// endpoint and per-member placement.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeploymentInfo {
    pub deployment_cluster_id: String,
    pub engine_id: String,
    pub model: String,
    pub served_model_name: String,
    pub tp_size: u32,
    pub head_node_id: String,
    pub port: u32,
    pub endpoint_url: Option<String>,
    /// "deploying" | "running" | "failed" | "stopped".
    pub status: String,
    pub created_at: String,
    pub members: Vec<ClusterDeploymentMemberInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeploymentMemberInfo {
    pub node_id: String,
    pub hostname: Option<String>,
    /// "head" | "worker".
    pub role: String,
    /// Empty for a deployment that runs no container (native bundle).
    pub container_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterCreateResponse {
    pub cluster_id: String,
}

/// Partial-update request: `None` leaves the current value untouched server-side.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterUpdateResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeleteRequest {
    pub cluster_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeleteResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterAddMemberRequest {
    pub cluster_id: String,
    pub node_id: String,
    pub interface_type: Option<String>,
    pub interface_speed_mbps: Option<u32>,
    /// Netdev interconnectu wskazany RECZNIE przez admina (np. `enp1s0f0np0`).
    /// `None` = zostaw wybor automatowi z testu polaczen. Dla istniejacego
    /// czlonka to zmiana karty — handler robi UPSERT, nie duplikat.
    #[serde(default)]
    pub interface_name: Option<String>,
    /// Adres IPv4 tej karty, uzywany jako adres interconnectu klastra.
    #[serde(default)]
    pub interface_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterAddMemberResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRemoveMemberRequest {
    pub cluster_id: String,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRemoveMemberResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterProbeStreamRequest {
    pub node_ids: Vec<String>,
    /// Cluster the probe belongs to. When present the handler persists the
    /// winning interface per member; when absent it is derived from the cluster
    /// whose member set equals `node_ids`.
    #[serde(default)]
    pub cluster_id: Option<String>,
}

/// Single probe event. `event_type` is one of "started" | "probing_pair" |
/// "result" | "complete"; the populated optional fields depend on it.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

/// Per-node interface chosen by the optimal-assignment algorithm. Streamed in
/// `ClusterProbeStreamEnd` so the UI can show the selected NIC per member.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterProbeAssignment {
    pub node_id: String,
    pub interface_name: String,
    pub interface_ip: String,
    pub interface_speed_mbps: u32,
    pub interface_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterProbeStreamEnd {
    pub total_pairs: u32,
    pub successful: u32,
    pub failed: u32,
    /// Slowest selected link across all pairs (Mbps) — the cluster bottleneck.
    #[serde(default)]
    pub bottleneck_mbps: Option<u32>,
    /// "optimal" | "partial" | "no_connections" from the assignment algorithm.
    #[serde(default)]
    pub assignment_status: Option<String>,
    /// Chosen interface per node.
    #[serde(default)]
    pub assignments: Vec<ClusterProbeAssignment>,
}

// =============================================================================
// Cluster RDMA auto-config — detect RoCE twins, assign IPs + MTU over mesh
// =============================================================================

/// Admin action triggered from cluster-detail: detect each member's RoCE "twin"
/// interfaces and bring up the unconfigured ones (assign IP on a dedicated RDMA
/// subnet + set MTU) so distributed deploy gets full RDMA bandwidth. The
/// `sudo_password` is needed by the per-node `NetworkConfig` mesh command and is
/// carried only for the duration of the request (never persisted).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRdmaConfigureRequest {
    pub cluster_id: String,
    pub sudo_password: String,
    /// Target MTU for the RoCE interfaces (default 9000 jumbo frames when None).
    #[serde(default)]
    pub mtu: Option<u32>,
}

/// One RoCE interface acted on (or inspected) during cluster RDMA auto-config.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRdmaInterface {
    pub netdev: String,
    pub roce_device: String,
    /// IPv4 the interface ended up with (assigned or pre-existing).
    pub ipv4: Option<String>,
    pub mtu: u32,
    /// "primary" (already carried the cluster interconnect IP) | "secondary"
    /// (the previously-unconfigured twin we brought up).
    pub role: String,
    /// "assigned" (we set IP+MTU) | "mtu_only" (IP kept, MTU set) | "unchanged"
    /// (already correct) | "failed".
    pub action: String,
}

/// Per-member outcome of the cluster RDMA auto-config.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRdmaMemberStatus {
    pub node_id: String,
    pub hostname: String,
    /// "online" | "offline".
    pub status: String,
    pub interfaces: Vec<ClusterRdmaInterface>,
    /// Non-empty when this member could not be configured (offline, no RoCE,
    /// missing primary IP, mesh command failure).
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterRdmaConfigureResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub members: Vec<ClusterRdmaMemberStatus>,
}

// =============================================================================
// Cluster distributed deploy — ONE model split across N members (vLLM TP=N)
// orchestrated over the mesh using each member's D1 RoCE config.
// =============================================================================

/// Frontend (D4) → coordinator: deploy one model split across the WHOLE cluster
/// with vLLM tensor-parallel (TP = total GPUs). The coordinator computes
/// head/worker roles from `cluster_members` + ich D1 RoCE config (`rdma_devices`,
/// `rdma_ip`, `rdma_socket_ifname`) and sends a per-node `ServiceDeployDistributed`
/// over the mesh. This chunk is the DOCKER path (deploy_method=docker); the model
/// is assumed already present on every member.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeployRequest {
    pub cluster_id: String,
    /// Engine id selecting the image + command dialect ("vllm" | "vllm-spark").
    pub engine_id: String,
    /// Custom HF repo to serve (wins over `model_preset_id`).
    #[serde(default)]
    pub model_repo: Option<String>,
    /// Preset id (manifest `model_presets`) when no custom repo is given.
    #[serde(default)]
    pub model_preset_id: Option<String>,
    /// Routing alias (`--served-model-name`); defaults to the model id.
    #[serde(default)]
    pub served_model_name: Option<String>,
    /// `--gpu-memory-utilization` (default 0.90 when None).
    #[serde(default)]
    pub gpu_memory_utilization: Option<f32>,
    /// `--max-model-len` (default 8192 when None).
    #[serde(default)]
    pub max_model_len: Option<u32>,
    /// Head OpenAI port (default 8100 when None).
    #[serde(default)]
    pub port: Option<u16>,
    /// GPUs per member (homogeneous cluster); default 1 (one GB10 per Spark).
    /// `tp_size` = members * gpus_per_node.
    #[serde(default)]
    pub gpus_per_node: Option<u32>,
    /// Extra user config (vllm_args, gpu_select_mode, gpu_ids) as JSON, forwarded
    /// verbatim into each member's deploy.
    #[serde(default)]
    pub config_json: Option<String>,
    /// Bounded wait for the member CONTAINER to come up (image build + container
    /// start) BEFORE the Ray-GCS/serve clocks start — a slow first image build
    /// extends THIS phase, not the GCS phase (default 600 s).
    #[serde(default)]
    pub build_timeout_secs: Option<u32>,
    /// Bounded wait for the head Ray GCS to come up before joining workers
    /// (default 60 s).
    #[serde(default)]
    pub gcs_timeout_secs: Option<u32>,
    /// Bounded wait for the full cluster to serve `/v1/models` after the workers
    /// joined (default 600 s — a 31B model can take a few minutes to load).
    #[serde(default)]
    pub ready_timeout_secs: Option<u32>,
    /// Optional per-model pricing captured at deploy time (persisted to
    /// `model_pricing` for the served model). All four are independent; any
    /// non-None value triggers an upsert, unset values default to 0.0.
    #[serde(default)]
    pub prompt_per_1k: Option<f64>,
    #[serde(default)]
    pub completion_per_1k: Option<f64>,
    #[serde(default)]
    pub audio_per_min: Option<f64>,
    #[serde(default)]
    pub image_each: Option<f64>,
}

/// Per-member outcome of a cluster distributed deploy.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeployMemberStatus {
    pub node_id: String,
    pub hostname: String,
    /// "head" | "worker".
    pub role: String,
    pub ok: bool,
    /// Deploy slug on that member (log streaming key) when the launch started.
    pub deploy_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeployResponse {
    pub ok: bool,
    /// UUID grouping head + workers; pass it to `ClusterDeployStopRequest`.
    pub deployment_cluster_id: String,
    /// node_id of the member that runs the OpenAI endpoint.
    pub head_node_id: String,
    /// Informational head endpoint (`http://<head>:<port>/v1`).
    pub endpoint_url: Option<String>,
    pub members: Vec<ClusterDeployMemberStatus>,
    pub message: Option<String>,
}

/// Tear down a running cluster distributed deployment (head + all workers).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeployStopRequest {
    pub cluster_id: String,
    pub deployment_cluster_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ClusterDeployStopResponse {
    pub ok: bool,
    pub members: Vec<ClusterDeployMemberStatus>,
    pub message: Option<String>,
}

// =============================================================================
// Flows phase 3 — partial update, node template palette, version history
// =============================================================================

/// Partial update — fields left `None` keep their existing server-side value.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowUpdateRequest {
    pub flow_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Full DAG JSON replacement when present.
    pub flow_json: Option<String>,
    /// Raw status column ("active" | "draft" | "decoded" ...).
    pub status: Option<String>,
    /// Update or clear the catalog publish name. `Some(Some("..."))`
    /// publishes / re-publishes; `Some(None)` un-publishes; `None` leaves
    /// the existing value untouched. Validated against the catalog before
    /// the row is written.
    pub published_model_name: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowUpdateResponse {
    pub ok: bool,
}

/// Single entry in the node-template palette shown by the flow builder.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowNodeTemplatesListResponse {
    pub templates: Vec<FlowNodeTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionListRequest {
    pub flow_id: String,
}

/// Lightweight view (no full flow_json) for the version-history list.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionListResponse {
    pub versions: Vec<FlowVersionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionGetRequest {
    pub flow_id: String,
    pub version_id: String,
}

/// Full version payload including embedded DAG JSON for diff/restore.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionGetResponse {
    pub version: FlowVersionFull,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionRestoreRequest {
    pub flow_id: String,
    pub version_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowVersionRestoreResponse {
    pub ok: bool,
}

/// Resets a factory flow (`FACTORY_FLOW_IDS`) to its canonical graph. The
/// current graph is snapshotted into `flow_versions` first, so the action is
/// itself reversible through `FlowVersionRestoreRequest`. The reply is the
/// refreshed `FlowDetailResponse`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct FlowFactoryRestoreRequest {
    pub flow_id: String,
}

// ----- SSO / TLS / NGC -----

/// Pojedynczy wpis providera SSO dla listy admina. `client_secret` nie jest
/// zwracany do GUI — jedynie pola nie-sekretne. `default_group_id` jest opcjonalny
/// (Option) bo provider moze nie mapowac uzytkownikow do grupy domyslnej.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProviderEntry {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub discovery_url: String,
    pub enabled: bool,
    pub auto_create_users: bool,
    pub default_group_id: Option<String>,
    pub created_at: String,
}

/// Response: lista wszystkich skonfigurowanych providerow SSO (Admin only).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProvidersListResponse {
    pub providers: Vec<SsoProviderEntry>,
}

/// Request: utworz nowego providera SSO/OIDC. `client_secret` jest szyfrowany
/// po stronie serwera przed zapisem do bazy (cipher w AppState).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProviderCreateRequest {
    pub name: String,
    pub provider_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub discovery_url: String,
    pub auto_create_users: bool,
    pub default_group_id: Option<String>,
}

/// Response: potwierdzenie utworzenia providera SSO.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProviderCreateResponse {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
}

/// Request: usun providera SSO po id.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProviderDeleteRequest {
    pub id: i64,
}

/// Response: flagaczy provider istnial i zostal usuniety.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct SsoProviderDeleteResponse {
    pub deleted: bool,
}

/// Response: status konfiguracji TLS (obecnosc cert/key w settings, bez ujawniania wartosci).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TlsStatusResponse {
    pub has_cert: bool,
    pub has_key: bool,
}

/// Response: status konfiguracji NGC (czy API key jest ustawiony, bez ujawniania wartosci).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct NgcStatusResponse {
    pub configured: bool,
}

// =============================================================================
// Models / aliases / catalog (FAZA 2 + FAZA 5 — REST -> binary)
// =============================================================================

/// One node hosting a service model. Reused inside `CatalogEntryKind::ServiceModel`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum CatalogEntryKindWire {
    ServiceModel {
        instances: Vec<CatalogModelInstance>,
    },
    Flow {
        flow_id: String,
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct CatalogEntryWire {
    pub id: String,
    pub kind: CatalogEntryKindWire,
    pub service_surfaces: Vec<String>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    /// Poziomy rozumowania wspierane przez model. `#[serde(default)]`, bo wezel
    /// ze starsza binarka wysle wpis bez tego pola — dekoder musi to znosic.
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    pub diagnostic: Option<CatalogDiagnosticWire>,
    /// `tentaflow-service` | `tentaflow-flow` | `tentaflow-alias`.
    pub owned_by: String,
}

/// Catalog list request. The wire form lets callers narrow by surface and
/// admin tooling opt into seeing entries hidden from `/v1/models`.
#[derive(Debug, Clone, PartialEq, Eq, Default, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct CatalogListResponse {
    pub entries: Vec<CatalogEntryWire>,
    pub version: u64,
}

/// Single model alias entry mapped from `DbModelAlias`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasEntry {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: bool,
    pub fallback_targets: Option<String>,
    pub strategy: Option<String>,
}

/// Response for `ModelAliasListRequest`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasListResponse {
    pub aliases: Vec<ModelAliasEntry>,
}

/// Request: create new model alias (Admin).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasCreateRequest {
    pub alias: String,
    pub target_model: String,
    pub strategy: Option<String>,
    pub fallback_targets: Option<String>,
}

/// Response: id of the newly created alias row.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasCreateResponse {
    pub id: i64,
}

/// Request: update existing model alias by id (Admin).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasUpdateRequest {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: Option<bool>,
    pub strategy: Option<String>,
    pub fallback_targets: Option<String>,
}

/// Response: whether update succeeded.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasUpdateResponse {
    pub ok: bool,
}

/// Request: delete alias by id (Admin).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasDeleteRequest {
    pub id: i64,
}

/// Response: whether delete succeeded.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelAliasDeleteResponse {
    pub ok: bool,
}

/// Single NIM catalog container entry mirrored from `api_nim::NimContainer`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct NimCatalogListResponse {
    pub containers: Vec<NimContainerEntry>,
    pub error: Option<String>,
}

// =============================================================================
// vLLM deploy recommend (TP/PP/ctx_len/max_seqs/kv_dtype calculator).
// f64 fields drop Eq; PartialEq only.
// =============================================================================

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmGpuInfo {
    pub index: u32,
    pub name: String,
    pub memory_gb: f64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
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
    /// Sciezka pliku .gguf w repo (np. `model-q4_k_m.gguf`). Ustawiana przez
    /// frontend gdy silnik to llama.cpp - repozytoria GGUF nie maja config.json,
    /// wiec metadane architektury czytamy z naglowka pliku zamiast config.json.
    #[serde(default)]
    pub gguf_file: Option<String>,
    /// Silnik deploymentu (`vllm` lub `llama-cpp`). Decyduje, ktorym modelem
    /// fizycznym liczony jest VRAM. Opcjonalne dla kompatybilnosci wire — None
    /// (oraz wykryty GGUF) mapuje na domyslny vLLM/llama.cpp w handlerze.
    #[serde(default)]
    pub engine: Option<String>,
    /// Typ kwantyzacji KV dla strony V (osobny od K dla llama.cpp, np. K=q8_0
    /// V=q4_0). None → rowny K/V. Wire-additive, wiec `#[serde(default)]`.
    #[serde(default)]
    pub kv_cache_dtype_v: Option<String>,
    /// Limit `--max-num-batched-tokens` (vLLM) — driver szczytu aktywacji w
    /// modelu puli KV. None → handler wylicza default z max_model_len.
    #[serde(default)]
    pub max_num_batched_tokens: Option<u64>,
    /// Metoda deployu (`docker` / `native`) — rozstrzyga baze komendy w
    /// podgladzie (`launch_command`). None → docker. Wire-additive.
    #[serde(default)]
    pub deploy_method: Option<String>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmConfig {
    pub tensor_parallel: u32,
    pub pipeline_parallel: u32,
    pub max_model_len: u64,
    pub max_num_seqs: u64,
    pub kv_cache_dtype: String,
    pub gpu_memory_utilization: f64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
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

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
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
    /// Rozmiar puli KV (vLLM: util*VRAM - wagi - aktywacje) w GB. To pula
    /// resztkowa, nie skladnik wymagany — UI pokazuje "pula KV X GiB".
    #[serde(default)]
    pub kv_pool_gb: f64,
    /// Ile tokenow miesci pula KV (`kv_pool_bytes / kv_per_token_per_gpu`).
    #[serde(default)]
    pub pool_tokens: u64,
    /// Informacyjna wspolbieznosc: `pool_tokens / max_model_len`. Ile pelnych
    /// sekwencji o dlugosci max_model_len zmiesci sie naraz.
    #[serde(default)]
    pub concurrent_full_len_seqs: f64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct DeployVllmGpuCompatibility {
    pub used_tp: u32,
    pub used_pp: u32,
    pub uses_all_gpus: bool,
    pub clean_partition: bool,
    pub better_gpu_counts: Vec<u32>,
    pub warning: Option<String>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
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
    /// Env vars from the matched vLLM recipe (e.g. VLLM_USE_FLASHINFER_MOE_FP4
    /// on Blackwell). The wizard sends these back as `engine_env` on deploy.
    /// Empty when no recipe matched the model.
    #[serde(default)]
    pub recommended_env: std::collections::HashMap<String, String>,
    /// hf_id of the applied recipe (for the "recipe applied" GUI badge). None
    /// when no recipe matched.
    #[serde(default)]
    pub recipe_applied: Option<String>,
    /// Pelna finalna komenda startowa silnika (baza + argumenty) w jego natywnym
    /// dialekcie, z placeholderami env (`$MODEL`/`$PORT`). Wizard pokazuje ja jako
    /// edytowalny podglad; edycja leci z powrotem jako `launch_command_override`
    /// w config_json. Pusta dla silnikow bez strojonych argumentow. Wire-additive.
    #[serde(default)]
    pub launch_command: String,
    /// The checkpoint ships its own MTP / NextN draft head (config
    /// `mtp_num_hidden_layers` / `num_nextn_predict_layers` > 0), so the wizard
    /// can offer `--speculative-config {"method":"mtp"}` without a draft repo.
    #[serde(default)]
    pub native_mtp_available: bool,
}

// =============================================================================
// F1a §6.6 — model / alias access control (visibility + consumer grants +
// per-addon `uses_*` declarations). Wire contract for the admin Access UI.
// =============================================================================

/// One consumer grant row in the access timeline (alias or model). `revoked_at`
/// = None ⇒ the grant is currently active; a value marks an admin-revoked
/// grant kept as a tombstone. Timestamps are Unix seconds (u64 — JS BigInt
/// validators tolerate them).
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccessConsumerEntry {
    pub addon_id: String,
    pub granted_by_user_id: Option<i64>,
    pub granted_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

/// One per-addon `[[uses_alias]]` / `[[uses_model]]` declaration row with its
/// reconciled grant state. `owner_visibility` is the current visibility of the
/// target (`private`/`restricted`/`public` for aliases; `restricted`/`public`
/// for models) so the Access tab can explain WHY a row is pending.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonUsesEntry {
    pub target: String,
    pub required: bool,
    pub reason: String,
    pub grant_status: String,
    pub owner_visibility: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasConsumerListRequest {
    pub alias_id: i64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasConsumerListResponse {
    pub alias_id: i64,
    pub consumers: Vec<AccessConsumerEntry>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasConsumerGrantRequest {
    pub alias_id: i64,
    pub addon_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasConsumerRevokeRequest {
    pub alias_id: i64,
    pub addon_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AliasVisibilitySetRequest {
    pub alias_id: i64,
    /// One of `private` / `restricted` / `public`.
    pub visibility: String,
}

/// Shared response for every access mutation (grant/revoke/visibility set,
/// alias and model). `transitions` lists the dependent `addon_uses_*` rows
/// whose `grant_status` flipped as a side effect, so the UI can refresh them
/// without a second round-trip.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccessMutationResponse {
    pub ok: bool,
    pub transitions: Vec<AccessTransition>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccessTransition {
    pub addon_id: String,
    pub before: String,
    pub after: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibilityEntry {
    pub model_id: String,
    /// `restricted` (default) or `public`.
    pub visibility: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibilityListResponse {
    pub models: Vec<ModelVisibilityEntry>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibilitySetRequest {
    pub model_id: String,
    /// `restricted` or `public`.
    pub visibility: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelConsumerListRequest {
    pub model_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelConsumerListResponse {
    pub model_id: String,
    pub consumers: Vec<AccessConsumerEntry>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelConsumerGrantRequest {
    pub model_id: String,
    pub addon_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModelConsumerRevokeRequest {
    pub model_id: String,
    pub addon_id: String,
}

/// Install-wizard / addon Access-tab view: every access declaration of one
/// addon (aliases + models) with reconciled grant state.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonAccessListRequest {
    pub addon_id: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonAccessListResponse {
    pub addon_id: String,
    pub uses_alias: Vec<AddonUsesEntry>,
    pub uses_model: Vec<AddonUsesEntry>,
}

/// Admin approve/deny of one access declaration of an addon. `kind` selects the
/// subtree (`alias`/`model`), `target` is the alias name or model id, and
/// `decision` is `approve` (→ grant a consumer row) or `deny` (→ revoke it).
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct AddonAccessDecisionRequest {
    pub addon_id: String,
    /// `alias` or `model`.
    pub kind: String,
    pub target: String,
    /// `approve` or `deny`.
    pub decision: String,
}

/// Ask the server which host port a fresh deploy would be assigned (the first
/// free port in the services range, skipping leased / OS-bound / docker-bound
/// ports). The deploy wizard pre-fills the editable port field with it.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct SuggestServicePortRequest {
    /// Deploy method the wizard is targeting ("docker", "native", …) — purely
    /// informational for now; the suggestion comes from the shared allocator.
    pub deploy_method: String,
}

/// Response to [`SuggestServicePortRequest`]. `available = false` (port 0) when
/// the whole range is exhausted. Advisory: the deploy re-allocates at commit, so
/// the value can change if another deploy grabs it first.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct SuggestServicePortResponse {
    pub port: u32,
    pub available: bool,
}

/// Generyczne wywolanie auto-tunera dla dowolnego silnika z `[[parameter]]`
/// schema w manifescie. Backend dispatchuje per `engine_id` (vllm/sglang/
/// tensorrt-llm uzywaja `auto_fit_config` z mapowaniem do typed pol; inne
/// silniki maja proste defaulty per kategoria). Zwraca typed mape
/// `parameter.key → JSON value` ktora wizard pre-filluje do formularza.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EngineRecommendRequest {
    pub engine_id: String,
    pub model_repo: String,
    pub gpus: Vec<DeployVllmGpuInfo>,
    pub hf_token: Option<String>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EngineRecommendResponse {
    /// JSON-serialized values per parameter key. Wizard JS deserializuje
    /// zgodnie z `parameter.kind` z manifestu.
    pub parameters: Vec<KeyValue>,
    pub warnings: Vec<String>,
}

/// Request: deploy engine described by Service Manifest (Admin).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceManifestDeployRequest {
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub config_json: String,
}

/// Response: deploy descriptor plus websocket URL for progress stream.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceManifestDeployResponse {
    pub status: String,
    pub deploy_id: String,
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub websocket_url: String,
}

/// Request: redeploy in-place lokalnego serwisu (Admin). Reużywa zapisanego
/// `config_json` z wiersza DB — zero ponownego wyboru parametrów przez usera.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceRedeployRequest {
    pub service_id: i64,
}

/// Response: deskryptor świeżego deployu po redeploy (lub kod błędu w `status`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceRedeployResponse {
    pub status: String,
    pub deploy_id: String,
    pub engine_id: String,
    pub deploy_method: String,
    pub node_id: String,
    pub message: String,
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    /// Multi-instance: szablon (pakiet) z ktorego ta instancja pochodzi.
    pub package_id: String,
    /// Przypieta wersja pakietu tej instancji.
    pub package_version: String,
    /// Nazwa instancji nadana przez usera (rozna od `name`/pakietu gdy zmieniona).
    pub display_name: String,
    /// True gdy w katalogu jest nowsza wersja pakietu niz `package_version`.
    pub update_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonsListResponse {
    pub addons: Vec<AddonInfo>,
}

// =============================================================================
// Multi-instance addons: katalog pakietow + operacje na instancjach.
// =============================================================================

/// Jeden pakiet (szablon) w katalogu — zagregowany po package_id ze wszystkich
/// wersji w `addon_packages`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPackageInfo {
    pub package_id: String,
    pub name: String,
    /// Najnowsza dostepna wersja (pierwsza z list_package_versions).
    pub latest_version: String,
    /// Wszystkie dostepne wersje, najnowsze najpierw.
    pub versions: Vec<String>,
    pub source: String,
    /// Ile instancji tego pakietu jest aktualnie zainstalowanych.
    pub installed_instances: i32,
    /// Connection parameters the package declares via `[[robot.connection_param]]`.
    /// Empty for non-robot packages. The install UI renders one input per entry
    /// and passes the collected values back in `AddonInstanceInstallRequest.config`.
    /// `#[serde(default)]` keeps CBOR compatibility with older peers.
    #[serde(default)]
    pub connection_params: Vec<AddonConnectionParam>,
}

/// One declared connection parameter (`[[robot.connection_param]]`). Drives the
/// per-install form so each robot instance carries its own concrete values
/// (e.g. the robot IP) instead of a hardcoded default.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonConnectionParam {
    pub key: String,
    pub label: String,
    pub param_type: String,
    pub required: bool,
    pub placeholder: String,
}

/// Instalacja nowej instancji pakietu z katalogu pod nadana nazwa.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceInstallRequest {
    pub package_id: String,
    pub version: String,
    pub display_name: String,
    /// Connection-param values entered at install time (key → value). For robot
    /// packages this carries the per-instance IP/serial; substituted into
    /// `${key}` placeholders in network rules and persisted to `addon_config`.
    /// `#[serde(default)]` keeps CBOR compatibility with older peers.
    #[serde(default)]
    pub config: Vec<(String, String)>,
}

/// Wspolna odpowiedz dla install/duplicate — zwraca addon_id nowej instancji.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceInstallResponse {
    pub ok: bool,
    pub addon_id: Option<String>,
    pub error: Option<String>,
}

/// Duplikacja istniejacej instancji pod nowa nazwa (puste dane).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceDuplicateRequest {
    pub source_addon_id: String,
    pub new_display_name: String,
}

/// Zapytanie o wersje dostepne dla instancji (do pickera update).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceVersionsRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceVersionsResponse {
    pub current: String,
    /// Wszystkie skatalogowane wersje pakietu, najnowsze najpierw.
    pub available: Vec<String>,
}

/// Hot-update instancji do wybranej wersji.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceUpdateRequest {
    pub addon_id: String,
    pub target_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstanceUpdateResponse {
    pub ok: bool,
    pub error: Option<String>,
}

/// Multiplex dla operacji katalog/instancje w 1 wariancie MessageBody (limit
/// 256 wariantow CBOR), wzorem `AddonUiBody`/`IamBody`. Req* przychodza z UI,
/// Res* wracaja. Routing po inner-nazwie (`variant_name_of`) do jednego handlera
/// `addon_instance_dispatch`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AddonInstancePayload {
    /// Lista pakietow w katalogu (zakladka "Katalog").
    ReqCatalogList,
    ResCatalogList {
        packages: Vec<AddonPackageInfo>,
    },
    /// Instalacja nowej instancji z katalogu.
    ReqInstall(AddonInstanceInstallRequest),
    ResInstall(AddonInstanceInstallResponse),
    /// Duplikacja istniejacej instancji (reuzywa ResInstall).
    ReqDuplicate(AddonInstanceDuplicateRequest),
    /// Wersje dostepne dla instancji (picker update).
    ReqVersions(AddonInstanceVersionsRequest),
    ResVersions(AddonInstanceVersionsResponse),
    /// Hot-update instancji do wybranej wersji.
    ReqUpdate(AddonInstanceUpdateRequest),
    ResUpdate(AddonInstanceUpdateResponse),
}

// =============================================================================
// Storage stats addona (zakladka Powiazania) — KV / SQL / Vector / Recording.
// =============================================================================

/// Statystyki KV store (tabela `addon_storage`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonKvStats {
    pub keys: i64,
    pub bytes: i64,
    /// Limit z `addon_resource_limits.storage_limit_mb`; 0 = bez limitu.
    pub limit_mb: i64,
}

/// Jedna tabela uzytkownika w per-addon SQLite.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonSqlTable {
    pub name: String,
    /// Liczba wierszy. Liczona z capem (LIMIT) zeby nie skanowac ogromnych tabel
    /// — gdy `rows_capped=true`, `rows` to dolna granica (np. "100000+").
    pub rows: i64,
    pub rows_capped: bool,
}

/// Statystyki per-addon SQLite (`orgs/<org>/addons/<addon_id>/data.db`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonSqlStats {
    /// Addon deklaruje `[storage] sql=true`.
    pub enabled: bool,
    /// Plik bazy istnieje i udalo sie go otworzyc (read-only).
    pub available: bool,
    /// Rozmiar bazy: `page_count * page_size` (tani pragma, bez skanu). -1 = nieznany.
    pub db_size_bytes: i64,
    /// Tabele uzytkownika (z pominieciem wewnetrznych `__tentaflow_%`).
    pub tables: Vec<AddonSqlTable>,
}

/// Jeden namespace wektorowy addona (`addon_vector_namespaces`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorNamespace {
    pub namespace: String,
    pub dim: i64,
    pub metric: String,
    /// Cachowana liczba wektorow z `addon_vector_namespaces.count`.
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorStats {
    /// Funkcja `vector` wkompilowana w ten build.
    pub available: bool,
    pub namespaces: Vec<AddonVectorNamespace>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonRecordingStats {
    /// Funkcja `camera` wkompilowana w ten build.
    pub available: bool,
    pub segments: i64,
    pub snapshots: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonStorageStatsRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonStorageStatsResponse {
    pub kv: AddonKvStats,
    pub sql: AddonSqlStats,
    pub vector: AddonVectorStats,
    pub recording: AddonRecordingStats,
}

/// Multiplex statystyk storage addona w 1 wariancie MessageBody (limit 256
/// wariantow CBOR), wzorem AddonUiBody/AddonInstanceBody.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AddonStoragePayload {
    StatsRequest(AddonStorageStatsRequest),
    StatsResponse(AddonStorageStatsResponse),
}

// =============================================================================
// Vector backend picker addona (zakladka Ustawienia): zvec vs Milvus
// (lokalny serwis / reczny URL). Config = __vector_config; sekrety osobno.
// =============================================================================

/// Odwolanie do serwisu Milvus (node + service id). Puste node_id == lokalny node.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorServiceRef {
    pub node_id: String,
    pub service_id: String,
}

/// Strukturalny config backendu wektorowego (wire). Pola zgodne z zapisem
/// `__vector_config`, ktory czyta NamespaceManager.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorConfig {
    /// "zvec" (domyslny) | "milvus".
    pub backend: String,
    /// Dla milvus: "service_ref" | "manual".
    pub milvus_source: Option<String>,
    pub service_ref: Option<AddonVectorServiceRef>,
    pub manual_uri: Option<String>,
    pub collection_override: Option<String>,
}

/// Jeden wykryty serwis Milvus do pickera.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonMilvusService {
    pub node_id: String,
    pub local: bool,
    pub service_id: String,
    pub display_name: String,
    pub endpoint: String,
    /// Czy ten node moze realnie polaczyc sie z tym serwisem (lokalny => tak;
    /// zdalny => tylko gdy ma osiagalny advertised_endpoint — patrz C-2).
    pub reachable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorGetConfigRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorConfigResponse {
    /// Czy build ma wkompilowany backend Milvus (vector-milvus).
    pub milvus_compiled: bool,
    pub config: AddonVectorConfig,
    /// Czy sekret jest ustawiony (wartosci nie zwracamy).
    pub has_milvus_user: bool,
    pub has_milvus_password: bool,
    pub milvus_services: Vec<AddonMilvusService>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorSetConfigRequest {
    pub addon_id: String,
    pub config: AddonVectorConfig,
    /// None = nie zmieniaj sekretu; Some("") = wyczysc; Some(x) = ustaw.
    pub milvus_user: Option<String>,
    pub milvus_password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVectorSetConfigResponse {
    pub ok: bool,
    pub error: Option<String>,
}

/// Multiplex pickera vector backendu (limit 256 wariantow CBOR).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AddonVectorPayload {
    GetConfigRequest(AddonVectorGetConfigRequest),
    GetConfigResponse(AddonVectorConfigResponse),
    SetConfigRequest(AddonVectorSetConfigRequest),
    SetConfigResponse(AddonVectorSetConfigResponse),
}

// =============================================================================
// SCHEMA v14: Apps menu + UI v2 endpointy
// =============================================================================

/// Application tile shown in the launcher / "My applications". Sourced from
/// the addon manifest `[application]` section at install time.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonApplicationInfo {
    pub addon_id: String,
    pub title: String,
    /// Entry panel id shown in the launcher tile.
    pub entry_panel: String,
    /// Icon name from the TentaFlow icon library (mandatory in manifest).
    pub icon: String,
    /// Short description shown under the tile in "All applications".
    pub description: String,
    /// Sort order (lower = higher). Default 100 when manifest omits it.
    pub sort_order: i32,
    /// Whether the addon is currently enabled in `addons.is_enabled`. The
    /// client uses this to gray out tiles for disabled addons.
    pub enabled: bool,
}

/// Multiplex Apps menu endpoints in a single `MessageBody` slot to stay within
/// the 256-variant CBOR limit. Panel get / UI action removed in chunk 4.2 —
/// addon UI now goes through the CBOR channel (`ui_render_cbor`).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AddonUiPayload {
    // ---- Apps menu ----
    ReqApplicationsList,
    ResApplicationsList {
        applications: Vec<AddonApplicationInfo>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonDetailRequest {
    pub addon_id: String,
}

/// Deklaracja uprawnienia (z manifestu addona).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionDecl {
    pub permission_id: String,
    pub display_name: String,
    pub description: String,
    pub risk: String,
    pub sort_order: i32,
}

/// Deklaracja providera OAuth (z manifestu).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToggleRequest {
    pub addon_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToggleResponse {
    pub ok: bool,
    pub enabled: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstallRequest {
    pub filename: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonInstallResponse {
    pub ok: bool,
    pub addon_id: Option<String>,
    pub version: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonUninstallRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonUninstallResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonReloadRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonReloadResponse {
    pub ok: bool,
    pub message: Option<String>,
}

/// Pojedyncze pole konfiguracji addona (z manifestu).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonConfigGetRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonConfigGetResponse {
    pub schema: Vec<AddonConfigField>,
    pub values: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonConfigSetRequest {
    pub addon_id: String,
    pub values: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonConfigSetResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonLogsRequest {
    pub addon_id: String,
    pub limit: i64,
    pub offset: i64,
    pub level: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub level: String,
    pub action: String,
    pub message: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonLogsResponse {
    pub entries: Vec<AddonLogEntry>,
    pub total: i64,
}

/// Parametr pojedynczego narzedzia deklarowanego przez addon.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToolParam {
    pub name: String,
    pub param_type: String,
    pub description: String,
    pub required: bool,
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Vec<AddonToolParam>,
    pub return_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToolsRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonToolsResponse {
    pub tools: Vec<AddonToolDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonResourcesGetRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonResourcesGetResponse {
    pub max_instances: i32,
    pub cpu_limit_pct: i32,
    pub ram_mb: i32,
    pub storage_mb: i32,
    pub http_requests_per_min: i32,
    pub llm_tokens_per_min: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonResourcesSetRequest {
    pub addon_id: String,
    pub max_instances: i32,
    pub cpu_limit_pct: i32,
    pub ram_mb: i32,
    pub storage_mb: i32,
    pub http_requests_per_min: i32,
    pub llm_tokens_per_min: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonResourcesSetResponse {
    pub ok: bool,
}

/// Zmergowana regula sieciowa zadeklarowana w manifescie + status pokrycia.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonNetworkRuleDecl {
    pub rule_id: String,
    pub host: String,
    pub port: Option<i32>,
    pub protocol: String,
    pub mode: String,
    pub status: String,
    pub required: bool,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonNetworkRulesGetRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonNetworkRulesGetResponse {
    pub allowed_hosts: Vec<String>,
    pub blocked_hosts: Vec<String>,
    pub mode: String,
    pub declared_rules: Vec<AddonNetworkRuleDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonNetworkRulesSetRequest {
    pub addon_id: String,
    pub allowed_hosts: Vec<String>,
    pub blocked_hosts: Vec<String>,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonNetworkRulesSetResponse {
    pub ok: bool,
}

/// Wiersz widocznosci addona per grupa.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVisibilityRow {
    pub addon_id: String,
    pub group_id: String,
    pub group_name: String,
    pub visible: bool,
    pub group_description: String,
    pub user_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVisibilityListRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVisibilityListResponse {
    pub addon_id: String,
    pub rows: Vec<AddonVisibilityRow>,
    pub show_in_catalog: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVisibilitySetRequest {
    pub addon_id: String,
    pub group_id: String,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonVisibilitySetResponse {
    pub addon_id: String,
    pub group_id: String,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonAdminOnlySetRequest {
    pub addon_id: String,
    pub admin_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonAdminOnlySetResponse {
    pub addon_id: String,
    pub admin_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonShowInCatalogSetRequest {
    pub addon_id: String,
    pub show_in_catalog: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonShowInCatalogSetResponse {
    pub addon_id: String,
    pub show_in_catalog: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionCatalogRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionCatalogResponse {
    pub addon_id: String,
    pub entries: Vec<AddonPermissionDecl>,
}

/// Explicit allow/deny/inherit per subject (user|group) + permission.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionRow {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at_epoch: u64,
}

/// Default grant per addon + permission.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionDefault {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionMatrixRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionMatrixResponse {
    pub addon_id: String,
    pub rows: Vec<AddonPermissionRow>,
    pub defaults: Vec<AddonPermissionDefault>,
    pub last_change_by: String,
    pub last_change_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionSetRequest {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionSetResponse {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionDefaultSetRequest {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionDefaultSetResponse {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionCheckRequest {
    pub addon_id: String,
    pub permission_id: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionCheckResponse {
    pub addon_id: String,
    pub permission_id: String,
    pub allowed: bool,
    pub reason: String,
}

/// Server-push event wysylany gdy admin zmieni grant/visibility/default.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonPermissionChangedEvent {
    pub addon_id: String,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub permission_id: Option<String>,
}

/// Konfiguracja OAuth per (addon, provider) — bez sekretow.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigListRequest {
    pub addon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigListResponse {
    pub addon_id: String,
    pub configs: Vec<AddonOAuthConfigRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigSetRequest {
    pub addon_id: String,
    pub provider_id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub enabled: bool,
    pub oauth_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigSetResponse {
    pub addon_id: String,
    pub provider_id: String,
    pub client_secret_set: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigClearSecretRequest {
    pub addon_id: String,
    pub provider_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthConfigClearSecretResponse {
    pub addon_id: String,
    pub provider_id: String,
    pub cleared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthAuthorizeStartRequest {
    pub addon_id: String,
    pub provider_id: String,
    pub mode: String,
    pub redirect_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthAuthorizeStartResponse {
    pub authorize_url: String,
    pub state: String,
}

/// Metadane konta OAuth (tokeny nigdy nie wychodza poza core).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct UserOAuthAccountRow {
    pub id: i64,
    pub user_id: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthLinkedAccountsRequest {
    pub addon_id: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthLinkedAccountsResponse {
    pub addon_id: String,
    pub accounts: Vec<UserOAuthAccountRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthRevokeRequest {
    pub account_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthRevokeResponse {
    pub account_id: i64,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthReauthorizeRequest {
    pub account_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthReauthorizeResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthTestConnectionRequest {
    pub addon_id: String,
    pub provider_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonOAuthTestConnectionResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub account_email: Option<String>,
}

/// Wpis widoku "Moje polaczone konta" (per uzytkownik).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MyOAuthAccountsListRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MyOAuthAccountsListResponse {
    pub accounts: Vec<MyOAuthEntry>,
}

// =============================================================================
// Deployments — real build/run pipeline with streaming progress + log tail.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentStatusRequest {
    pub deploy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentStatusResponse {
    pub deployment: DeploymentSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentListRequest {
    /// "" = wszystkie engines; inaczej filtr exact match.
    pub engine_id: String,
    /// "" = wszystkie; inaczej: "deploying"/"success"/"failed"/"cancelled"/"interrupted".
    pub status: String,
    /// true = tylko moje; false = wszystkie (wymaga admin).
    pub only_mine: bool,
    /// 0 = default 100.
    pub limit: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentListResponse {
    pub deployments: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentLogStreamRequest {
    pub deploy_id: String,
    /// Czy emitować historyczne log_tail zanim stream zacznie live.
    pub replay_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct DeploymentStreamEnd {
    pub deploy_id: String,
    /// "success" | "failed" | "cancelled" | "interrupted".
    pub final_status: String,
    pub image_tag: String,
    pub container_name: String,
    pub error_message: String,
    pub duration_ms: i64,
}

/// System events — push-only, wysylane przez serwer jako unsolicited frames.
/// Jeden wariant `MessageBody::SystemEventBody` oszczedza sloty dla kazdego
/// typu eventu (service status, mesh peer status, cokolwiek dalej).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    /// Personal notification push (Project Studio). Variant appended at the
    /// END — SystemEventPayload is append-only on the wire like every other
    /// enum. Unlike the broadcast variants above, this one is PRIVATE:
    /// ws_binary forwards it only to connections authenticated as `user_id`.
    /// The badge/list source of truth stays NotificationsListRequest; this
    /// event only triggers a toast + badge refresh.
    UserNotification {
        user_id: String,
        notification_id: String,
        project_id: String,
        kind: String,
        title: String,
        body: String,
        link_json: String,
    },
}

/// Zbiorczy payload deployment (req + res + stream chunks). Jeden wariant
/// `MessageBody::DeploymentBody` kosztuje 1 slot w 256-limicie — inner enum
/// rozgalezia sie lokalnie. Stream handler emituje `StreamChunk`/`StreamEnd`
/// przez SubscriptionEvent::Chunk/End tak samo jak ChatStream.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum DeploymentPayload {
    /// Start deploymentu — odpowiednik starego top-level
    /// `ServiceManifestDeployRequestBody`, przeniesiony tu żeby zmieścić się
    /// w 256-variant limicie CBOR (jedna top-level `DeploymentBody` zamiast
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
    ReqRedeploy(ServiceRedeployRequest),
    ResRedeploy(ServiceRedeployResponse),
}

// =============================================================================
// Meeting Bot (per-meeting container, live transcript, AI summary).
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
    pub owner_user_id: String,
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionStartRequest {
    pub meeting_url: String,
    pub title: String,
    pub platform: String,
    pub bot_name: String,
    pub stt_alias: String,
    pub tts_alias: String,
    pub llm_alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionStartResponse {
    pub session: MeetingSessionDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionLeaveRequest {
    pub session_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionLeaveResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionListRequest {
    /// true = tylko moje sesje, false = wszystkie (admin)
    pub only_mine: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionListResponse {
    pub sessions: Vec<MeetingSessionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionDetailRequest {
    pub session_id: i64,
    pub include_transcripts: bool,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSessionDetailResponse {
    pub session: MeetingSessionDescriptor,
    pub transcripts: Vec<MeetingTranscriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingTranscriptsListRequest {
    pub session_id: i64,
    /// Zwroc tylko wpisy z timestamp_ms > since_ms. 0 = wszystko.
    pub since_ms: i64,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingTranscriptsListResponse {
    pub entries: Vec<MeetingTranscriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActiveSessionRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActiveSessionResponse {
    /// session_id = 0 jesli brak aktywnej sesji.
    pub session: MeetingSessionDescriptor,
    pub has_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSettingKv {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSettingsGetRequest;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSettingsGetResponse {
    pub settings: Vec<MeetingSettingKv>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSettingsUpdateRequest {
    pub settings: Vec<MeetingSettingKv>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSettingsUpdateResponse {
    pub ok: bool,
}

// -----------------------------------------------------------------------------
// Summaries / action items / transcript export (post-Etap 2.1).
// -----------------------------------------------------------------------------

/// Jedno podsumowanie sesji z `meeting_summaries`. Protokolowa forma bez
/// content_hash — dedup jest szczegolem DB i nie jedzie po wire.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSummaryItem {
    pub id: i64,
    pub created_at: String,
    pub decisions_text: String,
    pub summary_text: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSummariesListRequest {
    pub meeting_key: String,
    /// Limit najnowszych rekordow. `None` = domyslnie 20.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSummariesListResponse {
    pub items: Vec<MeetingSummaryItem>,
}

/// Action item wyekstrahowany przez LLM z transkryptu.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActionItemItem {
    pub id: i64,
    pub owner: String,
    pub task: String,
    pub deadline: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActionItemsListRequest {
    pub meeting_key: String,
    /// `None` = wszystkie; `Some("pending"|"done"|"cancelled")` = filtr po statusie.
    pub status_filter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActionItemsListResponse {
    pub items: Vec<MeetingActionItemItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActionItemStatusUpdateRequest {
    pub item_id: i64,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingActionItemStatusUpdateResponse {
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingTranscriptExportRequest {
    pub meeting_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelOpenRequest {
    pub session_id: i64,
}

/// First frame on the subscription stream. When `status != "ok"`, the stream
/// also ends immediately and `tunnel_id` is empty.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelOpenResponse {
    pub status: String,
    pub tunnel_id: String,
    pub error: String,
}

/// RFB bytes read from the container TCP socket, pushed to the browser.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelChunk {
    pub tunnel_id: String,
    pub bytes: Vec<u8>,
}

/// Browser → container RFB bytes (keyboard/mouse, client init). One-shot.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelSendRequest {
    pub tunnel_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelSendResponse {
    pub ok: bool,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelCloseRequest {
    pub tunnel_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelCloseResponse {
    pub ok: bool,
}

/// Emitted as the terminal stream chunk when the container-side TCP socket
/// closes (either EOF, I/O error, or handler-initiated shutdown).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct VncTunnelStreamEnd {
    pub tunnel_id: String,
    pub reason: String,
}

/// Single inner enum carrying every VNC tunnel message so the top-level
/// `MessageBody` spends only one variant slot on the feature.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BrowserCaptureRequest {
    pub session_id: i64,
    /// `"screenshot"` albo `"dom"`. Inna wartość => `status="failed"`.
    pub kind: String,
    /// Ignorowane gdy `kind="dom"`. Dla screenshota: true => cała strona ze
    /// scrollowaniem, false => tylko viewport.
    pub full_page: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum BrowserCapturePayload {
    Request(BrowserCaptureRequest),
    Response(BrowserCaptureResponse),
}

/// Zbiorczy payload Meeting Bot (req + res w jednym enumie). Handler rozpoznaje
/// wariant i zwraca odpowiedni Res*. Pozwala na jeden wariant w MessageBody.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingWakeWordRequest {
    pub op: WakeWordOp,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingWakeWordResponse {
    pub words: Vec<WakeWord>,
}

// =============================================================================
// Translate (LLM-backed translator w user app).
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TranslateRequest {
    pub source_text: String,
    pub source_lang: String,
    pub target_lang: String,
    pub tone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TranslateResponse {
    pub translated_text: String,
    pub detected_source_lang: Option<String>,
    pub model_used: String,
    pub tokens_used: i32,
}

// Skonsolidowane w `TranslatePayload` — 1 slot w `MessageBody` zamiast 2,
// zeby zmiescic sie w limicie 256 wariantow CBOR 0.8.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum TranslatePayload {
    Req(TranslateRequest),
    Res(TranslateResponse),
}

// =============================================================================
// Users list (Admin only) — rozszerzone metadane konta z last_login.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct UserInfo {
    pub id: String,
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
    pub group_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct UsersListResponse {
    pub users: Vec<UserInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub member_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct PermissionEntry {
    pub resource_type: String,
    pub resource_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub access_level: String,
}

/// Inner-enum pack dla calego Identity & Access Management —
/// users + groups + resource permissions. Jeden slot w MessageBody (IamBody).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum IamPayload {
    // ---- Users ----
    ReqListUsers,
    ResListUsers {
        users: Vec<UserInfo>,
    },
    ReqGetUser {
        user_id: String,
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
        group_ids: Vec<String>,
    },
    ResCreateUser {
        user_id: String,
    },
    ReqUpdateUser {
        user_id: String,
        display_name: String,
        email: String,
        is_active: bool,
        role: String,
    },
    ReqDeleteUser {
        user_id: String,
    },
    ReqSetUserGroups {
        user_id: String,
        group_ids: Vec<String>,
    },
    ReqResetUserPassword {
        user_id: String,
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
        group_id: String,
    },
    ReqUpdateGroup {
        group_id: String,
        name: String,
        description: String,
    },
    ReqDeleteGroup {
        group_id: String,
    },
    ReqGroupMembers {
        group_id: String,
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
        subject_id: String,
        access_level: String,
    },
    ReqClearPermission {
        resource_type: String,
        resource_id: String,
        subject_type: String,
        subject_id: String,
    },
    ReqListPermsForResource {
        resource_type: String,
        resource_id: String,
    },
    ReqListPermsForSubject {
        subject_type: String,
        subject_id: String,
    },
    ResListPermissions {
        entries: Vec<PermissionEntry>,
    },

    // Generic OK dla mutacji (delete/update/set) bez specyficznego response.
    ResOk,
}

/// Jeden fragment pliku wgrywanego z panelu UI addona do JEGO document store.
/// Generyczny most uploadu: renderer FileInput w panelu addona emituje tylko
/// metadane wybranych plików (`files_selected`), a HOST (frontend) dzieli plik
/// na fragmenty `seq` (0..total_chunks) o wspólnym `upload_id` i wysyła je tu.
/// Core akumuluje fragmenty per `(org_usera, addon_id, upload_id)` i po ostatnim
/// fragmencie finalizuje content-addressed blob w document store instancji
/// `addon_id` — DOKŁADNIE tam, skąd addon czyta przez `document_get`. `org_id`
/// NIE jest polem requestu: serwer bierze org z uwierzytelnionej sesji i waliduje
/// własność `addon_id` (izolacja multi-tenant), więc klient nie może wskazać
/// cudzego store.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonDocumentUploadChunkRequest {
    pub addon_id: String,
    pub upload_id: String,
    pub filename: String,
    pub mime: String,
    pub seq: u32,
    pub total_chunks: u32,
    /// Zaufany marker źródła uploadu ustawiany WYŁĄCZNIE przez renderer po stronie
    /// hosta (dashboard SDK-runtime), NIE przez guest addon. Wartość
    /// `"audio_capture"` znaczy, że bajty pochodzą z mikrofonu (komponent
    /// `AudioCapture`) — host bramkuje takie uploady na uprawnieniu
    /// `audio.capture`. Zwykły upload plików (FileInput) zostawia to puste i NIE
    /// wymaga tego uprawnienia (wybór pliku audio to nie przechwycenie mikrofonu).
    /// Addon nie może podrobić tej wartości: nie kontroluje wywołania kanału
    /// upload, robi to zaufany renderer per typ komponentu.
    #[serde(default)]
    pub source: String,
    /// Surowe bajty fragmentu. `serde_bytes` wymusza w ciborium kodowanie jako CBOR
    /// byte-string (length-prefixed, zero narzutu per-bajt) — goły `Vec<u8>` przez
    /// serde+ciborium dałby array-of-integers (~2× rozmiar), co zabiło wydajność
    /// przy LIDAR. Dla uploadu plików (do setek MiB) to różnica krytyczna.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// Marker `AddonDocumentUploadChunkRequest.source` dla przechwytu mikrofonu
/// (komponent `AudioCapture`). Host bramkuje uploady z tym markerem na
/// uprawnieniu `audio.capture`.
pub const UPLOAD_SOURCE_AUDIO_CAPTURE: &str = "audio_capture";

/// Odpowiedź na fragment uploadu dokumentu addona. Dla fragmentów pośrednich
/// `doc_ref` jest `None` i zwracamy postęp; po ostatnim fragmencie `doc_ref`
/// zawiera id bloba (doc_id) w document store instancji — addon przekazuje go do
/// `ingest_document`/`document_get`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct AddonDocumentUploadChunkResponse {
    pub upload_id: String,
    pub received_chunks: u32,
    pub received_bytes: u64,
    pub doc_ref: Option<String>,
}

/// Ładunek wewnętrzny dla `MessageBody::AddonDocumentBody`. Jeden top-level
/// wariant `MessageBody` na całą rodzinę uploadu dokumentów addona (wzorzec jak
/// `MlStudioPayload` / `RobotsPayload`), bo `MessageBody` dobił do limitu 256
/// wariantów. Nowe warianty TYLKO dopisuj na KOŃCU — ciborium koduje wariant po
/// indeksie liczbowym, więc wstawienie w środku zerwałoby zgodność wire.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum AddonDocumentPayload {
    UploadChunkRequest(AddonDocumentUploadChunkRequest),
    UploadChunkResponse(AddonDocumentUploadChunkResponse),
}

/// policy table (`#[policy]` proc-macro z #26).
///
/// Kazda zmiana layoutu wymaga bump `SCHEMA_VERSION`.
///
/// UWAGA: `Eq` NIE implementowane bo ChatStreamRequest ma `Option<f32>` (floaty
/// nie sa Eq przez NaN). Uzywamy `PartialEq` wszedzie.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum MessageBody {
    // ---- Meta (schema/handshake/keepalive) ----
    /// Klient -> serwer: sprawdz wersje protokolu przy handshake.
    MetaSchemaVersionCheck {
        client_version: u16,
    },
    /// Serwer -> klient: potwierdzenie (accepted=false => disconnect).
    /// `asset_build_hash` to zbiorczy SHA-256 frontu serwera — front porownuje
    /// z wlasnym przy KAZDYM (re)connect i przy roznicy proponuje reload
    /// (nieaktualny front po aktualizacji backendu/addonu). Rozne od
    /// `server_version`: hash lapie zmiany JS/CSS/panelu bez zmiany protokolu.
    MetaSchemaVersionAck {
        server_version: u16,
        accepted: bool,
        asset_build_hash: String,
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

    // ---- Universal multimodal flow invoke (R-STREAM) ----
    FlowInvokeRequestBody(FlowInvokeRequest),
    FlowInvokeChunkBody(FlowInvokeChunk),
    FlowInvokeEndBody(FlowInvokeEnd),

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
    ClusterRdmaConfigureRequestBody(ClusterRdmaConfigureRequest),
    ClusterRdmaConfigureResponseBody(ClusterRdmaConfigureResponse),

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

    // ---- Sync baseline-adopt admin (donor list + start/status/clear) ----
    BaselineDonorListRequest,
    BaselineDonorListResponseBody(BaselineDonorListResponse),
    BaselineAdoptStartRequestBody(BaselineAdoptStartRequest),
    BaselineAdoptStartResponseBody(BaselineAdoptStartResponse),
    BaselineAdoptStatusRequest,
    BaselineAdoptStatusResponseBody(BaselineAdoptStatusResponse),
    BaselineAdoptClearRequest,
    BaselineAdoptClearResponseBody(BaselineAdoptClearResponse),

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

    // ----- Scheduler -----
    SchedulerBody(SchedulerPayload),

    // ----- ML Studio -----
    MlStudioBody(MlStudioPayload),

    // ----- Skills registry -----
    SkillsBody(SkillsPayload),

    // ----- Agents registry -----
    AgentsBody(AgentsPayload),

    // ----- Sync conflict manager -----
    SyncConflictBody(SyncConflictPayload),

    // ----- Sync storage pressure -----
    SyncStorageBody(SyncStoragePayload),

    // ---- Portainer (R-LIST + R-STREAM dla logs) ----
    // Wzorzec „1 slot per feature" — wszystkie operacje Container w jednym
    // wariancie MessageBody. Patrz `ContainerPayload`.
    ContainerBody(ContainerPayload),

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
    // Podglad TTS — synteza tekstu (po czyszczeniu/substytucji) do audio,
    // zeby admin uslyszal jak regula wyjdzie. Binary CBOR (jak cala reszta).
    TtsPreviewRequest {
        text: String,
        model: String,
        voice: String,
    },
    TtsPreviewResponse {
        bytes: Vec<u8>,
        format: String,
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
    // Skonsolidowane w `NetworkPayload` — 1 slot w enum (256-variant limit CBOR).
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

    // ---- Model / alias access control (F1a §6.6) ----
    AliasConsumerListRequestBody(AliasConsumerListRequest),
    AliasConsumerListResponseBody(AliasConsumerListResponse),
    AliasConsumerGrantRequestBody(AliasConsumerGrantRequest),
    AliasConsumerRevokeRequestBody(AliasConsumerRevokeRequest),
    AliasVisibilitySetRequestBody(AliasVisibilitySetRequest),
    ModelVisibilityListRequest,
    ModelVisibilityListResponseBody(ModelVisibilityListResponse),
    ModelVisibilitySetRequestBody(ModelVisibilitySetRequest),
    ModelConsumerListRequestBody(ModelConsumerListRequest),
    ModelConsumerListResponseBody(ModelConsumerListResponse),
    ModelConsumerGrantRequestBody(ModelConsumerGrantRequest),
    ModelConsumerRevokeRequestBody(ModelConsumerRevokeRequest),
    AddonAccessListRequestBody(AddonAccessListRequest),
    AddonAccessListResponseBody(AddonAccessListResponse),
    AddonAccessDecisionRequestBody(AddonAccessDecisionRequest),
    /// Shared response for every access grant/revoke/visibility-set mutation.
    AccessMutationResponseBody(AccessMutationResponse),

    SuggestServicePortRequestBody(SuggestServicePortRequest),
    SuggestServicePortResponseBody(SuggestServicePortResponse),
    EngineRecommendRequestBody(EngineRecommendRequest),
    EngineRecommendResponseBody(EngineRecommendResponse),
    // ServiceManifestDeployRequest/Response przeniesione do DeploymentPayload
    // (ReqStart/ResStart). Oszczędza 1 slot w 256-variant limicie CBOR.

    // ---- Addons: list / detail / toggle / lifecycle ----
    AddonsListRequest,
    AddonsListResponseBody(AddonsListResponse),
    // v14: Apps menu + UI v2 — multiplex w 1 slocie zeby zmiescic sie w 256
    // wariantach CBOR (vide IamBody/ServicePayload).
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
    // 9 par request/response w jednym slocie — CBOR 0.8 ma twardy limit 256
    // wariantow MessageBody, wiec wszystkie wiadomosci profiling pakujemy do
    // jednego `ProfilingPayload`.
    ProfilingBody(crate::profiling::ProfilingPayload),

    // ---- Vision inference (single-slot, req+res w inner enum) ----
    // Slot odzyskany przez konsolidacje PiiRuleListRequest/Response do
    // PiiRuleBody. Patrz ProfilingBody jako wzor inner-enum pack.
    VisionBody(crate::vision::VisionInferPayload),

    // ---- Rerank inference (single-slot, req+res w inner enum) ----
    // Natywny odpowiednik REST `/v1/rerank` / `/v1/ranking` dla Tier 1
    // (dashboard / addony przez protokol binarny). Request i response dziela
    // jeden slot — patrz VisionBody jako wzor inner-enum pack.
    RerankBody(crate::types::RerankExchange),

    // ---- Camera admin RPCs (F2 P7.a) ----
    // 2 par request/response (Discover, AddOnvif) spakowane w jeden slot,
    // analogicznie do ProfilingBody / VisionBody. Powod: CBOR 0.8 256-variant
    // limit + dashboard wizard need (P7.b).
    CameraAdminBody(crate::camera::CameraAdminPayload),

    // ---- Legal admin RPCs (F2 P8.c) ----
    // 3 par request/response (List, Generate, Revoke) spakowane w jeden slot,
    // analogicznie do CameraAdminBody / ProfilingBody. Powod: CBOR 0.8
    // 256-variant limit + dashboard RODO surface (P8.d).
    LegalAdminBody(crate::legal::LegalAdminPayload),

    // ---- Compliance Core admin RPCs ----
    // Odczyt ROPA, retencji i AI audit w jednym slocie, bez przenoszenia
    // treści promptów/odpowiedzi przez listę.
    ComplianceAdminBody(crate::compliance::ComplianceAdminPayload),

    // ---- Role catalog (administrowany katalog rol biznesowych, multi-tenant, i18n) ----
    // Wzorzec „1 slot per feature" — wszystkie req/res w `RoleCatalogPayload`.
    RoleCatalogBody(crate::types::RoleCatalogPayload),

    // ---- Binary stream pub/sub (Chunk B) ----
    // Subscribe/Frame/Close/Closed for the live streaming surface, packed
    // into a single discriminant to stay inside the 256-variant cap.
    StreamBody(crate::stream::StreamPayload),

    // ---- UI Channel CBOR (Faza 6 Krok 4) ----
    // Raw CBOR bytes for the addon UI binary protocol. The dispatch handler
    // decodes the UiTag, validates ownership/permissions via SessionState,
    // and routes to the appropriate panel lifecycle handler.
    UiChannelCbor(Vec<u8>),

    // ---- Error ----
    /// Ujednolicony blad. Towarzyszy `EnvelopeFlags::IS_ERROR`.
    Error(ProtocolError),

    // ---- Addony: multi-instance + storage stats ----
    // UWAGA: ciborium 0.8 koduje warianty enuma po INDEKSIE (twardy limit 256),
    // wiec NOWE warianty dopisujemy ZAWSZE na koncu — wstawienie w srodku
    // przesuwa indeksy kolejnych wariantow i lamie wire-compat z innymi nodami.
    // Multi-instance: katalog pakietow + install/duplicate/versions/update.
    AddonInstanceBody(AddonInstancePayload),
    // Storage stats addona (KV/SQL/Vector/Recording).
    AddonStorageBody(AddonStoragePayload),
    // Vector backend picker addona (zvec / Milvus config + discovery).
    AddonVectorBody(AddonVectorPayload),

    // ---- API key scope + rotation (admin-only) ----
    // Appended at the END of the enum: ciborium 0.8 encodes variants by index
    // (256-variant cap), so new discriminants must never be inserted mid-list.
    /// Lists the explicit allowlist of a general key (subject_type='api_key').
    ApiKeyScopeListRequest {
        key_uid: String,
    },
    ApiKeyScopeListResponse {
        entries: Vec<PermissionEntry>,
    },
    /// Sets one allow/deny scope entry for a general key.
    ApiKeyScopeSetRequest {
        key_uid: String,
        resource_type: String,
        resource_id: String,
        access_level: String,
    },
    /// Removes one scope entry for a general key.
    ApiKeyScopeClearRequest {
        key_uid: String,
        resource_type: String,
        resource_id: String,
    },
    /// Rotates a key's secret: new token, same uid + scope, old token invalid.
    ApiKeyRotateRequest {
        key_uid: String,
    },
    ApiKeyRotateResponse {
        token: String,
    },
    // Robots core app: list (org-scoped) + control routing + camera share.
    // Appended AFTER the API-key variants so origin's variant indices stay
    // wire-stable across the fleet; RobotsBody takes the new highest index.
    RobotsBody(RobotsPayload),
    // Token metrics admin: usage summary, quota CRUD, lease coordinator status.
    // Appended at the END so existing variant indices stay wire-stable across
    // the fleet (ciborium 0.8 encodes variants by index).
    TokenUsageBody(crate::token_usage::TokenUsagePayload),

    // ----- Addon UI document upload (generic FileInput → addon document store) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym):
    // wstawienie w środku przesunęłoby indeksy kolejnych wariantów i zerwało
    // zgodność wire. Po TokenUsageBody (które ma już wdrożony indeks we flocie),
    // więc nasz dopisek bierze NOWY najwyższy indeks i nie rusza istniejących.
    // JEDEN wariant na całą rodzinę (request+response w `AddonDocumentPayload`).
    AddonDocumentBody(AddonDocumentPayload),

    // ----- Cluster distributed deploy (D3) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym),
    // żeby nie ruszać indeksów istniejących wariantów. Deploy jednego modelu
    // rozłożonego na N węzłów klastra (vLLM TP=N) sterowany z GUI (D4).
    ClusterDeployRequestBody(ClusterDeployRequest),
    ClusterDeployResponseBody(ClusterDeployResponse),
    ClusterDeployStopRequestBody(ClusterDeployStopRequest),
    ClusterDeployStopResponseBody(ClusterDeployStopResponse),

    // ----- Model metrics (histogram rollup read + per-model pricing) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym),
    // żeby nie ruszać indeksów istniejących wariantów. JEDEN wariant na całą
    // rodzinę (summary + node×service + pricing) w `ModelMetricsPayload`.
    ModelMetricsBody(crate::model_metrics::ModelMetricsPayload),

    // ----- Benchmark Studio (definicje, targety, runy, wyniki, live progres) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym),
    // żeby nie ruszać indeksów istniejących wariantów. JEDEN wariant na całą
    // rodzinę (request+response+stream) w `BenchmarkPayload`.
    BenchmarkBody(crate::benchmark::BenchmarkPayload),

    // ----- Custom vision-model import (deploy wizard "Własny" tab) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym).
    // JEDEN wariant na całą rodzinę (fetch-manifest + import) w
    // `VisionImportPayload` — Core pobiera zdalny manifest przez klucz API i
    // importuje pojedynczy model wizyjny do lokalnego rejestru.
    VisionImportBody(VisionImportPayload),

    // ----- Ustawienia → Magazyn danych (ścieżki katalogów + migracja) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym).
    // JEDEN wariant na całą rodzinę (overview + browse + mkdir + migrate) w
    // `StorageAdminPayload`.
    StorageAdminBody(crate::storage::StorageAdminPayload),

    // ----- Project Studio (rejestr projektów, wiedza, ingest, chat, ustawienia) -----
    // Dopisane na KOŃCU enuma (ciborium koduje warianty po indeksie liczbowym),
    // żeby nie ruszać indeksów istniejących wariantów. JEDEN wariant na całą
    // rodzinę (request+response+stream) w `ProjectStudioPayload`.
    ProjectStudioBody(crate::project_studio::ProjectStudioPayload),

    // ----- Code Studio (rejestr workspace'ow, czlonkowie, sesje robocze) -----
    // Dopisane na KONCU enuma (ciborium koduje warianty po indeksie liczbowym),
    // zeby nie ruszac indeksow istniejacych wariantow. JEDEN wariant na cala
    // rodzine (request+response) w `CodeStudioPayload`.
    CodeStudioBody(crate::code_studio::CodeStudioPayload),

    // ----- Flows: "restore factory version" for FACTORY_FLOW_IDS -----
    // Appended at the END of the enum (ciborium tags by variant index). The
    // reply reuses `FlowDetailResponse`.
    FlowFactoryRestoreRequestBody(FlowFactoryRestoreRequest),
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
        let bytes = crate::cbor::encode(&body).expect("encode");
        crate::cbor::decode::<MessageBody>(&bytes).expect("decode")
    }

    #[test]
    fn flow_invoke_request_roundtrip_with_output_audio_fields() {
        let req = FlowInvokeRequest {
            flow_id: Some("f1".into()),
            model: "m".into(),
            service_type: "chat".into(),
            inputs: vec![FlowInputValue::Text("hi".into())],
            language: Some("pl".into()),
            session_id: None,
            output_audio: true,
            stt_model: Some("whisper".into()),
            tts_model: Some("piper".into()),
        };
        match round_trip(MessageBody::FlowInvokeRequestBody(req.clone())) {
            MessageBody::FlowInvokeRequestBody(d) => assert_eq!(d, req),
            other => panic!("unexpected {other:?}"),
        }
        // Peers that omit the new fields must decode with output_audio=false.
        let old_json = serde_json::json!({
            "flow_id": null, "model": "m", "service_type": "chat",
            "inputs": [], "language": null, "session_id": null
        });
        let d: FlowInvokeRequest = serde_json::from_value(old_json).expect("decode");
        assert!(!d.output_audio);
        assert!(d.stt_model.is_none() && d.tts_model.is_none());
    }

    #[test]
    fn flow_invoke_chunk_transcript_roundtrip() {
        let body = MessageBody::FlowInvokeChunkBody(FlowInvokeChunk::Transcript {
            text: "dzień dobry".into(),
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn addon_storage_stats_response_roundtrip() {
        let body = MessageBody::AddonStorageBody(AddonStoragePayload::StatsResponse(
            AddonStorageStatsResponse {
                kv: AddonKvStats {
                    keys: 42,
                    bytes: 4096,
                    limit_mb: 100,
                },
                sql: AddonSqlStats {
                    enabled: true,
                    available: true,
                    db_size_bytes: 1_048_576,
                    tables: vec![
                        AddonSqlTable {
                            name: "eureka_entries".to_string(),
                            rows: 1234,
                            rows_capped: false,
                        },
                        AddonSqlTable {
                            name: "huge".to_string(),
                            rows: 100_000,
                            rows_capped: true,
                        },
                    ],
                },
                vector: AddonVectorStats {
                    available: true,
                    namespaces: vec![AddonVectorNamespace {
                        namespace: "faces".to_string(),
                        dim: 512,
                        metric: "cosine".to_string(),
                        count: 7,
                    }],
                },
                recording: AddonRecordingStats {
                    available: false,
                    segments: 0,
                    snapshots: 0,
                    bytes: 0,
                },
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn system_event_user_notification_round_trip() {
        let body = MessageBody::SystemEventBody(SystemEventPayload::UserNotification {
            user_id: "u1".to_string(),
            notification_id: "n1".to_string(),
            project_id: "p1".to_string(),
            kind: "run_item_assigned".to_string(),
            title: "t".to_string(),
            body: "b".to_string(),
            link_json: "{}".to_string(),
        });
        assert_eq!(round_trip(body.clone()), body);
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
            asset_build_hash: "abc123def456".to_string(),
        };
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn ml_studio_classifier_train_start_request_round_trip() {
        let body = MessageBody::MlStudioBody(MlStudioPayload::ClassifierTrainStartRequest(
            MlStudioClassifierTrainStartRequest {
                project_id: "proj-1".to_string(),
                dataset_id: "ds-7".to_string(),
                attribute: "stan".to_string(),
                source_class: "tablica".to_string(),
                variant: "efficientnet_b0".to_string(),
                values: vec![
                    "czysta".to_string(),
                    "brudna".to_string(),
                    "uszkodzona".to_string(),
                    "nieczytelna".to_string(),
                ],
                hyperparams: MlStudioClassifierHyperparams {
                    epochs: 30,
                    batch_size: 32,
                    learning_rate: 1e-3,
                    image_size: 224,
                    freeze_backbone: true,
                },
                target_node_id: "node-B".to_string(),
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn ml_studio_classifier_train_start_request_default_target_node() {
        // `target_node_id` ma #[serde(default)] — CBOR bez tego pola musi się
        // zdekodować do pustego stringa (trening lokalny).
        let req = MlStudioClassifierTrainStartRequest {
            project_id: "p".to_string(),
            dataset_id: "d".to_string(),
            attribute: "stan".to_string(),
            source_class: String::new(),
            variant: "resnet50".to_string(),
            values: vec!["a".to_string(), "b".to_string()],
            hyperparams: MlStudioClassifierHyperparams {
                epochs: 1,
                batch_size: 8,
                learning_rate: 0.01,
                image_size: 128,
                freeze_backbone: false,
            },
            target_node_id: String::new(),
        };
        let body =
            MessageBody::MlStudioBody(MlStudioPayload::ClassifierTrainStartRequest(req.clone()));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn ml_studio_generic_train_status_response_round_trip() {
        let body = MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusResponse(
            MlStudioGenericTrainStatusResponse {
                run_id: "run-42".to_string(),
                status: "running".to_string(),
                epoch: 3,
                total_epochs: 30,
                curve: vec![
                    GenericMetricPoint {
                        epoch: 1,
                        metric_name: "train/loss".to_string(),
                        value: 1.25,
                    },
                    GenericMetricPoint {
                        epoch: 1,
                        metric_name: "val/macro_f1".to_string(),
                        value: 0.5,
                    },
                ],
                error: String::new(),
                sync_phase: Some("syncing".to_string()),
                sync_bytes_sent: 1_024,
                sync_bytes_total: 4_096,
                sync_rate_bps: 512,
                eta_s: 0.0,
                elapsed_s: 0.0,
                gpu_mem_mb: 0.0,
                stage: String::new(),
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn ml_studio_generic_train_status_response_none_sync_phase() {
        let body = MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusResponse(
            MlStudioGenericTrainStatusResponse {
                run_id: "r".to_string(),
                status: "succeeded".to_string(),
                epoch: 30,
                total_epochs: 30,
                curve: vec![],
                error: String::new(),
                sync_phase: None,
                sync_bytes_sent: 0,
                sync_bytes_total: 0,
                sync_rate_bps: 0,
                eta_s: 0.0,
                elapsed_s: 0.0,
                gpu_mem_mb: 0.0,
                stage: String::new(),
            },
        ));
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
        let bytes = crate::cbor::encode(&body).expect("encode");
        // Truncate aggressively (first quarter) zeby na pewno odciac CBOR
        // root pointer — half-bytes po RAG-removal cleanup'ie jest na tyle
        // krotki ze przypadkowo parsuje sie jako valid prefix dla maléjszego
        // payloadu. 1/4 jest gwarantowanie nizej niz pointer table.
        let quarter = &bytes[..bytes.len() / 4];
        assert!(crate::cbor::decode::<MessageBody>(quarter).is_err());
    }

    #[test]
    fn empty_body_bytes_rejected() {
        let result = crate::cbor::decode::<MessageBody>(&[]);
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
                key_type: "general".to_string(),
                subject_id: None,
                subject_label: None,
                scope_count: 2,
                is_active: true,
            }],
        };
        assert_eq!(round_trip(list.clone()), list);

        let create = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
            name: "svc".to_string(),
            key_type: "general".to_string(),
            subject_id: None,
            scope_resources: vec![ResourceRef {
                resource_type: "model".to_string(),
                resource_id: "gpt-4o".to_string(),
            }],
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
                    reasoning_content: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "Hi".to_string(),
                    reasoning_content: None,
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(256),
            flow_id: Some("flow-1".to_string()),
            session_id: Some("sess-1".to_string()),
        });
        assert_eq!(round_trip(req.clone()), req);

        let chunk = MessageBody::ChatStreamChunkBody(ChatStreamChunk {
            delta: "Hello".to_string(),
        });
        assert_eq!(round_trip(chunk.clone()), chunk);

        let end = MessageBody::ChatStreamEndBody(ChatStreamEnd {
            prompt_tokens: 12,
            completion_tokens: 34,
            text: Some("Hello".to_string()),
            ttft_ms: 50,
            prefill_tps: 120.0,
            decode_tps: 45.5,
            total_ms: 1670,
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
    fn skills_payload_round_trip() {
        let bodies = [
            MessageBody::SkillsBody(SkillsPayload::ListRequest(SkillsListRequest {
                tag: Some("crm".to_string()),
                source: None,
                status: Some("active".to_string()),
            })),
            MessageBody::SkillsBody(SkillsPayload::ListResponse(SkillsListResponse {
                skills_json: "[{\"id\":\"s1\"}]".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::DetailRequest(SkillsDetailRequest {
                skill_id: "s1".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::DetailResponse(SkillsDetailResponse {
                skill_json: "{\"id\":\"s1\"}".to_string(),
                files_json: "[{\"path\":\"references/api.md\"}]".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::UpsertRequest(SkillsUpsertRequest {
                skill_json: "{\"name\":\"my-skill\"}".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::UpsertResponse(SkillsUpsertResponse {
                skill_id: "s1".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::DeleteRequest(SkillsDeleteRequest {
                skill_id: "s1".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::DeleteResponse(SkillsDeleteResponse {
                deleted: true,
            })),
            MessageBody::SkillsBody(SkillsPayload::ForkRequest(SkillsForkRequest {
                skill_id: "s1".to_string(),
                new_name: "my-skill-copy".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::ForkResponse(SkillsForkResponse {
                skill_id: "s2".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubSearchRequest(SkillsHubSearchRequest {
                query: "pdf".to_string(),
                source: Some("anthropics/skills".to_string()),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubSearchResponse(SkillsHubSearchResponse {
                results_json: "[{\"name\":\"pdf\"}]".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubImportRequest(SkillsHubImportRequest {
                source: "anthropics/skills/pdf".to_string(),
                git_ref: Some("main".to_string()),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubImportResponse(SkillsHubImportResponse {
                skill_id: "s3".to_string(),
                verdict_json: "{\"clean\":true,\"findings\":[]}".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubApproveRequest(SkillsHubApproveRequest {
                skill_id: "s3".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubApproveResponse(
                SkillsHubApproveResponse { approved: true },
            )),
            MessageBody::SkillsBody(SkillsPayload::HubRejectRequest(SkillsHubRejectRequest {
                skill_id: "s3".to_string(),
            })),
            MessageBody::SkillsBody(SkillsPayload::HubRejectResponse(SkillsHubRejectResponse {
                rejected: true,
            })),
        ];
        for body in bodies {
            assert_eq!(round_trip(body.clone()), body);
        }
    }

    #[test]
    fn agents_payload_round_trip() {
        let bodies = [
            MessageBody::AgentsBody(AgentsPayload::ListRequest(AgentsListRequest {
                enabled: Some(true),
                routable: None,
            })),
            MessageBody::AgentsBody(AgentsPayload::ListResponse(AgentsListResponse {
                agents_json: "[{\"id\":\"a1\"}]".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::DetailRequest(AgentsDetailRequest {
                agent_id: "a1".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::DetailResponse(AgentsDetailResponse {
                agent_json: "{\"id\":\"a1\"}".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::UpsertRequest(AgentsUpsertRequest {
                agent_json: "{\"name\":\"my-agent\"}".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::UpsertResponse(AgentsUpsertResponse {
                agent_id: "a1".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::DeleteRequest(AgentsDeleteRequest {
                agent_id: "a1".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::DeleteResponse(AgentsDeleteResponse {
                deleted: true,
            })),
            MessageBody::AgentsBody(AgentsPayload::RunsListRequest(AgentRunsListRequest {
                agent_id: Some("a1".to_string()),
                status: Some("running".to_string()),
                parent_run_id: None,
            })),
            MessageBody::AgentsBody(AgentsPayload::RunsListResponse(AgentRunsListResponse {
                runs_json: "[{\"id\":\"r1\"}]".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::RunDetailRequest(AgentRunDetailRequest {
                run_id: "r1".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::RunDetailResponse(AgentRunDetailResponse {
                run_json: "{\"id\":\"r1\"}".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::ToolsCatalogRequest(ToolsCatalogRequest {})),
            MessageBody::AgentsBody(AgentsPayload::ToolsCatalogResponse(ToolsCatalogResponse {
                tools_json: "{\"addons\":[],\"core\":[]}".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::RunReplyRequest(AgentRunReplyRequest {
                run_id: "r1".to_string(),
                question_id: "q1".to_string(),
                answer: "yes".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::RunReplyResponse(AgentRunReplyResponse {
                delivered: true,
            })),
            MessageBody::AgentsBody(AgentsPayload::PermissionReplyRequest(
                AgentPermissionReplyRequest {
                    run_id: "r1".to_string(),
                    request_id: "p1".to_string(),
                    decision: "allow_for_run".to_string(),
                },
            )),
            MessageBody::AgentsBody(AgentsPayload::PermissionReplyResponse(
                AgentPermissionReplyResponse { delivered: true },
            )),
            MessageBody::AgentsBody(AgentsPayload::RunCancelRequest(AgentRunCancelRequest {
                run_id: "r1".to_string(),
            })),
            MessageBody::AgentsBody(AgentsPayload::RunCancelResponse(AgentRunCancelResponse {
                cancelled: true,
            })),
            MessageBody::AgentsBody(AgentsPayload::RunEventsSubscribeRequest(
                AgentRunEventsSubscribeRequest {
                    scope: AgentRunEventScope::Session {
                        session_id: "sess-1".to_string(),
                    },
                },
            )),
            MessageBody::AgentsBody(AgentsPayload::RunEventsSubscribeRequest(
                AgentRunEventsSubscribeRequest {
                    scope: AgentRunEventScope::Run {
                        run_id: "r1".to_string(),
                    },
                },
            )),
            MessageBody::AgentsBody(AgentsPayload::RunEvent(AgentRunEvent {
                scope: "sess-1".to_string(),
                kind: "tool_call_finished".to_string(),
                name: "memory.memory_search".to_string(),
                status: "ok".to_string(),
                ..Default::default()
            })),
            MessageBody::AgentsBody(AgentsPayload::RunEvent(AgentRunEvent {
                scope: "r1".to_string(),
                kind: "user_question".to_string(),
                run_id: "r1".to_string(),
                interaction_id: "q1".to_string(),
                question: "Which region?".to_string(),
                choices: vec!["EU".to_string(), "US".to_string()],
                ..Default::default()
            })),
        ];
        for body in bodies {
            assert_eq!(round_trip(body.clone()), body);
        }
    }

    #[test]
    fn sync_conflicts_list_round_trip() {
        let body = MessageBody::SyncConflictBody(SyncConflictPayload::ListResponse(
            SyncConflictsListResponse {
                conflicts: vec![SyncConflictRow {
                    operation_id: "aa".repeat(32),
                    org_id: "org-default".to_string(),
                    addon_id: "contacts".to_string(),
                    table_name: "companies".to_string(),
                    resource_type: "company".to_string(),
                    resource_id: "1".to_string(),
                    action: "insert".to_string(),
                    source_node_id: "node-b".to_string(),
                    error_kind: "sql_constraint".to_string(),
                    error_message: "constraint failed".to_string(),
                    status: "open".to_string(),
                    created_at_ms: 123,
                    resolved_at_ms: None,
                    resolution: None,
                }],
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn sync_conflict_resolve_round_trip() {
        let body = MessageBody::SyncConflictBody(SyncConflictPayload::ResolveRequest(
            SyncConflictResolveRequest {
                org_id: "org-default".to_string(),
                addon_id: "contacts".to_string(),
                operation_id: "bb".repeat(32),
                resolution: SyncConflictResolution::AcceptRemote,
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn sync_storage_report_round_trip() {
        let body = MessageBody::SyncStorageBody(SyncStoragePayload::ReportResponse(
            SyncStorageReportResponse {
                root: "/home/user/.tentaflow".to_string(),
                level: SyncStoragePressureLevel::Warning,
                total_bytes: Some(1000),
                available_bytes: Some(90),
                free_percent_bps: Some(900),
                sqlite_bytes: 10,
                fjall_ledger_bytes: 20,
                snapshot_blob_bytes: 30,
                final_blob_bytes: 40,
                pending_blob_chunk_bytes: 50,
                large_blob_block_bytes: 1024 * 1024,
                paths: vec![SyncStoragePathUsage {
                    label: "sqlite".to_string(),
                    path: "/home/user/.tentaflow/data/router.db".to_string(),
                    bytes: 10,
                }],
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
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
        let body =
            MessageBody::ProfilingBody(ProfilingPayload::StartRequest(ProfilingStartRequest {
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
            }));
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

    // -------------------------------------------------------------------------
    // RoleCatalogBody — round-trip dla wszystkich wariantow RoleCatalogPayload.
    // -------------------------------------------------------------------------

    fn sample_role_summary() -> crate::types::RoleCatalogSummary {
        crate::types::RoleCatalogSummary {
            id: "role-1".to_string(),
            slug: "sales-rep".to_string(),
            kind: "sales".to_string(),
            name_translations: vec![
                ("pl".to_string(), "Handlowiec".to_string()),
                ("en".to_string(), "Sales rep".to_string()),
            ],
            icon: Some("sales".to_string()),
            color_hint: Some("#0ea5e9".to_string()),
            is_manager: false,
            default_visibility_scope: "assigned".to_string(),
            is_active: true,
        }
    }

    fn sample_role_detail() -> crate::types::RoleCatalogDetail {
        crate::types::RoleCatalogDetail {
            id: "role-1".to_string(),
            org_id: "org-x".to_string(),
            slug: "sales-rep".to_string(),
            kind: "sales".to_string(),
            name_translations: vec![
                ("pl".to_string(), "Handlowiec".to_string()),
                ("en".to_string(), "Sales rep".to_string()),
            ],
            description_translations: vec![("pl".to_string(), "Opis".to_string())],
            icon: Some("sales".to_string()),
            color_hint: Some("#0ea5e9".to_string()),
            is_manager: false,
            default_visibility_scope: "assigned".to_string(),
            is_active: true,
            created_at: "2026-05-19T10:00:00Z".to_string(),
            updated_at: "2026-05-19T10:00:00Z".to_string(),
            created_by: Some("user-42".to_string()),
        }
    }

    #[test]
    fn test_role_catalog_list_request_roundtrip() {
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::ListRequest(
            crate::types::RoleCatalogListFilter {
                kind: Some("sales".to_string()),
                is_active: Some(true),
                search: Some("handl".to_string()),
                limit: Some(50),
                offset: Some(0),
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn test_role_catalog_list_response_roundtrip() {
        let role_a = sample_role_summary();
        let mut role_b = sample_role_summary();
        role_b.id = "role-2".to_string();
        role_b.slug = "team-lead".to_string();
        role_b.kind = "management".to_string();
        role_b.is_manager = true;
        role_b.default_visibility_scope = "section".to_string();
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::ListResponse {
            roles: vec![role_a, role_b],
        });
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn test_role_catalog_get_request_by_id_and_by_slug() {
        let by_id = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::GetRequest {
            id: "role-1".to_string(),
        });
        assert_eq!(round_trip(by_id.clone()), by_id);

        let by_slug =
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::GetBySlugRequest {
                slug: "sales-rep".to_string(),
            });
        assert_eq!(round_trip(by_slug.clone()), by_slug);
    }

    #[test]
    fn test_role_catalog_get_response_some_and_none() {
        let some = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::GetResponse {
            role: Some(sample_role_detail()),
        });
        assert_eq!(round_trip(some.clone()), some);

        let none = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::GetResponse {
            role: None,
        });
        assert_eq!(round_trip(none.clone()), none);
    }

    #[test]
    fn test_role_catalog_list_locales_request_response() {
        let req =
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::ListLocalesRequest);
        assert_eq!(round_trip(req.clone()), req);

        let res =
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::ListLocalesResponse {
                locales: vec![
                    crate::types::PlatformLocaleSummary {
                        code: "pl".to_string(),
                        display_name: "Polski".to_string(),
                        is_default: true,
                    },
                    crate::types::PlatformLocaleSummary {
                        code: "en".to_string(),
                        display_name: "English".to_string(),
                        is_default: false,
                    },
                ],
            });
        assert_eq!(round_trip(res.clone()), res);
    }

    #[test]
    fn test_role_catalog_create_request_full_fields() {
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::CreateRequest(
            crate::types::RoleCatalogCreateRequest {
                slug: "sales-rep".to_string(),
                kind: "sales".to_string(),
                name_translations: vec![
                    ("pl".to_string(), "Handlowiec".to_string()),
                    ("en".to_string(), "Sales rep".to_string()),
                ],
                description_translations: vec![("pl".to_string(), "Opis".to_string())],
                icon: Some("sales".to_string()),
                color_hint: Some("#0ea5e9".to_string()),
                is_manager: false,
                default_visibility_scope: "assigned".to_string(),
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn test_role_catalog_create_response() {
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::CreateResponse(
            sample_role_detail(),
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn test_role_catalog_update_request_only_kind() {
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::UpdateRequest(
            crate::types::RoleCatalogUpdateRequest {
                id: "role-1".to_string(),
                kind: Some("technical".to_string()),
                name_translations: None,
                description_translations: None,
                icon: None,
                color_hint: None,
                is_manager: None,
                default_visibility_scope: None,
            },
        ));
        assert_eq!(round_trip(body.clone()), body);
    }

    #[test]
    fn test_role_catalog_update_icon_set_to_null() {
        // Some(None) = wyzeruj (SET NULL). Sprawdza ze nested Option przechodzi
        // przez CBOR round-trip bez zmiany na None/None.
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::UpdateRequest(
            crate::types::RoleCatalogUpdateRequest {
                id: "role-1".to_string(),
                icon: Some(None),
                ..Default::default()
            },
        ));
        let decoded = round_trip(body.clone());
        assert_eq!(decoded, body);
        match decoded {
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::UpdateRequest(req)) => {
                assert_eq!(req.icon, Some(None));
            }
            _ => panic!("expected RoleCatalogBody::UpdateRequest"),
        }
    }

    #[test]
    fn test_role_catalog_update_icon_unchanged() {
        // None = nie ruszaj pola.
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::UpdateRequest(
            crate::types::RoleCatalogUpdateRequest {
                id: "role-1".to_string(),
                icon: None,
                ..Default::default()
            },
        ));
        let decoded = round_trip(body.clone());
        assert_eq!(decoded, body);
        match decoded {
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::UpdateRequest(req)) => {
                assert_eq!(req.icon, None);
            }
            _ => panic!("expected RoleCatalogBody::UpdateRequest"),
        }
    }

    #[test]
    fn test_role_catalog_deactivate_request_response() {
        let req =
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::DeactivateRequest {
                id: "role-1".to_string(),
            });
        assert_eq!(round_trip(req.clone()), req);

        let res =
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::DeactivateResponse {
                deactivated: true,
            });
        assert_eq!(round_trip(res.clone()), res);
    }

    #[test]
    fn test_role_catalog_translations_vec_ordering_preserved() {
        // Kolejnosc par w Vec<(String, String)> musi byc stabilna po round-trip
        // — zalezy od tego deterministyczne porownywanie po stronie repo i UI.
        let translations = vec![
            ("pl".to_string(), "A".to_string()),
            ("en".to_string(), "B".to_string()),
            ("de".to_string(), "C".to_string()),
            ("fr".to_string(), "D".to_string()),
        ];
        let body = MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::CreateRequest(
            crate::types::RoleCatalogCreateRequest {
                slug: "x".to_string(),
                kind: "other".to_string(),
                name_translations: translations.clone(),
                description_translations: vec![],
                icon: None,
                color_hint: None,
                is_manager: false,
                default_visibility_scope: "own".to_string(),
            },
        ));
        let decoded = round_trip(body.clone());
        match decoded {
            MessageBody::RoleCatalogBody(crate::types::RoleCatalogPayload::CreateRequest(req)) => {
                assert_eq!(req.name_translations, translations);
            }
            _ => panic!("expected RoleCatalogBody::CreateRequest"),
        }
    }

    #[test]
    fn baseline_donor_list_response_round_trip() {
        let body = MessageBody::BaselineDonorListResponseBody(BaselineDonorListResponse {
            candidates: vec![
                BaselineDonorCandidate {
                    node_id: "aabbccdd".to_string(),
                    display_name: "donor-host".to_string(),
                    trusted: true,
                    summary: Some(BaselineDonorSummary {
                        org_name: "Acme".to_string(),
                        users: 12,
                        flows: 4,
                        roles: 3,
                    }),
                },
                BaselineDonorCandidate {
                    node_id: "11223344".to_string(),
                    display_name: "11223344".to_string(),
                    trusted: true,
                    summary: None,
                },
            ],
        });
        match round_trip(body.clone()) {
            MessageBody::BaselineDonorListResponseBody(r) => {
                assert_eq!(r.candidates.len(), 2);
                assert_eq!(r.candidates[0].node_id, "aabbccdd");
                assert_eq!(r.candidates[0].summary.as_ref().map(|s| s.users), Some(12));
                assert!(r.candidates[1].summary.is_none());
            }
            other => panic!("expected BaselineDonorListResponseBody, got {other:?}"),
        }
    }

    #[test]
    fn baseline_adopt_start_round_trip() {
        let req = MessageBody::BaselineAdoptStartRequestBody(BaselineAdoptStartRequest {
            donor_node_id: "aabbccdd".to_string(),
        });
        match round_trip(req) {
            MessageBody::BaselineAdoptStartRequestBody(r) => {
                assert_eq!(r.donor_node_id, "aabbccdd");
            }
            other => panic!("expected BaselineAdoptStartRequestBody, got {other:?}"),
        }

        let resp = MessageBody::BaselineAdoptStartResponseBody(BaselineAdoptStartResponse {
            ok: true,
            started: true,
            message: "adopcja rozpoczeta".to_string(),
        });
        match round_trip(resp) {
            MessageBody::BaselineAdoptStartResponseBody(r) => {
                assert!(r.ok && r.started);
            }
            other => panic!("expected BaselineAdoptStartResponseBody, got {other:?}"),
        }
    }

    #[test]
    fn baseline_adopt_status_round_trip_with_report() {
        let body = MessageBody::BaselineAdoptStatusResponseBody(BaselineAdoptStatusResponse {
            phase: BaselineAdoptPhaseTag::Completed,
            peer: Some("aabbccdd".to_string()),
            is_joiner: Some(true),
            report: Some(BaselineAdoptReport {
                donor_org_id: "org-1".to_string(),
                users_merged_by_email: 2,
                users_joined_donor_org: 5,
                collisions_suffixed: 1,
            }),
        });
        match round_trip(body) {
            MessageBody::BaselineAdoptStatusResponseBody(r) => {
                assert_eq!(r.phase, BaselineAdoptPhaseTag::Completed);
                assert_eq!(r.peer.as_deref(), Some("aabbccdd"));
                assert_eq!(r.is_joiner, Some(true));
                let report = r.report.expect("report present");
                assert_eq!(report.users_joined_donor_org, 5);
                assert_eq!(report.collisions_suffixed, 1);
            }
            other => panic!("expected BaselineAdoptStatusResponseBody, got {other:?}"),
        }
    }

    #[test]
    fn baseline_adopt_status_round_trip_none() {
        let body = MessageBody::BaselineAdoptStatusResponseBody(BaselineAdoptStatusResponse {
            phase: BaselineAdoptPhaseTag::None,
            peer: None,
            is_joiner: None,
            report: None,
        });
        match round_trip(body) {
            MessageBody::BaselineAdoptStatusResponseBody(r) => {
                assert_eq!(r.phase, BaselineAdoptPhaseTag::None);
                assert!(r.peer.is_none() && r.report.is_none());
            }
            other => panic!("expected BaselineAdoptStatusResponseBody, got {other:?}"),
        }
    }

    #[test]
    fn baseline_adopt_clear_round_trip() {
        let body = MessageBody::BaselineAdoptClearResponseBody(BaselineAdoptClearResponse {
            ok: true,
            cleared: false,
            message: "brak stanu adopcji do wyczyszczenia".to_string(),
        });
        match round_trip(body) {
            MessageBody::BaselineAdoptClearResponseBody(r) => {
                assert!(r.ok && !r.cleared);
            }
            other => panic!("expected BaselineAdoptClearResponseBody, got {other:?}"),
        }
    }

    #[test]
    fn robot_entry_legacy_decode_without_actions_meta_defaults_empty() {
        // Mirror of a pre-actions_meta RobotEntry sender: the field simply did not
        // exist on the wire. ciborium APPEND-AT-END + #[serde(default)] must decode
        // it to an empty vec so the UI degrades to plain chips instead of failing.
        #[derive(SerdeSerialize)]
        struct LegacyRobotEntry {
            robot_id: String,
            owner_node_id: String,
            is_local: bool,
            kind: Option<String>,
            status: String,
            battery_percent: Option<f32>,
            rtt_ms: Option<u32>,
            camera_id: Option<String>,
            capabilities: Vec<String>,
        }

        let legacy = LegacyRobotEntry {
            robot_id: "go2-001".to_string(),
            owner_node_id: "abcdef0123".to_string(),
            is_local: true,
            kind: Some("quadruped".to_string()),
            status: "online".to_string(),
            battery_percent: Some(87.5),
            rtt_ms: Some(12),
            camera_id: Some("front".to_string()),
            capabilities: vec!["move".to_string(), "stop".to_string()],
        };

        let bytes = crate::cbor::encode(&legacy).expect("encode legacy");
        let decoded: RobotEntry = crate::cbor::decode(&bytes).expect("decode legacy");

        assert_eq!(decoded.robot_id, "go2-001");
        assert_eq!(decoded.status, "online");
        assert_eq!(decoded.capabilities, vec!["move", "stop"]);
        assert!(
            decoded.actions_meta.is_empty(),
            "missing actions_meta must default to an empty vec"
        );
        assert!(
            decoded.telemetry.is_none(),
            "missing telemetry must default to None"
        );
        assert!(
            decoded.lidar.is_none(),
            "missing lidar must default to None"
        );

        // Same guarantee through the list wrapper the UI actually consumes.
        #[derive(SerdeSerialize)]
        struct LegacyRobotsListResponse {
            robots: Vec<LegacyRobotEntry>,
        }
        let list = LegacyRobotsListResponse {
            robots: vec![LegacyRobotEntry {
                robot_id: "go2-002".to_string(),
                owner_node_id: "0011223344".to_string(),
                is_local: false,
                kind: None,
                status: "offline".to_string(),
                battery_percent: None,
                rtt_ms: None,
                camera_id: None,
                capabilities: vec![],
            }],
        };
        let list_bytes = crate::cbor::encode(&list).expect("encode legacy list");
        let decoded_list: RobotsListResponse =
            crate::cbor::decode(&list_bytes).expect("decode legacy list");
        assert_eq!(decoded_list.robots.len(), 1);
        assert!(decoded_list.robots[0].actions_meta.is_empty());
        assert!(decoded_list.robots[0].telemetry.is_none());
        assert!(decoded_list.robots[0].lidar.is_none());
    }

    #[test]
    fn robot_control_response_legacy_decode_without_result_defaults_none() {
        // Mirror of a pre-`result` RobotControlResponse sender: the field did not
        // exist on the wire. ciborium APPEND-AT-END + #[serde(default)] must decode
        // it to None so an older peer interoperates with read-only actions.
        #[derive(SerdeSerialize)]
        struct LegacyRobotControlResponse {
            ok: bool,
            rejected: Option<String>,
            error: Option<String>,
        }

        let legacy = LegacyRobotControlResponse {
            ok: true,
            rejected: None,
            error: None,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode legacy");
        let decoded: RobotControlResponse = crate::cbor::decode(&bytes).expect("decode legacy");
        assert!(decoded.ok);
        assert_eq!(decoded.rejected, None);
        assert_eq!(decoded.error, None);
        assert_eq!(decoded.result, None, "missing result must default to None");

        // Roundtrip preserving a populated result payload.
        let full = RobotControlResponse {
            ok: true,
            rejected: None,
            error: None,
            result: Some("{\"lidar_frame\":{\"point_count\":42}}".to_string()),
        };
        let full_bytes = crate::cbor::encode(&full).expect("encode full");
        let back: RobotControlResponse = crate::cbor::decode(&full_bytes).expect("decode full");
        assert_eq!(back, full);
        assert_eq!(
            back.result.as_deref(),
            Some("{\"lidar_frame\":{\"point_count\":42}}")
        );
    }

    #[test]
    fn robot_entry_telemetry_snapshot_roundtrips() {
        // A full telemetry snapshot must survive CBOR encode/decode, including the
        // nested IMU + battery sub-snapshots and the variable-length vectors.
        let entry = RobotEntry {
            robot_id: "go2-001".to_string(),
            owner_node_id: "abcdef0123".to_string(),
            is_local: true,
            kind: Some("quadruped".to_string()),
            status: "online".to_string(),
            battery_percent: Some(73.0),
            rtt_ms: Some(12),
            camera_id: Some("front".to_string()),
            capabilities: vec!["move".to_string()],
            actions_meta: vec![],
            telemetry: Some(RobotTelemetrySnapshot {
                mode: Some(1),
                gait_type: Some(3),
                body_height: Some(0.32),
                vx: Some(0.4),
                vy: Some(-0.1),
                vyaw: Some(0.05),
                position: vec![1.0, 2.0, 0.3],
                foot_force: vec![120.0, 118.0, 121.0, 119.0],
                joints: vec![
                    0.1, -0.8, 1.4, -0.1, -0.8, 1.4, 0.1, -0.9, 1.4, -0.1, -0.9, 1.4,
                ],
                imu: Some(RobotImuSnapshot {
                    roll: Some(0.01),
                    pitch: Some(-0.02),
                    yaw: Some(1.57),
                    quaternion: vec![0.707, 0.0, 0.0, 0.707],
                    temperature: Some(41.0),
                }),
                battery: Some(RobotBatterySnapshot {
                    soc: Some(73.0),
                    voltage: Some(28.4),
                    current: Some(-2.1),
                    temperature: Some(36.0),
                }),
                pose_position: vec![1.0, 2.0, 0.31],
                pose_orientation: vec![0.0, 0.0, 0.0, 1.0],
            }),
            lidar: Some(RobotLidarStatus {
                enabled: true,
                available: true,
                point_count: 4096,
                resolution: Some(0.05),
                origin: vec![-1.5, -1.5, -0.2],
                frame_seq: 7,
                last_update_ts: 1_700_000_000,
            }),
        };
        let bytes = crate::cbor::encode(&entry).expect("encode");
        let back: RobotEntry = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, entry);
        let t = back.telemetry.expect("telemetry present");
        assert_eq!(t.foot_force.len(), 4);
        assert_eq!(t.imu.unwrap().yaw, Some(1.57));
        assert_eq!(t.battery.unwrap().voltage, Some(28.4));
    }

    #[test]
    fn flow_factory_restore_request_round_trip() {
        let body = MessageBody::FlowFactoryRestoreRequestBody(FlowFactoryRestoreRequest {
            flow_id: "00000000-0000-4000-8000-000000000010".to_string(),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// `is_factory` was appended with `#[serde(default)]`: a peer that omits
    /// it must still decode to `false`.
    #[test]
    fn flow_summary_without_is_factory_decodes_as_false() {
        let mut value = serde_json::json!({
            "id": "f1",
            "name": "n",
            "description": null,
            "created_at_epoch": 1,
            "updated_at_epoch": 2,
            "enabled": true,
            "is_default": false,
            "published_model_name": null,
            "is_system": false,
        });
        let summary: FlowSummary = serde_json::from_value(value.clone()).expect("decode");
        assert!(!summary.is_factory);
        value["is_factory"] = serde_json::Value::Bool(true);
        let summary: FlowSummary = serde_json::from_value(value).expect("decode");
        assert!(summary.is_factory);
    }

    #[test]
    fn body_nests_inside_envelope() {
        use crate::envelope::{message_kind, Envelope};
        let body = MessageBody::ModelListRequest;
        let body_bytes = crate::cbor::encode(&body).expect("encode body").to_vec();
        let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, body_bytes);
        let env_bytes = crate::cbor::encode(&env).expect("encode env");
        let decoded_env: Envelope =
            crate::cbor::decode::<Envelope>(&env_bytes).expect("decode env");
        let decoded_body: MessageBody =
            crate::cbor::decode::<MessageBody>(&decoded_env.body).expect("decode body");
        assert_eq!(decoded_body, body);
    }

    /// DOWÓD wydajności: pole `bytes` MUSI trafiać na wire jako CBOR byte-string
    /// (major type 2), a NIE jako array-of-integers (major type 4). Goły `Vec<u8>`
    /// przez serde+ciborium dałby array (każdy bajt osobnym itemem ⇒ ~2× rozmiar);
    /// `#[serde(with = "serde_bytes")]` wymusza byte-string. Test koduje 1000 bajtów
    /// i sprawdza: (1) brak markera array-of-1000 w strumieniu, (2) obecność
    /// length-prefiksu byte-stringu 1000, (3) rozmiar ~1000+kilka B (nie ~2000+).
    #[test]
    fn upload_chunk_bytes_encode_as_cbor_byte_string_not_array() {
        let payload = AddonDocumentUploadChunkRequest {
            addon_id: "a".to_string(),
            upload_id: "u".to_string(),
            filename: "f".to_string(),
            mime: "m".to_string(),
            seq: 0,
            total_chunks: 1,
            source: String::new(),
            bytes: vec![0xABu8; 1000],
        };
        // Kodujemy SAMĄ strukturę (bez owijki MessageBody), żeby zmierzyć narzut pola.
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&payload, &mut buf).expect("encode");

        // Array-of-1000 zakodowałby się jako major type 4 z prefiksem 0x99 0x03 0xE8
        // (array, długość 1000) — taki ciąg NIE może wystąpić.
        let array_1000_prefix = [0x99u8, 0x03, 0xE8];
        assert!(
            !buf.windows(3).any(|w| w == array_1000_prefix),
            "bytes zakodowane jako CBOR array-of-integers (regresja LIDAR) — brak serde_bytes?"
        );

        // Byte-string długości 1000 ma prefiks 0x59 0x03 0xE8 (major type 2, u16 len).
        let bstr_1000_prefix = [0x59u8, 0x03, 0xE8];
        assert!(
            buf.windows(3).any(|w| w == bstr_1000_prefix),
            "brak prefiksu CBOR byte-string dla 1000 bajtów"
        );

        // Rozmiar całości: 1000 bajtów ładunku + drobny narzut pól/prefiksów,
        // znacznie poniżej 2000 (array dałby ~2000+ przez kodowanie 0xAB per-bajt).
        assert!(
            buf.len() < 1100,
            "zakodowany rozmiar {} B sugeruje array-of-ints zamiast byte-string",
            buf.len()
        );
    }
}
