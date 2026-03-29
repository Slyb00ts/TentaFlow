// =============================================================================
// Plik: addon/runtime/runtime_wasmtime.rs
// Opis: Backend Wasmtime — uzyty na Desktop i Router (nie-mobilne platformy).
//       Eksportuje ujednolicone type aliasy i funkcje do operacji na WASM.
// =============================================================================

use anyhow::{Context, Result};
use tracing::info;
use wasmtime::{Config, OptLevel};

use crate::addon::{AddonState, DEFAULT_FUEL_LIMIT, DEFAULT_MEMORY_LIMIT_BYTES};

// =============================================================================
// Type aliasy — ujednolicone nazwy dla obu backendow
// =============================================================================

pub type WasmEngine = wasmtime::Engine;
pub type WasmModule = wasmtime::Module;
pub type WasmStore<T> = wasmtime::Store<T>;
pub type WasmLinker<T> = wasmtime::Linker<T>;
pub type WasmInstance = wasmtime::Instance;
pub type WasmCaller<'a, T> = wasmtime::Caller<'a, T>;
pub type WasmMemory = wasmtime::Memory;

// =============================================================================
// Re-eksporty traitow potrzebnych w host functions
// =============================================================================

pub use wasmtime::AsContext;
pub use wasmtime::AsContextMut;

// =============================================================================
// Konfiguracja silnika Wasmtime
// =============================================================================

/// Tworzy skonfigurowany silnik Wasmtime z fuel metering, epoch interruption
/// i limitami pamieci
pub fn create_engine() -> Result<WasmEngine> {
    let mut config = Config::new();

    // Fuel metering — kazda instrukcja WASM zuzywa paliwo,
    // pozwala na ograniczanie czasu wykonania
    config.consume_fuel(true);

    // Epoch interruption — pozwala na przerywanie dlugotrwalych operacji
    // z innego watku (np. timeout)
    config.epoch_interruption(true);

    // Optymalizacje kompilacji
    config.cranelift_opt_level(OptLevel::Speed);

    // Wlacz cache kompilacji (przyspieszenie ponownych uruchomien)
    config.cranelift_nan_canonicalization(false);

    // Wielowatkowosc — kompilacja rownolega
    config.parallel_compilation(true);

    // Limit pamieci WASM — ogranicza rezerwacje pamieci per instancja
    config.memory_reservation(DEFAULT_MEMORY_LIMIT_BYTES as u64);
    config.memory_reservation_for_growth(0);

    let engine = WasmEngine::new(&config)
        .context("Nie udalo sie utworzyc silnika Wasmtime")?;

    info!("Silnik Wasmtime utworzony (fuel metering + epoch interruption)");

    Ok(engine)
}

// =============================================================================
// Kompilacja modulow WASM
// =============================================================================

/// Kompiluje bajty WASM do modulu Wasmtime z walidacja
pub fn compile_module(engine: &WasmEngine, wasm_bytes: &[u8]) -> Result<WasmModule> {
    let module = WasmModule::new(engine, wasm_bytes)
        .context("Nie udalo sie skompilowac modulu WASM")?;

    info!(
        "Modul WASM skompilowany ({} bajtow, {} eksportow)",
        wasm_bytes.len(),
        module.exports().count()
    );

    Ok(module)
}

// =============================================================================
// Tworzenie Store z limiterami
// =============================================================================

/// Tworzy nowy Store z limitem paliwa i limiterem pamieci
pub fn create_store(engine: &WasmEngine, state: AddonState) -> Result<WasmStore<AddonState>> {
    let mut store = WasmStore::new(engine, state);

    // Ustaw poczatkowe paliwo — addon zuzywa paliwo z kazdej instrukcji WASM
    store.set_fuel(DEFAULT_FUEL_LIMIT)
        .context("Nie udalo sie ustawic paliwa")?;

    // Ustaw epoch deadline — pozwala na przerywanie z innego watku
    store.epoch_deadline_async_yield_and_update(1);

    info!("Store Wasmtime utworzony (fuel={}, memory_limit={}MB)",
        DEFAULT_FUEL_LIMIT, DEFAULT_MEMORY_LIMIT_BYTES / (1024 * 1024));

    Ok(store)
}

/// Doladowuje paliwo w istniejacym store (np. po wznowieniu operacji)
pub fn refuel_store(store: &mut WasmStore<AddonState>, fuel: u64) -> Result<()> {
    store.set_fuel(fuel)
        .context("Nie udalo sie doladowac paliwa")?;
    Ok(())
}

/// Sprawdza ile paliwa pozostalo w store
pub fn remaining_fuel(store: &WasmStore<AddonState>) -> Result<u64> {
    store.get_fuel()
        .context("Nie udalo sie odczytac poziomu paliwa")
}

// =============================================================================
// Pomocnicze funkcje — dostep do pamieci WASM
// =============================================================================

/// Pobiera obiekt memory z instancji WASM przez Caller
pub fn get_memory(caller: &mut WasmCaller<'_, AddonState>) -> Option<WasmMemory> {
    caller.get_export("memory")?.into_memory()
}

/// Zwraca slice danych z pamieci guest (immutable)
pub fn memory_data<'a, T: 'a>(memory: &WasmMemory, store: &'a impl AsContext<Data = T>) -> &'a [u8] {
    memory.data(store)
}

/// Zwraca mutowalny slice danych z pamieci guest
pub fn memory_data_mut<'a, T: 'a>(memory: &WasmMemory, store: &'a mut impl AsContextMut<Data = T>) -> &'a mut [u8] {
    memory.data_mut(store)
}

/// Tworzy nowy Linker dla silnika (wrapper na Linker::new)
pub fn create_linker(engine: &WasmEngine) -> WasmLinker<AddonState> {
    WasmLinker::new(engine)
}

/// Instancjacja modulu WASM w podanym store
pub fn instantiate(
    linker: &WasmLinker<AddonState>,
    store: &mut WasmStore<AddonState>,
    module: &WasmModule,
) -> Result<WasmInstance> {
    linker.instantiate(store, module)
        .context("Nie udalo sie utworzyc instancji WASM")
}
