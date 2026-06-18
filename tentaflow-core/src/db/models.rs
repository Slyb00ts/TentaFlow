// =============================================================================
// Plik: db/models.rs
// Opis: Modele danych SQLite - struktury mapowane na tabele.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Klucz API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbApiKey {
    pub id: i64,
    /// Stable UUID used as the sync key and as `subject_id` for general keys.
    pub uid: String,
    /// HMAC-SHA256(org_pepper, token), hex-encoded. Replicated, never the token.
    pub key_verifier: String,
    pub key_prefix: String,
    pub name: String,
    /// 'user' | 'group' | 'general'.
    pub key_type: String,
    /// user_id (user) / group_id (group) / NULL (general).
    pub subject_id: Option<String>,
    pub rate_limit_rps: i64,
    pub is_active: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// Ustawienie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbSetting {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// Prompt systemowy lub szablon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbPrompt {
    pub id: i64,
    pub prompt_id: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub prompt_type: String,
    pub default_model: Option<String>,
    pub variables: Option<String>,
    pub cache_priority: i64,
    pub is_active: bool,
    pub version: i64,
    /// Kod jezyka ISO 639-1 (pl, en, de, es, fr). Ten sam `prompt_id` moze
    /// wystapic w wielu jezykach — runtime lookup wybiera wariant po lokalu.
    pub language: String,
    /// 1 = prompt seedowany (is_system), moze byc nadpisywany przy kolejnych
    /// uruchomieniach. 0 = prompt edytowany/utworzony przez uzytkownika,
    /// nie ruszamy go przy seed.
    pub is_system: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Alias modelu (mapowanie nazwy na docelowy model)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbModelAlias {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub is_active: bool,
    pub fallback_targets: Option<String>,
    pub strategy: Option<String>,
}

/// One row of per-model visibility (`model_visibility`). `model_id` is a
/// free-form string key (no `models` table to FK against in v0.6.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbModelVisibility {
    pub model_id: String,
    pub visibility: String,
    pub updated_at: i64,
    pub updated_by_user_id: Option<i64>,
}

/// One consumer-grant row of `model_consumers` / `model_alias_consumers`
/// with the full grant timeline. `revoked_at = None` means the grant is
/// active; a non-null value marks an admin-revoked grant kept as a tombstone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAccessConsumer {
    pub addon_id: String,
    pub granted_by_user_id: Option<i64>,
    pub granted_at: i64,
    pub revoked_at: Option<i64>,
}

/// One consumer-side `[[uses_model]]` / `[[uses_alias]]` declaration with its
/// reconciled grant state. Drives the addon Access tab and install wizard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAddonUses {
    pub addon_id: String,
    /// Alias name or model id the addon declared it needs.
    pub target: String,
    pub required: bool,
    pub reason: String,
    pub grant_status: String,
    pub grant_decided_at: Option<i64>,
}

/// One alias/model an addon is allowed to consume, joined from its
/// `[[uses_alias]]` declaration (`addon_uses_alias`) with the resolved alias
/// row (`model_aliases`) and its visibility. Drives the addon-facing
/// `alias_list_available_v1` discovery host function: the addon learns the
/// concrete target model, the capability methods, the grant status, and the
/// owner-set visibility for each alias it declared it needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAvailableAlias {
    /// Alias name the addon declared via `[[uses_alias]]`.
    pub alias_id: String,
    /// Concrete model the alias currently resolves to. `None` when the alias
    /// row does not exist yet (declaration is `pending`, owner not installed).
    pub target_model: Option<String>,
    /// Capability methods (detect/recognize/embed/...) declared by the owner
    /// addon. Empty when the alias has no methods or does not exist yet.
    pub methods: Vec<String>,
    /// Routing strategy of the resolved alias, if it exists.
    pub strategy: Option<String>,
    /// Reconciled grant state: `granted` / `auto_granted` / `pending` / `denied`.
    pub grant_status: String,
    /// Owner-set visibility (`private` / `restricted` / `public`) of the alias,
    /// or `None` when the alias row does not exist yet.
    pub visibility: Option<String>,
    /// Whether the resolved alias row is active. `false` when missing or gated.
    pub active: bool,
    /// `true` when the consumer declared the alias as `required`.
    pub required: bool,
}

/// Klaster nodow mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbCluster {
    pub id: i64,
    pub cluster_id: String,
    pub name: String,
    pub description: String,
    pub strategy: String,
    pub created_at: String,
    pub updated_at: String,
    pub total_vram_mb: i64,
    pub total_ram_mb: i64,
    pub total_cpu_cores: i64,
    pub bottleneck_speed_mbps: i64,
    pub interconnect_type: String,
    pub failover_enabled: bool,
    pub failover_target: Option<String>,
    pub health_check_interval_ms: i64,
    pub timeout_ms: i64,
}

/// Klaster z agregatami liczonymi w jednym SELECT JOIN — uzywany do
/// `ClusterListResponse` aby uniknac N+1 query na liczbe czlonkow.
#[derive(Debug, Clone)]
pub struct DbClusterWithCounts {
    pub cluster: DbCluster,
    pub members_count: i64,
    pub members_online: i64,
}

/// Czlonek klastra (node przypisany do klastra)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbClusterMember {
    pub id: i64,
    pub cluster_id: String,
    pub node_id: String,
    pub role: String,
    pub joined_at: String,
    pub interface_name: String,
    pub interface_ip: String,
    pub interface_speed_mbps: i64,
    pub interface_type: String,
}

/// Definicja flow (przeplyw przetwarzania)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: i64,
    pub is_default: bool,
    pub service_type: Option<String>,
    pub flow_json: String,
    pub status: String,
    /// When set, this flow is advertised as a model with this exact id by the
    /// catalog (`/v1/models`, mesh `catalog.list`, GUI). Uniqueness is
    /// enforced in domain logic against aliases and service model names.
    pub published_model_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Powiazanie flow z wzorcem nazwy modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowModelBinding {
    pub id: String,
    pub flow_id: String,
    pub model_pattern: String,
    pub priority: i64,
}

/// Szablon wezla flow (komponent palety)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowNodeTemplate {
    pub id: i64,
    pub node_type: String,
    pub category: String,
    pub label: String,
    pub description: Option<String>,
    pub default_config: String,
    pub icon: Option<String>,
    /// JSON-Schema-like opis pol konfiguracyjnych. NULL → GUI nie renderuje
    /// formy (pusty config tab). Niech-NULL JSON object z polami:
    /// `properties: { <key>: { type, title, description, default, enum?,
    /// minimum?, maximum?, format?, dynamic_enum? } }`, `required: []`,
    /// `order: [...]`. `dynamic_enum: { source: "models", category: "stt"
    /// | "tts" | "llm" | "embeddings" }` mowi GUI zeby wczytac liste
    /// modeli z runtime registry zamiast statycznego enum.
    pub params_schema: Option<String>,
}

/// Regula filtrowania danych osobowych (PII)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbPiiRule {
    pub id: i64,
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
    pub is_active: bool,
    pub priority: i64,
    pub description: Option<String>,
    pub test_examples: Option<String>,
    pub created_at: String,
}

/// Wzorzec szybkiej sciezki (fast path)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFastPathPattern {
    pub id: i64,
    pub module: String,
    pub pattern_type: String,
    pub pattern: String,
    pub match_type: String,
    pub result_json: String,
    pub is_active: bool,
    pub priority: i64,
}

/// Regula czyszczenia tekstu dla TTS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTtsCleaningRule {
    pub id: i64,
    pub rule_type: String,
    pub pattern: String,
    pub replacement: Option<String>,
    pub language: String,
    pub is_active: bool,
    pub priority: i64,
}

/// Snapshot wersji flow (historia zmian dla rollbacku)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowVersion {
    pub id: String,
    pub flow_id: String,
    pub version_num: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub created_at: String,
    pub created_by: Option<String>,
    /// Pelna tresc flow_json — pomijana w liscie (tylko w szczegolach)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_json: Option<String>,
}

/// Rekord wykonania flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowExecution {
    pub id: i64,
    pub flow_id: String,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub status: Option<String>,
    pub execution_log: Option<String>,
    pub total_latency_ms: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Parametry tworzenia nowego promptu
#[derive(Debug, Clone)]
pub struct NewPrompt<'a> {
    pub prompt_id: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub content: &'a str,
    pub prompt_type: &'a str,
    pub default_model: Option<&'a str>,
    pub variables: Option<&'a str>,
    pub cache_priority: i64,
    pub language: &'a str,
}

/// Parametry aktualizacji promptu
#[derive(Debug, Clone)]
pub struct UpdatePrompt<'a> {
    pub id: i64,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub content: &'a str,
    pub prompt_type: &'a str,
    pub default_model: Option<&'a str>,
    pub variables: Option<&'a str>,
    pub cache_priority: i64,
    pub is_active: bool,
    pub language: &'a str,
}

/// Parametry tworzenia/aktualizacji flow
#[derive(Debug, Clone)]
pub struct FlowParams<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub is_default: bool,
    pub service_type: Option<&'a str>,
    pub flow_json: &'a str,
    pub status: &'a str,
    /// Catalog publish name. `None` keeps the flow off `/v1/models`;
    /// `Some` advertises it as a model (validated against alias / flow
    /// collisions in the handler before this struct is built).
    pub published_model_name: Option<&'a str>,
    pub actor_user_id: Option<&'a str>,
}

/// Skill — markdown instruction for the LLM from the Skills registry (Harness §3.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbSkill {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub content: String,
    pub tags_json: String,
    pub category: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
    pub status: String,
    pub use_count: i64,
    pub last_used_at: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Skill reference file (markdown/text under references/ or templates/)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbSkillFile {
    pub skill_id: String,
    pub path: String,
    pub content: String,
}

/// Skill upsert parameters. `id` is caller-supplied: a random UUIDv4 for
/// user/hub skills, a deterministic UUIDv5 of the addon_id for addon skill
/// materialization (fleet-wide idempotent sync apply).
#[derive(Debug, Clone)]
pub struct SkillParams<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub display_name: Option<&'a str>,
    pub description: &'a str,
    pub content: &'a str,
    pub tags_json: &'a str,
    pub category: Option<&'a str>,
    pub source: &'a str,
    pub source_ref: Option<&'a str>,
    pub status: &'a str,
    pub created_by: Option<&'a str>,
    pub actor_user_id: Option<&'a str>,
}

/// Skill list filters (all optional, combined with AND)
#[derive(Debug, Clone, Default)]
pub struct SkillListFilter<'a> {
    pub source: Option<&'a str>,
    pub status: Option<&'a str>,
    pub tag: Option<&'a str>,
}

/// One pre-apply snapshot of a skill captured before the curator mutates it
/// (Harness §3.2 — reversible apply). `existed=false` records a skill that the
/// apply step is about to CREATE (umbrella target): rollback then deletes it. For
/// an existing skill every field carries the verbatim pre-apply value so rollback
/// restores the row exactly. `files_json` is the JSON-encoded reference-file set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbCuratorSnapshotRow {
    pub skill_id: String,
    pub existed: bool,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub tags_json: Option<String>,
    pub category: Option<String>,
    pub source: Option<String>,
    pub source_ref: Option<String>,
    pub status: Option<String>,
    pub files_json: String,
}

/// Header of a curator snapshot — the proposal it was taken for plus its
/// lifecycle (`open` → `applied` → `rolled_back`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbCuratorSnapshot {
    pub id: String,
    pub proposal_json: String,
    pub status: String,
    pub created_by: Option<String>,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub rolled_back_at: Option<String>,
}

/// Agent — a harness definition from the Agents registry (Harness §3.3).
/// Replicates fleet-wide like skills/flows; `name` is soft-unique (no UNIQUE).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAgent {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tools_json: String,
    pub skills_json: String,
    pub params_json: String,
    pub max_iterations: i64,
    pub timeout_secs: i64,
    pub max_subagents: i64,
    pub max_spawn_depth: i64,
    pub flow_id: Option<String>,
    pub routable: bool,
    pub is_enabled: bool,
    /// Behavior when a spawned child run finishes (Harness §3.6 level 3):
    /// `notify` (default) enqueues the mailbox + emits the event; `continue`
    /// also starts a fresh parent run with the child result (Ralph-style).
    pub on_child_complete: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Agent upsert parameters. `id` is caller-supplied: a random UUIDv4 for
/// user-created agents, a stable UUID for seeded agents (phase 5) so fleet-wide
/// sync apply stays idempotent.
#[derive(Debug, Clone)]
pub struct AgentParams<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub display_name: Option<&'a str>,
    pub description: &'a str,
    pub system_prompt: Option<&'a str>,
    pub model: Option<&'a str>,
    pub tools_json: &'a str,
    pub skills_json: &'a str,
    pub params_json: &'a str,
    pub max_iterations: i64,
    pub timeout_secs: i64,
    pub max_subagents: i64,
    pub max_spawn_depth: i64,
    pub flow_id: Option<&'a str>,
    pub routable: bool,
    pub is_enabled: bool,
    /// `notify` | `continue` (Harness §3.6 level 3). Validated against the set
    /// in `validate_agent_params`; the column CHECK is the fleet-wide backstop.
    pub on_child_complete: &'a str,
    pub actor_user_id: Option<&'a str>,
}

/// Agent list filters (all optional, combined with AND).
#[derive(Debug, Clone, Default)]
pub struct AgentListFilter {
    pub is_enabled: Option<bool>,
    pub routable: Option<bool>,
}

/// Agent run — RUNTIME state, one row per harness execution (Harness §3.3).
/// NOT a sync resource (like `flow_executions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAgentRun {
    pub id: String,
    pub agent_id: String,
    pub parent_run_id: Option<String>,
    pub flow_execution_id: Option<i64>,
    pub user_id: Option<String>,
    pub org_id: Option<String>,
    pub status: String,
    pub prompt: String,
    pub result: Option<String>,
    pub exit_reason: Option<String>,
    pub iterations: i64,
    pub total_tokens: i64,
    pub run_log: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

/// Parameters for creating an agent run (the `queued` insert).
#[derive(Debug, Clone)]
pub struct NewAgentRun<'a> {
    pub id: &'a str,
    pub agent_id: &'a str,
    pub parent_run_id: Option<&'a str>,
    pub flow_execution_id: Option<i64>,
    pub user_id: Option<&'a str>,
    pub org_id: Option<&'a str>,
    pub prompt: &'a str,
}

/// Status update for an agent run. Counters and terminal fields are optional so
/// a heartbeat, an iteration tick and a terminal write share one update path.
#[derive(Debug, Clone, Default)]
pub struct AgentRunStatusUpdate<'a> {
    pub status: &'a str,
    pub result: Option<&'a str>,
    pub exit_reason: Option<&'a str>,
    pub iterations: Option<i64>,
    pub total_tokens: Option<i64>,
    /// True once the run reaches a terminal state, stamping `finished_at`.
    pub set_finished: bool,
    /// True the first time the run enters `running`, stamping `started_at`.
    pub set_started: bool,
}

/// Agent-run list filters (all optional, combined with AND).
#[derive(Debug, Clone, Default)]
pub struct AgentRunListFilter<'a> {
    pub agent_id: Option<&'a str>,
    pub status: Option<&'a str>,
    pub parent_run_id: Option<&'a str>,
    pub user_id: Option<&'a str>,
}

/// One mailbox entry (Harness §3.6 level 2): a finished CHILD run's final answer
/// addressed back to the context that spawned it. RUNTIME state, never synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAgentMailbox {
    pub id: String,
    /// The finished child run whose result this carries.
    pub run_id: String,
    /// Chat session that should pick this up on its next interaction (if any).
    pub target_session_id: Option<String>,
    /// Agent that should pick this up the next time it is primed (if any).
    pub target_agent_id: Option<String>,
    /// The child's final answer text.
    pub payload: String,
    pub created_at: String,
    /// NULL until `agent_context` injects the entry into a run's context.
    pub delivered_at: Option<String>,
}

/// Parameters for enqueuing a mailbox entry (the undelivered insert). At least
/// one of `target_session_id` / `target_agent_id` must be set or the entry is
/// unreachable; the manager only enqueues when a target exists.
#[derive(Debug, Clone)]
pub struct NewAgentMailboxEntry<'a> {
    pub id: &'a str,
    pub run_id: &'a str,
    pub target_session_id: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    pub payload: &'a str,
}

/// Parametry tworzenia/aktualizacji szablonu wezla flow
#[derive(Debug, Clone)]
pub struct FlowNodeTemplateParams<'a> {
    pub node_type: &'a str,
    pub category: &'a str,
    pub label: &'a str,
    pub description: Option<&'a str>,
    pub default_config: &'a str,
    pub icon: Option<&'a str>,
}

/// Parametry tworzenia reguly PII
#[derive(Debug, Clone)]
pub struct NewPiiRule<'a> {
    pub name: &'a str,
    pub category: &'a str,
    pub pattern: &'a str,
    pub replacement: &'a str,
    pub priority: i64,
    pub description: Option<&'a str>,
    pub test_examples: Option<&'a str>,
}

/// Parametry aktualizacji reguly PII
#[derive(Debug, Clone)]
pub struct UpdatePiiRule<'a> {
    pub id: i64,
    pub name: &'a str,
    pub category: &'a str,
    pub pattern: &'a str,
    pub replacement: &'a str,
    pub is_active: bool,
    pub priority: i64,
    pub description: Option<&'a str>,
    pub test_examples: Option<&'a str>,
}

/// Parametry aktualizacji wzorca fast path
#[derive(Debug, Clone)]
pub struct UpdateFastPathPattern<'a> {
    pub id: i64,
    pub module: &'a str,
    pub pattern_type: &'a str,
    pub pattern: &'a str,
    pub match_type: &'a str,
    pub result_json: &'a str,
    pub is_active: bool,
    pub priority: i64,
}

/// Parametry aktualizacji reguly TTS
#[derive(Debug, Clone)]
pub struct UpdateTtsCleaningRule<'a> {
    pub id: i64,
    pub rule_type: &'a str,
    pub pattern: &'a str,
    pub replacement: Option<&'a str>,
    pub language: &'a str,
    pub is_active: bool,
    pub priority: i64,
}

/// Instancja Portainer
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbPortainerInstance {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub username: String,
    pub password: String,
}

/// Rejestr Docker (np. Docker Hub, Harbor, Nexus)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbDockerRegistry {
    pub id: i64,
    pub name: String,
    pub registry_type: String,
    pub url: String,
    pub username: String,
    pub password_encrypted: String,
    pub is_active: bool,
    pub skip_tls_verify: bool,
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// Modele systemu uzytkownikow, grup, addonow i uprawnien
// =============================================================================

/// Rozszerzone konto uzytkownika (tabela user_accounts)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAccount {
    pub id: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub display_name: String,
    pub email: String,
    pub is_active: bool,
    pub is_admin: bool,
    /// VULN-003: Wymuszenie zmiany domyslnego hasla
    #[serde(default)]
    pub must_change_password: bool,
    pub sso_provider: Option<String>,
    pub sso_subject: Option<String>,
    pub last_login_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Rola: "user" | "power_user" | "admin". Migracja 50 doda kolumne,
    /// is_admin=1 → "admin", reszta → "user". Power user mozna przypisac UI.
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "user".to_string()
}

/// Grupa uzytkownikow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_at: String,
}

/// Uprawnienie addonu (per addon per user/group per uprawnienie)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonPermission {
    pub id: i64,
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub permission_id: String,
    pub granted: bool,
    pub created_at: String,
}

/// Wpis logu audytowego
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub user_id: Option<String>,
    pub addon_id: Option<String>,
    pub action: String,
    pub resource: Option<String>,
    pub details: Option<String>,
    pub ip_address: Option<String>,
    pub node_id: Option<String>,
}

/// Zainstalowany addon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Addon {
    pub id: i64,
    pub addon_id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub platforms: String,
    pub manifest_json: String,
    pub is_enabled: bool,
    pub is_system: bool,
    pub installed_at: String,
    pub updated_at: String,
    /// Category label from manifest `[addon].category` (e.g. "communication").
    pub category: String,
    /// Sprite id from manifest `[addon].icon` (e.g. "i-meeting"). Empty when absent.
    pub icon: String,
    /// Runtime tag: "wasmtime" (desktop) or "wasmi" (mobile). Defaults to "wasmtime".
    pub runtime: String,
    /// Size of the compiled WASM module in bytes (captured at install/upgrade time).
    pub wasm_size_bytes: i64,
    /// Multi-instance: pakiet (szablon), z ktorego ta instancja pochodzi.
    pub package_id: String,
    /// Przypieta wersja pakietu tej instancji.
    pub package_version: String,
    /// Nazwa instancji nadana przez usera.
    pub display_name: String,
}

/// Sekret addonu (zaszyfrowany per addon per user)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonSecret {
    pub id: i64,
    pub addon_id: String,
    pub user_id: Option<i64>,
    pub key: String,
    #[serde(skip_serializing)]
    pub value_encrypted: String,
}

/// Konfiguracja providera SSO/OIDC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsoProvider {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub client_id: String,
    #[serde(skip_serializing)]
    pub client_secret_encrypted: String,
    pub discovery_url: String,
    pub enabled: bool,
    pub auto_create_users: bool,
    pub default_group_id: Option<String>,
    pub created_at: String,
}

// =============================================================================
// Modele mesh security — zaufane nody i parowania
// =============================================================================

/// Zaufany node w mesh (klucz publiczny Ed25519)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedNode {
    pub id: i64,
    pub node_id: String,
    pub public_key: String,
    pub hostname: String,
    pub approved_by: String,
    pub approved_at: String,
    pub is_active: bool,
    pub last_addresses: String,
}

/// Oczekujace parowanie z innym nodem
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPairing {
    pub id: i64,
    pub remote_node_id: String,
    #[serde(skip_serializing)]
    pub pin_code: String,
    pub direction: String,
    pub expires_at: String,
}

/// Techniczna tozsamosc node/device uzywana przez Sync Ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodeIdentity {
    pub node_id: String,
    pub public_key: String,
    pub public_key_type: String,
    pub display_name: String,
    pub node_kind: String,
    pub trust_status: String,
    pub owner_user_id: Option<String>,
    pub sync_profile: String,
    pub last_seen_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Kryptograficzny klucz uzytkownika, niezalezny od klucza node/device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdentityKey {
    pub key_id: String,
    pub user_id: String,
    pub key_type: String,
    pub public_key: String,
    pub purpose: String,
    pub status: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// Relacja okreslajaca, ktorzy uzytkownicy moga korzystac z danego noda.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeUserAssignment {
    pub node_id: String,
    pub user_id: String,
    pub assignment_mode: String,
    pub valid_from: String,
    pub valid_until: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
}

/// Profil organizacyjny usera uzywany przez Permission Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncUserOrgProfile {
    pub org_id: String,
    pub user_id: String,
    pub department_id: Option<String>,
    pub manager_user_id: Option<String>,
    pub is_department_manager: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Metadata dostepu do zasobu synchronizowanego przez Sync Ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResourceAcl {
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub owner_user_id: Option<String>,
    pub assigned_user_id: Option<String>,
    pub department_id: Option<String>,
    pub manager_user_id: Option<String>,
    pub visibility_scope: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Wynik decyzji Permission Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAccessDecision {
    pub allowed: bool,
    pub reason: String,
}

/// Konfiguracja trybu synchronizacji dla addonu/typu zasobu/zasobu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPolicyMode {
    LocalOnly,
    ReplicatedByPermission,
    AuthorityReadthrough,
    AuthorityWrite,
    Sharded,
    Ephemeral,
}

impl SyncPolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::ReplicatedByPermission => "replicated_by_permission",
            Self::AuthorityReadthrough => "authority_readthrough",
            Self::AuthorityWrite => "authority_write",
            Self::Sharded => "sharded",
            Self::Ephemeral => "ephemeral",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "local_only" => Some(Self::LocalOnly),
            "replicated_by_permission" => Some(Self::ReplicatedByPermission),
            "authority_readthrough" => Some(Self::AuthorityReadthrough),
            "authority_write" => Some(Self::AuthorityWrite),
            "sharded" => Some(Self::Sharded),
            "ephemeral" => Some(Self::Ephemeral),
            _ => None,
        }
    }

    pub fn is_authority_backed(self) -> bool {
        matches!(
            self,
            Self::AuthorityReadthrough
                | Self::AuthorityWrite
                | Self::ReplicatedByPermission
                | Self::Sharded
        )
    }

    pub fn materializes_by_permission(self) -> bool {
        matches!(self, Self::ReplicatedByPermission | Self::Sharded)
    }
}

impl std::fmt::Display for SyncPolicyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Konfiguracja trybu synchronizacji dla addonu/typu zasobu/zasobu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPolicy {
    pub policy_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub mode: SyncPolicyMode,
    pub authority_node_id: Option<String>,
    pub retention_days: Option<i64>,
    pub is_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Odbiorca wybrany przez Sync Policy i Permission Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPolicyTarget {
    pub node_id: String,
    pub reason: String,
}

/// Filtry do przeszukiwania logu audytowego
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditLogFilters {
    pub user_id: Option<String>,
    pub addon_id: Option<String>,
    pub action: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

// =============================================================================
// Voice Profile — profil glosowy osoby zapamietany do bulletproof rozpoznawania
// =============================================================================

/// Profil glosowy z bazy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbVoiceProfile {
    pub id: i64,
    /// Display name (computed z first/last/nickname lub podany explicit).
    /// Unikalny — sluzy jako main lookup key dla prostych wyszukiwan.
    pub name: String,
    /// Imie — wymagane, NOT NULL w DB.
    pub first_name: String,
    /// Nazwisko — opcjonalne.
    pub last_name: Option<String>,
    /// Nick (pseudonim) — opcjonalny.
    pub nickname: Option<String>,
    /// L2-znormalizowany centroid [192 × f32] = 768 bajtow
    pub centroid: Vec<u8>,
    pub sample_count: i64,
    pub reliability_score: f32,
    pub source: String,
    pub metadata_json: String,
    pub enrolled_at: String,
    pub last_seen_at: Option<String>,
    pub total_utterances: i64,
}

/// Parametry utworzenia nowego profilu
#[derive(Debug, Clone)]
pub struct NewVoiceProfile<'a> {
    pub name: &'a str,
    pub first_name: &'a str,
    pub last_name: Option<&'a str>,
    pub nickname: Option<&'a str>,
    pub centroid: &'a [u8],
    pub sample_count: i64,
    pub reliability_score: f32,
    pub source: &'a str,
    pub metadata_json: &'a str,
}

/// Pojedynczy sample glosu dla profilu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbVoiceProfileSample {
    pub id: i64,
    pub profile_id: i64,
    /// Raw (nieznormalizowany) embedding [192 × f32]
    pub embedding: Vec<u8>,
    pub duration_ms: i64,
    pub snr_db: f32,
    pub intra_similarity: f32,
    pub meeting_id: Option<String>,
    pub source: String,
    pub created_at: String,
}

/// Parametry dodania nowego sample do profilu
#[derive(Debug, Clone)]
pub struct NewVoiceProfileSample<'a> {
    pub profile_id: i64,
    pub embedding: &'a [u8],
    pub duration_ms: i64,
    pub snr_db: f32,
    pub intra_similarity: f32,
    pub meeting_id: Option<&'a str>,
    pub source: &'a str,
}

/// Podsumowanie sesji wygenerowane przez LLM po zakończonym meetingu. Jedna
/// sesja moze miec wiele rekordów (regeneracje, roznice modeli) — kolejność
/// po `created_at DESC`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbMeetingSummary {
    pub id: i64,
    pub session_id: i64,
    pub created_at: String,
    pub decisions_text: String,
    pub summary_text: String,
    pub model: String,
}

/// Pojedynczy action item wyekstrahowany z transkryptu. `content_hash` sluzy
/// deduplikacji w obrębie sesji (ta sama para owner+task generowana wielokrotnie
/// przez LLM nie tworzy duplikatów — zamiast tego aktualizujemy deadline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbMeetingActionItem {
    pub id: i64,
    pub session_id: i64,
    pub owner: String,
    pub task: String,
    pub deadline: Option<String>,
    pub status: String,
    pub content_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Tymczasowy mowca w trakcie meetingu (przed przypisaniem do profilu przez LLM)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbVoiceTempSpeaker {
    pub id: i64,
    pub meeting_id: String,
    pub temp_label: String,
    /// JSON array of base64-encoded f32 arrays — elastyczne dopisywanie embeddingow
    pub embeddings_blob: Vec<u8>,
    pub sample_count: i64,
    pub total_duration_ms: i64,
    pub assigned_profile_id: Option<i64>,
    pub created_at: String,
}

/// Durable conversation turn message (table `conversation_messages`). The
/// in-memory cache only buffered `role`+`content`; this is the full record the
/// `persist_turn` node writes and `conversation_history` replays, including the
/// `tool_calls`/`tool_call_id`/`name` round-trip and multimodal blob refs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConversationMessage {
    pub id: i64,
    pub session_id: String,
    pub seq: i64,
    pub role: String,
    pub content: Option<String>,
    /// JSON-encoded `Vec<LlmToolCall>` for assistant tool calls; NULL otherwise.
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    /// Blob id of a multimodal payload (audio/image/video/file); NULL for text.
    pub payload_ref: Option<String>,
    /// `FlowValue::kind()` tag of the multimodal payload; NULL for text.
    pub payload_kind: Option<String>,
    pub node_id: Option<String>,
    pub created_at: String,
}

/// Insert shape for `conversation_messages`. `seq` is assigned per session by
/// the repository (monotonic), so it is not part of this struct.
#[derive(Debug, Clone)]
pub struct NewConversationMessage<'a> {
    pub role: &'a str,
    pub content: Option<&'a str>,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub payload_ref: Option<&'a str>,
    pub payload_kind: Option<&'a str>,
    pub node_id: Option<&'a str>,
}
