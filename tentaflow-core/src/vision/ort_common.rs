// =============================================================================
// File: vision/ort_common.rs — shared ONNX Runtime session plumbing (ort)
// =============================================================================
//
// Wspolna warstwa `ort` dla wszystkich silnikow CV opartych o ONNX Runtime
// (detektor RF-DETR i generyczny runner `onnx-cv`): lokalizacja dylibu
// onnxruntime (native-libs → systemowy fallback) oraz budowa sesji z lancuchem
// execution providerow TensorRT→CUDA→(CoreML)→CPU. Wydzielone z
// `detector_rfdetr.rs`, zeby dynamiczne modele z rejestru `vision_models`
// dzielily dokladnie te sama sciezke wydajnosci.

#![cfg(feature = "inference-supertonic")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use tracing::{info, warn};

/// Domyślna ścieżka biblioteki ONNX Runtime dla dlopen (`ort` z `load-dynamic`),
/// gdy `ORT_DYLIB_PATH` nie jest ustawione w środowisku.
const DEFAULT_ORT_DYLIB: &str = "/usr/lib/libonnxruntime.so.1.24.4";

/// Ustawia `ORT_DYLIB_PATH` na wykrytą ścieżkę, jeśli nie ma jej w środowisku —
/// `ort` z `load-dynamic` dlopuje onnxruntime spod tej zmiennej przy pierwszym
/// użyciu. Preferujemy runtime z drzewa `native-libs/` (zawiera providery
/// TensorRT i CUDA), a dopiero gdy go brak — systemowy [`DEFAULT_ORT_DYLIB`]
/// (który ma zwykle tylko CUDA). Edycja 2021: `set_var` jest bezpieczne.
pub fn ensure_ort_dylib() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    let path = locate_ort_dylib().unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_ORT_DYLIB));
    std::env::set_var("ORT_DYLIB_PATH", &path);
}

/// Szuka `libonnxruntime.{so*,dylib}` w drzewie `native-libs/<platform>/lib-dynamic/`
/// (build-all.sh provisionuje tam runtime GPU z TensorRT). Lustrzana logika do
/// `services::document::rasterize::locate_pdfium_library`, ale zawężona do runtime
/// ONNX. Zwraca pierwszy trafiony plik albo `None` (wtedy caller bierze systemowy).
fn locate_ort_dylib() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    let (platform, lib_glob): (&str, &[&str]) = if cfg!(target_os = "macos") {
        (
            if cfg!(target_arch = "aarch64") { "macos-arm64" } else { "macos-x86_64" },
            &["libonnxruntime.dylib"],
        )
    } else if cfg!(target_os = "linux") {
        (
            if cfg!(target_arch = "aarch64") { "linux-aarch64" } else { "linux-x86_64" },
            // Prebuilty rozpakowują wersjonowany soname (np. .so.1.26.0) obok
            // dowiązania .so — bierzemy oba warianty, wersjonowany jako pierwszy.
            &["libonnxruntime.so", "libonnxruntime.so.*"],
        )
    } else {
        return None;
    };

    // Wspinamy się w górę od CARGO_MANIFEST_DIR / cwd / katalogu binarki aż do
    // katalogu zawierającego `native-libs/`.
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        starts.push(PathBuf::from(manifest));
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        let mut cur: Option<&std::path::Path> = Some(start.as_path());
        while let Some(dir) = cur {
            let lib_dir = dir.join("native-libs").join(platform).join("lib-dynamic");
            if lib_dir.is_dir() {
                if let Some(found) = pick_ort_dylib(&lib_dir, lib_glob) {
                    return Some(found);
                }
            }
            cur = dir.parent();
        }
    }
    None
}

/// Wybiera najlepszy plik runtime ONNX z katalogu `lib-dynamic`. Dla wersjonowanego
/// soname (`libonnxruntime.so.*`) preferuje najświeższą wersję (sort malejący po
/// nazwie), by uniknąć niedeterminizmu gdy leży kilka wariantów.
fn pick_ort_dylib(lib_dir: &std::path::Path, lib_glob: &[&str]) -> Option<std::path::PathBuf> {
    for pattern in lib_glob {
        if let Some(suffix) = pattern.strip_suffix('*') {
            // Wzorzec wersjonowany: dopasuj prefiks, wybierz najświeższy.
            let entries = std::fs::read_dir(lib_dir).ok()?;
            let mut matches: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(suffix) && n != suffix)
                })
                .collect();
            matches.sort();
            if let Some(latest) = matches.pop() {
                return Some(latest);
            }
        } else {
            let candidate = lib_dir.join(pattern);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Explicit TensorRT optimization profile for one dynamic-batch NCHW image
/// input. Without it the TRT EP compiles a new engine for EVERY batch size it
/// has not seen yet (a long stall on the first inference of each size — the
/// B300 killer for variable-batch detectors); with an explicit min/opt/max
/// range TRT builds ONE engine covering the whole batch span up front.
///
/// Changing any field invalidates previously cached engine plans in
/// `trt_cache_dir` — TensorRT detects the profile mismatch and rebuilds once
/// (slow first forward), then serves the new plan from cache again.
#[derive(Debug, Clone)]
pub struct TrtShapeProfile {
    /// Name of the image input tensor in the ONNX graph (never hardcode it —
    /// registry models name inputs arbitrarily; see [`onnx_first_input_name`]).
    pub input_name: String,
    pub min_batch: usize,
    pub opt_batch: usize,
    pub max_batch: usize,
    pub channels: usize,
    pub height: u32,
    pub width: u32,
}

impl TrtShapeProfile {
    /// Renders one `name:NxCxHxW` spec in the format the ONNX Runtime TensorRT
    /// EP expects for `trt_profile_{min,opt,max}_shapes`.
    fn shape_spec(&self, batch: usize) -> String {
        format!(
            "{}:{}x{}x{}x{}",
            self.input_name, batch, self.channels, self.height, self.width
        )
    }
}

/// Hard ceiling on any model's session-pool size, shared by every ort runner
/// (dynamic `onnx-cv` models and the fixed camera-CV engines). Bounds resident
/// VRAM: N pooled sessions = N model copies on the GPU.
pub const MAX_SESSIONS_PER_MODEL: usize = 16;

/// Reads a session-pool size from `env_var`, clamped to `1..=MAX_SESSIONS_PER_MODEL`.
/// Default `1` is byte-identical to a single-`Mutex<Session>` path (checkout
/// always locks slot 0), so the historical serialized behavior is the default.
pub fn pool_size_from_env(env_var: &str, default: usize) -> usize {
    std::env::var(env_var)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, MAX_SESSIONS_PER_MODEL)
}

/// Round-robin cursor over a pool of `len` sessions, separated from the sessions
/// themselves so the wrap-around is unit-testable without ort/GPU.
pub struct RoundRobin {
    next: AtomicUsize,
    len: usize,
}

impl RoundRobin {
    pub fn new(len: usize) -> Self {
        Self {
            next: AtomicUsize::new(0),
            len: len.max(1),
        }
    }

    /// Next slot index, wrapping modulo `len`. `Relaxed` is enough: the cursor
    /// only spreads load, it guards no data (each session has its own Mutex).
    pub fn pick(&self) -> usize {
        self.next.fetch_add(1, Ordering::Relaxed) % self.len
    }
}

/// N independent ort sessions of the SAME model, checked out round-robin so
/// concurrent forwards don't serialize on one `Mutex<Session>`. `Session::run`
/// needs `&mut self`, hence one Mutex per session rather than one shared session.
/// With a single session (pool size 1) `checkout` always locks index 0 —
/// exactly the historical single-Mutex path, zero behavioral change.
///
/// Shared by the dynamic `onnx-cv` registry runner and the fixed camera-CV
/// engines (state classifier, plate OCR) so all ort forwards ride the same
/// concurrency-safe path off the single-threaded Burn/wgpu executor.
///
/// Poison recovery differs by owner. A panic mid-`run` poisons that session's
/// Mutex (its `ort::Session` may hold inconsistent FFI/provider state):
///   * With a `rebuilder` (fixed engines, always the SAME model) `checkout`
///     rebuilds JUST that session in place from the same ONNX on the next use,
///     so one panic never permanently disables the runner — the process-lifetime
///     `OnceCell<Arc<_>>` singleton self-heals without a restart.
///   * Without a rebuilder (`onnx-cv` registry) the pool latches `poisoned` and
///     `checkout` errors "reload required"; `onnx-cv`'s cache evicts + rebuilds
///     the whole entry via its LRU (unchanged behavior).
pub struct SessionPool {
    sessions: Vec<Mutex<ort::session::Session>>,
    cursor: RoundRobin,
    /// Latched only for rebuilder-less pools (`onnx-cv`) — see type docs.
    poisoned: AtomicBool,
    /// Builds a fresh session for slot `i` (its own engine-cache subdir). `Some`
    /// only for fixed engines that always reload the identical model, enabling
    /// in-place per-session recovery instead of a permanent dead pool.
    #[allow(clippy::type_complexity)]
    rebuilder: Option<Box<dyn Fn(usize) -> Result<ort::session::Session> + Send + Sync>>,
}

impl SessionPool {
    pub fn new(sessions: Vec<ort::session::Session>) -> Self {
        let len = sessions.len();
        Self {
            sessions: sessions.into_iter().map(Mutex::new).collect(),
            cursor: RoundRobin::new(len),
            poisoned: AtomicBool::new(false),
            rebuilder: None,
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// Picks the next session round-robin and locks it. If that session is busy
    /// the caller blocks on its Mutex (never busy-loops); other pool sessions
    /// stay free for other callers.
    ///
    /// On a poisoned Mutex (panic mid-`run`): a rebuilder-backed pool rebuilds
    /// that one session in place from the same model and hands out the fresh
    /// guard (self-healing); a rebuilder-less pool latches `poisoned` and errors
    /// so the owner rebuilds the whole entry.
    pub fn checkout(&self) -> Result<std::sync::MutexGuard<'_, ort::session::Session>> {
        // Rebuilder-less pools stay dead once latched; rebuilder-backed pools
        // never latch, so this only short-circuits the `onnx-cv` path.
        if self.rebuilder.is_none() && self.is_poisoned() {
            bail!("ort session pool poisoned by a prior panicked forward — reload required");
        }
        let idx = self.cursor.pick();
        match self.sessions[idx].lock() {
            Ok(guard) => Ok(guard),
            Err(poison) => match &self.rebuilder {
                Some(rebuild) => {
                    // Recover the guard (the mutexed data is still there, just
                    // flagged), swap in a fresh session, clear the flag.
                    let mut guard = poison.into_inner();
                    *guard = rebuild(idx).map_err(|e| {
                        anyhow!("rebuild poisoned ort session slot {idx}: {e}")
                    })?;
                    self.sessions[idx].clear_poison();
                    warn!("[ort] rebuilt poisoned session slot {idx} in place after a panicked forward");
                    Ok(guard)
                }
                None => {
                    self.poisoned.store(true, Ordering::Release);
                    bail!("ort session poisoned by a panicked forward — reload required");
                }
            },
        }
    }
}

/// Builds a pool of `n` independent ort sessions from one on-disk ONNX model.
/// Each session gets its OWN TensorRT engine-cache subdir (`<cache_root>/s{i}`):
/// the lazy (unprofiled) TRT path compiles engines on the FIRST FORWARD, so once
/// sessions run concurrently (n>1) they must never share a cache dir. `n` is
/// clamped to `1..=MAX_SESSIONS_PER_MODEL`.
///
/// The pool carries a rebuilder (same model path + per-slot cache + profile) so
/// a session poisoned by a panicked forward is rebuilt in place on next use —
/// the fixed runner always loads the identical model, so this is safe.
pub fn build_session_pool_from_file(
    model_path: &std::path::Path,
    cache_root: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
    n: usize,
) -> Result<SessionPool> {
    let n = n.clamp(1, MAX_SESSIONS_PER_MODEL);
    let build_slot = {
        let model_path = model_path.to_path_buf();
        let cache_root = cache_root.to_path_buf();
        let trt_profile = trt_profile.cloned();
        move |i: usize| -> Result<ort::session::Session> {
            build_ort_session(&model_path, &cache_root.join(format!("s{i}")), trt_profile.as_ref())
        }
    };
    let mut sessions = Vec::with_capacity(n);
    for i in 0..n {
        sessions.push(build_slot(i)?);
    }
    let len = sessions.len();
    Ok(SessionPool {
        sessions: sessions.into_iter().map(Mutex::new).collect(),
        cursor: RoundRobin::new(len),
        poisoned: AtomicBool::new(false),
        rebuilder: Some(Box::new(build_slot)),
    })
}

/// Buduje sesję `ort` z modelu ONNX, rejestrując łańcuch execution providerów w
/// kolejności priorytetu z MIĘKKĄ rejestracją (bez `error_on_failure`): jeśli dany
/// EP jest niedostępny w załadowanym runtime, `ort` loguje ostrzeżenie i przechodzi
/// do następnego (patrz `ort::ep::apply_execution_providers`). ONNX Runtime sam
/// przydziela węzły grafu do najwyżej-priorytetowego zarejestrowanego EP, więc gdy
/// TensorRT jest obecny — użyje go, a inaczej płynnie zejdzie na CUDA (lub CPU).
///
/// `trt_cache_dir` to per-model katalog engine-cache TensorRT (zserializowane
/// plany silników; pierwszy forward po zmianie modelu/GPU buduje je od nowa).
/// `trt_profile` (opcjonalny) pinuje zakres batcha jednym silnikiem TRT —
/// patrz [`TrtShapeProfile`]; `None` zachowuje leniwe per-shape buildy.
///
/// Kolejność: TensorRT (engine-cache + FP16) → CUDA → [macOS] CoreML → CPU.
pub fn build_ort_session(
    model_path: &std::path::Path,
    trt_cache_dir: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir, trt_profile)?
        .commit_from_file(model_path)
        .map_err(|e| anyhow!("ort commit_from_file {}: {e}", model_path.display()))
}

/// Like [`build_ort_session`] but committing from an in-memory model. Used by
/// the `onnx-cv` runner so the sha256-verified bytes are EXACTLY the bytes the
/// session is built from (no hash-then-reopen TOCTOU window on the file).
pub fn build_ort_session_from_memory(
    model_bytes: &[u8],
    trt_cache_dir: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir, trt_profile)?
        .commit_from_memory(model_bytes)
        .map_err(|e| anyhow!("ort commit_from_memory: {e}"))
}

fn session_builder_with_eps(
    trt_cache_dir: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
) -> Result<ort::session::builder::SessionBuilder> {
    use ort::ep::{ExecutionProvider, ExecutionProviderDispatch};
    use ort::session::Session;

    let mut eps: Vec<ExecutionProviderDispatch> = Vec::new();

    #[cfg(not(target_os = "macos"))]
    {
        // TensorRT — najwyższy priorytet. Engine-cache trzyma zserializowane plany
        // silników na dysku (pierwszy forward po zmianie modelu/GPU buduje je od
        // nowa i jest wolny; kolejne wczytują z cache). FP16 dla przepustowości.
        if let Err(e) = std::fs::create_dir_all(trt_cache_dir) {
            warn!(
                "[ort] nie udało się utworzyć cache TensorRT {}: {e}",
                trt_cache_dir.display()
            );
        }
        let mut trt = ort::ep::TensorRT::default()
            .with_engine_cache(true)
            .with_engine_cache_path(trt_cache_dir.to_string_lossy().to_string())
            .with_timing_cache(true)
            .with_fp16(true);
        // Explicit min/opt/max shape profile (trt_profile_{min,opt,max}_shapes):
        // one engine covers the whole batch range instead of a per-batch-size
        // rebuild stall. An input outside the range makes ORT's TRT EP update
        // the shape ranges and rebuild — same cost as today's lazy path, never
        // a hard error.
        if let Some(profile) = trt_profile {
            trt = trt
                .with_profile_min_shapes(profile.shape_spec(profile.min_batch))
                .with_profile_opt_shapes(profile.shape_spec(profile.opt_batch))
                .with_profile_max_shapes(profile.shape_spec(profile.max_batch));
        }
        eps.push(trt.build());
        // CUDA — dotychczasowa, działająca ścieżka; teraz MIĘKKO (bez
        // error_on_failure), bo poprzedza ją TensorRT.
        eps.push(ort::ep::CUDA::default().build());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (trt_cache_dir, trt_profile);
        // CoreML (Metal/ANE) — akceleracja na Apple Silicon.
        eps.push(ort::ep::CoreML::default().build());
    }
    // CPU — zawsze ostatni fallback.
    eps.push(ort::ep::CPU::default().build());

    // Introspekcja: logujemy które akceleratory widzi załadowany runtime (ort nie
    // raportuje per-węzeł finalnego EP, ale ONNX Runtime bierze najwyżej-priorytetowy
    // z dostępnych, więc to jednoznacznie wskazuje realnie użytą ścieżkę).
    #[cfg(not(target_os = "macos"))]
    {
        let trt = ort::ep::TensorRT::default().is_available().unwrap_or(false);
        let cuda = ort::ep::CUDA::default().is_available().unwrap_or(false);
        info!("[ort] dostępne EP w runtime — TensorRT={trt}, CUDA={cuda} (priorytet: TensorRT>CUDA>CPU)");
    }
    #[cfg(target_os = "macos")]
    {
        let coreml = ort::ep::CoreML::default().is_available().unwrap_or(false);
        info!("[ort] dostępne EP w runtime — CoreML={coreml} (priorytet: CoreML>CPU)");
    }

    Session::builder()
        .map_err(|e| anyhow!("ort Session::builder: {e}"))?
        .with_execution_providers(eps)
        .map_err(|e| anyhow!("ort with_execution_providers: {e}"))
}

/// Extracts the name of the first REAL graph input from raw ONNX model bytes.
///
/// The TRT shape profile has to name the input tensor at session-BUILD time,
/// but `ort` only exposes input names after the session exists — so we read the
/// name straight from the model protobuf with a minimal wire-format scan
/// (`ModelProto.graph`=7, `GraphProto.input`=11 / `initializer`=5,
/// `ValueInfoProto.name`=1, `TensorProto.name`=8). Legacy exports list weights
/// under `graph.input` too, hence inputs that are also initializers are
/// skipped. Skipping over length-delimited fields is O(1) per field, so the
/// scan stays cheap even for multi-hundred-MB models.
///
/// Returns `None` on any malformed/unexpected encoding — callers treat that as
/// "no TRT shape profile" (lazy per-shape engines), never as a load error.
pub fn onnx_first_input_name(model_bytes: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    let mut graph: Option<&[u8]> = None;
    while pos < model_bytes.len() {
        let key = read_varint(model_bytes, &mut pos)?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        if field == 7 && wire == 2 {
            graph = Some(read_len_delimited(model_bytes, &mut pos)?);
            break;
        }
        skip_field(model_bytes, &mut pos, wire)?;
    }
    let graph = graph?;

    let mut inputs: Vec<String> = Vec::new();
    let mut initializers: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pos = 0usize;
    while pos < graph.len() {
        let key = read_varint(graph, &mut pos)?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        match (field, wire) {
            // graph.input: ValueInfoProto (name = field 1)
            (11, 2) => {
                let value_info = read_len_delimited(graph, &mut pos)?;
                if let Some(name) = message_string_field(value_info, 1) {
                    inputs.push(name);
                }
            }
            // graph.initializer: TensorProto (name = field 8)
            (5, 2) => {
                let tensor = read_len_delimited(graph, &mut pos)?;
                if let Some(name) = message_string_field(tensor, 8) {
                    initializers.insert(name);
                }
            }
            _ => skip_field(graph, &mut pos, wire)?,
        }
    }
    inputs.into_iter().find(|name| !initializers.contains(name))
}

/// Reads a base-128 varint at `*pos`, advancing it. `None` on truncation.
fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut out = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        out |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(out);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Reads a length-delimited (wire type 2) payload at `*pos`, advancing past it.
fn read_len_delimited<'a>(buf: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = usize::try_from(read_varint(buf, pos)?).ok()?;
    let end = pos.checked_add(len)?;
    if end > buf.len() {
        return None;
    }
    let out = &buf[*pos..end];
    *pos = end;
    Some(out)
}

/// Skips one field of the given wire type. Groups (wire 3/4) are rejected —
/// ONNX never uses them, and rejecting aborts the scan safely.
fn skip_field(buf: &[u8], pos: &mut usize, wire: u8) -> Option<()> {
    let advance = match wire {
        0 => {
            read_varint(buf, pos)?;
            return Some(());
        }
        1 => 8usize,
        2 => usize::try_from(read_varint(buf, pos)?).ok()?,
        5 => 4usize,
        _ => return None,
    };
    let end = pos.checked_add(advance)?;
    if end > buf.len() {
        return None;
    }
    *pos = end;
    Some(())
}

/// Returns the first string field with number `field_no` from an embedded
/// message, or `None` when absent/malformed.
fn message_string_field(msg: &[u8], field_no: u64) -> Option<String> {
    let mut pos = 0usize;
    while pos < msg.len() {
        let key = read_varint(msg, &mut pos)?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        if field == field_no && wire == 2 {
            let bytes = read_len_delimited(msg, &mut pos)?;
            return String::from_utf8(bytes.to_vec()).ok();
        }
        skip_field(msg, &mut pos, wire)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{onnx_first_input_name, pool_size_from_env, RoundRobin, TrtShapeProfile};

    /// Round-robin cursor: len 1 always yields index 0; len 3 cycles
    /// 0,1,2,0,... — the pool's load-spread contract at any size.
    #[test]
    fn round_robin_wraps_over_len() {
        let rr = RoundRobin::new(1);
        assert_eq!([rr.pick(), rr.pick(), rr.pick()], [0, 0, 0]);
        let rr = RoundRobin::new(3);
        assert_eq!(
            [rr.pick(), rr.pick(), rr.pick(), rr.pick(), rr.pick()],
            [0, 1, 2, 0, 1]
        );
        // A zero-length pool is impossible (builder clamps ≥1), but the cursor
        // must not divide by zero if constructed with 0.
        let rr = RoundRobin::new(0);
        assert_eq!(rr.pick(), 0);
    }

    /// Pool size parses from env and clamps to `1..=MAX`; an unset/garbage var
    /// falls back to the default (never 0).
    #[test]
    fn pool_size_parses_and_clamps() {
        let var = "TENTAFLOW_TEST_POOL_SIZE_KNOB";
        std::env::remove_var(var);
        assert_eq!(pool_size_from_env(var, 1), 1);
        std::env::set_var(var, "4");
        assert_eq!(pool_size_from_env(var, 1), 4);
        std::env::set_var(var, "0");
        assert_eq!(pool_size_from_env(var, 1), 1);
        std::env::set_var(var, "999");
        assert_eq!(pool_size_from_env(var, 1), super::MAX_SESSIONS_PER_MODEL);
        std::env::set_var(var, "not-a-number");
        assert_eq!(pool_size_from_env(var, 2), 2);
        std::env::remove_var(var);
    }

    /// Encodes one length-delimited protobuf field (`field << 3 | 2`).
    fn len_field(field: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut key = field << 3 | 2;
        loop {
            let byte = (key & 0x7f) as u8;
            key >>= 7;
            if key == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        // Test payloads stay < 128 bytes, so the length varint is one byte.
        assert!(payload.len() < 128);
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    /// ModelProto{ graph: Graph{ initializer: Tensor{name:"weight"},
    /// input: [VI{name:"weight"}, VI{name:"pixel_values"}] } } — the first
    /// REAL input skips the initializer-shadowed one.
    #[test]
    fn first_input_skips_initializers() {
        let mut graph = Vec::new();
        graph.extend(len_field(5, &len_field(8, b"weight"))); // initializer
        graph.extend(len_field(11, &len_field(1, b"weight"))); // legacy input
        graph.extend(len_field(11, &len_field(1, b"pixel_values")));
        let model = len_field(7, &graph);
        assert_eq!(
            onnx_first_input_name(&model).as_deref(),
            Some("pixel_values")
        );
    }

    #[test]
    fn malformed_bytes_yield_none() {
        assert_eq!(onnx_first_input_name(&[]), None);
        assert_eq!(onnx_first_input_name(&[0xff, 0xff, 0xff]), None);
        // Graph present but with no inputs.
        let model = len_field(7, &[]);
        assert_eq!(onnx_first_input_name(&model), None);
    }

    #[test]
    fn shape_spec_matches_trt_option_format() {
        let profile = TrtShapeProfile {
            input_name: "input".to_string(),
            min_batch: 1,
            opt_batch: 8,
            max_batch: 8,
            channels: 3,
            height: 560,
            width: 560,
        };
        assert_eq!(profile.shape_spec(1), "input:1x3x560x560");
        assert_eq!(profile.shape_spec(8), "input:8x3x560x560");
    }
}
