// =============================================================================
// Plik: addon/runtime/runtime_wasmi.rs
// Opis: Backend wasmi — uzyty na iOS i Android (platformy mobilne).
//       Eksportuje ujednolicone type aliasy i funkcje do operacji na WASM.
//       wasmi jest interpreterem — wolniejszy niz Wasmtime ale dziala wszedzie.
// =============================================================================

use anyhow::{Context, Result};
use tracing::info;

use crate::addon::{AddonState, DEFAULT_FUEL_LIMIT, DEFAULT_MEMORY_LIMIT_BYTES};

// =============================================================================
// Type aliasy — ujednolicone nazwy dla obu backendow
// =============================================================================

pub type WasmEngine = wasmi::Engine;
pub type WasmModule = wasmi::Module;
pub type WasmStore<T> = wasmi::Store<T>;
pub type WasmLinker<T> = wasmi::Linker<T>;
pub type WasmInstance = wasmi::Instance;
pub type WasmCaller<'a, T> = wasmi::Caller<'a, T>;
pub type WasmMemory = wasmi::Memory;

// =============================================================================
// Re-eksporty traitow potrzebnych w host functions
// =============================================================================

pub use wasmi::AsContext;
pub use wasmi::AsContextMut;

// =============================================================================
// Konfiguracja silnika wasmi
// =============================================================================

/// Tworzy skonfigurowany silnik wasmi z fuel metering
pub fn create_engine() -> Result<WasmEngine> {
    let mut config = wasmi::Config::default();

    // Fuel metering — kazda instrukcja WASM zuzywa paliwo,
    // pozwala na ograniczanie czasu wykonania
    config.consume_fuel(true);

    let engine = WasmEngine::new(&config);

    info!("Silnik wasmi utworzony (fuel metering + memory limit)");

    Ok(engine)
}

// =============================================================================
// Kompilacja modulow WASM
// =============================================================================

/// Kompiluje bajty WASM do modulu wasmi z walidacja
pub fn compile_module(engine: &WasmEngine, wasm_bytes: &[u8]) -> Result<WasmModule> {
    let module = WasmModule::new(engine, wasm_bytes)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie skompilowac modulu WASM: {}", e))?;

    info!("Modul WASM skompilowany ({} bajtow)", wasm_bytes.len(),);

    Ok(module)
}

// =============================================================================
// Tworzenie Store z limiterami
// =============================================================================

/// Tworzy nowy Store z limitem paliwa i limitem pamieci
pub fn create_store(engine: &WasmEngine, state: AddonState) -> Result<WasmStore<AddonState>> {
    let memory_limit = state.memory_limit;
    let mut store = WasmStore::new(engine, state);

    // Ustaw poczatkowe paliwo — addon zuzywa paliwo z kazdej instrukcji WASM
    store
        .set_fuel(DEFAULT_FUEL_LIMIT)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie ustawic paliwa: {}", e))?;

    // Limit pamieci — ogranicza memory.grow i table.grow per instancja
    // Uzywamy store_limits z AddonState (pole cfg-gated)
    store.limiter(|state| &mut state.store_limits);

    info!(
        "Store wasmi utworzony (fuel={}, memory_limit={}MB)",
        DEFAULT_FUEL_LIMIT,
        memory_limit / (1024 * 1024)
    );

    Ok(store)
}

/// Doladowuje paliwo w istniejacym store (np. po wznowieniu operacji)
pub fn refuel_store(store: &mut WasmStore<AddonState>, fuel: u64) -> Result<()> {
    store
        .set_fuel(fuel)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie doladowac paliwa: {}", e))?;
    Ok(())
}

/// Sprawdza ile paliwa pozostalo w store
pub fn remaining_fuel(store: &WasmStore<AddonState>) -> Result<u64> {
    store
        .get_fuel()
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac poziomu paliwa: {}", e))
}

// =============================================================================
// Pomocnicze funkcje — dostep do pamieci WASM
// =============================================================================

/// Pobiera obiekt memory z instancji WASM przez Caller
pub fn get_memory(caller: &mut WasmCaller<'_, AddonState>) -> Option<WasmMemory> {
    caller.get_export("memory")?.into_memory()
}

/// Zwraca slice danych z pamieci guest (immutable)
pub fn memory_data<'a, T: 'a>(
    memory: &WasmMemory,
    store: &'a impl AsContext<Data = T>,
) -> &'a [u8] {
    memory.data(store)
}

/// Zwraca mutowalny slice danych z pamieci guest
pub fn memory_data_mut<'a, T: 'a>(
    memory: &WasmMemory,
    store: &'a mut impl AsContextMut<Data = T>,
) -> &'a mut [u8] {
    memory.data_mut(store)
}

/// Tworzy nowy Linker dla silnika (wrapper na Linker::new)
pub fn create_linker(engine: &WasmEngine) -> WasmLinker<AddonState> {
    WasmLinker::new(engine)
}

/// Instancjacja modulu WASM w podanym store.
///
/// wasmi 1.0.x eksportuje tylko `instantiate_and_start` — nie ma publicznego
/// `InstancePre`/`ensure_no_start` jak w 0.x, ani jak w Wasmtime. Jesli modul
/// ma funkcje `_start`, zostanie uruchomiona. Addony TentaFlow powinny
/// eksportowac funkcje wywolywane na zadanie, wiec `_start` nie jest oczekiwany.
pub fn instantiate(
    linker: &WasmLinker<AddonState>,
    store: &mut WasmStore<AddonState>,
    module: &WasmModule,
) -> Result<WasmInstance> {
    linker
        .instantiate_and_start(&mut *store, module)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc instancji WASM: {}", e))
}
