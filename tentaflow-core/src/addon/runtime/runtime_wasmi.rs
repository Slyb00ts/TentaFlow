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

/// Creates a new linker with a minimal `wasi_snapshot_preview1` shim wired
/// in. Addons compiled to `wasm32-wasip1` import a few WASI symbols through
/// the Rust stdlib (panic handler, allocator init, getrandom). Unlike
/// `wasmtime_wasi::p1` on desktop, `wasmi 1.0` does not ship a stable WASI
/// implementation, so we provide the bare minimum needed to instantiate a
/// `wasm32-wasip1` module on iOS/Android.
///
/// Capabilities matched to `wasmtime_wasi::WasiCtxBuilder::new().build_p1()`:
/// - `random_get`: OS RNG (rand::rngs::OsRng) for `getrandom` crate.
/// - `fd_write`: silently discarded (no real stdout/stderr on mobile).
/// - `environ_get` / `environ_sizes_get`: empty environment.
/// - `proc_exit`: traps the WASM execution.
/// - `clock_time_get`: monotonic clock from `std::time::Instant` reference,
///   wall-clock from `std::time::SystemTime`. Required by Rust panic + log.
///
/// Addons access TentaFlow IO through host functions in the `tentaflow`
/// namespace (see `host_functions/`); WASI is only for stdlib internals.
pub fn create_linker(engine: &WasmEngine) -> WasmLinker<AddonState> {
    let mut linker = WasmLinker::new(engine);
    wire_wasi_preview1(&mut linker);
    linker
}

/// Reads a little-endian `u32` from guest memory at `ptr` (32-bit WASM).
fn read_le_u32_at(memory: &WasmMemory, store: &impl AsContext, ptr: i32) -> Option<u32> {
    let data = memory.data(store);
    let off = ptr as usize;
    let bytes = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Writes a little-endian `u32` into guest memory at `ptr`.
fn write_le_u32_at<T: 'static>(
    memory: &WasmMemory,
    store: &mut impl AsContextMut<Data = T>,
    ptr: i32,
    value: u32,
) -> Option<()> {
    let data = memory.data_mut(store);
    let off = ptr as usize;
    let slot = data.get_mut(off..off.checked_add(4)?)?;
    slot.copy_from_slice(&value.to_le_bytes());
    Some(())
}

/// Writes a little-endian `u64` into guest memory at `ptr`.
fn write_le_u64_at<T: 'static>(
    memory: &WasmMemory,
    store: &mut impl AsContextMut<Data = T>,
    ptr: i32,
    value: u64,
) -> Option<()> {
    let data = memory.data_mut(store);
    let off = ptr as usize;
    let slot = data.get_mut(off..off.checked_add(8)?)?;
    slot.copy_from_slice(&value.to_le_bytes());
    Some(())
}

/// WASI errno values used here. Full table in
/// <https://wasix.org/docs/api-reference/wasi/errno>.
const WASI_ERRNO_SUCCESS: i32 = 0;
const WASI_ERRNO_BADF: i32 = 8;
const WASI_ERRNO_INVAL: i32 = 28;

fn wire_wasi_preview1(linker: &mut WasmLinker<AddonState>) {
    // random_get(buf_ptr, buf_len) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: WasmCaller<'_, AddonState>, buf_ptr: i32, buf_len: i32| -> i32 {
                if buf_ptr < 0 || buf_len < 0 {
                    return WASI_ERRNO_INVAL;
                }
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return WASI_ERRNO_INVAL,
                };
                let off = buf_ptr as usize;
                let len = buf_len as usize;
                let data = memory_data_mut(&memory, &mut caller);
                let Some(end) = off.checked_add(len) else {
                    return WASI_ERRNO_INVAL;
                };
                let Some(slot) = data.get_mut(off..end) else {
                    return WASI_ERRNO_INVAL;
                };
                // `getrandom` reads directly from the OS CSPRNG (no rand crate
                // RNG needed) — matches what wasmtime_wasi backs random_get with.
                if getrandom::fill(slot).is_err() {
                    return WASI_ERRNO_INVAL;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .expect("define wasi_snapshot_preview1::random_get");

    // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    // Sink: nothing is written, but we still report the requested byte count
    // so libc / Rust panic / log paths see the message as "delivered".
    // Only stdout (fd=1) and stderr (fd=2) are accepted; any other fd returns
    // EBADF — addons have no preopens, so no real fds exist.
    // Both `iovs_ptr/iovs_len` and each ciovec's `buf_ptr/buf_len` must lie
    // entirely within guest memory; out-of-range or negative inputs return
    // EINVAL (matches WASI preview1 spec).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: WasmCaller<'_, AddonState>,
             fd: i32,
             iovs_ptr: i32,
             iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                if fd != 1 && fd != 2 {
                    return WASI_ERRNO_BADF;
                }
                if iovs_ptr < 0 || iovs_len < 0 || nwritten_ptr < 0 {
                    return WASI_ERRNO_INVAL;
                }
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return WASI_ERRNO_INVAL,
                };
                let mem_size = memory.data(&caller).len();
                // Each ciovec is { buf_ptr: u32, buf_len: u32 } — 8 bytes.
                let iovs_off = iovs_ptr as usize;
                let iovs_count = iovs_len as usize;
                let Some(iovs_bytes) = iovs_count.checked_mul(8) else {
                    return WASI_ERRNO_INVAL;
                };
                let Some(iovs_end) = iovs_off.checked_add(iovs_bytes) else {
                    return WASI_ERRNO_INVAL;
                };
                if iovs_end > mem_size {
                    return WASI_ERRNO_INVAL;
                }
                let mut total: u32 = 0;
                for i in 0..iovs_count {
                    let entry_ptr = (iovs_off + i * 8) as i32;
                    let len_ptr = entry_ptr + 4;
                    let Some(buf_ptr) = read_le_u32_at(&memory, &caller, entry_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let Some(buf_len) = read_le_u32_at(&memory, &caller, len_ptr) else {
                        return WASI_ERRNO_INVAL;
                    };
                    let buf_off = buf_ptr as usize;
                    let Some(buf_end) = buf_off.checked_add(buf_len as usize) else {
                        return WASI_ERRNO_INVAL;
                    };
                    if buf_end > mem_size {
                        return WASI_ERRNO_INVAL;
                    }
                    total = total.saturating_add(buf_len);
                }
                if write_le_u32_at(&memory, &mut caller, nwritten_ptr, total).is_none() {
                    return WASI_ERRNO_INVAL;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .expect("define wasi_snapshot_preview1::fd_write");

    // environ_sizes_get(num_ptr, buf_size_ptr) -> errno (always 0/0).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: WasmCaller<'_, AddonState>, num_ptr: i32, buf_size_ptr: i32| -> i32 {
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return WASI_ERRNO_INVAL,
                };
                if write_le_u32_at(&memory, &mut caller, num_ptr, 0).is_none() {
                    return WASI_ERRNO_INVAL;
                }
                if write_le_u32_at(&memory, &mut caller, buf_size_ptr, 0).is_none() {
                    return WASI_ERRNO_INVAL;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .expect("define wasi_snapshot_preview1::environ_sizes_get");

    // environ_get(environ_ptr_ptr, environ_buf_ptr) -> errno (no-op for empty env).
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_caller: WasmCaller<'_, AddonState>,
             _environ_ptr_ptr: i32,
             _environ_buf_ptr: i32|
             -> i32 { WASI_ERRNO_SUCCESS },
        )
        .expect("define wasi_snapshot_preview1::environ_get");

    // proc_exit(rval) -> ! — issues a wasmi i32_exit trap so the addon
    // instance terminates cleanly without panicking the host process.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_caller: WasmCaller<'_, AddonState>, code: i32| -> Result<(), wasmi::Error> {
                Err(wasmi::Error::i32_exit(code))
            },
        )
        .expect("define wasi_snapshot_preview1::proc_exit");

    // clock_time_get(clock_id, precision, time_ptr) -> errno
    // clock_id 0 = realtime, 1 = monotonic. We answer both with system clocks.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: WasmCaller<'_, AddonState>,
             clock_id: i32,
             _precision: i64,
             time_ptr: i32|
             -> i32 {
                let nanos: u64 = match clock_id {
                    0 => std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0),
                    1 => {
                        // Monotonic — anchor to a process-static Instant so the
                        // clock starts near zero and never goes backwards.
                        use std::sync::OnceLock;
                        static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
                        let epoch = EPOCH.get_or_init(std::time::Instant::now);
                        std::time::Instant::now()
                            .saturating_duration_since(*epoch)
                            .as_nanos() as u64
                    }
                    _ => return WASI_ERRNO_INVAL,
                };
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return WASI_ERRNO_INVAL,
                };
                if write_le_u64_at(&memory, &mut caller, time_ptr, nanos).is_none() {
                    return WASI_ERRNO_INVAL;
                }
                WASI_ERRNO_SUCCESS
            },
        )
        .expect("define wasi_snapshot_preview1::clock_time_get");
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
