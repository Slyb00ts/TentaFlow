// =============================================================================
// Plik: addon/runtime/runtime_wasmtime.rs
// Opis: Backend Wasmtime — uzyty na Desktop i Router (nie-mobilne platformy).
//       Eksportuje ujednolicone type aliasy i funkcje do operacji na WASM.
// =============================================================================

use anyhow::Result;
use tracing::{info, warn};
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

    // WASM stack — wasmtime default = 512 KB. Za malo dla addonow ktore
    // buduja glebokie drzewa `serde_json::Value` (kazdy zagniezdzony Object
    // dodaje stack frame). Zaobserwowane trap'y w TentaVision::on_start gdy
    // pre-renderowane 11 paneli (kazdy z Card→Stack→Grid→Card→...).
    //
    // Wasmtime wymaga `max_wasm_stack <= async_stack_size`. Default
    // async_stack_size = 2 MB, wiec ustawiamy oba: async = 8 MB (gosc rust
    // dostaje pelen budget), max_wasm = 6 MB (zostawiamy 2 MB margines na
    // ramki async runtime'u). Jesli kiedys addony beda potrzebowac wiecej,
    // podbij oba symetrycznie.
    // Wasmtime 44 validates max_wasm_stack <= async_stack_size on Engine
    // creation. Both calls must succeed (panic if feature missing).
    config.async_stack_size(16 * 1024 * 1024);
    config.max_wasm_stack(4 * 1024 * 1024);
    info!("stack config: async_stack=16MB, max_wasm_stack=4MB");

    let engine = WasmEngine::new(&config)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc silnika Wasmtime: {e}"))?;

    // Steady epoch ticker: jeden detached watek bije epoke co EPOCH_TICK_MS.
    // Dzieki temu KAZDE wywolanie ustawia WLASNY wzgledny deadline
    // (`set_epoch_deadline(ticks)`) i trapuje wylacznie po swoim czasie —
    // koniec z dawnym wzorcem "watek-per-call wola increment_epoch raz", ktory
    // trapowal WSZYSTKIE instancje z deadline ≤ current (cross-trap miedzy
    // niezwiazanymi addonami). Ticker zyje do konca procesu (Engine to Arc;
    // klon w watku utrzymuje go przy zyciu — to celowe, jeden silnik = jeden
    // ticker).
    let ticker_engine = engine.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("wasm-epoch-ticker".to_string())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(EPOCH_TICK_MS));
            ticker_engine.increment_epoch();
        })
    {
        warn!("nie udalo sie wystartowac epoch tickera: {e} — timeouty WASM nieaktywne");
    }

    info!("Silnik Wasmtime utworzony (fuel metering + epoch interruption, ticker {EPOCH_TICK_MS}ms)");

    Ok(engine)
}

/// Rozdzielczosc steady epoch tickera (ms). Per-call deadline jest zaokraglany
/// w gore do wielokrotnosci tej wartosci, wiec to dolna granica precyzji timeoutu.
pub const EPOCH_TICK_MS: u64 = 10;

/// Przelicza timeout w ms na liczbe ticków epoki (delta dla `set_epoch_deadline`),
/// zaokraglajac w gore, min 1 — deadline 0 trapilby natychmiast.
pub fn epoch_ticks_for_timeout(timeout_ms: u64) -> u64 {
    timeout_ms.div_ceil(EPOCH_TICK_MS).max(1)
}

/// Ustawia per-call deadline epoki na store. `Some(ms)` → store wytrapuje po
/// ~ms (liczone od teraz, niezaleznie od innych instancji). `None` → "nigdy"
/// (dlugozyjace instancje nie sa trapowane gdy nie maja wlasnego limitu).
pub fn set_call_epoch_deadline(store: &mut WasmStore<AddonState>, timeout_ms: Option<u64>) {
    match timeout_ms {
        Some(ms) => store.set_epoch_deadline(epoch_ticks_for_timeout(ms)),
        None => store.set_epoch_deadline(u64::MAX / 4),
    }
}

/// Czysci per-call deadline (po zakonczeniu wywolania) — store wraca do "nigdy",
/// wiec moze byc bezpiecznie reuzyty / oddany do puli bez ryzyka trapu.
pub fn clear_call_epoch_deadline(store: &mut WasmStore<AddonState>) {
    store.set_epoch_deadline(u64::MAX / 4);
}

// =============================================================================
// Kompilacja modulow WASM
// =============================================================================

/// Kompiluje bajty WASM do modulu Wasmtime z walidacja
pub fn compile_module(engine: &WasmEngine, wasm_bytes: &[u8]) -> Result<WasmModule> {
    let module = WasmModule::new(engine, wasm_bytes)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie skompilowac modulu WASM: {e}"))?;

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

/// Tworzy nowy Store z limitem paliwa i limiterem pamieci.
/// Epoch deadline domyslnie `u64::MAX` — store nigdy nie wytrapuje przez
/// global increment_epoch. Per-call (invoke_block, call_tick_static) ustawia
/// unikalny N przed wywolaniem WASM i watchdog inkrementuje counter do tego
/// konkretnego N. Po call deadline jest przywracany na u64::MAX, dzieki czemu
/// dlugo zyjace instancje (start_addon → on_event handler) nie sa trapowane
/// gdy inny addon ma odpalony watchdog.
pub fn create_store(engine: &WasmEngine, state: AddonState) -> Result<WasmStore<AddonState>> {
    let mut store = WasmStore::new(engine, state);

    store
        .set_fuel(DEFAULT_FUEL_LIMIT)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie ustawic paliwa: {e}"))?;

    // UWAGA: set_epoch_deadline przyjmuje DELTA od current_epoch (wasmtime
    // wewnetrznie robi `current_epoch + delta`). Wartosc u64::MAX powoduje
    // overflow (panic). Uzywamy u64::MAX / 4 — bezpieczna delta ktora nigdy
    // nie zostanie osiagnieta przez normalne incrementy, ale tez nie
    // przepelnia gdy wasmtime dodaje do current_epoch.
    store.set_epoch_deadline(u64::MAX / 4);

    info!(
        "Store Wasmtime utworzony (fuel={}, memory_limit={}MB, epoch_delta=u64::MAX/4)",
        DEFAULT_FUEL_LIMIT,
        DEFAULT_MEMORY_LIMIT_BYTES / (1024 * 1024)
    );

    Ok(store)
}

/// Doladowuje paliwo w istniejacym store (np. po wznowieniu operacji)
pub fn refuel_store(store: &mut WasmStore<AddonState>, fuel: u64) -> Result<()> {
    store
        .set_fuel(fuel)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie doladowac paliwa: {e}"))?;
    Ok(())
}

/// Sprawdza ile paliwa pozostalo w store
pub fn remaining_fuel(store: &WasmStore<AddonState>) -> Result<u64> {
    store
        .get_fuel()
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac poziomu paliwa: {e}"))
}

// =============================================================================
// Pomocnicze funkcje — dostep do pamieci WASM
// =============================================================================

/// Pobiera obiekt memory z instancji WASM przez Caller
pub fn get_memory(caller: &mut WasmCaller<'_, AddonState>) -> Option<WasmMemory> {
    caller.get_export("memory")?.into_memory()
}

/// Zwraca slice danych z pamieci guest (immutable)
pub fn memory_data<'a, T: 'static>(
    memory: &WasmMemory,
    store: &'a impl AsContext<Data = T>,
) -> &'a [u8] {
    memory.data(store)
}

/// Zwraca mutowalny slice danych z pamieci guest
pub fn memory_data_mut<'a, T: 'static>(
    memory: &WasmMemory,
    store: &'a mut impl AsContextMut<Data = T>,
) -> &'a mut [u8] {
    memory.data_mut(store)
}

/// Creates a new linker with WASI preview1 wired in.
///
/// Addons compiled to `wasm32-wasip1` automatically import
/// `wasi_snapshot_preview1` (environ_get, fd_write, proc_exit, random_get)
/// through the Rust stdlib (panic handler, allocator init, getrandom).
/// `wasmtime_wasi::p1::add_to_linker_sync` provides those imports backed by
/// the per-instance `WasiP1Ctx` stored in `AddonState.wasi`.
pub fn create_linker(engine: &WasmEngine) -> WasmLinker<AddonState> {
    let mut linker = WasmLinker::new(engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut AddonState| &mut s.wasi)
        .expect("wire WASI preview1 to wasmtime linker");
    linker
}

/// Instancjacja modulu WASM w podanym store
pub fn instantiate(
    linker: &WasmLinker<AddonState>,
    store: &mut WasmStore<AddonState>,
    module: &WasmModule,
) -> Result<WasmInstance> {
    linker
        .instantiate(store, module)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc instancji WASM: {e}"))
}
