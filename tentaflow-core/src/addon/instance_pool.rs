// =============================================================================
// Plik: addon/instance_pool.rs
// Opis: Pre-warmed WASM instance pool — utrzymuje gotowe instancje Wasmtime
//       do natychmiastowego uzycia. Minimalizuje latency przy uruchamianiu
//       addonow przez unikanie kompilacji i instancjacji WASM.
// =============================================================================

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::runtime::{WasmEngine, WasmInstance, WasmLinker, WasmModule, WasmStore};
use anyhow::Result;
use parking_lot::Mutex;
use tracing::{info, trace, warn};

use super::event_bus::EventBus;
use super::host_functions;
use super::permissions::PermissionChecker;
use super::{AddonInstance, AddonManifest, AddonState};
use crate::db::DbPool;

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(1);

// =============================================================================
// InstancePool — pula pre-warmed instancji
// =============================================================================

/// Pula pre-warmed instancji WASM dla konkretnego addonu.
/// Utrzymuje N gotowych instancji, ktore mozna natychmiast przydzielac.
pub struct InstancePool {
    engine: WasmEngine,
    module: WasmModule,
    linker: WasmLinker<AddonState>,
    pool: Mutex<VecDeque<PoolEntry>>,
    pool_size: usize,
    addon_id: String,
    db: DbPool,
    event_bus: Arc<EventBus>,
    permission_checker: Arc<PermissionChecker>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    /// Deklarowane uprawnienia addonu (zaladowane raz)
    declared_permissions: Vec<String>,
    /// Router do routowania requestow LLM
    router: Option<Arc<crate::routing::router::Router>>,
    /// Per-account mutex map for OAuth refresh deduplication.
    oauth_refresh_guard: Arc<super::oauth_refresh_guard::OAuthRefreshGuard>,
    /// Manifest runtime id ("wasmtime", "dotnet", "python") for adapter resolution.
    runtime_id: String,
    /// Cached fuel limit from DB (0 = use default)
    fuel_limit: u64,
    /// Parsed manifest, shared across all instances
    manifest: Arc<AddonManifest>,
}

/// Wpis w puli — gotowa instancja WASM
struct PoolEntry {
    store: WasmStore<AddonState>,
    instance: WasmInstance,
}

impl InstancePool {
    /// Tworzy nowa pule instancji i pre-warmuje N instancji.
    ///
    /// # Argumenty
    /// - `engine` — silnik Wasmtime
    /// - `module` — skompilowany modul WASM
    /// - `pool_size` — ilosc pre-warmed instancji
    /// - `addon_id` — identyfikator addonu
    /// - `db` — pool polaczen DB
    /// - `event_bus` — bus eventow
    /// - `permission_checker` — checker uprawnien
    /// - `declared_permissions` — deklarowane uprawnienia addonu
    pub fn new(
        engine: WasmEngine,
        module: WasmModule,
        pool_size: usize,
        addon_id: String,
        db: DbPool,
        event_bus: Arc<EventBus>,
        permission_checker: Arc<PermissionChecker>,
        settings_cipher: Arc<crate::crypto::SettingsCipher>,
        declared_permissions: Vec<String>,
        router: Option<Arc<crate::routing::router::Router>>,
        runtime_id: String,
    ) -> Result<Self> {
        // Build linker once — reused for every instantiate() call
        let mut linker = super::runtime::create_linker(&engine);
        host_functions::register_host_functions(&mut linker)?;

        // Cache fuel_limit from DB
        let fuel_limit = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT fuel_limit FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![&addon_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
        };

        // Cache parsed manifest from DB
        let manifest = {
            let conn = db.lock().unwrap();
            let manifest_content: String = conn
                .query_row(
                    "SELECT manifest_json FROM addons WHERE addon_id = ?1",
                    rusqlite::params![&addon_id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|_| "{}".to_string());
            Arc::new(
                super::lifecycle::parse_manifest_toml(&manifest_content).unwrap_or_else(|_| {
                    super::AddonManifest {
                        addon_id: addon_id.clone(),
                        ..Default::default()
                    }
                }),
            )
        };

        let pool = InstancePool {
            engine,
            module,
            linker,
            pool: Mutex::new(VecDeque::with_capacity(pool_size)),
            pool_size,
            addon_id,
            db,
            event_bus,
            permission_checker,
            settings_cipher,
            declared_permissions,
            router,
            oauth_refresh_guard: Arc::new(super::oauth_refresh_guard::OAuthRefreshGuard::new()),
            runtime_id,
            fuel_limit,
            manifest,
        };

        // Pre-warm instancje
        pool.fill_pool()?;

        Ok(pool)
    }

    /// Pobiera instancje z puli — jesli pula pusta, tworzy nowa.
    /// Ustawia user_id na instancji.
    pub fn acquire(&self, user_id: Option<String>) -> Result<AddonInstance> {
        let entry = {
            let mut pool = self.pool.lock();
            pool.pop_front()
        };

        let (mut store, instance) = match entry {
            Some(e) => (e.store, e.instance),
            None => {
                // Pula pusta — tworzymy nowa instancje na zywo
                warn!(
                    "InstancePool[{}]: pula wyczerpana, tworzenie instancji on-demand",
                    self.addon_id
                );
                self.create_instance(user_id.clone())?
            }
        };

        // Ustaw user_id na pobranej instancji
        store.data_mut().user_id = user_id.clone();

        // Monotonic counter — no syscall, unique within process lifetime
        let seq = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let instance_id = format!("{}:{}", self.addon_id, seq);
        store.data_mut().instance_id = instance_id.clone();

        let effective_fuel = if self.fuel_limit > 0 {
            self.fuel_limit
        } else {
            super::DEFAULT_FUEL_LIMIT
        };

        // Doladuj paliwo
        super::runtime::refuel_store(&mut store, effective_fuel)?;

        trace!(
            "InstancePool[{}]: instancja przydzielona (instance_id={}, user_id={:?})",
            self.addon_id,
            instance_id,
            user_id
        );

        let language_adapter = super::runtime::adapter_for_runtime(&self.runtime_id)
            .unwrap_or_else(|| Box::new(super::runtime::RustAdapter));

        Ok(AddonInstance {
            addon_id: self.addon_id.clone(),
            instance_id,
            user_id,
            store,
            instance,
            language_adapter,
        })
    }

    /// Zwraca instancje do puli po zakonczeniu uzycia.
    /// CR-007: Resetuje guest memory przed zwroceniem do puli — zapobiega wyciekowi
    /// danych miedzy roznymi uzytkownikami/sesjami.
    pub fn release(&self, addon_instance: AddonInstance) {
        let mut pool = self.pool.lock();

        // Jesli pula jest pelna — po prostu dropujemy instancje
        if pool.len() >= self.pool_size {
            trace!(
                "InstancePool[{}]: pula pelna, instancja {} zostanie zdropowana",
                self.addon_id,
                addon_instance.instance_id
            );
            return;
        }

        // Resetuj stan
        let AddonInstance {
            mut store,
            instance,
            ..
        } = addon_instance;

        // CR-007: Zero guest memory before returning to pool.
        // data_size() returns the actual wasm linear memory (pages * 64KB),
        // not the virtual reservation — only the committed region is zeroed.
        if let Some(memory) = instance.get_memory(&mut store, "memory") {
            let used = memory.data_size(&store);
            let mem_data = memory.data_mut(&mut store);
            mem_data[..used].fill(0);
        }

        // Wyczysc stan uzytkownika
        store.data_mut().user_id = None;
        store.data_mut().instance_id = String::new();
        store.data_mut().fuel_consumed = 0;
        store.data_mut().is_system_call = false;

        pool.push_back(PoolEntry { store, instance });

        trace!(
            "InstancePool[{}]: instancja zwrocona do puli ({}/{})",
            self.addon_id,
            pool.len(),
            self.pool_size
        );
    }

    /// Zwraca aktualny rozmiar puli (ilosc dostepnych instancji)
    pub fn available(&self) -> usize {
        self.pool.lock().len()
    }

    /// Zwraca maksymalny rozmiar puli
    pub fn capacity(&self) -> usize {
        self.pool_size
    }

    // =========================================================================
    // Metody prywatne
    // =========================================================================

    /// Wypelnia pule do pelnego rozmiaru
    fn fill_pool(&self) -> Result<()> {
        let mut pool = self.pool.lock();
        let current = pool.len();
        let needed = self.pool_size.saturating_sub(current);

        for _i in 0..needed {
            let (store, instance) = self.create_instance(None)?;
            pool.push_back(PoolEntry { store, instance });
        }

        info!(
            "InstancePool[{}]: pre-warmed {} instancji (total {}/{})",
            self.addon_id,
            needed,
            pool.len(),
            self.pool_size
        );

        Ok(())
    }

    /// Tworzy nowa instancje WASM (store + instance)
    fn create_instance(
        &self,
        user_id: Option<String>,
    ) -> Result<(WasmStore<AddonState>, WasmInstance)> {
        let is_system_call = user_id.is_none();
        let state = AddonState {
            addon_id: self.addon_id.clone(),
            instance_id: String::new(), // Bedzie ustawiony przy acquire()
            user_id,
            org_id: None,
            db: self.db.clone(),
            permissions: self.declared_permissions.clone(),
            event_bus: self.event_bus.clone(),
            permission_checker: self.permission_checker.clone(),
            fuel_consumed: 0,
            is_system_call,
            rate_limiter: None,
            net_manager: Arc::new(parking_lot::Mutex::new(
                host_functions::network::NetworkConnectionManager::new(),
            )),
            settings_cipher: self.settings_cipher.clone(),
            manifest: Arc::clone(&self.manifest),
            memory_limit: super::DEFAULT_MEMORY_LIMIT_BYTES,
            router: self.router.clone(),
            oauth_refresh_guard: self.oauth_refresh_guard.clone(),
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
            #[cfg(any(target_os = "ios", target_os = "android"))]
            store_limits: wasmi::StoreLimitsBuilder::new()
                .memory_size(super::DEFAULT_MEMORY_LIMIT_BYTES)
                .trap_on_grow_failure(true)
                .instances(10)
                .memories(1)
                .tables(10)
                .build(),
        };

        let mut store = super::runtime::create_store(&self.engine, state)?;

        let instance = super::runtime::instantiate(&self.linker, &mut store, &self.module)?;

        Ok((store, instance))
    }

    /// Reload cached fuel_limit from DB (call after admin changes resource limits)
    pub fn refresh_fuel_limit(&mut self) {
        let conn = self.db.lock().unwrap();
        self.fuel_limit = conn
            .query_row(
                "SELECT fuel_limit FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![&self.addon_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;
    }
}
