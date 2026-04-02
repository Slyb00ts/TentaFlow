// =============================================================================
// Plik: db/models.rs
// Opis: Modele danych SQLite - struktury mapowane na tabele.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Serwis AI z bazy danych
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbService {
    pub id: i64,
    pub name: String,
    pub service_type: String,
    pub strategy: String,
    pub model_category: Option<String>,
    pub status: String,
    pub config_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub service_uuid: Option<String>,
    pub node_id: Option<String>,
}

/// Backend serwisu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbServiceBackend {
    pub id: i64,
    pub service_id: i64,
    pub connection_type: String,
    pub config_json: String,
    pub max_concurrent: i64,
    pub timeout_ms: i64,
    pub weight: i64,
    pub model_name_override: Option<String>,
    pub health_check_path: Option<String>,
    pub is_active: bool,
}

/// Parametry tworzenia nowego backendu
#[derive(Debug, Clone)]
pub struct NewBackend<'a> {
    pub service_id: i64,
    pub connection_type: &'a str,
    pub config_json: &'a str,
    pub max_concurrent: i64,
    pub timeout_ms: i64,
    pub weight: i64,
    pub model_name_override: Option<&'a str>,
    pub health_check_path: Option<&'a str>,
}

/// Klucz API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbApiKey {
    pub id: i64,
    pub key_hash: String,
    pub key_prefix: String,
    pub name: String,
    pub rate_limit_rps: i64,
    pub is_active: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// Alias serwisu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbServiceAlias {
    pub id: i64,
    pub alias: String,
    pub target_service_id: i64,
}

/// Ustawienie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbSetting {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// Uzytkownik dashboardu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbUser {
    pub id: i64,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    #[serde(default)]
    pub must_change_password: bool,
    pub created_at: String,
    pub last_login_at: Option<String>,
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
    pub created_at: String,
    pub updated_at: String,
}

/// Wpis rejestru modeli AI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbModelEntry {
    pub id: i64,
    pub model_name: String,
    pub display_name: Option<String>,
    pub service_type: String,
    pub connection_type: String,
    pub service_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub is_public: bool,
    pub is_active: bool,
    pub config_json: String,
    pub created_at: String,
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
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub version: i64,
    pub is_default: bool,
    pub service_type: Option<String>,
    pub flow_json: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Powiazanie flow z wzorcem nazwy modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowModelBinding {
    pub id: i64,
    pub flow_id: i64,
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

/// Rekord wykonania flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFlowExecution {
    pub id: i64,
    pub flow_id: i64,
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
}

/// Parametry tworzenia wpisu rejestru modeli
#[derive(Debug, Clone)]
pub struct NewModelEntry<'a> {
    pub model_name: &'a str,
    pub display_name: Option<&'a str>,
    pub service_type: &'a str,
    pub connection_type: &'a str,
    pub service_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub is_public: bool,
    pub config_json: &'a str,
}

/// Parametry aktualizacji wpisu rejestru modeli
#[derive(Debug, Clone)]
pub struct UpdateModelEntry<'a> {
    pub id: i64,
    pub display_name: Option<&'a str>,
    pub service_type: &'a str,
    pub connection_type: &'a str,
    pub service_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub is_public: bool,
    pub is_active: bool,
    pub config_json: &'a str,
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
    pub id: i64,
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
}

/// Grupa uzytkownikow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGroup {
    pub id: i64,
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
    pub subject_id: i64,
    pub permission_id: String,
    pub granted: bool,
    pub created_at: String,
}

/// Wpis logu audytowego
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: String,
    pub user_id: Option<i64>,
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
    pub default_group_id: Option<i64>,
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

/// Filtry do przeszukiwania logu audytowego
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditLogFilters {
    pub user_id: Option<i64>,
    pub addon_id: Option<String>,
    pub action: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}
