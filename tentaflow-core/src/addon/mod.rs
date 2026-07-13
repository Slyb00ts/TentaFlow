// =============================================================================
// Plik: addon/mod.rs
// Opis: Centralny modul systemu addonow WASM — eksporty publiczne, AddonManager
//       zarzadzajacy cyklem zycia addonow, instancjami i eventami.
// =============================================================================

pub mod bundled;
pub mod errors;
pub mod event_bus;
pub mod event_publish;
pub mod flow_blocks;
pub mod fs_sandbox;
pub mod host_functions;
pub mod lifecycle;
pub mod manifest;
pub mod migrations;
pub mod oauth;
pub mod oauth_cleanup;
pub mod oauth_crypto;
pub mod oauth_master_key;
pub mod oauth_refresh_guard;
pub mod permissions;
pub mod rate_limiter;
pub mod runtime;
pub mod sdk_version;
pub mod signature;
pub mod state_flusher;
pub mod state_store;
pub mod storage_sql;
pub mod storage_sql_exec;
pub mod tool_dispatch;
pub mod ui;
pub mod ui_audit;
pub mod ui_session;
pub mod utils;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use parking_lot::{Mutex, RwLock as PlRwLock};
use runtime::{WasmEngine, WasmInstance, WasmLinker, WasmModule, WasmStore};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::db::DbPool;
use event_bus::{Event, EventBus};
use permissions::PermissionChecker;

// =============================================================================
// Stale konfiguracyjne
// =============================================================================

/// Domyslna ilosc paliwa (fuel) dla kazdej operacji WASM. Bumped z 10M do
/// 200M po obserwacji ze legit-rozmiarowe addony (TentaVision z 14 panelami
/// + AccessMatrix + StepProgress + Charts) potrzebuja >50M na samo on_start.
/// Pojedyncza intra-procesowa instrukcja WASM to nanosekundy, wiec 200M ~=
/// 0.5–2 sek scisle limitu CPU per wywolanie — wciaz tanio dla DoS-guard.
const DEFAULT_FUEL_LIMIT: u64 = 200_000_000;

/// Domyslny budzet paliwa na pojedynczy service-tick (gdy manifest nie ustawia
/// `tick_fuel_budget`). 50M — fuel to NIE jest glowny anti-hang (tym jest
/// `tick_timeout_ms` przez epoch watchdog); fuel-out tylko przerywa biezacy tick
/// i refueluje nastepny. Realna praca per-tick (np. dekodowanie ~32k punktow
/// LiDAR, parsowanie, krypto) latwo przekracza dawne 5M i trapowala CICHO (bez
/// panic-hooka). 50M to ~kilka ms — wciaz mocno ograniczone dla 200ms ticka.
const DEFAULT_TICK_FUEL_BUDGET: u64 = 50_000_000;

/// Domyslny limit pamieci WASM w bajtach (256 MB)
const DEFAULT_MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

// =============================================================================
// AddonManifest — parsowany z manifest.toml
// =============================================================================

/// Manifest addonu odczytany z manifest.toml. Mapuje kanoniczny format
/// z sekcja [addon], tablicami [[permission]], [[oauth_provider]], [[tool]],
/// [[network_rule]] oraz sekcjami [visibility], [resources], [lifecycle],
/// [config.schema]. Inne formaty (stare [permissions] z listami kategorii,
/// [[addon_permissions]], [permissions.llm]) sa odrzucane przez parser.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddonManifest {
    pub addon_id: String,
    pub version: String,
    pub display_name: String,
    pub description: Option<String>,
    pub author: Option<String>,
    /// Platformy docelowe (puste = wszystkie)
    pub platforms: Vec<String>,
    /// Sciezka do pliku WASM wzgledem katalogu addonu
    pub wasm_file: String,
    /// Slowa kluczowe addona (PL+EN) do semantic retrieval
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Kategoria addona (np. "communication", "storage", "ai")
    pub category: Option<String>,
    /// Identyfikator ikony sprite (np. "meeting") z pola `[addon].icon`.
    pub icon: Option<String>,
    /// Runtime wykonawczy: `wasmtime` (desktop) lub `wasmi` (mobile).
    pub runtime: Option<String>,
    /// Narzedzia LLM (tool calling) z [[tool]]
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
    /// Granularne uprawnienia addona z [[permission]] — jedyne zrodlo prawdy.
    #[serde(default)]
    pub declared_permissions: Vec<AddonDeclaredPermission>,
    /// Reguly sieciowe TCP/UDP z [[network_rule]]
    #[serde(default)]
    pub network_rules: Vec<ManifestNetworkRule>,
    /// Reguly disambiguation — rozstrzyganie niejednoznacznych zapytan
    #[serde(default)]
    pub disambiguation: Vec<DisambiguationRule>,
    /// Wymagania zasobow deklarowane w sekcji [resources]
    pub resources: Option<ResourceRequirements>,
    /// Sekcja [visibility] — ograniczenia widocznosci addona w GUI
    #[serde(default)]
    pub visibility: Option<AddonVisibilitySection>,
    /// Deklaracje providerow OAuth z [[oauth_provider]]
    #[serde(default)]
    pub oauth_provider: Vec<AddonOAuthProviderSection>,
    /// Identyfikator licencji addona (np. "Apache-2.0").
    pub license: Option<String>,
    /// Flaga widocznosci w katalogu "Available apps" (default true w lifecycle).
    pub show_in_catalog: Option<bool>,
    /// Sekcja [service] — gdy obecna, addon dziala w trybie ciaglym: po
    /// `start_addon` AddonManager spawnuje dedykowany tokio task ktory wola
    /// `on_tick(timestamp_ms)` co `tick_interval_ms`. Stop_addon anuluje task.
    /// `None` = klasyczny tryb request/response + event-driven (bez tickow).
    #[serde(default)]
    pub service: Option<AddonServiceSection>,
    /// Sekcja [application] — gdy obecna, addon rejestruje sie jako aplikacja
    /// widoczna w glownym menu GUI (osobno od katalogu addonow). User klika
    /// ikone w menu → GUI ladowuje route'a i renderuje UI panel addonu.
    /// `None` = addon tylko jako tool/flow block, bez wlasnego UI launchera.
    #[serde(default)]
    pub application: Option<AddonApplicationSection>,
    /// Sekcja [storage] — deklaracja KV i SQL storage. Domyslnie `None` =
    /// KV wlaczony, SQL wylaczony (zachowanie istniejacych addonow przed F1a).
    #[serde(default)]
    pub storage: Option<manifest::StorageConfig>,
    /// Lista deklaracji aliasow AI z `[[alias]]` — przy install tworzone w
    /// globalnej tabeli `model_aliases`.
    #[serde(default)]
    pub aliases: Vec<manifest::AliasSpec>,
    /// Lista bramek prawno-biznesowych z `[[gate]]`. Wymagania `required_claims`
    /// sa interpretowane przez policy engine (F2).
    #[serde(default)]
    pub gates: Vec<manifest::GateSpec>,
    /// Deklaracje vector namespace z `[[vector_namespace]]`.
    /// F1a tylko parsuje i przechowuje; vector API stub do F1c/F2.
    #[serde(default)]
    pub vector_namespaces: Vec<manifest::VectorNamespaceSpec>,
    /// Deklaracje kolekcji grafowych z `[[graph_collection]]` (CozoDB,
    /// services/graph). Addon MUSI zadeklarować kolekcję, żeby `graph_*` host-fn
    /// jej dotknęły — blokuje ad-hoc kolekcje w runtime.
    #[serde(default)]
    pub graph_collections: Vec<manifest::GraphCollectionSpec>,
    /// Szablony Flow z `[[flow_template]]` — opt-in install do flow-engine.
    #[serde(default)]
    pub flow_templates: Vec<manifest::FlowTemplateSpec>,
    /// Flow silnika flow_engine z `[[engine_flow]]` — rejestrowane przy install
    /// instancji jako published model o unikalnej-per-instancję nazwie
    /// (`{addon_id}:{id}`). Addon wyzwala je JAKO MODEL (RAG E2.0).
    #[serde(default)]
    pub engine_flows: Vec<manifest::EngineFlowSpec>,
    /// Custom komponenty UI z `[[ui_component]]`. Sygnatura Ed25519
    /// weryfikowana w F1c packaging tools.
    #[serde(default)]
    pub ui_components: Vec<manifest::UiComponentSpec>,
    /// Sekcja [gpu] — informacyjne wskazowki o wymaganiach GPU.
    #[serde(default)]
    pub gpu: Option<manifest::GpuInfo>,
    /// Wymagana wersja SDK (`addon.sdk_version`) jako semver range,
    /// np. `">=0.2.0"`. Walidowane przez `manifest::validate_manifest_extensions`.
    #[serde(default)]
    pub sdk_version: Option<String>,
    /// Deklaracje `[[uses_alias]]` — consumer-side dostep do aliasow innych
    /// addonow (F1a §6.6 v0.6.0 Chunk C).
    #[serde(default)]
    pub uses_aliases: Vec<manifest::UsesAliasSpec>,
    /// Deklaracje `[[uses_model]]` — consumer-side dostep do konkretnych
    /// modeli (free-form `model_id`, bez FK).
    #[serde(default)]
    pub uses_models: Vec<manifest::UsesModelSpec>,
    /// Sekcja `[publisher]` — Ed25519 public key + label wydawcy. Wymagana
    /// gdy addon deklaruje `[[ui_component]]` (signatures verify). Klucz musi
    /// byc obecny w `trusted_publishers` (DB v26) zeby install przeszedl.
    #[serde(default)]
    pub publisher: Option<manifest::PublisherInfo>,
    /// Sekcja `[runtime]` — per-addon override dla flow_runtime concurrency
    /// cap i service_call rate-limit. Pusta sekcja / brak sekcji = defaults.
    #[serde(default)]
    pub runtime_overrides: Option<manifest::RuntimeSection>,
    /// Sekcja `[robot]` — universal robot capability descriptor. Present only on
    /// robot-control addons (e.g. go2). The cross-node `RobotControl` receiver
    /// resolves the owning addon by this block and reads `[robot.safety]`.
    #[serde(default)]
    pub robot: Option<RobotManifestSection>,
}

/// `[robot]` manifest section — marks an addon as a robot controller and carries
/// the movement safety envelope the cross-node control receiver enforces. Only
/// the fields the receiver needs are modeled; the Robots-app discovery fields
/// (capabilities, connection params) are not parsed here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RobotManifestSection {
    /// `true` when this addon controls a robot. Receiver treats a `[robot]`
    /// section as authoritative; this is the explicit confirmation flag.
    #[serde(default)]
    pub controls_robot: bool,
    /// Robot kind ("quadruped", "drone", ...). Informational for the receiver.
    #[serde(default)]
    pub kind: Option<String>,
    /// Movement safety envelope — the receiver clamps commanded velocity to
    /// `max_linear_mps`. Absent → fall back to the protocol ceiling.
    #[serde(default)]
    pub safety: Option<RobotSafetySection>,
}

/// `[robot.safety]` — the velocity ceiling the controller enforces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RobotSafetySection {
    /// Max commanded linear velocity (normalized -1..1). The cross-node receiver
    /// uses this to clamp `Move` before dispatch.
    #[serde(default)]
    pub max_linear_mps: Option<f64>,
    #[serde(default)]
    pub max_yaw_rps: Option<f64>,
    #[serde(default)]
    pub require_estop_clear: Option<bool>,
}

/// `[application]` manifest section — registers the addon as a user-facing
/// application in the "My applications" launcher. The addon must render
/// `entry_panel` via `ui_render` so the host can serve it when the user
/// clicks the tile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonApplicationSection {
    /// Panel id served on tile click. Must be rendered by the addon through
    /// `ui_render` with this exact `panel_id`.
    pub entry_panel: String,
    /// Application title shown in sidebar / tile (e.g. "TentaVision").
    pub title: String,
    /// Icon name from the TentaFlow icon library (e.g. "video", "camera").
    pub icon: String,
    /// Short description shown under the tile in "All applications".
    #[serde(default)]
    pub description: String,
    /// Sort order in "My applications" (lower = higher). Default 100.
    #[serde(default = "default_app_sort_order")]
    pub sort_order: i32,
}

fn default_app_sort_order() -> i32 {
    100
}

impl AddonApplicationSection {
    /// Structural validation. Rejects malformed `entry_panel`, oversize titles,
    /// invalid icon names and out-of-range sort orders. Reason strings are
    /// static so audit and install logs can correlate on them.
    pub fn validate(&self) -> anyhow::Result<()> {
        use regex::Regex;
        use std::sync::OnceLock;
        static PANEL_RX: OnceLock<Regex> = OnceLock::new();
        static ICON_RX: OnceLock<Regex> = OnceLock::new();
        let panel_rx =
            PANEL_RX.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]*$").expect("static regex"));
        let icon_rx =
            ICON_RX.get_or_init(|| Regex::new(r"^[a-z][a-z0-9-]*$").expect("static regex"));

        if self.entry_panel.is_empty() || self.entry_panel.len() > 64 {
            bail!(
                "application.entry_panel length {} out of range 1..=64",
                self.entry_panel.len()
            );
        }
        if !panel_rx.is_match(&self.entry_panel) {
            bail!("application.entry_panel invalid format");
        }
        let title_len = self.title.chars().count();
        if !(1..=60).contains(&title_len) {
            bail!("application.title length {} out of range 1..=60", title_len);
        }
        let icon_len = self.icon.chars().count();
        if !(1..=40).contains(&icon_len) {
            bail!("application.icon length {} out of range 1..=40", icon_len);
        }
        if !icon_rx.is_match(&self.icon) {
            bail!("application.icon invalid format");
        }
        if !(0..=10_000).contains(&self.sort_order) {
            bail!(
                "application.sort_order {} out of range 0..=10000",
                self.sort_order
            );
        }
        Ok(())
    }
}

/// Sekcja [service] manifestu — deklaracja trybu ciaglego addonu.
/// Wymagane dla addonow ktore musza pracowac 24/7 (analiza wideo z kamer,
/// monitoring sieci, background sync) zamiast czekac na request/event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonServiceSection {
    /// Czy service ma byc uruchamiany. Default `true` gdy sekcja istnieje
    /// (admin moze szybko wylaczyc bez usuwania sekcji).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Interwal miedzy wywolaniami `on_tick`, w ms. `0` lub `None` = brak
    /// tickow (service tylko reaguje na eventy przez `on_event`, persistent
    /// instance daje wlasciwosc trzymania stanu miedzy eventami).
    #[serde(default)]
    pub tick_interval_ms: Option<u64>,
    /// Budzet paliwa na pojedynczy tick. `None` = `DEFAULT_TICK_FUEL_BUDGET`.
    /// Fuel to NIE jest glowny anti-hang (tym jest `tick_timeout_ms` przez
    /// epoch watchdog) — fuel-out tylko przerywa BIEZACY tick i refueluje
    /// nastepny, wiec moze byc hojny. Domyslnie 50M, bo realna praca per-tick
    /// (dekodowanie danych/parsowanie/krypto) latwo przekracza kilka M, a
    /// fuel-out trapuje BEZ panic-hooka (cichy, mylacy crash).
    #[serde(default)]
    pub tick_fuel_budget: Option<u64>,
    /// Hard deadline na pojedynczy tick w ms. Watchdog thread po wygasnieciu
    /// wola `engine.increment_epoch()` — guest dostaje trap nawet jesli paliwo
    /// jeszcze jest (np. addon zablokowany w host_function long-poll).
    /// `None` = brak deadline, wystarczy fuel limit.
    #[serde(default)]
    pub tick_timeout_ms: Option<u64>,
}

/// Sekcja [visibility] manifestu — kontrola widocznosci addona w GUI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AddonVisibilitySection {
    #[serde(default)]
    pub admin_only: bool,
    #[serde(default)]
    pub default_groups: Vec<String>,
    /// Domyslna widocznosc w katalogu "Available apps" (default true).
    #[serde(default)]
    pub show_in_catalog: Option<bool>,
}

/// Deklaracja providera OAuth w manifescie ([[oauth_provider]]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonOAuthProviderSection {
    pub id: String,
    pub display_name: String,
    pub authorize_url: String,
    pub token_url: String,
    #[serde(default)]
    pub revoke_url: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Tryb uwierzytelnienia: "global"|"individual"|"none"
    pub mode: String,
    #[serde(default = "default_true")]
    pub pkce: bool,
}

fn default_true() -> bool {
    true
}
fn default_risk() -> String {
    "low".to_string()
}

/// Wymagania zasobow deklarowane w sekcji [resources] manifestu addonu.
/// Jesli podane, nadpisuja domyslne limity z tabeli addon_resource_limits przy instalacji.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceRequirements {
    /// Calkowity limit storage w MB
    pub storage_total_mb: Option<u64>,
    /// Limit pojedynczej wartosci storage w MB
    pub storage_value_mb: Option<u64>,
    /// Calkowity limit document/blob store (RAG E1.3) w MB. 0/None = bez limitu.
    pub document_storage_mb: Option<u64>,
    /// Limit tokenow LLM na minute
    pub llm_tokens_per_minute: Option<u64>,
    /// Limit requestow HTTP na minute
    pub http_requests_per_minute: Option<u64>,
    /// Limit pamieci RAM w MB
    pub memory_mb: Option<u64>,
    /// Limit paliwa WASM per wywolanie (0 = domyslny 10M instrukcji)
    pub fuel_limit: Option<u64>,
}

/// Definicja narzedzia w sekcji [[tool]] — id, display_name, opis + lista
/// parametrow z [[tool.parameter]]. `parameters_schema` jest skladane do
/// JSON Schema przez parser (tool_dispatch/host functions wymagaja tej formy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTool {
    /// Identyfikator narzedzia (stabilny, uzywany przez LLM function calling)
    pub name: String,
    /// Opis widoczny dla LLM
    pub description: String,
    /// JSON Schema zbudowany z parametrow — host functions uzywaja go bezposrednio
    pub parameters_schema: serde_json::Value,
    /// Opcjonalny schemat wyniku
    pub return_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Parametr narzedzia z [[tool.parameter]] — skladany do `parameters_schema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestToolParameter {
    pub name: String,
    /// Typ parametru w JSON Schema: "string"|"number"|"boolean"|"array"|"object"
    pub param_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

/// Regula disambiguation — rozstrzyganie niejednoznacznych zapytan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisambiguationRule {
    pub trigger: Vec<String>,
    pub prefer: String,
    pub over: String,
    pub when: String,
}

/// Regula sieciowa TCP/UDP deklarowana w manifescie addonu ([[network_rules]])
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestNetworkRule {
    /// Unikalny identyfikator reguly (np. "redis-main", "mqtt-broker")
    pub id: String,
    /// Protokol: "tcp" lub "udp"
    pub protocol: String,
    /// Host docelowy (np. "redis.internal", "192.168.1.100")
    pub host: String,
    /// Port docelowy
    pub port: u16,
    /// Opis reguly widoczny w panelu administracyjnym
    pub description: Option<String>,
    /// Czy regula jest wymagana do dzialania addonu
    pub required: bool,
}

/// Granularne uprawnienie deklarowane przez addon w [[permission]].
/// Id zgodne z konwencja host-function (np. "storage.read", "http.request",
/// "llm.generate") lub domenowe (np. "teams.join_meeting").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonDeclaredPermission {
    /// Unikalny identyfikator uprawnienia
    pub id: String,
    /// Nazwa wyswietlana w panelu administracyjnym (angielski)
    pub display_name: String,
    /// Krotki opis uprawnienia (angielski)
    pub description: String,
    /// Poziom ryzyka uprawnienia: "low"|"medium"|"high"|"critical"
    #[serde(default = "default_risk")]
    pub risk: String,
}

// =============================================================================
// ToolDefinition — opis narzedzia dla LLM
// =============================================================================

/// Definicja narzedzia zarejestrowanego przez addon (dla LLM function calling)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub addon_id: String,
    pub tool_name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub return_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

// =============================================================================
// AddonState — stan przechowywany w Wasmtime Store
// =============================================================================

/// Stan addonu przechowywany w WASM Store — dostepny z host functions
pub struct AddonState {
    pub addon_id: String,
    pub instance_id: String,
    pub user_id: Option<String>,
    /// F2 P1.b — owning organization for this addon instance. Threaded through
    /// audit emits and per-org filesystem sandbox paths. `None` for system /
    /// boot starts that pre-date a real org context (treated as `org-default`
    /// by downstream consumers).
    pub org_id: Option<String>,
    pub db: DbPool,
    pub permissions: Vec<String>,
    pub event_bus: Arc<EventBus>,
    pub permission_checker: Arc<PermissionChecker>,
    /// Pozostale paliwo (fuel) — do resource limiting
    pub fuel_consumed: u64,
    /// CR-006: Flaga systemowego wywolania — omija sprawdzanie user_id w check_permission
    pub is_system_call: bool,
    /// K2: In-memory rate limiter — unika zapytan COUNT(*) na audit_log
    pub rate_limiter: Option<Arc<rate_limiter::AddonRateLimiter>>,
    /// Menedzer polaczen sieciowych TCP/UDP (proxy dla addonow)
    pub net_manager: Arc<Mutex<host_functions::network::NetworkConnectionManager>>,
    /// Cipher do szyfrowania/deszyfrowania sekretow w settings DB
    pub settings_cipher: Arc<crate::crypto::SettingsCipher>,
    /// Manifest addonu — potrzebny do walidacji regul sieciowych
    pub manifest: Arc<AddonManifest>,
    /// Limit pamieci WASM w bajtach
    pub memory_limit: usize,
    /// Router do routowania requestow LLM (ustawiany po inicjalizacji)
    pub router: Option<Arc<crate::routing::router::Router>>,
    /// Per-account mutex map used to serialize OAuth refresh_token calls.
    pub oauth_refresh_guard: Arc<oauth_refresh_guard::OAuthRefreshGuard>,
    /// Shared cache of raw validated CBOR bytes from `ui_render_cbor`.
    /// `None` in isolated event_bus tests.
    pub ui_panels: Option<Arc<PlRwLock<HashMap<(String, String, String), Vec<u8>>>>>,
    /// Limiter zasobow wasmi (iOS/Android) — pole uzywane przez Store::limiter()
    #[cfg(any(target_os = "ios", target_os = "android"))]
    pub store_limits: wasmi::StoreLimits,
    /// WASI preview1 context for wasmtime (Desktop/Router). Addons compiled
    /// to `wasm32-wasip1` import `wasi_snapshot_preview1::{environ_get,
    /// fd_write, proc_exit, random_get}` through Rust stdlib (panic handler,
    /// allocator init, getrandom). Without a wired WASI linker addons fail
    /// to instantiate; `wasmtime_wasi::p1::add_to_linker_sync` in
    /// `runtime_wasmtime::create_linker` provides the implementations.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub wasi: wasmtime_wasi::p1::WasiP1Ctx,
}

impl AddonState {
    /// Stabilny klucz izolacji storage KV — ODPIĘTY od `instance_id` WASM, więc
    /// pula wielu wymiennych instancji obsługujących ten sam zakres widzi te same
    /// dane (a restart procesu nie gubi storage, jak działo się gdy kluczem był
    /// losowy uuid instancji). Zakres z manifestu: `org` (współdzielony w
    /// organizacji — domyślny, np. TentaVision) albo `user` (per użytkownik).
    /// Wartość trafia do kolumny `addon_storage.instance_id` oraz pola
    /// `instance_id` protokołu storage-proxy — te nazwy pozostają, ale niosą
    /// teraz klucz zakresu (deterministyczny → poprawny przy replikacji synca).
    pub fn storage_scope_key(&self) -> String {
        let org = self
            .org_id
            .clone()
            .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
        let user_scoped = self
            .manifest
            .storage
            .as_ref()
            .map(|s| s.is_user_scoped())
            .unwrap_or(false);
        if user_scoped {
            format!("{org}::user::{}", self.user_id.clone().unwrap_or_default())
        } else {
            org
        }
    }
}

// =============================================================================
// AddonInstance — uruchomiona instancja addonu WASM
// =============================================================================

/// Pojedyncza uruchomiona instancja addonu WASM
pub struct AddonInstance {
    pub addon_id: String,
    pub instance_id: String,
    pub user_id: Option<String>,
    pub store: WasmStore<AddonState>,
    pub instance: WasmInstance,
    /// Language-specific export name mapping (Rust / .NET / Python).
    pub language_adapter: Box<dyn runtime::LanguageAdapter>,
}

// =============================================================================
// AddonManager — centralny manager addonow
// =============================================================================

/// Centralny manager addonow — zarzadza cyklem zycia, instancjami, uprawnieniami i eventami
pub struct AddonManager {
    db: DbPool,
    /// Wraps `HashMap<String, Vec<AddonInstance>>` in a `Mutex` (not `RwLock`)
    /// because `AddonInstance.store` contains `WasiP1Ctx` whose
    /// `Box<dyn StdinStream>` is `Send` but not `Sync`. `Mutex<T>: Sync`
    /// requires only `T: Send`, while `RwLock<T>: Sync` would additionally
    /// require `T: Sync`. The map is small and access patterns are mostly
    /// brief writes (insert/remove), so serializing reads has negligible
    /// cost compared to the WASM execution time.
    instances: Arc<Mutex<HashMap<String, Vec<AddonInstance>>>>,
    event_bus: Arc<EventBus>,
    engine: WasmEngine,
    /// Linker zbudowany RAZ (host-funkcje + WASI) i współdzielony przez wszystkie
    /// instancjacje. Linker jest niezmienny po zbudowaniu, a `instantiate` tylko
    /// go czyta — dawniej tworzyliśmy go i rejestrowali ~50 host-fn przy KAŻDYM
    /// tworzeniu instancji (start_addon / pula / invoke_block).
    linker: WasmLinker<AddonState>,
    permission_checker: Arc<PermissionChecker>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    /// Skompilowane moduly WASM — cache po addon_id
    compiled_modules: Arc<PlRwLock<HashMap<String, WasmModule>>>,
    /// Per-account mutex map used to serialize OAuth refresh_token calls.
    oauth_refresh_guard: Arc<oauth_refresh_guard::OAuthRefreshGuard>,
    /// Zarejestrowane narzedzia ze wszystkich addonow
    registered_tools: Arc<PlRwLock<Vec<ToolDefinition>>>,
    /// Router do routowania requestow LLM z addonow
    router: Arc<PlRwLock<Option<Arc<crate::routing::router::Router>>>>,
    /// Rejestr custom flow blocks z addonow. Resolver `AdapterRegistry` woła
    /// `find_block` po prefiksowanym node_type ("addon.{id}.{name}").
    flow_blocks_registry: Arc<flow_blocks::AddonFlowRegistry>,
    /// Tokens anulujace petle service tasków per instance_id. Stop_addon
    /// wola `cancel()` na tokenie — tick loop wychodzi po nastepnym
    /// `select!`, zwalnia uchwyt do instancji.
    service_tasks: Arc<Mutex<HashMap<String, tokio_util::sync::CancellationToken>>>,
    /// Cache of raw validated CBOR bytes from `ui_render_cbor`, keyed by
    /// (user_id, addon_id, slot). Frontend receives CBOR via event bus push.
    ui_panels: Arc<PlRwLock<HashMap<(String, String, String), Vec<u8>>>>,
    /// Per-instance (addon_id) lock serializing lifecycle ops — uninstall_instance,
    /// update_instance — and `start_addon` against each other, so a hot update
    /// can't interleave with a concurrent start of the SAME instance. The hot
    /// invoke path (invoke_block/call_tool) intentionally does NOT take this lock;
    /// a call landing inside the brief update swap may transiently fail with
    /// "tool not found" until re-register completes — acceptable for a hot update
    /// (no corruption), and keeping it lock-free avoids penalizing invocations.
    addon_op_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// instance_id'y zarezerwowane dla pętli serwisowej (on_tick). Ścieżki
    /// user-facing (call_tool/call_panel_open) pomijają je przy wyborze z puli,
    /// żeby tick nie kolidował z obsługą żądań (i odwrotnie).
    service_instance_ids: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Liczba ŻYWYCH instancji per addon (idle w puli + wypożyczone). Limit
    /// wzrostu puli — patrz `pool_cap`. Reset w stop_addon/disable.
    instance_total: Arc<Mutex<HashMap<String, usize>>>,
    /// Cancels the write-behind state flusher (A2). Spawned once in `new`,
    /// cancelled in `shutdown` so the flusher does its final drain and exits
    /// cleanly — mirrors how `service_tasks` tokens are cancelled on shutdown.
    state_flusher_shutdown: tokio_util::sync::CancellationToken,
    /// JoinHandle of the spawned flusher task. `await_state_flusher_drain`
    /// awaits it after cancel so graceful shutdown does NOT exit before the
    /// final drain persists all dirty durable state (bounded by a timeout so a
    /// stuck DB can never hang shutdown).
    state_flusher_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl crate::sync::runtime::AddonSyncReconciler for AddonManager {
    fn reconcile_addon(&self, addon_id: &str) {
        self.reconcile_synced_addon(addon_id);
    }
}

/// Returns the subset of `owned` alias names that should be activated on
/// addon start: every name owned by the addon **except** the ones the
/// manifest marks with `[gate]`. Pure function — separated out so the
/// gated-skip invariant is unit-testable without standing up an
/// AddonManager (and its WASM engine).
fn pick_aliases_to_activate<'a>(
    owned: &'a [String],
    manifest_aliases: &[manifest::AliasSpec],
) -> Vec<&'a str> {
    let gated: std::collections::HashSet<&str> = manifest_aliases
        .iter()
        .filter(|a| a.gate.is_some())
        .map(|a| a.id.as_str())
        .collect();
    owned
        .iter()
        .map(|s| s.as_str())
        .filter(|name| !gated.contains(name))
        .collect()
}

/// Principal pod jaki wykonywane jest wywołanie narzędzia. Rozstrzyga tożsamość
/// `AddonState` workera (a więc decyzję `check_permission`): `User` = konkretny
/// principal z per-user grantami; `System` = core-internal trusted call bez
/// principala (CR-006: `user_id=None` + `is_system_call=true`).
#[derive(Clone, Copy)]
enum CallIdentity<'a> {
    User(&'a str),
    System,
}

impl AddonManager {
    /// Tworzy nowy AddonManager z podana baza danych
    pub fn new(db: DbPool, settings_cipher: Arc<crate::crypto::SettingsCipher>) -> Result<Self> {
        let engine = runtime::create_engine()?;

        // Zbuduj Linker raz — host-funkcje + WASI rejestrowane jednokrotnie,
        // reużywane przy każdym instantiate (anty-narzut na ścieżce tworzenia
        // instancji).
        let mut linker = runtime::create_linker(&engine);
        host_functions::register_host_functions(&mut linker)?;

        let event_bus = Arc::new(EventBus::new());
        // Publish a process-global handle so core flow nodes (camera_alert) can
        // emit events to subscribed addons without threading the bus through.
        event_bus::set_global_event_bus(event_bus.clone());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));

        // Warm-up cache uprawnien — zaladuj wszystko z DB do cache
        permission_checker.refresh_all();

        // Uruchom background refresh co 5 minut
        permission_checker.start_background_refresh();

        // A2: spawn the write-behind state flusher once. It drains the shared
        // `AddonStateStore` Durable tier into the `addon_state` SQLite table on
        // a fixed cadence and does a final drain when cancelled in `shutdown`.
        let state_flusher_shutdown = tokio_util::sync::CancellationToken::new();
        let state_flusher_handle = state_flusher::spawn_flusher(
            db.clone(),
            state_store::AddonStateStore::global(),
            state_flusher::DEFAULT_FLUSH_INTERVAL,
            state_flusher_shutdown.clone(),
        );

        info!("AddonManager zainicjalizowany");

        Ok(Self {
            db,
            instances: Arc::new(Mutex::new(HashMap::new())),
            event_bus,
            engine,
            linker,
            permission_checker,
            settings_cipher,
            compiled_modules: Arc::new(PlRwLock::new(HashMap::new())),
            oauth_refresh_guard: Arc::new(oauth_refresh_guard::OAuthRefreshGuard::new()),
            registered_tools: Arc::new(PlRwLock::new(Vec::new())),
            router: Arc::new(PlRwLock::new(None)),
            flow_blocks_registry: Arc::new(flow_blocks::AddonFlowRegistry::new()),
            service_tasks: Arc::new(Mutex::new(HashMap::new())),
            ui_panels: Arc::new(PlRwLock::new(HashMap::new())),
            addon_op_locks: Arc::new(Mutex::new(HashMap::new())),
            service_instance_ids: Arc::new(Mutex::new(std::collections::HashSet::new())),
            instance_total: Arc::new(Mutex::new(HashMap::new())),
            state_flusher_shutdown,
            state_flusher_handle: Mutex::new(Some(state_flusher_handle)),
        })
    }

    /// Zwraca per-instancyjny mutex operacji lifecycle (lazy-create po addon_id).
    /// Trzymany przez uninstall_instance/update_instance i start_addon, zeby
    /// serializowac operacje na tej samej instancji.
    fn addon_op_lock(&self, addon_id: &str) -> Arc<Mutex<()>> {
        self.addon_op_locks
            .lock()
            .entry(addon_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Handle to the raw validated CBOR bytes cache written by `ui_render_cbor`.
    pub fn ui_panels(&self) -> Arc<PlRwLock<HashMap<(String, String, String), Vec<u8>>>> {
        self.ui_panels.clone()
    }

    /// Graceful shutdown — wolane z main.rs przed wyjsciem. Bez tego:
    /// 1) dispatcher event_bus task wisi na `blocking_recv` (cykl
    ///    referencyjny Arc<AddonManager> trzymany przez spawn_blocking),
    /// 2) service tick tasks pętlowicze przez select (token nigdy nie
    ///    cancelled na shutdown), 3) running instances blokują WAL exit.
    ///
    /// Po `shutdown()` proces moze normalnie wyjsc. Idempotent — wielokrotne
    /// wolanie OK (np. signal handler + tests cleanup).
    pub fn shutdown(&self) {
        info!("AddonManager: shutdown initiated");

        // 1. Anuluj wszystkie service tick loops — token.cancel() wybudza
        //    `select!` w petli, ktora wychodzi cleanly.
        let task_count = {
            let mut tasks = self.service_tasks.lock();
            let count = tasks.len();
            for (_iid, token) in tasks.drain() {
                token.cancel();
            }
            count
        };
        if task_count > 0 {
            info!("AddonManager: anulowano {} service tick loops", task_count);
        }

        // 1b. Cancel the write-behind state flusher — it runs ONE final drain
        //     of pending durable writes into SQLite before exiting. The drain is
        //     AWAITED separately by `await_state_flusher_drain` (the caller must
        //     run it from an async context) so the process does not exit before
        //     the final flush completes and loses pending durable state.
        self.state_flusher_shutdown.cancel();

        // 2. Zamknij dispatcher event_bus — drop sender, blocking_recv
        //    zwroci None, spawn_blocking task wychodzi. To uwalnia ostatni
        //    Arc<AddonManager> trzymany w tasku.
        self.event_bus.close_dispatcher();

        // 3. Drop wszystkich instances — wasmtime cleanup, net connections
        //    closed, host functions zaktualizuja audit DB.
        let instance_count = {
            let mut instances = self.instances.lock();
            let count: usize = instances.values().map(|v| v.len()).sum();
            instances.clear();
            count
        };
        if instance_count > 0 {
            info!(
                "AddonManager: rozwalonio {} addon instances",
                instance_count
            );
        }
    }

    /// Await the write-behind flusher's final drain after `shutdown()` cancelled
    /// it. Guarantees that, once this returns `Ok`, all durable state dirty at
    /// cancel time has been persisted (or the failure surfaced in logs). Bounded
    /// by `timeout` so a stuck DB writer can never hang process shutdown — on
    /// timeout the handle is dropped (the task is detached) and `Err` is
    /// returned. Idempotent: a second call (no handle left) is a no-op `Ok`.
    pub async fn await_state_flusher_drain(&self, timeout: std::time::Duration) -> Result<()> {
        let handle = self.state_flusher_handle.lock().take();
        let Some(handle) = handle else {
            return Ok(());
        };
        match tokio::time::timeout(timeout, handle).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(join_err)) => {
                Err(anyhow::anyhow!("state flusher task join failed: {join_err}"))
            }
            Err(_) => Err(anyhow::anyhow!(
                "state flusher final drain timed out after {:?} — pending durable writes may be unflushed",
                timeout
            )),
        }
    }

    /// Czy addon ma przynajmniej jedna running instancje.
    pub fn has_running_instance(&self, addon_id: &str) -> bool {
        self.instances
            .lock()
            .get(addon_id)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Zwraca rejestr flow blocks — dispatcher buduje z tego dynamic resolver
    /// dla `AdapterRegistry`, GUI handler dla listy bloków serializuje
    /// `list_all_blocks()`.
    pub fn flow_blocks_registry(&self) -> &Arc<flow_blocks::AddonFlowRegistry> {
        &self.flow_blocks_registry
    }

    /// Owning org of a running addon instance, read from its `AddonState`. The
    /// org context is fixed at start and lives only on the wasmtime Store (never
    /// on the DB `addons` row, which is org-agnostic), so the mesh robot
    /// advertiser must read it here to tenant-scope a robot's camera. Returns
    /// `None` when the addon has no running instance, or the instance pre-dates a
    /// real org context (system/boot start with `org_id == None`).
    pub fn instance_org_id(&self, addon_id: &str) -> Option<String> {
        let instances = self.instances.lock();
        let list = instances.get(addon_id)?;
        let first = list.first()?;
        first.store.data().org_id.clone()
    }

    /// Ustawia router do routowania requestow LLM z addonow
    pub fn set_router(&self, router: Arc<crate::routing::router::Router>) {
        *self.router.write() = Some(router);
        info!("AddonManager: router ustawiony dla host functions LLM");
    }

    /// Instaluje addon z podanego katalogu — czyta manifest.toml, waliduje,
    /// rejestruje w DB, kopiuje WASM
    pub fn install_addon(&self, addon_path: &Path) -> Result<()> {
        info!("Instalacja addonu z: {:?}", addon_path);

        // Parsuj manifest i zainstaluj
        let manifest = lifecycle::install(addon_path, &self.db)?;

        // Rejestracja runtime (toole, flow bloki, aliasy, [runtime] overrides).
        // blocks.json czytamy z katalogu zrodlowego addonu.
        self.register_addon_runtime(&manifest, addon_path)?;

        info!(
            "Addon '{}' v{} zainstalowany pomyslnie",
            manifest.addon_id, manifest.version
        );
        Ok(())
    }

    /// Rejestruje w pamieci managera wszystkie runtime'owe artefakty addonu/
    /// instancji z manifestu: toole, custom flow bloki (z `source_dir`), aliasy
    /// i [runtime] overrides. Wspolne dla install_addon, install_instance i
    /// re-rejestracji po update_instance. `source_dir` to katalog z plikami
    /// addonu (dla instancji: katalog wersjonowanego pakietu).
    fn register_addon_runtime(&self, manifest: &AddonManifest, source_dir: &Path) -> Result<()> {
        self.register_tools_from_manifest(manifest)?;

        // Custom flow bloki (jesli addon dostarcza blocks.json obok manifestu).
        // Brak blocks.json = addon ich nie deklaruje — graceful skip.
        match flow_blocks::load_blocks_from_addon(&manifest.addon_id, source_dir) {
            Ok(blocks) if !blocks.is_empty() => {
                let count = blocks.len();
                self.flow_blocks_registry
                    .register_addon_blocks(&manifest.addon_id, blocks);
                info!(
                    "Addon '{}': zarejestrowano {} flow block(s)",
                    manifest.addon_id, count
                );
            }
            Ok(_) => {}
            Err(e) => warn!(
                "Addon '{}': blad ladowania blocks.json: {}",
                manifest.addon_id, e
            ),
        }

        // Aliasy z [[alias]] + consumer-side [[uses_alias]]/[[uses_model]].
        // Jeden SQLite tx — czesciowa rejestracja rolluje sie czysto.
        if !manifest.aliases.is_empty()
            || !manifest.uses_aliases.is_empty()
            || !manifest.uses_models.is_empty()
        {
            self.install_manifest_aliases(manifest)?;
        }

        // [runtime] overrides → scheduler concurrency cap + service_call rate
        // limiter, zeby cap/limit dzialal od nastepnego wywolania. 0 = clear.
        if let Some(rt) = manifest.runtime_overrides.as_ref() {
            let sched = crate::flow_runtime::scheduler::FlowScheduler::global();
            match rt.max_concurrency {
                Some(n) if n > 0 => sched.set_addon_concurrency_cap(&manifest.addon_id, n),
                _ => sched.clear_addon_concurrency_cap(&manifest.addon_id),
            }
            let rl = crate::services::service_call_rate_limit::service_call_rate_limiter();
            match rt.rate_limit_per_min {
                Some(n) if n > 0 => rl.set_addon_rate_limit_per_min(&manifest.addon_id, n),
                _ => rl.clear_addon_rate_limit(&manifest.addon_id),
            }
        }
        Ok(())
    }

    /// Make the local runtime match the (possibly just-replicated) `addons` row:
    /// load + materialize derived state when the instance is installed & enabled,
    /// unload when it is disabled or removed. Idempotent. Called by the mesh-sync
    /// reconcile hook after a replicated `core.addon_instance` op commits.
    ///
    /// If the package files are not yet in the local store (an uploaded package
    /// whose blob has not arrived), this logs and skips — the row stays in the DB
    /// and the sync runtime re-reconciles once the package blob lands.
    fn reconcile_synced_addon(&self, addon_id: &str) {
        let addon = match crate::db::repository::get_addon(&self.db, addon_id) {
            Ok(Some(a)) => a,
            Ok(None) => {
                // Uninstalled on origin (materializer already purged the scoped
                // DB rows) → unload runtime + drop the per-instance SQLite pool
                // and data dir here (fs cleanup the materializer can't do).
                self.unregister_addon_runtime(addon_id);
                let org_id = crate::services::org::DEFAULT_ORG_ID;
                crate::addon::storage_sql::close_addon_db(org_id, addon_id);
                if let Ok(dir) = crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id) {
                    if dir.exists() {
                        if let Err(e) = std::fs::remove_dir_all(&dir) {
                            warn!("sync reconcile: usuwanie danych '{addon_id}' nieudane: {e}");
                        }
                    }
                }
                info!("sync reconcile: addon '{addon_id}' usuniety — odladowano i wyczyszczono");
                return;
            }
            Err(e) => {
                warn!("sync reconcile addon '{addon_id}': blad odczytu wiersza: {e}");
                return;
            }
        };
        if !addon.is_enabled {
            self.unregister_addon_runtime(addon_id);
            return;
        }
        let pkg_dir = crate::addon::bundled::package_dir(&addon.package_id, &addon.package_version);
        if !pkg_dir.join("manifest.toml").exists() {
            // Package bytes not here yet (uploaded package whose blob is still in
            // flight). The sync runtime re-reconciles this instance once the
            // package blob lands — so this is a transient skip, not a failure.
            tracing::debug!(
                "sync reconcile addon '{addon_id}': pakiet '{}' v{} jeszcze niedostepny — \
                 czekam na blob pakietu",
                addon.package_id,
                addon.package_version
            );
            return;
        }
        let manifest = match self.load_addon_manifest(addon_id) {
            Ok(m) => m,
            Err(e) => {
                warn!("sync reconcile addon '{addon_id}': blad manifestu: {e}");
                return;
            }
        };
        // Odladuj przed ponownym zaladowaniem — idempotentny, czysty stan runtime
        // (np. po zmianie enable albo ponownym reconcile tej samej instancji).
        self.unregister_addon_runtime(addon_id);
        // Materializacja musi sie udac PRZED rejestracja runtime — inaczej
        // zaladowalibysmy enabled addon bez migracji SQL / metadanych / flow.
        if let Err(e) =
            crate::addon::lifecycle::materialize_addon_derived_state(&self.db, &manifest, &pkg_dir)
        {
            warn!(
                "sync reconcile addon '{addon_id}': blad materializacji stanu — NIE laduje runtime: {e}"
            );
            return;
        }
        if let Err(e) = self.register_addon_runtime(&manifest, &pkg_dir) {
            warn!("sync reconcile addon '{addon_id}': blad rejestracji runtime: {e}");
            return;
        }
        info!("sync reconcile: addon '{addon_id}' zsynchronizowany i zaladowany");
    }

    /// Iterates `manifest.aliases` and registers each in `model_aliases`
    /// with `owner_type='addon'`. Gated aliases are deactivated until the
    /// policy engine (M2) or admin (M16) activates them.
    ///
    /// All alias writes for the manifest run inside a single SQLite
    /// transaction. On any per-alias failure the transaction is dropped
    /// uncommitted, which rolls back not just the `model_aliases` rows but
    /// also the `model_alias_owners` and `model_alias_changes` audit rows
    /// inserted in this call (the audit table has no FK on the alias, so
    /// a row-by-row `DELETE` style rollback would leave orphan audit
    /// entries that look like duplicate "create" events on the next try).
    ///
    /// Visibility note: kept `pub` (not `pub(crate)`) because integration
    /// tests under `tests/install_flow_e2e.rs` and `tests/abi_error_sweep.rs`
    /// are a separate crate from `tentaflow-core` and need direct access to
    /// drive the manifest install path without spinning up a full WASM
    /// instance. Moving these tests under `src/addon/mod.rs::tests` would
    /// pull in heavy fixtures (DB pool, cipher, runtime) that are already
    /// owned by the integration layer — the cost outweighs the surface
    /// reduction.
    pub fn install_manifest_aliases(&self, manifest: &AddonManifest) -> Result<()> {
        use crate::db::repository::{
            add_alias_consumer_within_tx, audit_consumer_revoked_by_manifest_within_tx,
            audit_reconcile_uses_alias_within_tx, lookup_alias_visibility_within_tx,
            reconcile_uses_alias_for_alias_within_tx, revoke_obsolete_manifest_consumers_within_tx,
            upsert_uses_alias_within_tx, upsert_uses_model_within_tx,
        };

        // 1. Owned [[alias]] rows (model_aliases + owner + visibility + methods +
        //    gate). Shared with install_core / mesh-sync reconcile so the alias
        //    exists no matter which path materializes the addon.
        crate::addon::lifecycle::materialize_addon_aliases(&self.db, manifest)?;

        let mut conn = self
            .db
            .write()
            .map_err(|e| anyhow::anyhow!("db write for alias install: {}", e))?;
        let tx = conn.transaction()?;

        // 1b. Consumer whitelist for `restricted` aliases (manager-only — the
        //     materializer creates the alias rows, this reconciles who may
        //     consume them). Drop manifest-granted consumers no longer listed;
        //     admin-granted rows (`granted_by_user_id IS NOT NULL`) are kept.
        for alias_spec in &manifest.aliases {
            let alias_id = match lookup_alias_visibility_within_tx(&tx, &alias_spec.id)? {
                Some((id, _)) => id,
                None => continue,
            };
            let desired_consumers: &[String] = match alias_spec.visibility {
                crate::addon::manifest::AliasVisibility::Restricted => {
                    &alias_spec.allowed_consumers
                }
                _ => &[],
            };
            let revoked =
                revoke_obsolete_manifest_consumers_within_tx(&tx, alias_id, desired_consumers)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "addon '{}' alias '{}' consumer revoke failed: {}",
                            manifest.addon_id,
                            alias_spec.id,
                            e
                        )
                    })?;
            for consumer in &revoked {
                audit_consumer_revoked_by_manifest_within_tx(
                    &tx,
                    &manifest.addon_id,
                    &alias_spec.id,
                    consumer,
                )?;
            }

            for consumer in &alias_spec.allowed_consumers {
                add_alias_consumer_within_tx(&tx, alias_id, consumer, None).map_err(|e| {
                    anyhow::anyhow!(
                        "addon '{}' alias '{}' consumer '{}' write failed: {}",
                        manifest.addon_id,
                        alias_spec.id,
                        consumer,
                        e
                    )
                })?;
            }
        }

        // 2. Process consumer-side [[uses_alias]] declarations. Status is
        //    computed against the current view of model_alias_visibility /
        //    model_alias_consumers (which already includes this addon's
        //    own [[alias]] writes above).
        for uses in &manifest.uses_aliases {
            let status = upsert_uses_alias_within_tx(
                &tx,
                &manifest.addon_id,
                &uses.id,
                uses.required,
                &uses.reason,
            )?;
            if uses.required && status != "granted" && status != "auto_granted" {
                anyhow::bail!(
                    "addon '{}' requires alias '{}' but grant_status='{}'; install rejected",
                    manifest.addon_id,
                    uses.id,
                    status
                );
            }
        }

        // 3. Same for [[uses_model]].
        for uses in &manifest.uses_models {
            let status = upsert_uses_model_within_tx(
                &tx,
                &manifest.addon_id,
                &uses.id,
                uses.required,
                &uses.reason,
            )?;
            if uses.required && status != "granted" && status != "auto_granted" {
                anyhow::bail!(
                    "addon '{}' requires model '{}' but grant_status='{}'; install rejected",
                    manifest.addon_id,
                    uses.id,
                    status
                );
            }
        }

        // 4. Reconciliation. For each alias we just (re)installed, scan
        //    addon_uses_alias rows pointing at this alias and recompute
        //    statuses. Audit every transition as risk_class=A.
        for alias_spec in &manifest.aliases {
            // Only reconcile when the alias actually exists in this tx —
            // gated aliases are still inserted, but stay is_active=0.
            if lookup_alias_visibility_within_tx(&tx, &alias_spec.id)?.is_none() {
                continue;
            }
            let transitions = reconcile_uses_alias_for_alias_within_tx(&tx, &alias_spec.id)?;
            for (consumer, before, after) in transitions {
                audit_reconcile_uses_alias_within_tx(
                    &tx,
                    &consumer,
                    &alias_spec.id,
                    &before,
                    &after,
                )?;
            }
        }

        tx.commit()?;
        drop(conn);
        self.reload_router_alias_cache();
        Ok(())
    }

    /// Odinstalowuje addon — usuwa z DB, czysci storage, zatrzymuje instancje
    pub fn uninstall_addon(&self, addon_id: &str) -> Result<()> {
        info!("Odinstalowanie addonu: {}", addon_id);
        self.unregister_addon_runtime(addon_id);
        lifecycle::uninstall(addon_id, &self.db)?;
        self.event_bus.unsubscribe_all(addon_id);
        // A2: an uninstalled addon's persisted state must not survive — drop the
        // RAM shard and purge the SQLite rows. Done after lifecycle teardown so a
        // failed uninstall never strands an addon with no state.
        self.purge_addon_state(addon_id);
        info!("Addon '{}' odinstalowany pomyslnie", addon_id);
        Ok(())
    }

    /// Odinstalowuje INSTANCJE — jak uninstall_addon, ale dodatkowo usuwa
    /// katalog danych instancji (orgs/<org>/addons/<addon_id>/). Dane instancji
    /// sa jej wlasnoscia i nie powinny zostawac po deinstalacji. Nie rusza
    /// wspoldzielonego store'u pakietow.
    pub fn uninstall_instance(&self, addon_id: &str) -> Result<()> {
        info!("Odinstalowanie instancji: {}", addon_id);
        let op = self.addon_op_lock(addon_id);
        let _guard = op.lock();
        self.unregister_addon_runtime(addon_id);
        lifecycle::uninstall_instance(addon_id, &self.db)?;
        self.event_bus.unsubscribe_all(addon_id);
        // A2: instance data is its own — drop its RAM shard and purge its rows.
        self.purge_addon_state(addon_id);
        info!("Instancja '{}' odinstalowana", addon_id);
        Ok(())
    }

    /// A2 uninstall cleanup: drop the in-RAM state shard (any unflushed durable
    /// writes are intentionally discarded — the addon is being removed) and
    /// purge its persisted rows from SQLite. Best-effort: a purge failure is
    /// logged, not fatal, so it never blocks uninstall (orphaned rows are inert
    /// and a reinstall under the same id reloads them, which is acceptable).
    fn purge_addon_state(&self, addon_id: &str) {
        let store = state_store::AddonStateStore::global();
        store.drop_addon(addon_id);
        // L1: the addon is being removed — drop its latest LiDAR frame slot too
        // (keyed by addon_id == robot_id), matching the state shard's
        // best-effort purge-on-uninstall lifecycle above. Like `drop_addon`, this
        // is best-effort: on a force-uninstall while a service tick is still alive,
        // a later `lidar_publish_v1` can transiently recreate the slot (the hub
        // analog of the state store re-resolving a fresh, orphaned shard). It is
        // bounded — at most one latest-wins frame (≤4 MiB) until the tick dies.
        crate::services::lidar_hub::LidarStreamHub::global().remove(addon_id);
        crate::services::slam_scene::SlamSceneManager::global().remove(addon_id);
        crate::services::localization::LocalizationEngine::global().remove(addon_id);
        crate::services::mobile_camera::MobileCameraIngest::global().remove(addon_id);
        if let Err(e) = state_flusher::purge_addon(&self.db, addon_id) {
            warn!(
                "addon state: purge on uninstall failed for '{}': {}",
                addon_id, e
            );
        }
    }

    /// Zdejmuje z pamieci managera wszystkie runtime'owe artefakty addonu:
    /// zatrzymuje uruchomione instancje wasm, czysci cache modulu, wyrejestrowuje
    /// toole i flow bloki, invaliduje FlowCache i deaktywuje aliasy. NIE rusza
    /// DB ani danych — to robi caller (uninstall/update). Wspolne dla
    /// uninstall_addon, uninstall_instance i update_instance (hot reload).
    fn unregister_addon_runtime(&self, addon_id: &str) {
        // Zatrzymaj wszystkie instancje wasm tego addonu.
        let instance_ids: Vec<String> = {
            let instances = self.instances.lock();
            instances
                .get(addon_id)
                .map(|v| v.iter().map(|i| i.instance_id.clone()).collect())
                .unwrap_or_default()
        };
        for instance_id in &instance_ids {
            if let Err(e) = self.stop_addon(instance_id) {
                warn!("Blad przy zatrzymywaniu instancji '{}': {}", instance_id, e);
            }
        }

        // Usun skompilowany modul z cache (nastepny start skompiluje aktualny
        // wasm — istotne dla update_instance).
        self.compiled_modules.write().remove(addon_id);

        // Usun zarejestrowane narzedzia.
        self.registered_tools
            .write()
            .retain(|t| t.addon_id != addon_id);

        // Usun custom flow bloki — adapter resolver natychmiast przestanie ich
        // znajdowac.
        self.flow_blocks_registry.unregister_addon_blocks(addon_id);

        // Invalidate FlowCache — cached `CompiledFlow` moze miec dangling
        // reference do bloku tego addonu, ktory wlasnie znika z resolvera.
        if let Some(router) = self.router.read().clone() {
            if let Some(dispatcher) = router.flow_dispatcher() {
                dispatcher.invalidate_cache();
            }
        }

        // Deaktywuj aliasy posiadane przez addon — czytamy owner table wprost
        // (manifest moze byc juz nieosiagalny). Owner rows zostaja dla audytu.
        self.deactivate_aliases_owned_by_addon(addon_id);

        // Zamknij i usun wszystkie kanaly WebRTC tego addonu (peer connections
        // nie moga przeciekac po unload/disable/uninstall).
        crate::addon::host_functions::webrtc::cleanup_addon_channels(addon_id);
    }

    /// Instaluje NOWA instancje pakietu z katalogu pod wlasnym addon_id i
    /// rejestruje jej runtime (toole, flow bloki z katalogu pakietu, aliasy).
    /// Zwraca addon_id utworzonej instancji. Nie startuje jej — start jest
    /// osobna akcja (jak przy install_addon).
    pub fn install_instance(
        &self,
        package_id: &str,
        version: &str,
        display_name: &str,
        config: &std::collections::BTreeMap<String, String>,
    ) -> Result<String> {
        info!(
            "Instalacja instancji pakietu '{}' v{} jako '{}'",
            package_id, version, display_name
        );
        let instance_id =
            lifecycle::install_instance(&self.db, package_id, version, display_name, config)?;
        let manifest = self.load_addon_manifest(&instance_id)?;
        let pkg_dir = bundled::package_dir(package_id, version);
        self.register_addon_runtime(&manifest, &pkg_dir)?;
        info!(
            "Instancja '{}' (pakiet '{}' v{}) zainstalowana",
            instance_id, package_id, version
        );
        Ok(instance_id)
    }

    /// Duplikuje istniejaca instancje: nowa instancja tego samego pakietu i
    /// wersji pod nowa nazwa, z pustymi danymi.
    pub fn duplicate_instance(
        &self,
        source_addon_id: &str,
        new_display_name: &str,
    ) -> Result<String> {
        let (package_id, version) =
            crate::db::repository::get_addon_instance_package_ref(&self.db, source_addon_id)?
                .ok_or_else(|| anyhow::anyhow!("instancja '{source_addon_id}' nie istnieje"))?;
        // Carry the source instance's connection-param values so the duplicate
        // passes required-param validation and points at the same robot.
        let config: std::collections::BTreeMap<String, String> =
            crate::db::repository::list_addon_config_rows(&self.db, source_addon_id)?
                .into_iter()
                .filter(|row| !row.is_secret)
                .map(|row| (row.key, row.value))
                .collect();
        self.install_instance(&package_id, &version, new_display_name, &config)
    }

    /// Hot-update instancji do innej (juz skatalogowanej) wersji jej pakietu,
    /// bez restartu glownego procesu TentaFlow i bez ruszania innych instancji.
    /// Zatrzymuje wasm instancji, podbija wersje + aplikuje brakujace migracje do
    /// jej wlasnego SQLite, przerejestrowuje toole/flow bloki/metadane z nowej
    /// wersji i restartuje instancje w trybie service (on-demand wstana leniwie
    /// z nowym modulem).
    pub fn update_instance(&self, addon_id: &str, target_version: &str) -> Result<()> {
        let (package_id, current) =
            crate::db::repository::get_addon_instance_package_ref(&self.db, addon_id)?
                .ok_or_else(|| anyhow::anyhow!("instancja '{addon_id}' nie istnieje"))?;
        if current == target_version {
            // Ta sama wersja: pomijamy TYLKO gdy tresc pakietu sie nie zmienila.
            // Bundled addony czesto wypuszczaja zmiany pod tym samym numerem wersji
            // — wtedy `bundle_hash` w katalogu rozni sie od zapisanego na instancji
            // i przeladowanie tej samej wersji odswieza manifest (uprawnienia, storage).
            let catalog_hash = crate::db::repository::get_package_bundle_hash(
                &self.db,
                &package_id,
                target_version,
            )?
            .unwrap_or_default();
            let installed_hash =
                crate::db::repository::get_instance_installed_bundle_hash(&self.db, addon_id)?;
            if catalog_hash.is_empty() || catalog_hash == installed_hash {
                info!(
                    "Instancja '{}' juz na wersji {} bez zmian tresci — pomijam update",
                    addon_id, target_version
                );
                return Ok(());
            }
            info!(
                "Instancja '{}' v{}: ta sama wersja, zmieniona tresc (bundle_hash) — przeladowuje",
                addon_id, target_version
            );
        }
        info!(
            "Hot-update instancji '{}': v{} -> v{}",
            addon_id, current, target_version
        );

        // Sekcja krytyczna pod per-instancyjnym lockiem: serializuje update
        // wzgledem rownoleglego startu/innego update tej samej instancji.
        // Restart service-mode robimy PO zwolnieniu locka, bo start_addon sam go
        // bierze (parking_lot Mutex nie jest reentrant).
        let manifest = {
            let op = self.addon_op_lock(addon_id);
            let _guard = op.lock();

            // Manifest sprzed update — do rollbacku gdy lifecycle zawiedzie.
            let old_manifest = self.load_addon_manifest(addon_id).ok();
            let old_pkg_dir = bundled::package_dir(&package_id, &current);

            // 1. Zdejmij runtime: stop wasm instancji + czysc cache modulu +
            //    wyrejestruj toole/flow bloki + invalidate FlowCache.
            self.unregister_addon_runtime(addon_id);

            // 2. DB: podbij wersje, zaaplikuj brakujace migracje, zsynchronizuj
            //    metadane i flow templates. Zwraca docelowy manifest. Przy bledzie
            //    wiersz addons zostaje na starej wersji (migracje leca przed tx) —
            //    re-rejestrujemy stary runtime, zeby instancja nie zostala martwa.
            let updated = match lifecycle::update_instance(&self.db, addon_id, target_version) {
                Ok(m) => m,
                Err(e) => {
                    if let Some(om) = old_manifest {
                        if let Err(re) = self.register_addon_runtime(&om, &old_pkg_dir) {
                            warn!(
                                "Rollback re-rejestracji instancji '{}' po nieudanym update: {}",
                                addon_id, re
                            );
                        }
                    }
                    return Err(e);
                }
            };

            // 3. Zarejestruj runtime z nowej wersji (flow bloki z nowego katalogu
            //    pakietu). Gdy to zawiedzie, DB jest juz na nowej wersji —
            //    propagujemy blad; kolejny start/boot zarejestruje z DB.
            let pkg_dir = bundled::package_dir(&package_id, target_version);
            self.register_addon_runtime(&updated, &pkg_dir)?;
            updated
        };

        // 4. Restart service-mode (on-demand instancje wstana leniwie przy
        //    nastepnym wywolaniu, juz z nowym modulem).
        if let Some(service) = manifest.service.as_ref() {
            if service.enabled && service.tick_interval_ms.map(|i| i > 0).unwrap_or(false) {
                if let Err(e) = self.start_addon(addon_id, None, None) {
                    warn!(
                        "Restart service instancji '{}' po update nie powiodl sie: {}",
                        addon_id, e
                    );
                }
            }
        }

        info!(
            "Instancja '{}' zaktualizowana do v{}",
            addon_id, target_version
        );
        Ok(())
    }

    /// Buduje w pełni zainicjalizowaną instancję WASM: state → store →
    /// instantiate → WASI `_start`/`_initialize` → `on_start`. Wspólne dla
    /// `start_addon` (instancja główna/serwisowa) i puli workerów
    /// (`acquire_instance`). `on_start` to inicjalizacja PER-INSTANCJA, więc
    /// każdy worker przechodzi ją niezależnie — zamierzone (instancje są
    /// wymienne, stan trwały idzie przez scope-keyed storage, nie pamięć WASM).
    fn build_ready_instance(
        &self,
        addon_id: &str,
        user_id: Option<String>,
        org_id: Option<String>,
        module: &WasmModule,
        manifest: AddonManifest,
        permissions: Vec<String>,
    ) -> Result<AddonInstance> {
        let rt_id = manifest
            .runtime
            .clone()
            .unwrap_or_else(|| "wasmtime".to_string());
        let instance_id = uuid::Uuid::new_v4().to_string();
        let is_system_call = user_id.is_none();
        let state = AddonState {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id: user_id.clone(),
            org_id,
            db: self.db.clone(),
            permissions,
            event_bus: self.event_bus.clone(),
            permission_checker: self.permission_checker.clone(),
            fuel_consumed: 0,
            is_system_call,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(
                host_functions::network::NetworkConnectionManager::new(),
            )),
            settings_cipher: self.settings_cipher.clone(),
            manifest: Arc::new(manifest),
            memory_limit: DEFAULT_MEMORY_LIMIT_BYTES,
            router: self.router.read().clone(),
            oauth_refresh_guard: self.oauth_refresh_guard.clone(),
            ui_panels: Some(self.ui_panels.clone()),
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
            #[cfg(any(target_os = "ios", target_os = "android"))]
            store_limits: wasmi::StoreLimitsBuilder::new()
                .memory_size(DEFAULT_MEMORY_LIMIT_BYTES)
                .trap_on_grow_failure(true)
                .instances(10)
                .memories(1)
                .tables(10)
                .build(),
        };

        let mut store = runtime::create_store(&self.engine, state)?;
        let instance = runtime::instantiate(&self.linker, &mut store, module)?;

        let adapter =
            runtime::adapter_for_runtime(&rt_id).unwrap_or_else(|| Box::new(runtime::RustAdapter));

        // .NET NativeAOT / CPython need _start or _initialize to bootstrap their
        // managed runtime before any lifecycle call.
        if adapter.needs_wasi_start() {
            let init_fuel = adapter.init_fuel_budget();
            if init_fuel > 0 {
                runtime::refuel_store(&mut store, init_fuel)?;
            }
            let wasi_start = instance
                .get_typed_func::<(), ()>(&mut store, "_start")
                .ok()
                .or_else(|| {
                    instance
                        .get_typed_func::<(), ()>(&mut store, "_initialize")
                        .ok()
                });
            if let Some(f) = wasi_start {
                f.call(&mut store, ())
                    .map_err(|e| anyhow::anyhow!("WASI _start/_initialize failed: {e}"))?;
            }
            runtime::refuel_store(&mut store, DEFAULT_FUEL_LIMIT)?;
        }

        if let Ok(on_start) =
            instance.get_typed_func::<(), i32>(&mut store, adapter.export_on_start())
        {
            let result = on_start.call(&mut store, ()).map_err(|e| {
                anyhow::anyhow!("Blad wywolania {}(): {e}", adapter.export_on_start())
            })?;
            if result != 0 {
                bail!("{}() zwrocil blad: {}", adapter.export_on_start(), result);
            }
        }

        Ok(AddonInstance {
            addon_id: addon_id.to_string(),
            instance_id,
            user_id,
            store,
            instance,
            language_adapter: adapter,
        })
    }

    /// Górny limit instancji workerów per addon. Z manifestu `[runtime].max_concurrency`
    /// (jeśli >0), inaczej `min(8, liczba_rdzeni)`. Limit bounduje pamięć puli;
    /// pod chwilowym burstem powyżej limitu `acquire_instance` tworzy efemeryczne
    /// instancje (dropowane przy zwrocie), więc nigdy nie blokuje na twardo.
    fn pool_cap(&self, manifest: &AddonManifest) -> usize {
        manifest
            .runtime_overrides
            .as_ref()
            .and_then(|rt| rt.max_concurrency)
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or_else(|| num_cpus::get().clamp(1, 8))
    }

    /// Wypożycza gotowy worker dla wywołania user-facing (call_tool/panel_open).
    /// Kolejność: (1) wolny worker z puli (pomijając instancje serwisowe),
    /// (2) budowa nowego do `pool_cap`, (3) krótkie oczekiwanie na zwrot,
    /// (4) fallback: efemeryczny worker ponad limit. Zwraca `(instancja,
    /// ephemeral)`; `release_instance` dropuje efemeryczne, a resztę oddaje do
    /// puli. Zastępuje dawny 3-sekundowy busy-loop, który padał „zajęty".
    fn acquire_instance(
        &self,
        addon_id: &str,
        user_id: Option<String>,
        system: bool,
    ) -> Result<(AddonInstance, bool)> {
        // (1) wolny, nie-serwisowy worker z puli.
        if let Some(inst) = self.take_idle_instance(addon_id, &user_id, system) {
            return Ok((inst, false));
        }

        let module = self.get_or_compile_module(addon_id)?;
        let manifest = self.load_addon_manifest(addon_id)?;
        let permissions = self.load_addon_permissions(addon_id)?;
        let cap = self.pool_cap(&manifest);
        // On-demand workery dziedziczą org instancji głównej (start_addon), żeby
        // scope-keyed storage trafiał do właściwego najemcy.
        let org_id = self.instance_org_id(addon_id);

        // (2) rośnij do limitu.
        {
            let mut totals = self.instance_total.lock();
            let n = totals.entry(addon_id.to_string()).or_insert(0);
            if *n < cap {
                *n += 1;
                drop(totals);
                return match self.build_ready_instance(
                    addon_id,
                    user_id.clone(),
                    org_id.clone(),
                    &module,
                    manifest,
                    permissions,
                ) {
                    Ok(inst) => Ok((inst, false)),
                    Err(e) => {
                        let mut t = self.instance_total.lock();
                        let c = t.entry(addon_id.to_string()).or_insert(0);
                        *c = c.saturating_sub(1);
                        Err(e)
                    }
                };
            }
        }

        // (3) przy limicie — krótkie oczekiwanie aż ktoś zwróci worker.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Some(inst) = self.take_idle_instance(addon_id, &user_id, system) {
                return Ok((inst, false));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        // (4) burst fallback — efemeryczny worker ponad limit (drop przy zwrocie).
        let inst =
            self.build_ready_instance(addon_id, user_id, org_id, &module, manifest, permissions)?;
        Ok((inst, true))
    }

    /// Bierze wolny, nie-serwisowy worker z puli i ustawia tożsamość wywołania na
    /// czas wywołania (org zostaje — worker zna swojego najemcę z budowy). `None`
    /// gdy brak wolnego workera nie-serwisowego.
    ///
    /// Workery są reużywane między wywołaniami różnych principalów, więc tożsamość
    /// decydująca o uprawnieniach (`user_id` + `is_system_call`) MUSI być
    /// nadpisana PER WYWOŁANIE — inaczej worker zbudowany dla usera mógłby wykonać
    /// system call ze starym user_id (lub odwrotnie), psując decyzję permission.
    /// `system=true` ⇒ `user_id=None` + `is_system_call=true` (ścieżka CR-006);
    /// wywołanie user-facing ⇒ konkretny `user_id` + `is_system_call=false`.
    fn take_idle_instance(
        &self,
        addon_id: &str,
        user_id: &Option<String>,
        system: bool,
    ) -> Option<AddonInstance> {
        // Kolejność locków: ZAWSZE instances przed service_instance_ids (ta sama
        // co w stop_addon) — inaczej groziłoby zakleszczenie.
        let mut map = self.instances.lock();
        let list = map.get_mut(addon_id)?;
        let pos = {
            let svc = self.service_instance_ids.lock();
            list.iter().position(|i| !svc.contains(&i.instance_id))?
        };
        let mut inst = list.remove(pos);
        let state = inst.store.data_mut();
        state.user_id = user_id.clone();
        state.is_system_call = system;
        inst.user_id = user_id.clone();
        Some(inst)
    }

    /// Zwraca wypożyczony worker. Efemeryczny (burst ponad limit) jest dropowany
    /// i zmniejsza licznik; zwykły wraca do puli do ponownego użycia.
    fn release_instance(&self, addon_id: &str, instance: AddonInstance, ephemeral: bool) {
        if ephemeral {
            let mut t = self.instance_total.lock();
            let c = t.entry(addon_id.to_string()).or_insert(0);
            *c = c.saturating_sub(1);
            return;
        }
        self.instances
            .lock()
            .entry(addon_id.to_string())
            .or_default()
            .push(instance);
    }

    /// Uruchamia addon — tworzy instancje WASM, zwraca instance_id.
    ///
    /// F2 P1.b — `org_id` scopes the instance to its owning tenant. `None`
    /// means "system / boot start" (legacy single-tenant nodes) and gets
    /// recorded as `org-default` in downstream audit / sandbox paths.
    pub fn start_addon(
        &self,
        addon_id: &str,
        user_id: Option<String>,
        org_id: Option<String>,
    ) -> Result<String> {
        let t_total = std::time::Instant::now();

        // Serializuj start wzgledem hot-update/uninstall tej samej instancji
        // (per-instancyjny lock). Zapobiega skompilowaniu/uruchomieniu starego
        // modulu w trakcie wymiany wersji. Update bierze ten lock tylko w swojej
        // sekcji krytycznej i zwalnia go przed wywolaniem start_addon, wiec brak
        // reentrancy.
        let op = self.addon_op_lock(addon_id);
        let _op_guard = op.lock();

        let t0 = std::time::Instant::now();
        let module = self.get_or_compile_module(addon_id)?;
        let dt_compile = t0.elapsed();

        let t0 = std::time::Instant::now();
        let permissions = self.load_addon_permissions(addon_id)?;
        let manifest = self.load_addon_manifest(addon_id)?;
        let dt_db = t0.elapsed();

        // A2: seed the shared in-RAM Durable state from SQLite BEFORE on_start
        // runs (inside build_ready_instance), so the addon observes its
        // persisted state. The store is per-addon shared RAM keyed by addon_id;
        // the load-once guard makes the seed idempotent so a second instance
        // start of the same addon does not reload/clobber live state. A genuine
        // DB ERROR FAILS start: running on phantom-empty state would let on_start
        // overwrite/invalidate persisted durable data (an empty store is Ok with
        // 0 rows, which is fine — only a real read failure aborts start).
        let outcome =
            state_flusher::load_addon(&self.db, state_store::AddonStateStore::global(), addon_id)
                .map_err(|e| {
                anyhow::anyhow!(
                    "addon '{}' start aborted: cannot load persisted durable state: {}",
                    addon_id,
                    e
                )
            })?;
        if !outcome.already_loaded
            && (outcome.loaded > 0
                || outcome.skipped_quota > 0
                || outcome.skipped_value_too_large > 0
                || outcome.skipped_present > 0)
        {
            info!(
                "addon state: loaded {} durable entr(ies) for '{}' (skipped: {} oversized, {} over-quota, {} already-present)",
                outcome.loaded,
                addon_id,
                outcome.skipped_value_too_large,
                outcome.skipped_quota,
                outcome.skipped_present
            );
        }

        // Buduj w pełni zainicjalizowaną instancję główną przez wspólny builder
        // (ta sama ścieżka co workery puli — instancje są wymienne).
        let addon_instance = self.build_ready_instance(
            addon_id,
            user_id.clone(),
            org_id.clone(),
            &module,
            manifest,
            permissions,
        )?;
        let instance_id = addon_instance.instance_id.clone();

        info!(
            "start_addon '{}' timing: compile={:?} db={:?} total={:?}",
            addon_id,
            dt_compile,
            dt_db,
            t_total.elapsed()
        );

        // Zaktualizuj status instancji w DB
        {
            let conn = self.db.write().unwrap();
            conn.execute(
                "INSERT INTO addon_instances (addon_id, instance_id, instance_name, status, created_by, started_at) \
                 VALUES (?1, ?2, ?3, 'running', ?4, datetime('now'))",
                rusqlite::params![addon_id, &instance_id, format!("{}-{}", addon_id, &instance_id[..8]), user_id],
            ).map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac instancji w DB: {e}"))?;
        }

        // Policz instancję główną w limicie puli (idle w mapie + wypożyczone).
        *self
            .instance_total
            .lock()
            .entry(addon_id.to_string())
            .or_insert(0) += 1;

        // Dodaj do mapy instancji
        self.instances
            .lock()
            .entry(addon_id.to_string())
            .or_default()
            .push(addon_instance);

        // Opublikuj event
        self.event_bus.publish(Event {
            event_type: "addon.started".to_string(),
            source_addon: Some(addon_id.to_string()),
            source_user: user_id,
            payload: serde_json::json!({
                "addon_id": addon_id,
                "instance_id": &instance_id,
            }),
            timestamp: chrono::Utc::now(),
        });

        // Tryb ciagly (service mode) — manifest deklaruje sekcje [service] z
        // tick_interval_ms. AddonManager spawnuje dedykowany tokio task
        // ktory periodycznie wola `on_tick(timestamp_ms)` na trzymanej
        // instancji. Persistent state w guest memory zostaje miedzy tickami.
        // Cancel token w `service_tasks` pozwala stop_addon zatrzymac petle.
        let manifest_for_service = self.load_addon_manifest(addon_id).ok();
        if let Some(manifest) = manifest_for_service.as_ref() {
            if let Some(service) = manifest.service.as_ref() {
                if service.enabled {
                    if let Some(interval_ms) = service.tick_interval_ms {
                        if interval_ms > 0 {
                            let fuel = service.tick_fuel_budget.unwrap_or(DEFAULT_TICK_FUEL_BUDGET);
                            let timeout_ms = service.tick_timeout_ms;
                            self.spawn_service_tick_loop(
                                addon_id.to_string(),
                                instance_id.clone(),
                                interval_ms,
                                fuel,
                                timeout_ms,
                            );
                        }
                    }
                }
            }
        }

        // Reactivate non-gated aliases owned by this addon. Gated aliases
        // stay parked until policy engine / admin flips them on.
        self.activate_aliases_owned_by_addon(addon_id);

        info!(
            "Addon '{}' uruchomiony, instance_id={}",
            addon_id, instance_id
        );
        Ok(instance_id)
    }

    /// Auto-start wszystkich zainstalowanych addonow w trybie service ktore
    /// maja `is_enabled = true` w DB. Wolane raz przy starcie binarki po
    /// `start_event_dispatcher` — bez tego addony service mode dzialaja tylko
    /// w sesji w ktorej zostaly explicit `start_addon`'em, a po reboocie
    /// tentaflow trzeba je rece startowac.
    pub fn auto_start_services(&self) {
        let addons = match crate::db::repository::list_addons(&self.db) {
            Ok(a) => a,
            Err(e) => {
                warn!("auto_start_services: list_addons: {}", e);
                return;
            }
        };
        for a in addons {
            if !a.is_enabled {
                continue;
            }
            // UWAGA: `manifest_json` w DB to RAW manifest.toml string
            // (nazwa kolumny myli, patrz lifecycle.rs:125). Parsujemy
            // przez `parse_manifest_toml`, NIE serde_json.
            let manifest: AddonManifest = match lifecycle::parse_manifest_toml(&a.manifest_json) {
                Ok(m) => m,
                Err(e) => {
                    warn!(
                        "auto_start_services: '{}' manifest_json niepoprawny: {}",
                        a.addon_id, e
                    );
                    continue;
                }
            };
            let has_service = manifest
                .service
                .as_ref()
                .map(|s| s.enabled && s.tick_interval_ms.map(|i| i > 0).unwrap_or(false))
                .unwrap_or(false);
            if !has_service {
                continue;
            }
            match self.start_addon(&a.addon_id, None, None) {
                Ok(iid) => info!(
                    "auto_start_services: '{}' uruchomiony, instance_id={}",
                    a.addon_id, iid
                ),
                Err(e) => warn!("auto_start_services: '{}' fail: {}", a.addon_id, e),
            }
        }
    }

    /// Toggle `is_enabled` flagi w DB + runtime side-effects:
    /// - `enabled = false`: zatrzymuje wszystkie running instances tego addonu
    ///   (anulujac service tick loops). Konfiguracja zostaje w DB, mozna
    ///   wlaczyc z powrotem bez deinstalacji.
    /// - `enabled = true`: aktualizuje flage; jezeli addon ma service mode,
    ///   startuje swiezo instancje.
    pub fn set_addon_enabled(&self, addon_id: &str, enabled: bool) -> Result<()> {
        info!("Toggle is_enabled dla addonu '{}' -> {}", addon_id, enabled);

        {
            let conn = self.db.write().unwrap();
            conn.execute(
                "UPDATE addons SET is_enabled = ?1, updated_at = datetime('now') WHERE addon_id = ?2",
                rusqlite::params![enabled as i64, addon_id],
            )
            .map_err(|e| anyhow::anyhow!("UPDATE is_enabled: {e}"))?;
        }

        if !enabled {
            // Zatrzymaj wszystkie instancje
            let instance_ids: Vec<String> = {
                let instances = self.instances.lock();
                instances
                    .get(addon_id)
                    .map(|v| v.iter().map(|i| i.instance_id.clone()).collect())
                    .unwrap_or_default()
            };
            for iid in instance_ids {
                if let Err(e) = self.stop_addon(&iid) {
                    warn!("set_addon_enabled stop '{}': {}", iid, e);
                }
            }
        } else {
            // Sprawdz czy ma service mode — jesli tak, wystartuj
            let manifest = self.load_addon_manifest(addon_id)?;
            let has_service = manifest
                .service
                .as_ref()
                .map(|s| s.enabled && s.tick_interval_ms.map(|i| i > 0).unwrap_or(false))
                .unwrap_or(false);
            if has_service {
                self.start_addon(addon_id, None, None)?;
            }
        }

        Ok(())
    }

    /// Spawnuje petle tickow dla addonu w trybie service. Loop dziala dopóki
    /// `stop_addon` nie anuluje tokenu w `service_tasks`. Kazdy tick:
    /// - sprawdza cancel token (select),
    /// - czeka `interval_ms`,
    /// - woluje `call_tick(addon_id, instance_id, fuel)` — bierze instancje
    ///   z mapy, refueluje store, wola WASM `on_tick(timestamp_ms)`.
    /// Bledy tick'a nie zabijaja petli — addon w trybie service ma szanse
    /// odzyskac sprawnosc przy nastepnym ticku. Crash z fuel exhaustion =
    /// trap, instancja zostaje porzucona, w przyszlosci moglibyśmy ja
    /// odtworzyc; MVP zostawia kierownikowi (admin) decyzje przez logi.
    fn spawn_service_tick_loop(
        &self,
        addon_id: String,
        instance_id: String,
        interval_ms: u64,
        fuel_per_tick: u64,
        timeout_ms: Option<u64>,
    ) {
        let token = tokio_util::sync::CancellationToken::new();
        self.service_tasks
            .lock()
            .insert(instance_id.clone(), token.clone());
        // Rezerwuj tę instancję dla pętli serwisowej — user-calls (call_tool/
        // call_panel_open) jej nie wezmą, więc tick i obsługa żądań się nie biją.
        self.service_instance_ids.lock().insert(instance_id.clone());

        let manager_instances = self.instances.clone();
        let event_bus = self.event_bus.clone();
        let addon_id_for_log = addon_id.clone();
        let instance_id_for_log = instance_id.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Pierwsze tick() wraca natychmiast — odpuscic, zeby addon mial
            // chwile na ustawienie sie po on_start.
            interval.tick().await;

            info!(
                "Service tick loop wystartowany dla '{}' (instance={}, interval={}ms, fuel={})",
                addon_id_for_log, instance_id_for_log, interval_ms, fuel_per_tick
            );

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        info!(
                            "Service tick loop dla '{}' (instance={}) zatrzymany",
                            addon_id_for_log, instance_id_for_log
                        );
                        break;
                    }
                    _ = interval.tick() => {
                        let res = tokio::task::block_in_place(|| {
                            Self::call_tick_static(
                                &manager_instances,
                                &addon_id_for_log,
                                &instance_id_for_log,
                                fuel_per_tick,
                                timeout_ms,
                            )
                        });
                        if let Err(e) = res {
                            warn!(
                                "on_tick failed for '{}' (instance={}): {}",
                                addon_id_for_log, instance_id_for_log, e
                            );
                            event_bus.publish(Event {
                                event_type: "addon.tick_error".to_string(),
                                source_addon: Some(addon_id_for_log.clone()),
                                source_user: None,
                                payload: serde_json::json!({
                                    "addon_id": &addon_id_for_log,
                                    "instance_id": &instance_id_for_log,
                                    "error": e.to_string(),
                                }),
                                timestamp: chrono::Utc::now(),
                            });
                        }
                    }
                }
            }
        });
    }

    /// Wykonanie pojedynczego ticka — wzorowane na `handle_event`: bierze
    /// instancje z mapy pod krotkim lockiem, refueluje store, wola
    /// `on_tick(timestamp_ms) -> i32` na guest, wklada instancje z powrotem.
    /// Static zeby uniknac trzymania referencji do `&self` w spawnowanym
    /// tasku — przekazujemy Arc'i pól bezposrednio.
    fn call_tick_static(
        instances_map: &Arc<Mutex<HashMap<String, Vec<AddonInstance>>>>,
        addon_id: &str,
        instance_id: &str,
        fuel_per_tick: u64,
        timeout_ms: Option<u64>,
    ) -> Result<()> {
        // Wyciagnij instancje (lock briefly)
        let mut addon_instance = {
            let mut instances = instances_map.lock();
            let addon_instances = instances.get_mut(addon_id).ok_or_else(|| {
                anyhow::anyhow!("addon '{}' nie ma uruchomionych instancji", addon_id)
            })?;
            let pos = addon_instances
                .iter()
                .position(|i| i.instance_id == instance_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("instance '{}' nie znaleziona w mapie", instance_id)
                })?;
            addon_instances.remove(pos)
        };

        // Refuel — kazdy tick dostaje swiezy budzet. Silent failure tutaj
        // by spowodowala ze on_tick natychmiast wytrapuje na fuel exhaustion
        // (przeniesione z poprzedniego ticka), wiec raportujemy i abort.
        if let Err(e) = runtime::refuel_store(&mut addon_instance.store, fuel_per_tick) {
            warn!(
                "refuel_store failed for '{}' (instance={}): {}",
                addon_id, instance_id, e
            );
            // Wloz instancje z powrotem zeby stop_addon mogl ja znalezc.
            instances_map
                .lock()
                .entry(addon_id.to_string())
                .or_default()
                .push(addon_instance);
            return Err(anyhow::anyhow!("refuel_store: {e}"));
        }

        // Per-call epoch deadline: store wytrapuje po `timeout_ms` liczonych od
        // teraz, niezaleznie od innych instancji — steady epoch ticker (jeden
        // watek per silnik, patrz create_engine) bije epoke, a kazdy store ma
        // WLASNY wzgledny deadline. Brak watka-per-call i brak cross-trapu
        // miedzy addonami. Epoch jest wasmtime-only; mobile (wasmi) idzie przez
        // fuel (refuel_store powyzej), wiec na nim deadline jest pomijany.
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        runtime::set_call_epoch_deadline(&mut addon_instance.store, timeout_ms);

        // Lifecycle on_tick (export name from language adapter)
        let tick_export = addon_instance.language_adapter.export_on_tick();
        let res: Result<()> = (|| {
            if let Ok(on_tick) = addon_instance
                .instance
                .get_typed_func::<i64, i32>(&mut addon_instance.store, tick_export)
            {
                let ts_ms = chrono::Utc::now().timestamp_millis();
                let code = on_tick
                    .call(&mut addon_instance.store, ts_ms)
                    .map_err(|e| anyhow::anyhow!("on_tick call: {e}"))?;
                if code != 0 {
                    bail!("on_tick zwrocil kod {}", code);
                }
            }
            Ok(())
        })();

        // Reset epoch deadline — store wraca do mapy i moze byc uzyty przez
        // handle_event lub call_tool; bez wlasnego limitu nie wytrapuje.
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        runtime::clear_call_epoch_deadline(&mut addon_instance.store);

        // Wloz z powrotem nawet przy bledzie — pojedyncza nieudana tura nie
        // zabija service (np. transient error w dispatch'u host_function).
        instances_map
            .lock()
            .entry(addon_id.to_string())
            .or_default()
            .push(addon_instance);

        res
    }

    /// Calls on_panel_open on a running addon instance. The addon emits
    /// PanelShell/SlotContent/StateSnapshot for the requested panel without
    /// restarting. Returns Ok(true) if the export existed and was called,
    /// Ok(false) if the addon doesn't export on_panel_open (legacy addon).
    pub fn call_panel_open(
        &self,
        addon_id: &str,
        panel_id: &str,
        epoch: u64,
        user_id: Option<String>,
    ) -> Result<bool> {
        // Wypożycz worker z puli (rośnie do limitu, bez 3s busy-loopa). Instancja
        // serwisowa jest pomijana, więc panel nie kłóci się z on_tick.
        let (mut addon_instance, ephemeral) = self.acquire_instance(addon_id, user_id, false)?;

        // Cała praca WASM w domknięciu, żeby worker został ZWRÓCONY na każdej
        // ścieżce (błąd też) — inaczej pula by się kurczyła / licznik przeciekał.
        let result = (|| -> Result<bool> {
            runtime::refuel_store(&mut addon_instance.store, DEFAULT_FUEL_LIMIT)?;

            let export_name = addon_instance.language_adapter.export_on_panel_open();
            let has_export = addon_instance
                .instance
                .get_typed_func::<(i32, i32, i64), i32>(&mut addon_instance.store, export_name)
                .is_ok();

            if has_export {
                let on_panel_open = addon_instance
                    .instance
                    .get_typed_func::<(i32, i32, i64), i32>(
                        &mut addon_instance.store,
                        export_name,
                    )?;

                // Allocate guest memory for panel_id string
                let alloc_fn = addon_instance
                    .instance
                    .get_typed_func::<i32, i32>(&mut addon_instance.store, "alloc")
                    .map_err(|e| anyhow::anyhow!("alloc export missing: {e}"))?;

                let panel_id_bytes = panel_id.as_bytes();
                let ptr = alloc_fn.call(&mut addon_instance.store, panel_id_bytes.len() as i32)?;
                if ptr < 0 {
                    bail!("alloc returned invalid pointer for panel_id");
                }

                // Copy panel_id into guest memory
                if let Some(memory) = addon_instance
                    .instance
                    .get_memory(&mut addon_instance.store, "memory")
                {
                    let mem = memory.data_mut(&mut addon_instance.store);
                    let end = match (ptr as usize).checked_add(panel_id_bytes.len()) {
                        Some(e) if e <= mem.len() => e,
                        _ => {
                            bail!("panel_id buffer exceeds guest memory");
                        }
                    };
                    mem[ptr as usize..end].copy_from_slice(panel_id_bytes);
                }

                let call_result = on_panel_open.call(
                    &mut addon_instance.store,
                    (ptr, panel_id_bytes.len() as i32, epoch as i64),
                )?;

                // Dealloc
                if let Ok(dealloc_fn) = addon_instance
                    .instance
                    .get_typed_func::<(i32, i32), ()>(&mut addon_instance.store, "dealloc")
                {
                    let _ = dealloc_fn.call(
                        &mut addon_instance.store,
                        (ptr, panel_id_bytes.len() as i32),
                    );
                }

                if call_result != 0 {
                    warn!(
                        "on_panel_open('{}', '{}') returned error: {}",
                        addon_id, panel_id, call_result
                    );
                }
            }

            Ok(has_export)
        })();

        self.release_instance(addon_id, addon_instance, ephemeral);
        result
    }

    /// Zatrzymuje instancje addonu
    pub fn stop_addon(&self, instance_id: &str) -> Result<()> {
        info!("Zatrzymywanie instancji: {}", instance_id);

        // Anuluj service tick loop (jesli ten instance ma service mode).
        // Token wyzwala `select` w petli, ktora wychodzi cleanly bez
        // szarpania trzymanej instancji — po cancel mozemy bezpiecznie
        // wyciagnac instancje z mapy ponizej.
        let had_service_token = if let Some(token) = self.service_tasks.lock().remove(instance_id) {
            token.cancel();
            true
        } else {
            false
        };
        // Zdejmij rezerwację instancji serwisowej (no-op dla workerów puli).
        self.service_instance_ids.lock().remove(instance_id);

        // P2 race fix (codex review): tick loop moze byc IN-FLIGHT, ze
        // wyciagnal juz instancje z mapy w call_tick_static. Cancel tokenu
        // zatrzyma kolejne iteracje, ale aktualnie running tick wciaz
        // konczy WASM call i odda instancje. Czekamy do 5s na powrot
        // instancji do mapy. Po timeout: surface error — user moze
        // wyowulac stop ponownie.
        let (mut instances, addon_id, pos) = {
            let mut attempt = 0u32;
            loop {
                {
                    let mut instances = self.instances.lock();
                    let mut found = None;
                    for (aid, addon_instances) in instances.iter_mut() {
                        if let Some(p) = addon_instances
                            .iter()
                            .position(|i| i.instance_id == instance_id)
                        {
                            found = Some((aid.clone(), p));
                            break;
                        }
                    }
                    if let Some((aid, p)) = found {
                        break (instances, aid, p);
                    }
                }
                // Nie w mapie. Tylko instancja serwisowa może być IN-FLIGHT w
                // ticku (call_tick_static ją wyjął) — wtedy czekamy do 5s na
                // powrót. Worker/nieznana instancja: nic do zatrzymania → Ok.
                if !had_service_token {
                    return Ok(());
                }
                attempt += 1;
                if attempt > 50 {
                    bail!(
                        "Instancja '{}' nie znaleziona po cancel tokenu \
                         (czekano 5s na powrot z tick loop)",
                        instance_id
                    );
                }
                // 100ms × 50 = 5s window — wystarcza dla typowego tick
                // (fuel limit 5M instrukcji + tick_timeout_ms default 30s
                // jest watchdog ceiling). W praktyce tick wraca w ms.
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        };

        // Pobierz instancje
        let mut addon_instance = instances.get_mut(&addon_id).unwrap().remove(pos);

        // VULN-046: Jawnie zamknij polaczenia sieciowe przed drop instancji
        {
            let net_mgr = addon_instance.store.data().net_manager.clone();
            let mut mgr = net_mgr.lock();
            let count = mgr.connection_count();
            mgr.close_all();
            if count > 0 {
                info!(
                    "stop_addon '{}': zamknieto {} polaczen sieciowych",
                    addon_id, count
                );
            }
        }

        // Anuluj aktywne strumienie LLM addonu — pump-taski sa abortowane, a
        // porzucone strumienie backendu przerywaja generacje.
        host_functions::llm::cleanup_addon_streams(&addon_id);

        // Lifecycle on_stop (export name from language adapter)
        let stop_export = addon_instance.language_adapter.export_on_stop();
        if let Some(on_stop) = addon_instance
            .instance
            .get_typed_func::<(), i32>(&mut addon_instance.store, stop_export)
            .ok()
        {
            if let Err(e) = on_stop.call(&mut addon_instance.store, ()) {
                warn!(
                    "Blad wywolania {}() dla '{}': {}",
                    stop_export, instance_id, e
                );
            }
        }

        // Zaktualizuj status w DB
        {
            let conn = self.db.write().unwrap();
            conn.execute(
                "UPDATE addon_instances SET status = 'stopped', stopped_at = datetime('now') WHERE instance_id = ?1",
                rusqlite::params![instance_id],
            ).map_err(|e| anyhow::anyhow!("Nie udalo sie zaktualizowac statusu instancji: {e}"))?;
        }

        // Opublikuj event
        self.event_bus.publish(Event {
            event_type: "addon.stopped".to_string(),
            source_addon: Some(addon_id.clone()),
            source_user: addon_instance.user_id,
            payload: serde_json::json!({
                "addon_id": &addon_id,
                "instance_id": instance_id,
            }),
            timestamp: chrono::Utc::now(),
        });

        // Opróżnij workery puli tego addonu (POMIJAJĄC instancje serwisowe —
        // tych nie wolno wyrwać spod działającego ticka). Workery są wymienne,
        // stan trwały siedzi w scope-keyed storage, nie w pamięci WASM. Zamknij
        // ich połączenia i wywołaj on_stop (best-effort). Licznik puli ustaw na
        // liczbę zachowanych instancji serwisowych.
        if let Some(list) = instances.get_mut(&addon_id) {
            let svc = self.service_instance_ids.lock();
            let mut keep = Vec::new();
            let mut drained = Vec::new();
            for inst in list.drain(..) {
                if svc.contains(&inst.instance_id) {
                    keep.push(inst);
                } else {
                    drained.push(inst);
                }
            }
            let keep_len = keep.len();
            *list = keep;
            drop(svc);
            for mut w in drained {
                w.store.data().net_manager.clone().lock().close_all();
                let se = w.language_adapter.export_on_stop();
                if let Ok(f) = w.instance.get_typed_func::<(), i32>(&mut w.store, se) {
                    let _ = f.call(&mut w.store, ());
                }
            }
            let mut totals = self.instance_total.lock();
            if keep_len == 0 {
                totals.remove(&addon_id);
            } else {
                totals.insert(addon_id.clone(), keep_len);
            }
        }

        // Usun pusta liste jesli brak instancji
        let no_instances_left = instances.get(&addon_id).map_or(true, |v| v.is_empty());
        if no_instances_left {
            instances.remove(&addon_id);
        }

        // Deactivate aliases when the last instance of any addon is gone.
        if no_instances_left {
            self.deactivate_aliases_owned_by_addon(&addon_id);

            // L2: the addon's LAST instance is gone — drop its latest LiDAR frame
            // slot (keyed by addon_id == robot_id). Doing this here, not per-
            // instance, is the correct granularity: a single pooled-worker stop
            // must NOT wipe a slot the still-live service instance keeps feeding.
            crate::services::lidar_hub::LidarStreamHub::global().remove(&addon_id);
            crate::services::slam_scene::SlamSceneManager::global().remove(&addon_id);
            crate::services::localization::LocalizationEngine::global().remove(&addon_id);
            crate::services::mobile_camera::MobileCameraIngest::global().remove(&addon_id);

            // A2: the addon is fully stopped — flush any durable writes that
            // have not yet hit the periodic flush so a stop+exit before the next
            // tick does not lose them. The in-RAM shard is intentionally kept
            // (cheap; a restart re-seeds it) — only uninstall drops + purges it.
            if let Err(e) = state_flusher::flush_addon(
                &self.db,
                state_store::AddonStateStore::global(),
                &addon_id,
            ) {
                warn!(
                    "addon state: flush on stop failed for '{}': {} — periodic flusher will retry",
                    addon_id, e
                );
            }
        }

        info!("Instancja '{}' zatrzymana", instance_id);
        Ok(())
    }

    /// Wywoluje narzedzie addonu (dla LLM tool calling).
    /// K4: Minimalizacja czasu trzymania lock — instancja jest wyjmowana z mapy
    /// pod lockiem (krotko), WASM jest wykonywany poza lockiem, potem wkladana z powrotem.
    pub fn call_tool(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        self.call_tool_inner(
            addon_id,
            tool_name,
            params,
            CallIdentity::User(user_id),
            false,
        )
    }

    /// Like `call_tool` but skips the per-addon `"llm"` permission check because
    /// the caller already adjudicated it (§3.13 B: the harness raised a grant
    /// card and the operator chose AllowOnce / AllowForRun — neither persists a
    /// grant the checker would see, so the in-line check is bypassed for this
    /// pre-authorized retry). NEVER call without first gating the permission
    /// yourself; the only caller is the harness tool_exec permission path.
    pub fn call_tool_preauthorized(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        self.call_tool_inner(
            addon_id,
            tool_name,
            params,
            CallIdentity::User(user_id),
            true,
        )
    }

    /// Invokes a tool as a GENUINE system call: the worker's `AddonState` runs
    /// with `user_id = None` and `is_system_call = true`, so host-fn
    /// `check_permission` takes the CR-006 path and grants the addon's DECLARED
    /// permissions (still gated on the addon actually declaring them) WITHOUT a
    /// per-user grant. The tool-level `"llm"` check is skipped because there is no
    /// principal to adjudicate. This is for CORE-INTERNAL trusted reads only
    /// (e.g. the robot status refresh loop), NOT for LLM/user tool calls — those
    /// MUST keep per-user `permission_checker` gating via `call_tool` /
    /// `call_tool_preauthorized`.
    pub fn call_tool_system(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.call_tool_inner(addon_id, tool_name, params, CallIdentity::System, true)
    }

    fn call_tool_inner(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: serde_json::Value,
        identity: CallIdentity<'_>,
        skip_permission_check: bool,
    ) -> Result<serde_json::Value> {
        // System calls carry no principal; user calls carry their user_id.
        let acquire_user = match identity {
            CallIdentity::System => None,
            CallIdentity::User(uid) => Some(uid.to_string()),
        };
        let system = matches!(identity, CallIdentity::System);
        // For audit + request JSON, system calls are tagged "system" (clearly a
        // system-originated call, not a real principal); user calls carry the uid.
        let audit_user = match identity {
            CallIdentity::System => "system",
            CallIdentity::User(uid) => uid,
        };
        info!(
            "Wywolanie narzedzia '{}.{}' przez user_id={} (system={})",
            addon_id, tool_name, audit_user, system
        );

        // Sprawdz uprawnienia uzytkownika (pomijane gdy harness już wydał zgodę
        // lub gdy to system call — system call nie ma principala do adjudykacji,
        // a uprawnienia host-fn rozstrzyga check_permission ścieżką CR-006).
        if !skip_permission_check {
            let perm_result = self
                .permission_checker
                .check(addon_id, audit_user, "llm", None);
            if !perm_result.is_granted() {
                bail!(
                    "Brak uprawnien do wywolania narzedzia '{}.{}' dla user_id={}",
                    addon_id,
                    tool_name,
                    audit_user
                );
            }
        }

        // Wypożycz worker z puli — rośnie do limitu, pomija instancję serwisową,
        // bez dawnego 3s busy-loopa, który padał „zajęty" gdy tick trzymał
        // jedyną instancję. `ephemeral` = worker burstowy (drop przy zwrocie).
        // `system` ⇒ worker dostaje user_id=None + is_system_call=true na czas
        // tego wywołania (mirror dawnej semantyki boot/service instancji).
        let (mut addon_instance, ephemeral) =
            self.acquire_instance(addon_id, acquire_user, system)?;

        // Refuel przed wywolaniem — workery są reużywane, więc każde wywołanie
        // dostaje świeży budżet (refuel_store ustawia, nie dodaje).
        if let Err(e) = runtime::refuel_store(&mut addon_instance.store, DEFAULT_FUEL_LIMIT) {
            self.release_instance(addon_id, addon_instance, ephemeral);
            return Err(anyhow::anyhow!("refuel_store: {e}"));
        }

        // Przygotuj dane wejsciowe jako JSON
        let request_json = serde_json::json!({
            "tool": tool_name,
            "params": params,
            "user_id": audit_user,
        });
        let request_bytes = serde_json::to_vec(&request_json)?;

        // Wykonaj WASM poza lockiem
        let result = (|| -> Result<serde_json::Value> {
            // Pobierz alloc z guest
            let alloc_fn = addon_instance
                .instance
                .get_typed_func::<i32, i32>(&mut addon_instance.store, "alloc")
                .map_err(|e| anyhow::anyhow!("Addon nie eksportuje funkcji alloc(): {e}"))?;

            // Alokuj bufor wejsciowy w guest memory
            let input_ptr = alloc_fn
                .call(&mut addon_instance.store, request_bytes.len() as i32)
                .map_err(|e| anyhow::anyhow!("Blad alokacji pamieci guest: {e}"))?;

            // CR-004: Sprawdz poprawnosc wskaznika
            if input_ptr < 0 {
                bail!("alloc() zwrocil niepoprawny wskaznik: {}", input_ptr);
            }

            // Zapisz dane do guest memory
            let memory = addon_instance
                .instance
                .get_memory(&mut addon_instance.store, "memory")
                .ok_or_else(|| anyhow::anyhow!("Brak eksportu 'memory' w module WASM"))?;

            // CR-005: Sprawdz granice pamieci z checked_add
            let input_end = (input_ptr as usize)
                .checked_add(request_bytes.len())
                .ok_or_else(|| {
                    anyhow::anyhow!("Przepelnienie przy obliczaniu konca bufora wejsciowego")
                })?;
            let mem_size = memory.data(&addon_instance.store).len();
            if input_end > mem_size {
                bail!(
                    "Bufor wejsciowy wykracza poza pamiec guest ({} > {})",
                    input_end,
                    mem_size
                );
            }

            memory.data_mut(&mut addon_instance.store)[input_ptr as usize..input_end]
                .copy_from_slice(&request_bytes);

            // Alokuj bufor wyjsciowy (64KB)
            let out_cap: i32 = 65536;
            let out_ptr = alloc_fn
                .call(&mut addon_instance.store, out_cap)
                .map_err(|e| anyhow::anyhow!("Blad alokacji bufora wyjsciowego: {e}"))?;

            if out_ptr < 0 {
                bail!(
                    "alloc() zwrocil niepoprawny wskaznik wyjsciowy: {}",
                    out_ptr
                );
            }

            // Alokuj miejsce na dlugosc wyniku (4 bajty)
            let out_len_ptr = alloc_fn
                .call(&mut addon_instance.store, 4)
                .map_err(|e| anyhow::anyhow!("Blad alokacji out_len: {e}"))?;

            if out_len_ptr < 0 {
                bail!(
                    "alloc() zwrocil niepoprawny wskaznik out_len: {}",
                    out_len_ptr
                );
            }

            // Lifecycle on_request (export name from language adapter)
            let req_export = addon_instance.language_adapter.export_on_request();
            let on_request = addon_instance
                .instance
                .get_typed_func::<(i32, i32, i32, i32, i32), i32>(
                    &mut addon_instance.store,
                    req_export,
                )
                .map_err(|e| {
                    anyhow::anyhow!("Addon nie eksportuje funkcji {}(): {e}", req_export)
                })?;

            let result_code = on_request
                .call(
                    &mut addon_instance.store,
                    (
                        input_ptr,
                        request_bytes.len() as i32,
                        out_ptr,
                        out_cap,
                        out_len_ptr,
                    ),
                )
                .map_err(|e| anyhow::anyhow!("Blad wywolania on_request(): {e}"))?;

            if result_code != 0 {
                bail!("on_request() zwrocil blad: {}", result_code);
            }

            // Odczytaj dlugosc wyniku
            let mem_data = memory.data(&addon_instance.store);

            // CR-005: Sprawdz granice pamieci przy odczycie dlugosci
            let out_len_end = (out_len_ptr as usize)
                .checked_add(4)
                .ok_or_else(|| anyhow::anyhow!("Przepelnienie przy obliczaniu konca out_len"))?;
            if out_len_end > mem_data.len() {
                bail!("out_len_ptr wykracza poza pamiec guest");
            }

            let out_len_bytes = &mem_data[out_len_ptr as usize..out_len_end];
            let out_len = i32::from_le_bytes([
                out_len_bytes[0],
                out_len_bytes[1],
                out_len_bytes[2],
                out_len_bytes[3],
            ]);

            if out_len < 0 {
                bail!("out_len jest ujemny: {}", out_len);
            }

            // CR-005: Sprawdz granice pamieci przy odczycie wyniku
            let result_end = (out_ptr as usize)
                .checked_add(out_len as usize)
                .ok_or_else(|| anyhow::anyhow!("Przepelnienie przy obliczaniu konca wyniku"))?;
            if result_end > mem_data.len() {
                bail!(
                    "Bufor wyniku wykracza poza pamiec guest ({} > {})",
                    result_end,
                    mem_data.len()
                );
            }

            // Odczytaj wynik
            let result_bytes = &mem_data[out_ptr as usize..result_end];
            let result: serde_json::Value = serde_json::from_slice(result_bytes).map_err(|e| {
                anyhow::anyhow!("Nie udalo sie zdekodowac odpowiedzi z addonu: {e}")
            })?;

            // Zwolnij pamiec guest
            if let Ok(dealloc_fn) = addon_instance
                .instance
                .get_typed_func::<(i32, i32), ()>(&mut addon_instance.store, "dealloc")
            {
                let _ = dealloc_fn.call(
                    &mut addon_instance.store,
                    (input_ptr, request_bytes.len() as i32),
                );
                let _ = dealloc_fn.call(&mut addon_instance.store, (out_ptr, out_cap));
                let _ = dealloc_fn.call(&mut addon_instance.store, (out_len_ptr, 4));
            }

            Ok(result)
        })();

        // Zwróć worker do puli (lub zdropuj, jeśli burstowy ponad limit).
        self.release_instance(addon_id, addon_instance, ephemeral);

        // Loguj do audit
        self.log_audit(addon_id, audit_user, "tool.call", Some(tool_name), None);

        result
    }

    /// Wywoluje pojedynczy blok flow z addonu — fresh instancja per call,
    /// per-call fuel budget, opcjonalny deadline (epoch interruption z
    /// background task'a). Decyzje #6 i #7 z planu addonow:
    /// - fresh instance per call (zero state leakage miedzy invocations)
    /// - per-call fuel/memory/timeout (DoS protection przed addon z `while {}`).
    ///
    /// ABI guest: ten sam co `call_tool` (`on_request(in_ptr, in_len, out_ptr,
    /// out_cap, out_len_ptr) -> i32`), z konwencja tool name = "block.{block_type}".
    /// `envelope_json` to serialized FlowEnvelope; response to envelope JSON
    /// po wykonaniu logiki bloku.
    pub fn invoke_block(
        &self,
        addon_id: &str,
        block_type: &str,
        envelope_json: &[u8],
        user_id: Option<String>,
        org_id: Option<String>,
        fuel_budget: u64,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<u8>> {
        info!(
            "Wywolanie flow blocku '{}.{}' (user_id={:?}, fuel={}, deadline={:?})",
            addon_id, block_type, user_id, fuel_budget, deadline
        );

        // Permission: addon musi miec "flow_blocks" (opcjonalnie z resource =
        // block_type, ale dla MVP wystarczy ogolne). Brak uprawnien = bail.
        if let Some(uid) = user_id.as_deref() {
            let perm =
                self.permission_checker
                    .check(addon_id, uid, "flow_blocks", Some(block_type));
            if !perm.is_granted() {
                bail!(
                    "Brak uprawnien 'flow_blocks' dla addonu '{}' (user_id={})",
                    addon_id,
                    uid
                );
            }
        }

        let module = self.get_or_compile_module(addon_id)?;
        let permissions = self.load_addon_permissions(addon_id)?;
        let manifest = self.load_addon_manifest(addon_id)?;
        let block_rt_id = manifest
            .runtime
            .clone()
            .unwrap_or_else(|| "wasmtime".to_string());

        let instance_id = format!("block-{}", uuid::Uuid::new_v4());

        // Fresh AddonState — odizolowane od running instances w `self.instances`.
        let state = AddonState {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id: user_id.clone(),
            org_id: org_id.clone(),
            db: self.db.clone(),
            permissions,
            event_bus: self.event_bus.clone(),
            permission_checker: self.permission_checker.clone(),
            fuel_consumed: 0,
            is_system_call: user_id.is_none(),
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(
                host_functions::network::NetworkConnectionManager::new(),
            )),
            settings_cipher: self.settings_cipher.clone(),
            manifest: Arc::new(manifest),
            memory_limit: DEFAULT_MEMORY_LIMIT_BYTES,
            router: self.router.read().clone(),
            oauth_refresh_guard: self.oauth_refresh_guard.clone(),
            ui_panels: Some(self.ui_panels.clone()),
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
            #[cfg(any(target_os = "ios", target_os = "android"))]
            store_limits: wasmi::StoreLimitsBuilder::new()
                .memory_size(DEFAULT_MEMORY_LIMIT_BYTES)
                .trap_on_grow_failure(true)
                .instances(10)
                .memories(1)
                .tables(10)
                .build(),
        };

        let mut store = runtime::create_store(&self.engine, state)?;
        store
            .set_fuel(fuel_budget)
            .map_err(|e| anyhow::anyhow!("set_fuel({}): {e}", fuel_budget))?;

        // Per-call epoch deadline: store wytrapuje po pozostalym czasie do
        // `deadline`, niezaleznie od innych instancji (steady epoch ticker
        // bije epoke, store ma wlasny wzgledny deadline — brak cross-trapu i
        // watka-per-call). Epoch = wasmtime-only; mobile idzie przez fuel.
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            let timeout_ms = deadline.map(|d| {
                d.saturating_duration_since(std::time::Instant::now())
                    .as_millis() as u64
            });
            runtime::set_call_epoch_deadline(&mut store, timeout_ms);
        }

        let instance = runtime::instantiate(&self.linker, &mut store, &module)?;

        // Language adapter for correct export names
        let block_adapter = runtime::adapter_for_runtime(&block_rt_id)
            .unwrap_or_else(|| Box::new(runtime::RustAdapter));

        // .NET / Python WASI init for ephemeral block instances
        if block_adapter.needs_wasi_start() {
            let init_fuel = block_adapter.init_fuel_budget();
            if init_fuel > 0 {
                runtime::refuel_store(&mut store, init_fuel)?;
            }
            let wasi_start = instance
                .get_typed_func::<(), ()>(&mut store, "_start")
                .ok()
                .or_else(|| {
                    instance
                        .get_typed_func::<(), ()>(&mut store, "_initialize")
                        .ok()
                });
            if let Some(f) = wasi_start {
                f.call(&mut store, ())
                    .map_err(|e| anyhow::anyhow!("WASI _start/_initialize failed: {e}"))?;
            }
            runtime::refuel_store(&mut store, fuel_budget)?;
        }

        // Request: konwencja tool = "block.{block_type}", params = envelope JSON.
        // Addon parsuje `params` jako FlowEnvelope-shaped Value.
        let envelope_value: serde_json::Value = serde_json::from_slice(envelope_json)
            .map_err(|e| anyhow::anyhow!("invoke_block: envelope_json nie jest valid JSON: {e}"))?;
        let request_json = serde_json::json!({
            "tool": format!("block.{}", block_type),
            "params": envelope_value,
            "user_id": user_id,
        });
        let request_bytes = serde_json::to_vec(&request_json)?;

        let result = (|| -> Result<Vec<u8>> {
            let alloc_fn = instance
                .get_typed_func::<i32, i32>(&mut store, "alloc")
                .map_err(|e| anyhow::anyhow!("brak alloc(): {e}"))?;

            let input_ptr = alloc_fn
                .call(&mut store, request_bytes.len() as i32)
                .map_err(|e| anyhow::anyhow!("alloc(input): {e}"))?;
            if input_ptr < 0 {
                bail!("alloc(input) zwrocil {} ", input_ptr);
            }

            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| anyhow::anyhow!("brak export 'memory'"))?;

            let input_end = (input_ptr as usize)
                .checked_add(request_bytes.len())
                .ok_or_else(|| anyhow::anyhow!("input range overflow"))?;
            if input_end > memory.data(&store).len() {
                bail!("input buffer poza guest memory");
            }
            memory.data_mut(&mut store)[input_ptr as usize..input_end]
                .copy_from_slice(&request_bytes);

            // 256KB output buffer — flow blocks moga zwracac caly envelope z
            // historia, wiec wiekszy niz tool calls (64KB).
            let out_cap: i32 = 256 * 1024;
            let out_ptr = alloc_fn
                .call(&mut store, out_cap)
                .map_err(|e| anyhow::anyhow!("alloc(output): {e}"))?;
            if out_ptr < 0 {
                bail!("alloc(output) zwrocil {}", out_ptr);
            }
            let out_len_ptr = alloc_fn
                .call(&mut store, 4)
                .map_err(|e| anyhow::anyhow!("alloc(out_len): {e}"))?;
            if out_len_ptr < 0 {
                bail!("alloc(out_len) zwrocil {}", out_len_ptr);
            }

            let block_req_export = block_adapter.export_on_request();
            let on_request = instance
                .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, block_req_export)
                .map_err(|e| anyhow::anyhow!("brak {}: {e}", block_req_export))?;

            let result_code = on_request
                .call(
                    &mut store,
                    (
                        input_ptr,
                        request_bytes.len() as i32,
                        out_ptr,
                        out_cap,
                        out_len_ptr,
                    ),
                )
                .map_err(|e| anyhow::anyhow!("{} fail: {e}", block_req_export))?;

            if result_code != 0 {
                bail!("{} zwrocil kod bledu: {}", block_req_export, result_code);
            }

            let mem_data = memory.data(&store);
            let out_len_end = (out_len_ptr as usize)
                .checked_add(4)
                .ok_or_else(|| anyhow::anyhow!("out_len range overflow"))?;
            if out_len_end > mem_data.len() {
                bail!("out_len_ptr poza guest memory");
            }
            let out_len = i32::from_le_bytes([
                mem_data[out_len_ptr as usize],
                mem_data[out_len_ptr as usize + 1],
                mem_data[out_len_ptr as usize + 2],
                mem_data[out_len_ptr as usize + 3],
            ]);
            if out_len < 0 {
                bail!("out_len ujemny: {}", out_len);
            }
            if out_len > out_cap {
                bail!("out_len > out_cap ({} > {})", out_len, out_cap);
            }

            let result_end = (out_ptr as usize)
                .checked_add(out_len as usize)
                .ok_or_else(|| anyhow::anyhow!("output range overflow"))?;
            if result_end > mem_data.len() {
                bail!("output buffer poza guest memory");
            }

            Ok(mem_data[out_ptr as usize..result_end].to_vec())
        })();

        // Reset epoch deadline — store jest efemeryczny (dropowany po bloku),
        // ale czyscimy dla spojnosci i bezpieczenstwa ewentualnego reuzycia.
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        runtime::clear_call_epoch_deadline(&mut store);

        // Loguj do audit. Pusty user_id = system call (None mapuje na "").
        self.log_audit(
            addon_id,
            user_id.as_deref().unwrap_or(""),
            "flow_block.invoke",
            Some(block_type),
            None,
        );

        result
    }

    /// Rozsyla event do zasubskrybowanych addonow.
    /// K5: Minimalizacja lock contention — zbierz instancje pod lockiem,
    /// wykonaj WASM poza lockiem, wloz z powrotem.
    pub fn handle_event(&self, event: Event) -> Result<()> {
        let subscribers = self.event_bus.get_subscribers(&event.event_type);

        let event_json = serde_json::to_vec(&serde_json::json!({
            "event_type": &event.event_type,
            "source_addon": &event.source_addon,
            "source_user": &event.source_user,
            "payload": &event.payload,
            "timestamp": event.timestamp.to_rfc3339(),
        }))?;

        // K5: Zbierz instancje pod lockiem (krotko)
        let mut extracted: Vec<(String, usize, AddonInstance)> = Vec::new();
        {
            let mut instances = self.instances.lock();
            for subscriber in &subscribers {
                if let Some(addon_instances) = instances.get_mut(&subscriber.addon_id) {
                    if let Some(pos) = addon_instances
                        .iter()
                        .position(|i| i.instance_id == subscriber.instance_id)
                    {
                        let inst = addon_instances.remove(pos);
                        extracted.push((subscriber.addon_id.clone(), pos, inst));
                    }
                }
            }
        }
        // Write lock zwolniony — inne watki moga operowac na mapie

        // Wykonaj WASM poza lockiem
        for (addon_id, _pos, ref mut addon_instance) in &mut extracted {
            // Refuel — store wspoldzielony z tickami, ktore zostawiaja maly
            // budzet (patrz call_tool_inner).
            if let Err(e) = runtime::refuel_store(&mut addon_instance.store, DEFAULT_FUEL_LIMIT) {
                warn!(
                    "refuel_store przed on_event nieudany dla '{}': {e}",
                    addon_id
                );
                continue;
            }
            let event_export = addon_instance.language_adapter.export_on_event();
            if let Ok(on_event) = addon_instance
                .instance
                .get_typed_func::<(i32, i32), i32>(&mut addon_instance.store, event_export)
            {
                if let Ok(alloc_fn) = addon_instance
                    .instance
                    .get_typed_func::<i32, i32>(&mut addon_instance.store, "alloc")
                {
                    if let Ok(ptr) =
                        alloc_fn.call(&mut addon_instance.store, event_json.len() as i32)
                    {
                        // CR-004: Sprawdz poprawnosc wskaznika
                        if ptr < 0 {
                            warn!("alloc() zwrocil niepoprawny wskaznik dla eventu: {}", ptr);
                            continue;
                        }
                        if let Some(memory) = addon_instance
                            .instance
                            .get_memory(&mut addon_instance.store, "memory")
                        {
                            let mem = memory.data_mut(&mut addon_instance.store);
                            // CR-005: Sprawdz granice z checked_add
                            let end = match (ptr as usize).checked_add(event_json.len()) {
                                Some(e) if e <= mem.len() => e,
                                _ => {
                                    warn!(
                                        "Event buffer wykracza poza pamiec guest dla '{}'",
                                        addon_id
                                    );
                                    continue;
                                }
                            };
                            mem[ptr as usize..end].copy_from_slice(&event_json);
                            if let Err(e) = on_event
                                .call(&mut addon_instance.store, (ptr, event_json.len() as i32))
                            {
                                warn!("Blad wywolania on_event() dla '{}': {}", addon_id, e);
                            }
                        }
                    }
                }
            }
        }

        // K5: Wloz instancje z powrotem do mapy
        {
            let mut instances = self.instances.lock();
            for (addon_id, _pos, inst) in extracted {
                instances.entry(addon_id).or_default().push(inst);
            }
        }

        self.event_bus.record_delivery(subscribers.len() as u64);

        Ok(())
    }

    /// Startuje dispatcher eventow — tworzy kanal mpsc, podpina sender do
    /// `EventBus` (kazdy `publish` trafia na ten kanal) i odpala dedykowany
    /// blocking-thread, ktory drenuje kanal i woluje `self.handle_event`
    /// dla kazdego eventu. Wywolaj raz po `AddonManager::new`.
    pub fn start_event_dispatcher(self: Arc<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::addon::event_bus::Event>();
        self.event_bus.set_dispatch_sender(tx);

        let manager = self;
        tokio::task::spawn_blocking(move || {
            while let Some(event) = rx.blocking_recv() {
                let event_type = event.event_type.clone();
                if let Err(e) = manager.handle_event(event) {
                    warn!(
                        "Dispatcher: handle_event('{}') zwrocil blad: {}",
                        event_type, e
                    );
                }
            }
            info!("Dispatcher eventow zakonczony — kanal zamkniety");
        });

        info!("AddonManager: dispatcher eventow wystartowany");
    }

    /// Zwraca liste narzedzi ze wszystkich addonow (dla LLM)
    pub fn list_tools(&self) -> Vec<ToolDefinition> {
        self.registered_tools.read().clone()
    }

    /// Zwraca referencje do event bus
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// Zwraca referencje do permission checker
    pub fn permission_checker(&self) -> &Arc<PermissionChecker> {
        &self.permission_checker
    }

    // =========================================================================
    // Metody prywatne
    // =========================================================================

    /// Pobiera skompilowany modul z cache lub kompiluje z WASM z DB
    fn get_or_compile_module(&self, addon_id: &str) -> Result<WasmModule> {
        // Sprawdz cache
        if let Some(module) = self.compiled_modules.read().get(addon_id) {
            return Ok(module.clone());
        }

        // Tabela `addon_wasm` jest martwa od pierwszego commita — lifecycle
        // nigdy do niej nie zapisuje. WASM zyje na dysku w wersjonowanym
        // pakiecie: packages/{package_id}/{package_version}/{wasm_file}.
        // Instancja przypina (package_id, package_version) w tabeli `addons`;
        // czytamy je i sciezke wasm z manifestu.
        let manifest = self.load_addon_manifest(addon_id)?;
        let (package_id, package_version) = self.addon_package_ref(addon_id)?;

        // SECURITY GATE: an instance must never RUN code that differs from what the user
        // approved. The reconciler materializes the package on disk and can overwrite a
        // version's bytes IN PLACE (e.g. a bundled addon edited without a version bump),
        // so the on-disk wasm can change underneath a running instance. We refuse to load
        // unless the catalog's current bundle hash (= what's on disk now) equals the
        // instance's `installed_bundle_hash` (= what was approved at install/"Aktualizuj").
        // A content change without a manual update is rejected, not silently executed.
        let catalog_hash = crate::db::repository::get_package_bundle_hash(
            &self.db,
            &package_id,
            &package_version,
        )?;
        let approved_hash =
            crate::db::repository::get_instance_installed_bundle_hash(&self.db, addon_id)?;
        if let Some(catalog) = catalog_hash.as_deref() {
            if !approved_hash.is_empty() && approved_hash != catalog {
                bail!(
                    "Addon '{addon_id}': kod na dysku ({}…) rozni sie od zatwierdzonego ({}…) \
                     — pakiet zmieniono bez recznej aktualizacji. Kliknij 'Aktualizuj' aby \
                     zatwierdzic zanim instancja sie uruchomi.",
                    &catalog[..catalog.len().min(8)],
                    &approved_hash[..approved_hash.len().min(8)],
                );
            }
        }

        let wasm_path =
            bundled::package_dir(&package_id, &package_version).join(&manifest.wasm_file);
        let wasm_bytes = std::fs::read(&wasm_path).with_context(|| {
            format!(
                "Nie znaleziono WASM dla addonu '{}' (oczekiwana sciezka: {:?})",
                addon_id, wasm_path
            )
        })?;

        // Kompiluj modul
        let module = runtime::compile_module(&self.engine, &wasm_bytes)?;

        // Zapisz w cache
        self.compiled_modules
            .write()
            .insert(addon_id.to_string(), module.clone());

        Ok(module)
    }

    /// Laduje manifest addonu z DB (z kolumny manifest_json)
    fn load_addon_manifest(&self, addon_id: &str) -> Result<AddonManifest> {
        let conn = self.db.read().unwrap();
        let manifest_content: String = conn
            .query_row(
                "SELECT manifest_json FROM addons WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |row| row.get(0),
            )
            .context(format!(
                "Nie znaleziono manifestu dla addonu '{}'",
                addon_id
            ))?;

        lifecycle::parse_manifest_toml(&manifest_content).context(format!(
            "Nie udalo sie sparsowac manifestu addonu '{}'",
            addon_id
        ))
    }

    /// Zwraca (package_id, package_version) dla instancji — okresla z ktorej
    /// wersji pakietu na dysku zaladowac wasm/migracje. Fallback na
    /// (addon_id, version) gdy kolumny puste (defensywnie; backfill v60 wypelnia
    /// istniejace wiersze, a install ustawia je dla nowych).
    fn addon_package_ref(&self, addon_id: &str) -> Result<(String, String)> {
        let conn = self.db.read().unwrap();
        let (pkg, ver, version): (String, String, String) = conn
            .query_row(
                "SELECT package_id, package_version, version FROM addons WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .context(format!("Nie znaleziono instancji addonu '{}'", addon_id))?;
        let package_id = if pkg.is_empty() {
            addon_id.to_string()
        } else {
            pkg
        };
        let package_version = if ver.is_empty() { version } else { ver };
        Ok((package_id, package_version))
    }

    /// Zwraca uprawnienia deklarowane przez addon — zarowno kategorie (prefix
    /// przed kropka, np. "storage", "http", "llm") jak i pelne identyfikatory
    /// permission id w formie "kategoria.akcja" (np. "alias.read",
    /// "storage.read"). Host functions wolaja `check_permission` z roznymi
    /// granulacjami: starsze API z kategoria ("llm"), nowsze z pelnym id
    /// ("alias.read"). Zwracamy oba warianty, deduplikowane, zeby pojedyncze
    /// `state.permissions` pasowalo do obu konwencji.
    fn load_addon_permissions(&self, addon_id: &str) -> Result<Vec<String>> {
        let manifest = self.load_addon_manifest(addon_id)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(manifest.declared_permissions.len() * 2);
        for perm in &manifest.declared_permissions {
            if seen.insert(perm.id.clone()) {
                out.push(perm.id.clone());
            }
            let category = perm.id.split('.').next().unwrap_or(perm.id.as_str());
            if !category.is_empty() && seen.insert(category.to_string()) {
                out.push(category.to_string());
            }
        }
        Ok(out)
    }

    /// Rejestruje narzedzia z manifestu addonu
    fn register_tools_from_manifest(&self, manifest: &AddonManifest) -> Result<()> {
        let mut tools = self.registered_tools.write();

        for tool in &manifest.tools {
            tools.push(ToolDefinition {
                addon_id: manifest.addon_id.clone(),
                tool_name: tool.name.clone(),
                description: tool.description.clone(),
                parameters_schema: tool.parameters_schema.clone(),
                return_schema: tool.return_schema.clone(),
                keywords: tool.keywords.clone(),
            });
        }

        Ok(())
    }

    /// Loguje operacje do audit log
    fn log_audit(
        &self,
        addon_id: &str,
        user_id: &str,
        action: &str,
        resource_id: Option<&str>,
        error_message: Option<&str>,
    ) {
        let result_str = if error_message.is_some() {
            "error"
        } else {
            "ok"
        };
        let action_hash = fnv1a_hash(action);

        if let Ok(conn) = self.db.write() {
            let _ = conn.execute(
                "INSERT INTO audit_log (user_id, addon_id, action, resource_id, result, error_message, action_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![user_id, addon_id, action, resource_id, result_str, error_message, action_hash],
            );
        }
    }

    /// Returns the list of alias names registered to this addon in
    /// `model_alias_owners`. Used by start/stop lifecycle paths so the
    /// activate/deactivate logic is generic across addons.
    fn aliases_owned_by_addon(&self, addon_id: &str) -> Vec<String> {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(e) => {
                warn!("aliases_owned_by_addon: db lock: {}", e);
                return Vec::new();
            }
        };
        let mut stmt = match conn.prepare(
            "SELECT m.alias FROM model_aliases m \
             JOIN model_alias_owners o ON o.alias_id = m.id \
             WHERE o.owner_type = 'addon' AND o.owner_id = ?1",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("aliases_owned_by_addon: prepare: {}", e);
                return Vec::new();
            }
        };
        let rows = stmt
            .query_map(rusqlite::params![addon_id], |row| row.get::<_, String>(0))
            .and_then(|it| it.collect::<rusqlite::Result<Vec<_>>>());
        rows.unwrap_or_default()
    }

    /// Reactivates aliases whose owner is this addon, skipping those with
    /// a manifest-declared `[gate]`. Called from `start_addon`. Gated
    /// aliases stay `is_active=0` until the policy engine (M2) or admin
    /// (M16) explicitly flips them on; activating them unconditionally on
    /// restart would bypass the gate. Failures are logged but do not
    /// abort startup — chain conflicts are operator-visible via the
    /// registry UI.
    fn activate_aliases_owned_by_addon(&self, addon_id: &str) {
        let owned = self.aliases_owned_by_addon(addon_id);
        if owned.is_empty() {
            return;
        }
        // Build the set of gated alias ids from the manifest. If the
        // manifest cannot be loaded (corrupt row, missing addon) we have
        // no way to tell gated from ungated, so skip activation entirely
        // rather than risk a bypass — admin can still toggle in M16.
        let manifest = match self.load_addon_manifest(addon_id) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    "Addon '{}': activate skipped — manifest load failed: {}",
                    addon_id, e
                );
                return;
            }
        };
        let to_activate = pick_aliases_to_activate(&owned, &manifest.aliases);
        let gated_count = owned.len() - to_activate.len();

        let mut activated = 0usize;
        for alias in &to_activate {
            if let Err(e) = crate::db::repository::set_model_alias_active_audited(
                &self.db,
                alias,
                true,
                Some(addon_id),
            ) {
                warn!(
                    "Addon '{}': failed to activate alias '{}': {}",
                    addon_id, alias, e
                );
            } else {
                activated += 1;
            }
        }
        self.reload_router_alias_cache();
        info!(
            "Addon '{}': activated {} of {} alias(es) ({} gated)",
            addon_id,
            activated,
            owned.len(),
            gated_count
        );
    }

    /// Deactivates every alias whose owner is this addon. Owner rows are
    /// preserved for audit and future reinstall.
    fn deactivate_aliases_owned_by_addon(&self, addon_id: &str) {
        let aliases = self.aliases_owned_by_addon(addon_id);
        if aliases.is_empty() {
            return;
        }
        for alias in &aliases {
            if let Err(e) = crate::db::repository::set_model_alias_active_audited(
                &self.db,
                alias,
                false,
                Some(addon_id),
            ) {
                warn!(
                    "Addon '{}': failed to deactivate alias '{}': {}",
                    addon_id, alias, e
                );
            }
        }
        self.reload_router_alias_cache();
        info!(
            "Addon '{}': deactivated {} alias(es)",
            addon_id,
            aliases.len()
        );
    }

    /// Odswieza alias cache w routerze (jesli router jest ustawiony)
    fn reload_router_alias_cache(&self) {
        if let Some(router) = self.router.read().as_ref() {
            router.reload_alias_cache();
        }
    }
}

/// D5: Reuzywany hash FNV-1a z utils
fn fnv1a_hash(s: &str) -> i64 {
    utils::fnv1a_hash(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activate_skips_gated_aliases() {
        // Two aliases owned by the addon; only one is gated. The pure
        // helper that drives `activate_aliases_owned_by_addon` must drop
        // the gated id from the activate list so restart cannot bypass
        // the policy gate by flipping every owned alias back to active.
        let owned = vec!["normal-alias".to_string(), "gated-alias".to_string()];
        let manifest_aliases = vec![
            manifest::AliasSpec {
                id: "normal-alias".to_string(),
                display_name: "Normal".to_string(),
                methods: vec![],
                suggested_default: "model-a".to_string(),
                gate: None,
                visibility: manifest::AliasVisibility::Private,
                allowed_consumers: vec![],
            },
            manifest::AliasSpec {
                id: "gated-alias".to_string(),
                display_name: "Gated".to_string(),
                methods: vec![],
                suggested_default: "model-b".to_string(),
                gate: Some("require-dpia".to_string()),
                visibility: manifest::AliasVisibility::Private,
                allowed_consumers: vec![],
            },
        ];

        let to_activate = pick_aliases_to_activate(&owned, &manifest_aliases);
        assert_eq!(to_activate, vec!["normal-alias"]);
        assert!(!to_activate.contains(&"gated-alias"));
    }

    #[test]
    fn test_activate_returns_all_when_no_gates() {
        let owned = vec!["a".to_string(), "b".to_string()];
        let manifest_aliases = vec![
            manifest::AliasSpec {
                id: "a".to_string(),
                display_name: "A".to_string(),
                methods: vec![],
                suggested_default: String::new(),
                gate: None,
                visibility: manifest::AliasVisibility::Private,
                allowed_consumers: vec![],
            },
            manifest::AliasSpec {
                id: "b".to_string(),
                display_name: "B".to_string(),
                methods: vec![],
                suggested_default: String::new(),
                gate: None,
                visibility: manifest::AliasVisibility::Private,
                allowed_consumers: vec![],
            },
        ];
        let to_activate = pick_aliases_to_activate(&owned, &manifest_aliases);
        assert_eq!(to_activate.len(), 2);
    }

    #[test]
    fn resource_requirements_full_toml() {
        // Pelna sekcja [resources] z wszystkimi polami
        let toml_str = r#"
            [resources]
            storage_total_mb = 1024
            storage_value_mb = 50
            llm_tokens_per_minute = 10000
            http_requests_per_minute = 300
            memory_mb = 512
            fuel_limit = 20000000
        "#;

        #[derive(serde::Deserialize)]
        struct Wrapper {
            resources: ResourceRequirements,
        }

        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.resources.storage_total_mb, Some(1024));
        assert_eq!(w.resources.storage_value_mb, Some(50));
        assert_eq!(w.resources.llm_tokens_per_minute, Some(10000));
        assert_eq!(w.resources.http_requests_per_minute, Some(300));
        assert_eq!(w.resources.memory_mb, Some(512));
        assert_eq!(w.resources.fuel_limit, Some(20_000_000));
    }

    #[test]
    fn resource_requirements_partial_toml() {
        // Czesciowa sekcja — tylko niektore pola
        let toml_str = r#"
            [resources]
            memory_mb = 256
            fuel_limit = 5000000
        "#;

        #[derive(serde::Deserialize)]
        struct Wrapper {
            resources: ResourceRequirements,
        }

        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.resources.memory_mb, Some(256));
        assert_eq!(w.resources.fuel_limit, Some(5_000_000));
        assert!(w.resources.storage_total_mb.is_none());
        assert!(w.resources.storage_value_mb.is_none());
        assert!(w.resources.llm_tokens_per_minute.is_none());
        assert!(w.resources.http_requests_per_minute.is_none());
    }

    #[test]
    fn resource_requirements_empty_section() {
        // Pusta sekcja [resources] — wszystkie pola None
        let toml_str = r#"
            [resources]
        "#;

        #[derive(serde::Deserialize)]
        struct Wrapper {
            resources: ResourceRequirements,
        }

        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert!(w.resources.storage_total_mb.is_none());
        assert!(w.resources.memory_mb.is_none());
        assert!(w.resources.fuel_limit.is_none());
    }

    #[test]
    fn resource_requirements_missing_section() {
        // Brak sekcji [resources] — Option<ResourceRequirements> = None
        let toml_str = r#"
            addon_id = "sdk-showcase"
            version = "1.0.0"
            display_name = "Test"
            permissions = []
            platforms = []
            wasm_file = "test.wasm"
            tools = []
        "#;

        #[derive(serde::Deserialize)]
        struct MinManifest {
            resources: Option<ResourceRequirements>,
        }

        let m: MinManifest = toml::from_str(toml_str).unwrap();
        assert!(m.resources.is_none());
    }

    #[test]
    fn resource_requirements_default() {
        // Default trait — wszystkie pola None
        let req = ResourceRequirements::default();
        assert!(req.storage_total_mb.is_none());
        assert!(req.storage_value_mb.is_none());
        assert!(req.llm_tokens_per_minute.is_none());
        assert!(req.http_requests_per_minute.is_none());
        assert!(req.memory_mb.is_none());
        assert!(req.fuel_limit.is_none());
    }
}
