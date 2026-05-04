// =============================================================================
// Plik: addon/mod.rs
// Opis: Centralny modul systemu addonow WASM — eksporty publiczne, AddonManager
//       zarzadzajacy cyklem zycia addonow, instancjami i eventami.
// =============================================================================

pub mod bundled;
pub mod event_bus;
pub mod flow_blocks;
pub mod host_functions;
pub mod instance_pool;
pub mod lifecycle;
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
        })
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

        // Automatyczne aliasy modeli dla teams-bot
        if manifest.addon_id == "teams-bot" {
            self.activate_teams_aliases();
        }

        info!(
            "Addon '{}' v{} zainstalowany pomyslnie",
            manifest.addon_id, manifest.version
        );
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

        // Usun z DB
        lifecycle::uninstall(addon_id, &self.db)?;

        // Odsubskrybuj z event bus
        self.event_bus.unsubscribe_all(addon_id);

        // Dezaktywuj aliasy modeli dla teams-bot
        if addon_id == "teams-bot" {
            self.deactivate_teams_aliases();
        }

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

        // Reaktywuj aliasy modeli dla teams-bot
        if addon_id == "teams-bot" {
            self.activate_teams_aliases();
        }

        info!(
            "Addon '{}' uruchomiony, instance_id={}",
            addon_id, instance_id
        );
        Ok(instance_id)
    }

    /// Zatrzymuje instancje addonu
    pub fn stop_addon(&self, instance_id: &str) -> Result<()> {
        info!("Zatrzymywanie instancji: {}", instance_id);

        let mut instances = self.instances.lock();

        // Znajdz addon_id i indeks instancji
        let mut found = None;
        for (addon_id, addon_instances) in instances.iter_mut() {
            if let Some(pos) = addon_instances
                .iter()
                .position(|i| i.instance_id == instance_id)
            {
                found = Some((addon_id.clone(), pos));
                break;
            }
        }

        let (addon_id, pos) =
            found.ok_or_else(|| anyhow::anyhow!("Instancja '{}' nie znaleziona", instance_id))?;

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

        // Dezaktywuj aliasy gdy ostatnia instancja teams-bot zostala zatrzymana
        if addon_id == "teams-bot" && no_instances_left {
            self.deactivate_teams_aliases();
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

        // Opublikuj dalej na bus (dla innych subskrybentow)
        self.event_bus.publish(event);

        Ok(())
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

        // Pobierz WASM z DB
        let wasm_bytes: Vec<u8> = {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT wasm_bytes FROM addon_wasm WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |row| row.get(0),
            )
            .context(format!("Nie znaleziono WASM dla addonu '{}'", addon_id))?
        };

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

    /// Zwraca kategorie uprawnien (prefix przed kropka) deklarowane przez addon.
    /// Host functions przy `check_permission` podaja kategorie (np. "storage",
    /// "http", "llm"), a permission id w manifescie ma forme "kategoria.akcja"
    /// (np. "storage.read"). Tutaj wyciagamy deduplikowany zbior kategorii z
    /// manifestu — jedyne zrodlo prawdy.
    fn load_addon_permissions(&self, addon_id: &str) -> Result<Vec<String>> {
        let manifest = self.load_addon_manifest(addon_id)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(manifest.declared_permissions.len());
        for perm in &manifest.declared_permissions {
            let category = perm.id.split('.').next().unwrap_or(perm.id.as_str());
            if seen.insert(category.to_string()) {
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

    /// Aliasy STT/TTS/Summary powiazane z addonem teams-bot.
    /// `teams-summary` ma pusty default target — admin musi recznie wskazac model
    /// (qwen/gpt-oss/etc) w Models. Jesli pusty, meeting summary handler zwraca
    /// "not configured" error zamiast generowac udawana odpowiedz.
    const TEAMS_BOT_ALIASES: [(&'static str, &'static str); 5] = [
        ("teams-stt", "whisper-1"),
        ("teams-tts", "tts-1"),
        ("teams-summary", ""),
        // Vision aliasy są puste przy starcie — wypełnia je auto_bind po
        // pierwszym deployu odpowiedniego silnika (SCRFD → face,
        // HSEmotion → emotion). Brak deployu = pipeline w
        // `mesh/inference_proxy.rs::VideoFrame` skipuje inferencję bez błędu.
        ("teams-vision-face", ""),
        ("teams-vision-emotion", ""),
    ];

    /// Tworzy lub reaktywuje aliasy teams-stt / teams-tts i odswieza cache routera
    fn activate_teams_aliases(&self) {
        for (alias, default_target) in &Self::TEAMS_BOT_ALIASES {
            if let Err(e) = crate::db::repository::create_or_reactivate_model_alias(
                &self.db,
                alias,
                default_target,
                "first_available",
            ) {
                warn!(
                    "Nie udalo sie utworzyc/reaktywowac aliasu '{}': {}",
                    alias, e
                );
            }
        }
        self.reload_router_alias_cache();
        info!("Aliasy teams-stt/teams-tts aktywowane");
    }

    /// Dezaktywuje aliasy teams-stt / teams-tts i odswieza cache routera
    fn deactivate_teams_aliases(&self) {
        for (alias, _) in &Self::TEAMS_BOT_ALIASES {
            if let Err(e) = crate::db::repository::set_model_alias_active(&self.db, alias, false) {
                warn!("Nie udalo sie dezaktywowac aliasu '{}': {}", alias, e);
            }
        }
        self.reload_router_alias_cache();
        info!("Aliasy teams-stt/teams-tts dezaktywowane");
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
