// =============================================================================
// Plik: addon/mod.rs
// Opis: Centralny modul systemu addonow WASM — eksporty publiczne, AddonManager
//       zarzadzajacy cyklem zycia addonow, instancjami i eventami.
// =============================================================================

pub mod bundled;
pub mod errors;
pub mod event_bus;
pub mod flow_blocks;
pub mod fs_sandbox;
pub mod host_functions;
pub mod instance_pool;
pub mod lifecycle;
pub mod manifest;
pub mod migrations;
pub mod sdk_version;
pub mod storage_sql;
pub mod oauth;
pub mod oauth_cleanup;
pub mod oauth_crypto;
pub mod oauth_master_key;
pub mod oauth_refresh_guard;
pub mod permissions;
pub mod rate_limiter;
pub mod runtime;
pub mod tool_dispatch;
pub mod ui_framework;
pub mod utils;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use parking_lot::{Mutex, RwLock as PlRwLock};
use runtime::{WasmEngine, WasmInstance, WasmModule, WasmStore};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::db::DbPool;
use event_bus::{Event, EventBus};
use permissions::PermissionChecker;

// =============================================================================
// Stale konfiguracyjne
// =============================================================================

/// Domyslna ilosc paliwa (fuel) dla kazdej operacji WASM (10M instrukcji)
const DEFAULT_FUEL_LIMIT: u64 = 10_000_000;

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
    /// Szablony Flow z `[[flow_template]]` — opt-in install do flow-engine.
    #[serde(default)]
    pub flow_templates: Vec<manifest::FlowTemplateSpec>,
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
}

/// Sekcja [application] manifestu — rejestracja addonu jako aplikacji
/// widocznej w glownym menu GUI (Apps launcher). Wymaga zeby addon
/// eksportowal `on_request` i renderowal UI panel przez `ui_render`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonApplicationSection {
    /// ID panelu UI startowego — frontend po kliknieciu w menu woła
    /// `AddonUiPanelGetRequest { addon_id, panel_id }`.
    pub entry_panel: String,
    /// Tytul widoczny pod ikona w launchu.
    pub title: String,
    /// Identyfikator ikony sprite (np. "i-camera"). Domyslnie `addon.icon`.
    #[serde(default)]
    pub icon: Option<String>,
    /// Opcjonalna kolejnosc w menu (mniejsza wartosc = wyzej). Default 100.
    #[serde(default)]
    pub sort_order: Option<i32>,
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
    /// Budzet paliwa na pojedynczy tick. Default 5M instrukcji — wystarczy
    /// na typowy poll/aggregation, blokuje runaway loop w guest.
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
    pub user_id: Option<i64>,
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
    /// Shared cache UI panel state — host function `ui_render` zapisuje tu
    /// drzewo komponentow, MessageBody handler `AddonUiPanelGetRequest`
    /// odczytuje. `None` w testach event_bus w izolacji.
    pub ui_panels:
        Option<Arc<PlRwLock<HashMap<(i64, String, String), serde_json::Value>>>>,
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

// =============================================================================
// AddonInstance — uruchomiona instancja addonu WASM
// =============================================================================

/// Pojedyncza uruchomiona instancja addonu WASM
pub struct AddonInstance {
    pub addon_id: String,
    pub instance_id: String,
    pub user_id: Option<i64>,
    pub store: WasmStore<AddonState>,
    pub instance: WasmInstance,
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
    /// Cache ostatnio wyrenderowanego UI tree per (addon_id, panel_id).
    /// Addon woła `ui_render(panel_id, tree)` z guest WASM, host zapisuje
    /// `tree` w tym cache; frontend GUI pyta przez MessageBody
    /// `AddonUiPanelGetRequest`. Push do frontu przez bus subscribe wraca
    /// w przyszlej iteracji.
    ui_panels: Arc<PlRwLock<HashMap<(i64, String, String), serde_json::Value>>>,
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

impl AddonManager {
    /// Tworzy nowy AddonManager z podana baza danych
    pub fn new(db: DbPool, settings_cipher: Arc<crate::crypto::SettingsCipher>) -> Result<Self> {
        let engine = runtime::create_engine()?;

        let event_bus = Arc::new(EventBus::new());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));

        // Warm-up cache uprawnien — zaladuj wszystko z DB do cache
        permission_checker.refresh_all();

        // Uruchom background refresh co 5 minut
        permission_checker.start_background_refresh();

        info!("AddonManager zainicjalizowany");

        Ok(Self {
            db,
            instances: Arc::new(Mutex::new(HashMap::new())),
            event_bus,
            engine,
            permission_checker,
            settings_cipher,
            compiled_modules: Arc::new(PlRwLock::new(HashMap::new())),
            oauth_refresh_guard: Arc::new(oauth_refresh_guard::OAuthRefreshGuard::new()),
            registered_tools: Arc::new(PlRwLock::new(Vec::new())),
            router: Arc::new(PlRwLock::new(None)),
            flow_blocks_registry: Arc::new(flow_blocks::AddonFlowRegistry::new()),
            service_tasks: Arc::new(Mutex::new(HashMap::new())),
            ui_panels: Arc::new(PlRwLock::new(HashMap::new())),
        })
    }

    /// Zwraca handle do cache UI panel state — host function `ui_render`
    /// uzywa do zapisu, handler `AddonUiPanelGetRequest` do odczytu.
    pub fn ui_panels(&self) -> Arc<PlRwLock<HashMap<(i64, String, String), serde_json::Value>>> {
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

    /// Czy addon ma przynajmniej jedna running instancje. Uzywane przez
    /// handler `AddonUiPayload::ReqPanelGet` zeby zdecydowac czy lazy-start
    /// jest potrzebny.
    pub fn has_running_instance(&self, addon_id: &str) -> bool {
        self.instances
            .lock()
            .get(addon_id)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Wywoluje `on_request` na running instance addonu z action_id i params
    /// — uzywane dla button click / form submit z UI panel.
    /// Konwencja tool name: `ui.{panel_id}.{action_id}`. Reuse istniejacego
    /// on_request ABI (parse params z JSON, wykonaj akcje, zwroc JSON).
    pub fn invoke_ui_action(
        &self,
        addon_id: &str,
        panel_id: &str,
        action_id: &str,
        params: serde_json::Value,
        user_id: Option<i64>,
    ) -> Result<serde_json::Value> {
        let tool_name = format!("ui.{}.{}", panel_id, action_id);
        self.call_tool(addon_id, &tool_name, params, user_id.unwrap_or(0))
    }

    /// Zwraca rejestr flow blocks — dispatcher buduje z tego dynamic resolver
    /// dla `AdapterRegistry`, GUI handler dla listy bloków serializuje
    /// `list_all_blocks()`.
    pub fn flow_blocks_registry(&self) -> &Arc<flow_blocks::AddonFlowRegistry> {
        &self.flow_blocks_registry
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

        // Zarejestruj narzedzia z manifestu
        self.register_tools_from_manifest(&manifest)?;

        // Zarejestruj custom flow blocks (jesli addon dostarcza blocks.json
        // obok manifest.toml). Brak blocks.json = addon nie deklaruje
        // bloków — graceful skip.
        match flow_blocks::load_blocks_from_addon(&manifest.addon_id, addon_path) {
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

        // Generic alias registration from [[alias]] manifest sections plus
        // consumer-side `[[uses_alias]]` / `[[uses_model]]` declarations and
        // reconciliation of pending grants. All run in a single SQLite tx
        // so a partial install rolls back cleanly. Trigger when any of the
        // three sections is present — uses_* alone is enough for a pure
        // consumer addon.
        if !manifest.aliases.is_empty()
            || !manifest.uses_aliases.is_empty()
            || !manifest.uses_models.is_empty()
        {
            self.install_manifest_aliases(&manifest)?;
        }

        info!(
            "Addon '{}' v{} zainstalowany pomyslnie",
            manifest.addon_id, manifest.version
        );
        Ok(())
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
            audit_reconcile_uses_alias_within_tx, create_or_reactivate_model_alias_within_tx,
            lookup_alias_visibility_within_tx, reconcile_uses_alias_for_alias_within_tx,
            revoke_obsolete_manifest_consumers_within_tx, set_alias_visibility_within_tx,
            set_model_alias_active_audited_within_tx, upsert_uses_alias_within_tx,
            upsert_uses_model_within_tx,
        };

        let mut conn = self.db.lock().map_err(|e| {
            anyhow::anyhow!("db lock for alias install: {}", e)
        })?;
        let tx = conn.transaction()?;

        // 1. Register owned [[alias]] entries: model_aliases + ownership +
        //    visibility + consumer whitelist for `restricted`.
        for alias_spec in &manifest.aliases {
            let alias_id = create_or_reactivate_model_alias_within_tx(
                &tx,
                &alias_spec.id,
                &alias_spec.suggested_default,
                "first_available",
                "addon",
                Some(&manifest.addon_id),
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "addon '{}' alias '{}' registration failed: {}",
                    manifest.addon_id,
                    alias_spec.id,
                    e
                )
            })?;

            set_alias_visibility_within_tx(
                &tx,
                alias_id,
                alias_spec.visibility.as_db_str(),
                None,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "addon '{}' alias '{}' visibility write failed: {}",
                    manifest.addon_id,
                    alias_spec.id,
                    e
                )
            })?;

            // Revoke manifest-granted consumers that were dropped from the
            // current manifest (reinstall path). Admin-granted rows
            // (`granted_by_user_id IS NOT NULL`) are preserved — only the
            // operator can revoke those. Each revoke is audited so the
            // downstream `addon_uses_alias` reconcile transition has a
            // recorded upstream cause.
            let desired_consumers: &[String] = match alias_spec.visibility {
                crate::addon::manifest::AliasVisibility::Restricted => {
                    &alias_spec.allowed_consumers
                }
                _ => &[],
            };
            let revoked = revoke_obsolete_manifest_consumers_within_tx(
                &tx,
                alias_id,
                desired_consumers,
            )
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

            if alias_spec.gate.is_some() {
                set_model_alias_active_audited_within_tx(
                    &tx,
                    &alias_spec.id,
                    false,
                    Some(&manifest.addon_id),
                )
                .map_err(|e| {
                    anyhow::anyhow!(
                        "addon '{}' gated alias '{}' deactivate failed: {}",
                        manifest.addon_id,
                        alias_spec.id,
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
            let transitions =
                reconcile_uses_alias_for_alias_within_tx(&tx, &alias_spec.id)?;
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

        // Zatrzymaj wszystkie instancje tego addonu
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

        // Usun skompilowany modul z cache
        self.compiled_modules.write().remove(addon_id);

        // Usun zarejestrowane narzedzia
        self.registered_tools
            .write()
            .retain(|t| t.addon_id != addon_id);

        // Usun custom flow blocks — adapter resolver natychmiast przestanie
        // ich znajdowac, kompilacje flow w trakcie zostawiamy w spokoju (mają
        // własną kopię flow definition, executor po prostu zwroci "no
        // adapter for node" przy nastepnym uzyciu).
        self.flow_blocks_registry.unregister_addon_blocks(addon_id);

        // Wymus invalidate FlowCache — flow z cached `CompiledFlow` moze
        // miec dangling reference do bloku tego addonu, ktory wlasnie znika
        // z resolvera. Executor i tak by zwrocil "no adapter for node"
        // przy nastepnym uzyciu, ale clean cache produkuje czytelniejszy
        // blad przy compile (R2 "no adapter").
        if let Some(router) = self.router.read().clone() {
            if let Some(dispatcher) = router.flow_dispatcher() {
                dispatcher.invalidate_cache();
            }
        }

        // Deactivate aliases owned by this addon before uninstall — read
        // owner table directly (manifest may already be unreachable). Owner
        // rows stay so the audit trail and future reinstall match.
        self.deactivate_aliases_owned_by_addon(addon_id);

        // Usun z DB
        lifecycle::uninstall(addon_id, &self.db)?;

        // Odsubskrybuj z event bus
        self.event_bus.unsubscribe_all(addon_id);

        info!("Addon '{}' odinstalowany pomyslnie", addon_id);
        Ok(())
    }

    /// Uruchamia addon — tworzy instancje WASM, zwraca instance_id
    pub fn start_addon(&self, addon_id: &str, user_id: Option<i64>) -> Result<String> {
        info!(
            "Uruchamianie addonu '{}' dla user_id={:?}",
            addon_id, user_id
        );

        // Pobierz lub skompiluj modul WASM
        let module = self.get_or_compile_module(addon_id)?;

        // Pobierz uprawnienia addonu z DB
        let permissions = self.load_addon_permissions(addon_id)?;

        // Zaladuj manifest z DB (potrzebny do walidacji regul sieciowych)
        let manifest = self.load_addon_manifest(addon_id)?;

        let instance_id = uuid::Uuid::new_v4().to_string();

        // Utworz stan addonu
        let state = AddonState {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id,
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

        // Utworz store z limitem paliwa
        let mut store = runtime::create_store(&self.engine, state)?;

        // Utworz linker z host functions
        let mut linker = runtime::create_linker(&self.engine);
        host_functions::register_host_functions(&mut linker)?;

        // Utworz instancje WASM
        let instance = runtime::instantiate(&linker, &mut store, &module)?;

        // Wywolaj on_start() jesli addon go eksportuje
        if let Some(on_start) = instance
            .get_typed_func::<(), i32>(&mut store, "on_start")
            .ok()
        {
            let result = on_start
                .call(&mut store, ())
                .map_err(|e| anyhow::anyhow!("Blad wywolania on_start(): {e}"))?;
            if result != 0 {
                bail!("on_start() zwrocil blad: {}", result);
            }
        }

        // Zaktualizuj status instancji w DB
        {
            let conn = self.db.lock().unwrap();
            conn.execute(
                "INSERT INTO addon_instances (addon_id, instance_id, instance_name, status, created_by, started_at) \
                 VALUES (?1, ?2, ?3, 'running', ?4, datetime('now'))",
                rusqlite::params![addon_id, &instance_id, format!("{}-{}", addon_id, &instance_id[..8]), user_id],
            ).map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac instancji w DB: {e}"))?;
        }

        let addon_instance = AddonInstance {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id,
            store,
            instance,
        };

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
                            let fuel = service.tick_fuel_budget.unwrap_or(5_000_000);
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
            match self.start_addon(&a.addon_id, None) {
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
        info!(
            "Toggle is_enabled dla addonu '{}' -> {}",
            addon_id, enabled
        );

        {
            let conn = self.db.lock().unwrap();
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
                self.start_addon(addon_id, None)?;
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

        let manager_instances = self.instances.clone();
        let engine = self.engine.clone();
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
                                &engine,
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
        engine: &runtime::WasmEngine,
        addon_id: &str,
        instance_id: &str,
        fuel_per_tick: u64,
        timeout_ms: Option<u64>,
    ) -> Result<()> {
        // Wyciagnij instancje (lock briefly)
        let mut addon_instance = {
            let mut instances = instances_map.lock();
            let addon_instances = instances
                .get_mut(addon_id)
                .ok_or_else(|| anyhow::anyhow!("addon '{}' nie ma uruchomionych instancji", addon_id))?;
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

        // Per-call epoch deadline: store trapuje po 1 increment counter.
        // Watchdog (jesli timeout_ms set) zwiększa counter raz po `t` ms.
        // UWAGA: wasmtime epoch jest engine-global — trap dotyczy wszystkich
        // stores z deadline ≤ current. Per-store isolated cancellation
        // wymaga epoch_deadline_callback (follow-up).
        addon_instance.store.set_epoch_deadline(1);

        let watchdog = timeout_ms.map(|t| {
            let engine = engine.clone();
            let dur = std::time::Duration::from_millis(t);
            std::thread::spawn(move || {
                std::thread::sleep(dur);
                engine.increment_epoch();
            })
        });

        // Wywolaj on_tick(timestamp_ms) -> i32 — addon nie musi go eksportowac;
        // brak exportu = no-op (instance zostaje aktywna na inne hooks
        // jak on_event).
        let res: Result<()> = (|| {
            if let Ok(on_tick) = addon_instance
                .instance
                .get_typed_func::<i64, i32>(&mut addon_instance.store, "on_tick")
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

        // Watchdog cleanup — niezaleznie od wyniku ticka, watek detached
        // i tak sie skonczy po sleep. Drop nie blokuje na join.
        drop(watchdog);

        // Reset epoch deadline — store wraca do mapy i moze byc uzyty
        // przez handle_event lub call_tool. Set_epoch_deadline jest DELTA,
        // wiec u64::MAX/4 zachowuje sie jak "nigdy nie wytrap" (current+
        // delta nie osiagniete normalnymi incrementami).
        addon_instance.store.set_epoch_deadline(u64::MAX / 4);

        // Wloz z powrotem nawet przy bledzie — pojedyncza nieudana tura nie
        // zabija service (np. transient error w dispatch'u host_function).
        instances_map
            .lock()
            .entry(addon_id.to_string())
            .or_default()
            .push(addon_instance);

        res
    }

    /// Zatrzymuje instancje addonu
    pub fn stop_addon(&self, instance_id: &str) -> Result<()> {
        info!("Zatrzymywanie instancji: {}", instance_id);

        // Anuluj service tick loop (jesli ten instance ma service mode).
        // Token wyzwala `select` w petli, ktora wychodzi cleanly bez
        // szarpania trzymanej instancji — po cancel mozemy bezpiecznie
        // wyciagnac instancje z mapy ponizej.
        if let Some(token) = self.service_tasks.lock().remove(instance_id) {
            token.cancel();
        }

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

        // Wywolaj on_stop() jesli addon go eksportuje
        if let Some(on_stop) = addon_instance
            .instance
            .get_typed_func::<(), i32>(&mut addon_instance.store, "on_stop")
            .ok()
        {
            if let Err(e) = on_stop.call(&mut addon_instance.store, ()) {
                warn!("Blad wywolania on_stop() dla '{}': {}", instance_id, e);
            }
        }

        // Zaktualizuj status w DB
        {
            let conn = self.db.lock().unwrap();
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

        // Usun pusta liste jesli brak instancji
        let no_instances_left = instances.get(&addon_id).map_or(true, |v| v.is_empty());
        if no_instances_left {
            instances.remove(&addon_id);
        }

        // Deactivate aliases when the last instance of any addon is gone.
        if no_instances_left {
            self.deactivate_aliases_owned_by_addon(&addon_id);
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
        user_id: i64,
    ) -> Result<serde_json::Value> {
        info!(
            "Wywolanie narzedzia '{}.{}' przez user_id={}",
            addon_id, tool_name, user_id
        );

        // Sprawdz uprawnienia uzytkownika
        let perm_result = self
            .permission_checker
            .check(addon_id, user_id, "llm", None);
        if !perm_result.is_granted() {
            bail!(
                "Brak uprawnien do wywolania narzedzia '{}.{}' dla user_id={}",
                addon_id,
                tool_name,
                user_id
            );
        }

        // K4: Wez instancje z mapy pod lockiem (krotko)
        let mut addon_instance = {
            let mut instances = self.instances.lock();
            let addon_instances = instances.get_mut(addon_id).ok_or_else(|| {
                anyhow::anyhow!("Addon '{}' nie ma uruchomionych instancji", addon_id)
            })?;

            if addon_instances.is_empty() {
                bail!("Brak dostepnych instancji addonu '{}'", addon_id);
            }
            // Wyjmij pierwsza instancje — lock jest zwalniany natychmiast
            addon_instances.remove(0)
        };
        // Write lock zwolniony — inne watki moga operowac na mapie

        // Przygotuj dane wejsciowe jako JSON
        let request_json = serde_json::json!({
            "tool": tool_name,
            "params": params,
            "user_id": user_id,
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

            // Wywolaj on_request w guest
            let on_request = addon_instance
                .instance
                .get_typed_func::<(i32, i32, i32, i32, i32), i32>(
                    &mut addon_instance.store,
                    "on_request",
                )
                .map_err(|e| anyhow::anyhow!("Addon nie eksportuje funkcji on_request(): {e}"))?;

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

        // K4: Wloz instancje z powrotem do mapy
        {
            let mut instances = self.instances.lock();
            instances
                .entry(addon_id.to_string())
                .or_default()
                .push(addon_instance);
        }

        // Loguj do audit
        self.log_audit(addon_id, user_id, "tool.call", Some(tool_name), None);

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
        user_id: Option<i64>,
        fuel_budget: u64,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<u8>> {
        info!(
            "Wywolanie flow blocku '{}.{}' (user_id={:?}, fuel={}, deadline={:?})",
            addon_id, block_type, user_id, fuel_budget, deadline
        );

        // Permission: addon musi miec "flow_blocks" (opcjonalnie z resource =
        // block_type, ale dla MVP wystarczy ogolne). Brak uprawnien = bail.
        if let Some(uid) = user_id {
            let perm = self
                .permission_checker
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

        let instance_id = format!("block-{}", uuid::Uuid::new_v4());

        // Fresh AddonState — odizolowane od running instances w `self.instances`.
        let state = AddonState {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id,
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

        // Per-call epoch deadline: store trapuje po 1 increment counter.
        // UWAGA: wasmtime epoch jest engine-global — increment_epoch
        // trapuje WSZYSTKIE stores z deadline ≤ current. Per-store
        // isolated cancellation wymaga epoch_deadline_callback z
        // per-store atomic flag; odlozone jako follow-up.
        store.set_epoch_deadline(1);

        let watchdog = deadline.map(|d| {
            let engine = self.engine.clone();
            std::thread::spawn(move || {
                let now = std::time::Instant::now();
                if d > now {
                    std::thread::sleep(d - now);
                }
                engine.increment_epoch();
            })
        });

        let mut linker = runtime::create_linker(&self.engine);
        host_functions::register_host_functions(&mut linker)?;
        let instance = runtime::instantiate(&linker, &mut store, &module)?;

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

            let on_request = instance
                .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, "on_request")
                .map_err(|e| anyhow::anyhow!("brak on_request: {e}"))?;

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
                .map_err(|e| anyhow::anyhow!("on_request fail: {e}"))?;

            if result_code != 0 {
                bail!("on_request zwrocil kod bledu: {}", result_code);
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

        // Watchdog cleanup — niezaleznie od wyniku.
        if let Some(handle) = watchdog {
            // Nie joinujemy — watek i tak sie skonczy po sleep+increment_epoch.
            // Drop join handle = detach.
            drop(handle);
        }

        // Loguj do audit. user_id=0 = system call (None mapuje na 0,
        // konwencja z call_tool gdzie sygnatura jest i64).
        self.log_audit(
            addon_id,
            user_id.unwrap_or(0),
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
            if let Ok(on_event) = addon_instance
                .instance
                .get_typed_func::<(i32, i32), i32>(&mut addon_instance.store, "on_event")
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
        // nigdy do niej nie zapisuje. WASM zyje na dysku w
        // bundled_addons_dir()/{addon_id}/{manifest.wasm_file}. Czytamy
        // sciezke z manifestu (pole `wasm_file` z [addon]).
        let manifest = self.load_addon_manifest(addon_id)?;
        let wasm_path = bundled::bundled_addons_dir()
            .join(addon_id)
            .join(&manifest.wasm_file);
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
        let conn = self.db.lock().unwrap();
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
        user_id: i64,
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

        if let Ok(conn) = self.db.lock() {
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
        let conn = match self.db.lock() {
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
            addon_id = "test-addon"
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
