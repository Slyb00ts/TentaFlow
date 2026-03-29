// =============================================================================
// Plik: addon/mod.rs
// Opis: Centralny modul systemu addonow WASM — eksporty publiczne, AddonManager
//       zarzadzajacy cyklem zycia addonow, instancjami i eventami.
// =============================================================================

pub mod runtime;
pub mod host_functions;
pub mod instance_pool;
pub mod permissions;
pub mod lifecycle;
pub mod ui_framework;
pub mod event_bus;
pub mod tool_dispatch;
pub mod flow_blocks;
pub mod rate_limiter;
pub mod utils;
pub mod bundled;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use runtime::{WasmEngine, WasmModule, WasmStore, WasmInstance};

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

/// Manifest addonu odczytany z manifest.toml w katalogu addonu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonManifest {
    pub addon_id: String,
    pub version: String,
    pub display_name: String,
    pub description: Option<String>,
    pub author: Option<String>,
    /// Wymagane uprawnienia
    pub permissions: Vec<ManifestPermission>,
    /// Platformy docelowe (puste = wszystkie)
    pub platforms: Vec<String>,
    /// Sciezka do pliku WASM wzgledem katalogu addonu
    pub wasm_file: String,
    /// Opcjonalny plik SKILL.md (prompt dla LLM)
    pub skill_file: Option<String>,
    /// Opcjonalny plik blocks.json (bloczki flow builder)
    pub blocks_file: Option<String>,
    /// Opcjonalny plik ikony
    pub icon_file: Option<String>,
    /// Limity zasobow (opcjonalne, domyslne z tabeli addon_resource_limits)
    pub resource_limits: Option<ManifestResourceLimits>,
    /// Slowa kluczowe addona (PL+EN) do semantic retrieval
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Kategoria addona (np. "komunikacja", "pliki", "ai")
    pub category: Option<String>,
    /// Definicje narzedzi dla LLM tool calling
    pub tools: Vec<ManifestTool>,
    /// Granularne uprawnienia deklarowane przez addon (z [[addon_permissions]])
    #[serde(default)]
    pub declared_permissions: Vec<AddonDeclaredPermission>,
    /// Reguly sieciowe TCP/UDP deklarowane przez addon (z [[network_rules]])
    #[serde(default)]
    pub network_rules: Vec<ManifestNetworkRule>,
    /// Reguly disambiguation — rozstrzyganie niejednoznacznych zapytan
    #[serde(default)]
    pub disambiguation: Vec<DisambiguationRule>,
}

impl Default for AddonManifest {
    fn default() -> Self {
        Self {
            addon_id: String::new(),
            version: String::new(),
            display_name: String::new(),
            description: None,
            author: None,
            permissions: Vec::new(),
            platforms: Vec::new(),
            wasm_file: String::new(),
            skill_file: None,
            blocks_file: None,
            icon_file: None,
            resource_limits: None,
            keywords: Vec::new(),
            category: None,
            tools: Vec::new(),
            declared_permissions: Vec::new(),
            network_rules: Vec::new(),
            disambiguation: Vec::new(),
        }
    }
}

/// Uprawnienie deklarowane w manifescie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPermission {
    pub permission_type: String,
    pub resource_pattern: Option<String>,
    pub access_level: String,
    pub reason: Option<String>,
    pub required: bool,
}

/// Limity zasobow z manifestu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestResourceLimits {
    pub max_instances: Option<i64>,
    pub cpu_limit_millicores: Option<i64>,
    pub ram_limit_mb: Option<i64>,
    pub storage_limit_mb: Option<i64>,
}

/// Definicja narzedzia z manifestu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTool {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub return_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub keywords: Vec<String>,
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

/// Granularne uprawnienie deklarowane przez addon w [[addon_permissions]]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonDeclaredPermission {
    /// Unikalny identyfikator uprawnienia (np. "chat_read", "files_write")
    pub id: String,
    /// Nazwa wyswietlana (np. "Odczyt czatow")
    pub name: String,
    /// Opis uprawnienia widoczny w panelu administracyjnym
    pub description: String,
    /// Kategoria grupujaca uprawnienia (np. "Komunikacja", "Pliki")
    pub category: String,
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
    /// Manifest addonu — potrzebny do walidacji regul sieciowych
    pub manifest: Arc<AddonManifest>,
    /// Limit pamieci WASM w bajtach
    pub memory_limit: usize,
    /// Limiter zasobow wasmi (iOS/Android) — pole uzywane przez Store::limiter()
    #[cfg(any(target_os = "ios", target_os = "android"))]
    pub store_limits: wasmi::StoreLimits,
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
    instances: Arc<RwLock<HashMap<String, Vec<AddonInstance>>>>,
    event_bus: Arc<EventBus>,
    engine: WasmEngine,
    permission_checker: Arc<PermissionChecker>,
    /// Skompilowane moduly WASM — cache po addon_id
    compiled_modules: Arc<RwLock<HashMap<String, WasmModule>>>,
    /// Zarejestrowane narzedzia ze wszystkich addonow
    registered_tools: Arc<RwLock<Vec<ToolDefinition>>>,
}

impl AddonManager {
    /// Tworzy nowy AddonManager z podana baza danych
    pub fn new(db: DbPool) -> Result<Self> {
        let engine = runtime::create_engine()?;

        let event_bus = Arc::new(EventBus::new());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));

        // Warm-up cache uprawnien — zaladuj wszystko z DB do cache
        permission_checker.refresh_all();

        // Uruchom background refresh co 5 minut
        permission_checker.start_background_refresh();

        // Uzupelnij reguly sieciowe HTTP domains dla juz zainstalowanych addonow
        {
            let conn = db.lock().unwrap();
            if let Err(e) = lifecycle::ensure_http_domain_rules(&conn) {
                tracing::warn!("Blad uzupelniania regul HTTP: {}", e);
            }
        }

        info!("AddonManager zainicjalizowany");

        Ok(Self {
            db,
            instances: Arc::new(RwLock::new(HashMap::new())),
            event_bus,
            engine,
            permission_checker,
            compiled_modules: Arc::new(RwLock::new(HashMap::new())),
            registered_tools: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Instaluje addon z podanego katalogu — czyta manifest.toml, waliduje,
    /// rejestruje w DB, kopiuje WASM
    pub fn install_addon(&self, addon_path: &Path) -> Result<()> {
        info!("Instalacja addonu z: {:?}", addon_path);

        // Parsuj manifest i zainstaluj
        let manifest = lifecycle::install(addon_path, &self.db)?;

        // Zarejestruj narzedzia z manifestu
        self.register_tools_from_manifest(&manifest)?;

        info!("Addon '{}' v{} zainstalowany pomyslnie", manifest.addon_id, manifest.version);
        Ok(())
    }

    /// Odinstalowuje addon — usuwa z DB, czysci storage, zatrzymuje instancje
    pub fn uninstall_addon(&self, addon_id: &str) -> Result<()> {
        info!("Odinstalowanie addonu: {}", addon_id);

        // Zatrzymaj wszystkie instancje tego addonu
        let instance_ids: Vec<String> = {
            let instances = self.instances.read();
            instances.get(addon_id)
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
        self.registered_tools.write().retain(|t| t.addon_id != addon_id);

        // Usun z DB
        lifecycle::uninstall(addon_id, &self.db)?;

        // Odsubskrybuj z event bus
        self.event_bus.unsubscribe_all(addon_id);

        info!("Addon '{}' odinstalowany pomyslnie", addon_id);
        Ok(())
    }

    /// Uruchamia addon — tworzy instancje WASM, zwraca instance_id
    pub fn start_addon(&self, addon_id: &str, user_id: Option<i64>) -> Result<String> {
        info!("Uruchamianie addonu '{}' dla user_id={:?}", addon_id, user_id);

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
            net_manager: Arc::new(Mutex::new(host_functions::network::NetworkConnectionManager::new())),
            manifest: Arc::new(manifest),
            memory_limit: DEFAULT_MEMORY_LIMIT_BYTES,
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
        if let Some(on_start) = instance.get_typed_func::<(), i32>(&mut store, "on_start").ok() {
            let result = on_start.call(&mut store, ())
                .context("Blad wywolania on_start()")?;
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
            ).context("Nie udalo sie zapisac instancji w DB")?;
        }

        let addon_instance = AddonInstance {
            addon_id: addon_id.to_string(),
            instance_id: instance_id.clone(),
            user_id,
            store,
            instance,
        };

        // Dodaj do mapy instancji
        self.instances.write()
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

        info!("Addon '{}' uruchomiony, instance_id={}", addon_id, instance_id);
        Ok(instance_id)
    }

    /// Zatrzymuje instancje addonu
    pub fn stop_addon(&self, instance_id: &str) -> Result<()> {
        info!("Zatrzymywanie instancji: {}", instance_id);

        let mut instances = self.instances.write();

        // Znajdz addon_id i indeks instancji
        let mut found = None;
        for (addon_id, addon_instances) in instances.iter_mut() {
            if let Some(pos) = addon_instances.iter().position(|i| i.instance_id == instance_id) {
                found = Some((addon_id.clone(), pos));
                break;
            }
        }

        let (addon_id, pos) = found
            .ok_or_else(|| anyhow::anyhow!("Instancja '{}' nie znaleziona", instance_id))?;

        // Pobierz instancje
        let mut addon_instance = instances.get_mut(&addon_id).unwrap().remove(pos);

        // VULN-046: Jawnie zamknij polaczenia sieciowe przed drop instancji
        {
            let net_mgr = addon_instance.store.data().net_manager.clone();
            let mut mgr = net_mgr.lock();
            let count = mgr.connection_count();
            mgr.close_all();
            if count > 0 {
                info!("stop_addon '{}': zamknieto {} polaczen sieciowych", addon_id, count);
            }
        }

        // Wywolaj on_stop() jesli addon go eksportuje
        if let Some(on_stop) = addon_instance.instance
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
            ).context("Nie udalo sie zaktualizowac statusu instancji")?;
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
        if instances.get(&addon_id).map_or(false, |v| v.is_empty()) {
            instances.remove(&addon_id);
        }

        info!("Instancja '{}' zatrzymana", instance_id);
        Ok(())
    }

    /// Wywoluje narzedzie addonu (dla LLM tool calling).
    /// K4: Minimalizacja czasu trzymania write lock — instancja jest wyjmowana z mapy
    /// pod lockiem (krotko), WASM jest wykonywany poza lockiem, potem wkladana z powrotem.
    pub fn call_tool(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: serde_json::Value,
        user_id: i64,
    ) -> Result<serde_json::Value> {
        info!("Wywolanie narzedzia '{}.{}' przez user_id={}", addon_id, tool_name, user_id);

        // Sprawdz uprawnienia uzytkownika
        let perm_result = self.permission_checker.check(
            addon_id,
            user_id,
            "llm",
            None,
        );
        if !perm_result.is_granted() {
            bail!("Brak uprawnien do wywolania narzedzia '{}.{}' dla user_id={}", addon_id, tool_name, user_id);
        }

        // K4: Wez instancje z mapy pod write lockiem (krotko)
        let mut addon_instance = {
            let mut instances = self.instances.write();
            let addon_instances = instances.get_mut(addon_id)
                .ok_or_else(|| anyhow::anyhow!("Addon '{}' nie ma uruchomionych instancji", addon_id))?;

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
            let alloc_fn = addon_instance.instance
                .get_typed_func::<i32, i32>(&mut addon_instance.store, "alloc")
                .context("Addon nie eksportuje funkcji alloc()")?;

            // Alokuj bufor wejsciowy w guest memory
            let input_ptr = alloc_fn.call(&mut addon_instance.store, request_bytes.len() as i32)
                .context("Blad alokacji pamieci guest")?;

            // CR-004: Sprawdz poprawnosc wskaznika
            if input_ptr < 0 {
                bail!("alloc() zwrocil niepoprawny wskaznik: {}", input_ptr);
            }

            // Zapisz dane do guest memory
            let memory = addon_instance.instance
                .get_memory(&mut addon_instance.store, "memory")
                .context("Brak eksportu 'memory' w module WASM")?;

            // CR-005: Sprawdz granice pamieci z checked_add
            let input_end = (input_ptr as usize)
                .checked_add(request_bytes.len())
                .context("Przepelnienie przy obliczaniu konca bufora wejsciowego")?;
            let mem_size = memory.data(&addon_instance.store).len();
            if input_end > mem_size {
                bail!("Bufor wejsciowy wykracza poza pamiec guest ({} > {})", input_end, mem_size);
            }

            memory.data_mut(&mut addon_instance.store)
                [input_ptr as usize..input_end]
                .copy_from_slice(&request_bytes);

            // Alokuj bufor wyjsciowy (64KB)
            let out_cap: i32 = 65536;
            let out_ptr = alloc_fn.call(&mut addon_instance.store, out_cap)
                .context("Blad alokacji bufora wyjsciowego")?;

            if out_ptr < 0 {
                bail!("alloc() zwrocil niepoprawny wskaznik wyjsciowy: {}", out_ptr);
            }

            // Alokuj miejsce na dlugosc wyniku (4 bajty)
            let out_len_ptr = alloc_fn.call(&mut addon_instance.store, 4)
                .context("Blad alokacji out_len")?;

            if out_len_ptr < 0 {
                bail!("alloc() zwrocil niepoprawny wskaznik out_len: {}", out_len_ptr);
            }

            // Wywolaj on_request w guest
            let on_request = addon_instance.instance
                .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut addon_instance.store, "on_request")
                .context("Addon nie eksportuje funkcji on_request()")?;

            let result_code = on_request.call(
                &mut addon_instance.store,
                (input_ptr, request_bytes.len() as i32, out_ptr, out_cap, out_len_ptr),
            ).context("Blad wywolania on_request()")?;

            if result_code != 0 {
                bail!("on_request() zwrocil blad: {}", result_code);
            }

            // Odczytaj dlugosc wyniku
            let mem_data = memory.data(&addon_instance.store);

            // CR-005: Sprawdz granice pamieci przy odczycie dlugosci
            let out_len_end = (out_len_ptr as usize)
                .checked_add(4)
                .context("Przepelnienie przy obliczaniu konca out_len")?;
            if out_len_end > mem_data.len() {
                bail!("out_len_ptr wykracza poza pamiec guest");
            }

            let out_len_bytes = &mem_data[out_len_ptr as usize..out_len_end];
            let out_len = i32::from_le_bytes([out_len_bytes[0], out_len_bytes[1], out_len_bytes[2], out_len_bytes[3]]);

            if out_len < 0 {
                bail!("out_len jest ujemny: {}", out_len);
            }

            // CR-005: Sprawdz granice pamieci przy odczycie wyniku
            let result_end = (out_ptr as usize)
                .checked_add(out_len as usize)
                .context("Przepelnienie przy obliczaniu konca wyniku")?;
            if result_end > mem_data.len() {
                bail!("Bufor wyniku wykracza poza pamiec guest ({} > {})", result_end, mem_data.len());
            }

            // Odczytaj wynik
            let result_bytes = &mem_data[out_ptr as usize..result_end];
            let result: serde_json::Value = serde_json::from_slice(result_bytes)
                .context("Nie udalo sie zdekodowac odpowiedzi z addonu")?;

            // Zwolnij pamiec guest
            if let Ok(dealloc_fn) = addon_instance.instance
                .get_typed_func::<(i32, i32), ()>(&mut addon_instance.store, "dealloc")
            {
                let _ = dealloc_fn.call(&mut addon_instance.store, (input_ptr, request_bytes.len() as i32));
                let _ = dealloc_fn.call(&mut addon_instance.store, (out_ptr, out_cap));
                let _ = dealloc_fn.call(&mut addon_instance.store, (out_len_ptr, 4));
            }

            Ok(result)
        })();

        // K4: Wloz instancje z powrotem do mapy
        {
            let mut instances = self.instances.write();
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
    /// K5: Minimalizacja write lock contention — zbierz instancje pod lockiem,
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
            let mut instances = self.instances.write();
            for subscriber in &subscribers {
                if let Some(addon_instances) = instances.get_mut(&subscriber.addon_id) {
                    if let Some(pos) = addon_instances.iter()
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
            if let Ok(on_event) = addon_instance.instance
                .get_typed_func::<(i32, i32), i32>(&mut addon_instance.store, "on_event")
            {
                if let Ok(alloc_fn) = addon_instance.instance
                    .get_typed_func::<i32, i32>(&mut addon_instance.store, "alloc")
                {
                    if let Ok(ptr) = alloc_fn.call(&mut addon_instance.store, event_json.len() as i32) {
                        // CR-004: Sprawdz poprawnosc wskaznika
                        if ptr < 0 {
                            warn!("alloc() zwrocil niepoprawny wskaznik dla eventu: {}", ptr);
                            continue;
                        }
                        if let Some(memory) = addon_instance.instance.get_memory(&mut addon_instance.store, "memory") {
                            let mem = memory.data_mut(&mut addon_instance.store);
                            // CR-005: Sprawdz granice z checked_add
                            let end = match (ptr as usize).checked_add(event_json.len()) {
                                Some(e) if e <= mem.len() => e,
                                _ => {
                                    warn!("Event buffer wykracza poza pamiec guest dla '{}'", addon_id);
                                    continue;
                                }
                            };
                            mem[ptr as usize..end].copy_from_slice(&event_json);
                            if let Err(e) = on_event.call(&mut addon_instance.store, (ptr, event_json.len() as i32)) {
                                warn!("Blad wywolania on_event() dla '{}': {}", addon_id, e);
                            }
                        }
                    }
                }
            }
        }

        // K5: Wloz instancje z powrotem do mapy
        {
            let mut instances = self.instances.write();
            for (addon_id, _pos, inst) in extracted {
                instances
                    .entry(addon_id)
                    .or_default()
                    .push(inst);
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
            ).context(format!("Nie znaleziono WASM dla addonu '{}'", addon_id))?
        };

        // Kompiluj modul
        let module = runtime::compile_module(&self.engine, &wasm_bytes)?;

        // Zapisz w cache
        self.compiled_modules.write().insert(addon_id.to_string(), module.clone());

        Ok(module)
    }

    /// Laduje manifest addonu z DB (z kolumny manifest_json)
    fn load_addon_manifest(&self, addon_id: &str) -> Result<AddonManifest> {
        let conn = self.db.lock().unwrap();
        let manifest_content: String = conn.query_row(
            "SELECT manifest_json FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        ).context(format!("Nie znaleziono manifestu dla addonu '{}'", addon_id))?;

        lifecycle::parse_manifest_toml(&manifest_content)
            .context(format!("Nie udalo sie sparsowac manifestu addonu '{}'", addon_id))
    }

    /// Laduje uprawnienia addonu z DB
    fn load_addon_permissions(&self, addon_id: &str) -> Result<Vec<String>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT permission_type FROM addon_declared_permissions WHERE addon_id = ?1"
        )?;
        let permissions: Vec<String> = stmt.query_map(
            rusqlite::params![addon_id],
            |row| row.get(0),
        )?.filter_map(|r| r.ok()).collect();

        Ok(permissions)
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
        let result_str = if error_message.is_some() { "error" } else { "ok" };
        let action_hash = fnv1a_hash(action);

        if let Ok(conn) = self.db.lock() {
            let _ = conn.execute(
                "INSERT INTO audit_log (user_id, addon_id, action, resource_id, result, error_message, action_hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![user_id, addon_id, action, resource_id, result_str, error_message, action_hash],
            );
        }
    }
}

/// D5: Reuzywany hash FNV-1a z utils
fn fnv1a_hash(s: &str) -> i64 {
    utils::fnv1a_hash(s)
}
