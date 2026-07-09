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
use std::sync::{Arc, OnceLock};

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

/// Upper bound on the number of GPUs a single vision pool will spread across.
/// A misconfigured `TENTAFLOW_VISION_GPUS=1000` must not allocate a 1000-element
/// device vector; no real box has more than this many CUDA devices.
pub const MAX_VISION_GPUS: usize = 64;

/// Environment knob selecting which CUDA device ids the vision session pools
/// spread across. See [`vision_gpu_set`] for the grammar.
pub const VISION_GPUS_ENV: &str = "TENTAFLOW_VISION_GPUS";

/// Per-session TensorRT workspace cap in MiB. TensorRT 10.x defaults
/// `trt_max_workspace_size` to 0 = "use ALL available device memory": each
/// unbounded TRT session then reserves as much free VRAM as it can, so two
/// pooled sessions already eat ~18 GB and a third won't fit on a 24 GB card —
/// the session pool is unusable at N>1 without a cap. Overridable via
/// [`TRT_WORKSPACE_MB_ENV`].
pub const DEFAULT_TRT_WORKSPACE_MB: usize = 1024;

/// Environment knob for the per-session TensorRT workspace cap (MiB). Clamped to
/// [`TRT_WORKSPACE_MB_MIN`]..=[`TRT_WORKSPACE_MB_MAX`]; unset/garbage →
/// [`DEFAULT_TRT_WORKSPACE_MB`].
pub const TRT_WORKSPACE_MB_ENV: &str = "TENTAFLOW_TRT_WORKSPACE_MB";
pub const TRT_WORKSPACE_MB_MIN: usize = 128;
pub const TRT_WORKSPACE_MB_MAX: usize = 8192;

/// Per-session TensorRT workspace cap in BYTES, resolved ONCE for the process
/// lifetime from [`TRT_WORKSPACE_MB_ENV`]. See [`DEFAULT_TRT_WORKSPACE_MB`] for
/// why an explicit cap is mandatory for the pool to scale past one session.
pub fn trt_workspace_bytes() -> usize {
    static BYTES: OnceLock<usize> = OnceLock::new();
    *BYTES.get_or_init(|| {
        parse_trt_workspace_mb(std::env::var(TRT_WORKSPACE_MB_ENV).ok().as_deref())
            * 1024
            * 1024
    })
}

/// Pure parser behind [`trt_workspace_bytes`], split out for unit-testing the
/// clamp/default without touching the process environment or the `OnceLock`.
fn parse_trt_workspace_mb(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_TRT_WORKSPACE_MB)
        .clamp(TRT_WORKSPACE_MB_MIN, TRT_WORKSPACE_MB_MAX)
}

/// Parsed CUDA device-id set that vision session pools spread across, resolved
/// ONCE for the process lifetime from `TENTAFLOW_VISION_GPUS`.
///
/// Grammar (single-GPU device 0 is always the safe default — never panics):
///   * unset / empty / whitespace → `[0]` (today's single-GPU behavior).
///   * a bare count `N` (e.g. `"2"`) → devices `[0, 1, … N-1]`.
///   * an explicit comma list (e.g. `"0,2,3"`) → exactly those device ids,
///     de-duplicated with order preserved.
///   * anything unparseable / a non-positive count → `[0]`.
///
/// There is deliberately no CUDA auto-detection here: adding an nvml dependency
/// just to count GPUs is not worth it, and the mesh `NodeInfo.gpus` count is not
/// reachable from this low-level ort layer. Multi-GPU is opt-in by the operator
/// setting this var; the default stays single-device-0 and byte-identical.
pub fn vision_gpu_set() -> &'static [i32] {
    VISION_GPU_SET
        .get_or_init(|| parse_gpu_set(std::env::var(VISION_GPUS_ENV).ok().as_deref()))
        .as_slice()
}

/// Process-lifetime slot behind [`vision_gpu_set`], hoisted to module level so
/// [`init_vision_gpu_set`] can seed it programmatically before the first read.
static VISION_GPU_SET: OnceLock<Vec<i32>> = OnceLock::new();

/// Programmatic pin of the vision GPU set — for processes that receive their
/// device assignment explicitly instead of via the environment (the
/// `vision-worker` subprocess gets `--gpu <id>` from the spawning supervisor).
/// Must run before ANY vision singleton resolves [`vision_gpu_set`]; once the
/// set is frozen this returns an error so a late pin fails loudly instead of
/// silently building sessions on the wrong device.
pub fn init_vision_gpu_set(ids: &[i32]) -> anyhow::Result<()> {
    let mut set: Vec<i32> = Vec::new();
    for &id in ids {
        if id >= 0 && !set.contains(&id) && set.len() < MAX_VISION_GPUS {
            set.push(id);
        }
    }
    if set.is_empty() {
        anyhow::bail!("init_vision_gpu_set: no valid CUDA device ids in {ids:?}");
    }
    VISION_GPU_SET.set(set).map_err(|_| {
        anyhow::anyhow!(
            "vision GPU set already resolved to {:?} — init_vision_gpu_set must run before any \
             vision singleton loads",
            VISION_GPU_SET.get().map(Vec::as_slice).unwrap_or(&[])
        )
    })
}

/// Pure parser behind [`vision_gpu_set`], split out so the grammar is unit-testable
/// without touching the process environment or the `OnceLock`.
fn parse_gpu_set(raw: Option<&str>) -> Vec<i32> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return vec![0];
    }
    // No comma → a bare device COUNT (count semantics, per the documented grammar):
    // "2" means "use two GPUs, devices 0 and 1", NOT "use device 2".
    if !raw.contains(',') {
        return match raw.parse::<i32>() {
            Ok(count) if count >= 1 => {
                let count = (count as usize).min(MAX_VISION_GPUS);
                (0..count as i32).collect()
            }
            _ => vec![0],
        };
    }
    // Comma list → explicit device ids, de-duplicated, order preserved, garbage
    // and negatives dropped.
    let mut ids: Vec<i32> = Vec::new();
    for tok in raw.split(',') {
        if let Ok(id) = tok.trim().parse::<i32>() {
            if id >= 0 && !ids.contains(&id) && ids.len() < MAX_VISION_GPUS {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        vec![0]
    } else {
        ids
    }
}

/// Per-(device, session) TRT engine-cache subdir name. TRT engine plans are
/// GPU-specific, so a session on device `d` must never reuse a plan compiled for
/// another device. Device 0 keeps the historical `s<i>` name — single-GPU is the
/// default, so every existing on-disk cache stays valid (no forced rebuild for
/// current deployments, including the live instance). Non-zero devices get a
/// `d<device>_s<i>` name so multi-GPU plans never collide in one `trt-cache` root.
pub fn session_cache_subdir(device_id: i32, session_idx: usize) -> String {
    if device_id == 0 {
        format!("s{session_idx}")
    } else {
        format!("d{device_id}_s{session_idx}")
    }
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

/// A single type-erased forward submitted to a session's dedicated thread. The
/// closure runs `Session::run` (plus tensor extraction) on that thread and sends
/// its own owned result back; it returns `true` iff the forward errored, so the
/// worker knows whether to rebuild the session (its FFI/provider state may be
/// inconsistent after a failure). Generic over the session type `S` purely so the
/// worker machinery is unit-testable against a fake session without a GPU.
type Job<S> = Box<dyn FnOnce(&mut S) -> bool + Send + 'static>;

/// Rebuilds a fresh session for slot `i` (its own engine-cache subdir). `Some`
/// only for fixed engines that always reload the identical model, enabling
/// in-place per-session recovery instead of a permanent dead slot.
type Rebuilder<S> = Arc<dyn Fn(usize) -> Result<S> + Send + Sync>;

/// One pooled session bound to its OWN dedicated OS thread. The session is MOVED
/// into that thread and never shared — every `Session::run` for this slot
/// executes on this one thread, so ORT's CUDA execution provider caches its
/// per-OS-thread device resources (streams/handles) for exactly N threads total
/// instead of accumulating a fresh set on every `spawn_blocking` worker that ever
/// touched the session (the unbounded-VRAM leak this design fixes).
struct Worker<S> {
    /// MPMC job queue drained by the dedicated thread. `crossbeam` so the Sender
    /// is `Sync` (round-robin can route two concurrent callers to one slot).
    tx: crossbeam_channel::Sender<Job<S>>,
    /// Joined on pool drop (see `WorkerPool::drop`) so the thread — and the
    /// `ort::Session` it owns — is fully gone before any replacement pool builds.
    handle: std::thread::JoinHandle<()>,
}

/// N sessions of the SAME model, each pinned to its OWN dedicated OS thread and
/// fed forwards round-robin. Generic over `S` so the dedicated-thread mechanics
/// (ownership pinning, self-heal, poison latch) are unit-testable against a fake
/// session; the production alias [`SessionPool`] fixes `S = ort::session::Session`.
struct WorkerPool<S> {
    workers: Vec<Worker<S>>,
    cursor: RoundRobin,
    /// Latched only for rebuilder-less pools (`onnx-cv`) — see [`SessionPool`].
    poisoned: Arc<AtomicBool>,
}

impl<S: Send + 'static> WorkerPool<S> {
    /// Spawns one dedicated thread per session. `rebuilder` is `Some` for the
    /// fixed engines (self-heal in place) and `None` for `onnx-cv` (LRU rebuilds
    /// the whole entry on poison).
    fn from_sessions(label: &str, sessions: Vec<S>, rebuilder: Option<Rebuilder<S>>) -> Self {
        let len = sessions.len();
        let poisoned = Arc::new(AtomicBool::new(false));
        let workers = sessions
            .into_iter()
            .enumerate()
            .map(|(i, sess)| {
                spawn_worker(label, i, sess, rebuilder.clone(), Arc::clone(&poisoned))
            })
            .collect();
        Self {
            workers,
            cursor: RoundRobin::new(len),
            poisoned,
        }
    }

    fn len(&self) -> usize {
        self.workers.len()
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// Runs `f` on the dedicated thread owning the next round-robin session,
    /// blocking on the owned result. The forward executes ONLY on that thread;
    /// the caller merely submits + waits, so no per-thread CUDA resources
    /// accumulate on the (arbitrary) caller thread.
    fn run<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut S) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        if self.is_poisoned() {
            bail!("ort session pool poisoned by a prior panicked forward — reload required");
        }
        let idx = self.cursor.pick();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<Result<R>>();
        let job: Job<S> = Box::new(move |session| {
            let result = f(session);
            let errored = result.is_err();
            // Receiver may already be gone (caller dropped its future); the
            // forward still ran, so just discard the send in that case.
            let _ = resp_tx.send(result);
            errored
        });
        self.workers[idx]
            .tx
            .send(job)
            .map_err(|_| anyhow!("ort session worker slot {idx} is gone (pool shutting down)"))?;
        // Blocking `recv` on this (possibly `spawn_blocking`) thread is fine — the
        // Session::run cost is paid on the worker thread, not here.
        resp_rx
            .recv()
            .map_err(|_| anyhow!("ort session worker slot {idx} died before responding"))?
    }
}

/// Spawns the dedicated thread that exclusively owns and runs `session`. The
/// session is moved in and never shared, so ORT caches its per-thread CUDA
/// resources for this one thread only.
fn spawn_worker<S: Send + 'static>(
    label: &str,
    idx: usize,
    session: S,
    rebuilder: Option<Rebuilder<S>>,
    poisoned: Arc<AtomicBool>,
) -> Worker<S> {
    let (tx, rx) = crossbeam_channel::unbounded::<Job<S>>();
    let name = format!("vision-ort-{label}-{idx}");
    let handle = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || worker_loop(idx, rx, session, rebuilder, poisoned))
        .unwrap_or_else(|e| panic!("spawn dedicated ort session thread {name}: {e}"));
    Worker { tx, handle }
}

/// Joins every worker thread on pool drop so the old sessions are fully released
/// BEFORE the caller builds a replacement pool. Without the join the dropped
/// `Sender`s only *eventually* let idle workers exit, so an LRU eviction / poison
/// reload could start building fresh sessions while the old worker threads still
/// hold their `ort::Session`s → transient double-VRAM residency → OOM. Senders are
/// closed FIRST (so every `recv()` disconnects and each loop exits), only THEN do
/// we join — joining before closing would deadlock on a still-listening worker.
impl<S> Drop for WorkerPool<S> {
    fn drop(&mut self) {
        let workers = std::mem::take(&mut self.workers);
        let mut handles = Vec::with_capacity(workers.len());
        for Worker { tx, handle } in workers {
            drop(tx);
            handles.push(handle);
        }
        for handle in handles {
            let _ = handle.join();
        }
    }
}

/// The dedicated thread's loop: drain jobs, run each on the owned session, and
/// self-heal on failure. `catch_unwind` keeps a panicked forward from killing the
/// thread (one panic must never permanently disable a slot); a panic or a forward
/// error rebuilds the session in place (rebuilder-backed) or latches the pool
/// poisoned (rebuilder-less, `onnx-cv`).
fn worker_loop<S>(
    idx: usize,
    rx: crossbeam_channel::Receiver<Job<S>>,
    initial: S,
    rebuilder: Option<Rebuilder<S>>,
    poisoned: Arc<AtomicBool>,
) {
    let mut session: Option<S> = Some(initial);
    while let Ok(job) = rx.recv() {
        // Lazily rebuild after a prior poison (rebuilder-backed pools only).
        if session.is_none() {
            match &rebuilder {
                Some(rebuild) => match rebuild(idx) {
                    Ok(fresh) => {
                        warn!("[ort] rebuilt session slot {idx} in place after a prior failed forward");
                        session = Some(fresh);
                    }
                    Err(e) => {
                        // Drop the job (caller's recv errors) and retry the
                        // rebuild on the next submission — never a dead slot.
                        warn!("[ort] session slot {idx} rebuild failed ({e:#}); retrying on next forward");
                        continue;
                    }
                },
                // Rebuilder-less: the slot is poisoned; drop the job so the
                // caller errors (the pool already latched `poisoned`).
                None => continue,
            }
        }
        let sess = session.as_mut().expect("session present after rebuild");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(sess))) {
            // Forward succeeded — keep the session.
            Ok(false) => {}
            // Forward returned Err. A rebuilder-backed engine rebuilds the session
            // (its provider state may be inconsistent); a rebuilder-less pool
            // keeps it (a shape/registry error does not corrupt the runtime).
            Ok(true) => {
                if rebuilder.is_some() {
                    session = None;
                }
            }
            // Forward PANICKED mid-run; the response sender was dropped inside the
            // job so the caller already sees an error. Either way DROP the session
            // (its FFI/provider state may be torn) so no already-queued job runs on
            // it: a rebuilder-backed slot rebuilds a fresh one on the next job; a
            // rebuilder-less pool also latches `poisoned` so `run` short-circuits
            // and every queued job hits the `None`-rebuilder disconnect (the owner
            // then reloads the whole entry via its LRU).
            Err(_) => {
                session = None;
                if rebuilder.is_some() {
                    warn!("[ort] session slot {idx} forward panicked — rebuilding session in place");
                } else {
                    warn!("[ort] session slot {idx} forward panicked — pool poisoned (reload required)");
                    poisoned.store(true, Ordering::Release);
                }
            }
        }
    }
}

/// N independent ort sessions of the SAME model, each pinned to its OWN dedicated
/// OS thread and fed forwards round-robin (see [`WorkerPool`]). `Session::run`
/// needs `&mut self` and ORT caches CUDA resources PER OS THREAD, so instead of
/// sharing a session across arbitrary `spawn_blocking` threads (which leaks
/// resource sets ~O(threads × sessions) until OOM) each session lives on and runs
/// on exactly one thread. N threads = N concurrent forwards — the parallelism
/// goal is preserved, the per-thread resource sets are bounded to N.
///
/// Shared by the dynamic `onnx-cv` registry runner and the fixed camera-CV
/// engines (state classifier, plate OCR) so all ort forwards ride the same
/// concurrency-safe path off the single-threaded Burn/wgpu executor.
///
/// Poison recovery differs by owner. A forward that errors or panics may leave
/// its `ort::Session` in inconsistent FFI/provider state:
///   * With a `rebuilder` (fixed engines, always the SAME model) the owning
///     thread rebuilds JUST its session in place from the same ONNX before the
///     next job, so one panic never permanently disables the slot — the
///     process-lifetime `OnceCell<Arc<_>>` singleton self-heals without a restart.
///   * Without a rebuilder (`onnx-cv` registry) a panicked forward latches
///     `poisoned` and `run` errors "reload required"; `onnx-cv`'s cache evicts +
///     rebuilds the whole entry via its LRU (unchanged behavior).
pub struct SessionPool {
    inner: WorkerPool<ort::session::Session>,
}

impl SessionPool {
    /// Builds a pool from already-constructed sessions with NO rebuilder (the
    /// `onnx-cv` path, whose LRU rebuilds the whole entry on poison). `label`
    /// names the dedicated threads (`vision-ort-<label>-<i>`).
    pub fn new(label: &str, sessions: Vec<ort::session::Session>) -> Self {
        Self {
            inner: WorkerPool::from_sessions(label, sessions, None),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }

    /// Runs `f` on the dedicated thread owning the next round-robin session.
    ///
    /// `f` receives `&mut Session`, runs the forward and extracts everything it
    /// needs into an OWNED, `Send` value — that value (not the borrowed session
    /// outputs) is what crosses back to the caller. The forward therefore
    /// executes ONLY on the session's dedicated thread; the calling thread merely
    /// submits the job and blocks on the response, so no per-thread CUDA
    /// resources accumulate on the (arbitrary) caller thread. N sessions on N
    /// threads still give N concurrent forwards.
    pub fn run<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut ort::session::Session) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.inner.run(f)
    }
}

/// Builds a pool of `n` independent ort sessions from one on-disk ONNX model,
/// spread round-robin across the configured GPU set ([`vision_gpu_set`]): session
/// `i` lands on device `gpus[i % gpus.len()]`, so `n=8` over 2 GPUs gives 4
/// sessions per GPU. VRAM is budgeted PER DEVICE (each session is a full model
/// copy on its own GPU). With the default single-GPU set (`[0]`) this is the
/// historical behavior unchanged.
///
/// Each session gets its OWN TensorRT engine-cache subdir
/// ([`session_cache_subdir`] → `<cache_root>/s{i}` on device 0,
/// `<cache_root>/d{dev}_s{i}` elsewhere): the lazy (unprofiled) TRT path compiles
/// engines on the FIRST FORWARD, so once sessions run concurrently (n>1) they
/// must never share a cache dir, and a device-`d` session must never reuse a
/// plan built for another device. `n` is clamped to `1..=MAX_SESSIONS_PER_MODEL`.
///
/// The pool carries a rebuilder (same model path + per-(device,slot) cache +
/// profile) so a session poisoned by a panicked forward is rebuilt in place on
/// next use, on the SAME device — the fixed runner always loads the identical
/// model, so this is safe.
pub fn build_session_pool_from_file(
    model_path: &std::path::Path,
    cache_root: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
    n: usize,
    fp16: bool,
) -> Result<SessionPool> {
    let n = n.clamp(1, MAX_SESSIONS_PER_MODEL);
    // Self-heal an ONNX external-data name mismatch before ANY session opens the
    // model (see `ensure_external_data_present`): the training-service export can
    // bake a sibling name (`model.onnx.data`) that differs from the distributed
    // on-disk file (`model_stan.onnx.data`), which would make `commit_from_file`
    // fail on a fresh deploy. No-op for self-contained (inline-weight) models.
    ensure_external_data_present(model_path)?;
    let gpus: Vec<i32> = vision_gpu_set().to_vec();
    let build_slot = {
        let model_path = model_path.to_path_buf();
        let cache_root = cache_root.to_path_buf();
        let trt_profile = trt_profile.cloned();
        move |i: usize| -> Result<ort::session::Session> {
            let device_id = gpus[i % gpus.len()];
            let subdir = session_cache_subdir(device_id, i);
            build_ort_session(
                &model_path,
                &cache_root.join(subdir),
                trt_profile.as_ref(),
                device_id,
                fp16,
            )
            .map_err(|e| anyhow!("session slot {i} on GPU device {device_id}: {e:#}"))
        }
    };
    // Degrade, never disable: a TRT engine build can fail transiently on a busy
    // GPU (flaky builder under memory pressure). If slot 0 fails nothing can run —
    // hard error. But a failure PAST slot 0 must not take the whole model (and with
    // it camera analysis) down: keep the sessions that DID build and warn loudly.
    // This exact failure (slot 2/4 on a crowded device) once disabled production
    // analysis entirely while the video kept flowing.
    let mut sessions = Vec::with_capacity(n);
    for i in 0..n {
        match build_slot(i) {
            Ok(s) => sessions.push(s),
            Err(e) if i == 0 => return Err(e),
            Err(e) => {
                tracing::warn!(
                    "[ort] session slot {i}/{n} build failed — continuing with {} session(s): {e:#}",
                    sessions.len()
                );
                break;
            }
        }
    }
    // Name the dedicated threads after the model file stem (`vision-ort-<stem>-<i>`).
    let label = model_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model")
        .to_string();
    let rebuilder: Rebuilder<ort::session::Session> = Arc::new(build_slot);
    Ok(SessionPool {
        inner: WorkerPool::from_sessions(&label, sessions, Some(rebuilder)),
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
/// `device_id` pins BOTH the TensorRT and CUDA EPs to the same CUDA device
/// (default `0` = today's single-GPU path).
///
/// Kolejność: TensorRT (engine-cache + FP16) → CUDA → [macOS] CoreML → CPU.
pub fn build_ort_session(
    model_path: &std::path::Path,
    trt_cache_dir: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
    device_id: i32,
    fp16: bool,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir, trt_profile, device_id, fp16)?
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
    device_id: i32,
    fp16: bool,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir, trt_profile, device_id, fp16)?
        .commit_from_memory(model_bytes)
        .map_err(|e| anyhow!("ort commit_from_memory: {e}"))
}

/// Whether the small OCR / classifier CRNN heads run in TensorRT FP16. Default
/// FALSE (fp32) — fp16 rounding corrupts character reads. `TENTAFLOW_VISION_OCR_FP16=1`
/// forces fp16 back on for A/B comparison of accuracy vs speed on a live feed.
pub fn ocr_fp16() -> bool {
    std::env::var("TENTAFLOW_VISION_OCR_FP16")
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

fn session_builder_with_eps(
    trt_cache_dir: &std::path::Path,
    trt_profile: Option<&TrtShapeProfile>,
    device_id: i32,
    fp16: bool,
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
            .with_device_id(device_id)
            // Cap the workspace: the TRT 10.x default (0) grabs ALL free VRAM per
            // session, so an uncapped pool over-allocates and N>1 sessions won't
            // co-reside. 1 GiB is ample scratch for these detector/CV graphs; see
            // [`trt_workspace_bytes`].
            .with_max_workspace_size(trt_workspace_bytes())
            .with_engine_cache(true)
            .with_engine_cache_path(trt_cache_dir.to_string_lossy().to_string())
            .with_timing_cache(true)
            // FP16 boosts detector throughput and it tolerates the precision loss
            // (localization is robust). Small CRNN OCR heads do NOT: fp16 rounding
            // flips argmax on ambiguous glyphs (3↔2, 8↔0), silently corrupting reads
            // — so OCR/classifier sessions pass `fp16=false`. See `ocr_fp16`.
            .with_fp16(fp16);
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
        // CUDA Graphs (opt-in): capture the whole TRT forward once and replay it,
        // collapsing hundreds of per-forward kernel launches into one graph launch.
        // Targets the measured launch-bound detect plateau (~1300 forwards*batch/s
        // regardless of session count). Opt-in because graph capture requires
        // stable shapes per session — mixed batch sizes re-capture and can regress;
        // enable when the batcher feeds mostly-full fixed batches.
        if std::env::var("TENTAFLOW_VISION_TRT_CUDA_GRAPH").is_ok_and(|v| v.trim() == "1") {
            trt = trt.with_cuda_graph(true);
        }
        eps.push(trt.build());
        // CUDA — dotychczasowa, działająca ścieżka; teraz MIĘKKO (bez
        // error_on_failure), bo poprzedza ją TensorRT. Ten sam `device_id` co TRT,
        // żeby fallback TRT→CUDA został na tej samej karcie.
        eps.push(ort::ep::CUDA::default().with_device_id(device_id).build());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (trt_cache_dir, trt_profile, device_id);
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

    // Anti-spin: each pooled session lives on its OWN dedicated OS thread and runs
    // one forward at a time, and cross-session parallelism comes from N sessions —
    // NOT from ORT's per-session thread pools. By default those pools BUSY-SPIN
    // waiting for work/GPU sync; with 30 sessions that pins every core spinning
    // (perf: 82 % of CPU in one libonnxruntime spin loop) while the GPU sits at 0 %.
    // One intra/inter thread + spinning OFF makes the CPU SLEEP on GPU sync instead
    // of burning a core, so the card can actually be fed → many more cameras/GPU.
    Session::builder()
        .map_err(|e| anyhow!("ort Session::builder: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow!("ort with_intra_threads: {e}"))?
        .with_inter_threads(1)
        .map_err(|e| anyhow!("ort with_inter_threads: {e}"))?
        .with_intra_op_spinning(false)
        .map_err(|e| anyhow!("ort intra_op_spinning: {e}"))?
        .with_inter_op_spinning(false)
        .map_err(|e| anyhow!("ort inter_op_spinning: {e}"))?
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

/// Collects the external-data `location` filenames a model's tensors reference.
///
/// ONNX external weights live in a sibling file whose name is baked into the
/// graph: `ModelProto.graph`=7 → `GraphProto.initializer`=5 (TensorProto) →
/// `TensorProto.external_data`=13 (StringStringEntryProto: key=1, value=2); the
/// entry with key `"location"` names the file the runtime opens next to the
/// `.onnx`. De-duplicated, order preserved. Empty for a self-contained model
/// (inline weights) or any malformed encoding — the scan never errors.
pub fn onnx_external_data_locations(model_bytes: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    let mut graph: Option<&[u8]> = None;
    while pos < model_bytes.len() {
        let key = match read_varint(model_bytes, &mut pos) {
            Some(k) => k,
            None => return out,
        };
        let (field, wire) = (key >> 3, (key & 7) as u8);
        if field == 7 && wire == 2 {
            graph = read_len_delimited(model_bytes, &mut pos);
            break;
        }
        if skip_field(model_bytes, &mut pos, wire).is_none() {
            return out;
        }
    }
    let graph = match graph {
        Some(g) => g,
        None => return out,
    };

    let mut pos = 0usize;
    while pos < graph.len() {
        let key = match read_varint(graph, &mut pos) {
            Some(k) => k,
            None => return out,
        };
        let (field, wire) = (key >> 3, (key & 7) as u8);
        if field == 5 && wire == 2 {
            match read_len_delimited(graph, &mut pos) {
                Some(tensor) => collect_tensor_external_locations(tensor, &mut out),
                None => return out,
            }
        } else if skip_field(graph, &mut pos, wire).is_none() {
            return out;
        }
    }
    out
}

/// Scans one `TensorProto` for `external_data`=13 StringStringEntryProtos and
/// appends every `location` value (deduped) to `out`.
fn collect_tensor_external_locations(tensor: &[u8], out: &mut Vec<String>) {
    let mut pos = 0usize;
    while pos < tensor.len() {
        let key = match read_varint(tensor, &mut pos) {
            Some(k) => k,
            None => return,
        };
        let (field, wire) = (key >> 3, (key & 7) as u8);
        if field == 13 && wire == 2 {
            match read_len_delimited(tensor, &mut pos) {
                Some(entry) => {
                    if message_string_field(entry, 1).as_deref() == Some("location") {
                        if let Some(v) = message_string_field(entry, 2) {
                            if !v.is_empty() && !out.contains(&v) {
                                out.push(v);
                            }
                        }
                    }
                }
                None => return,
            }
        } else if skip_field(tensor, &mut pos, wire).is_none() {
            return;
        }
    }
}

/// Guarantees every external-data file an ONNX model references exists next to
/// it BEFORE the ort session opens the model, self-healing an export/on-disk
/// name mismatch.
///
/// Training-service exports bake the ORIGINAL sibling name into the graph (e.g.
/// `model.onnx.data`), but the distributed on-disk sidecar is named after the
/// model stem (`model_stan.onnx.data`); ort's `commit_from_file` then fails with
/// "External data path does not exist". For each referenced `location` that is
/// missing, this hardlinks (falling back to a byte copy) the real
/// `<model-filename>.data` sidecar to the referenced name in the same dir, so a
/// fresh bundle deploy loads with no manual intervention regardless of what the
/// export named the reference. A self-contained model (inline weights) has no
/// locations and is a no-op.
pub fn ensure_external_data_present(model_path: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(model_path)
        .map_err(|e| anyhow!("read onnx {}: {e}", model_path.display()))?;
    let locations = onnx_external_data_locations(&bytes);
    if locations.is_empty() {
        return Ok(());
    }
    let dir = model_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    // The real, on-disk weights sidecar the bundle distributes is named after
    // the model file itself (`<model>.onnx` → `<model>.onnx.data`).
    let file_name = model_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("onnx path {} has no file name", model_path.display()))?;
    let stem_data = dir.join(format!("{file_name}.data"));

    for loc in locations {
        // Exports emit a bare filename; reject any separator/traversal so a
        // crafted model can never point the loader outside the model directory.
        if loc.contains('/') || loc.contains('\\') || loc.contains("..") {
            bail!("onnx external-data location {loc:?} is not a bare sibling filename");
        }
        let target = dir.join(&loc);
        if target.exists() {
            continue;
        }
        if !stem_data.exists() {
            bail!(
                "onnx {} references external data {loc:?} but neither it nor the stem sidecar {} exists",
                model_path.display(),
                stem_data.display()
            );
        }
        // Hardlink is zero-copy on the same fs; fall back to a byte copy across
        // devices or when linking is unsupported.
        if let Err(link_err) = std::fs::hard_link(&stem_data, &target) {
            std::fs::copy(&stem_data, &target).map_err(|copy_err| {
                anyhow!(
                    "provision external data {} from {}: hardlink failed ({link_err}), copy failed ({copy_err})",
                    target.display(),
                    stem_data.display()
                )
            })?;
        }
        warn!(
            "[ort] provisioned missing external-data sidecar {} from {} (export named the reference differently than the on-disk file)",
            target.display(),
            stem_data.display()
        );
    }
    Ok(())
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
    use super::{
        ensure_external_data_present, onnx_external_data_locations, onnx_first_input_name,
        parse_gpu_set, parse_trt_workspace_mb, pool_size_from_env, session_cache_subdir, Rebuilder,
        RoundRobin, TrtShapeProfile, WorkerPool,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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

    /// TRT workspace cap: default when unset/garbage, clamped to the MiB range.
    #[test]
    fn trt_workspace_mb_parses_and_clamps() {
        use super::{DEFAULT_TRT_WORKSPACE_MB, TRT_WORKSPACE_MB_MAX, TRT_WORKSPACE_MB_MIN};
        assert_eq!(parse_trt_workspace_mb(None), DEFAULT_TRT_WORKSPACE_MB);
        assert_eq!(parse_trt_workspace_mb(Some("")), DEFAULT_TRT_WORKSPACE_MB);
        assert_eq!(parse_trt_workspace_mb(Some("garbage")), DEFAULT_TRT_WORKSPACE_MB);
        assert_eq!(parse_trt_workspace_mb(Some(" 2048 ")), 2048);
        // Clamp below min and above max.
        assert_eq!(parse_trt_workspace_mb(Some("1")), TRT_WORKSPACE_MB_MIN);
        assert_eq!(parse_trt_workspace_mb(Some("999999")), TRT_WORKSPACE_MB_MAX);
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

    /// ModelProto{ graph: Graph{ initializer: Tensor{ external_data:
    /// [Entry{key:"location", value:"model.onnx.data"}] } } } — the parser
    /// surfaces the referenced sidecar name (which may differ from the model's
    /// own filename). A model with no external_data yields an empty list.
    #[test]
    fn external_data_locations_read_from_proto() {
        let entry = {
            let mut e = len_field(1, b"location");
            e.extend(len_field(2, b"model.onnx.data"));
            e
        };
        let tensor = len_field(13, &entry);
        let graph = len_field(5, &tensor);
        let model = len_field(7, &graph);
        assert_eq!(
            onnx_external_data_locations(&model),
            vec!["model.onnx.data".to_string()]
        );
        // Self-contained model (graph with only a plain input, no external_data).
        let inline = len_field(7, &len_field(11, &len_field(1, b"input")));
        assert!(onnx_external_data_locations(&inline).is_empty());
    }

    /// Loader-side guarantee: a `model_stan.onnx` whose external ref
    /// (`model.onnx.data`) differs from its on-disk sidecar
    /// (`model_stan.onnx.data`) gets the referenced name provisioned next to it,
    /// so `commit_from_file` finds the weights. This is exactly the fresh-deploy
    /// mismatch that broke the classifier on the ort path.
    #[test]
    fn ensure_external_data_provisions_mismatched_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "tf_ort_extdata_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Minimal ONNX referencing `model.onnx.data`, written as `model_stan.onnx`.
        let entry = {
            let mut e = len_field(1, b"location");
            e.extend(len_field(2, b"model.onnx.data"));
            e
        };
        let model = len_field(7, &len_field(5, &len_field(13, &entry)));
        let onnx_path = dir.join("model_stan.onnx");
        std::fs::write(&onnx_path, &model).unwrap();
        // The distributed sidecar is named after the model file.
        std::fs::write(dir.join("model_stan.onnx.data"), b"WEIGHTS").unwrap();

        let referenced = dir.join("model.onnx.data");
        assert!(!referenced.exists(), "precondition: referenced name absent");
        ensure_external_data_present(&onnx_path).expect("provision succeeds");
        assert!(
            referenced.exists(),
            "referenced external-data name must exist after provisioning"
        );
        assert_eq!(std::fs::read(&referenced).unwrap(), b"WEIGHTS");

        // Idempotent: a second call with the name already present is a no-op.
        ensure_external_data_present(&onnx_path).expect("second call is a no-op");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A missing referenced sidecar with no stem fallback is a hard, explicit
    /// error (not a silent success that later fails deep inside ort).
    #[test]
    fn ensure_external_data_errors_when_no_sidecar_exists() {
        let dir = std::env::temp_dir().join(format!(
            "tf_ort_extdata_missing_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = {
            let mut e = len_field(1, b"location");
            e.extend(len_field(2, b"model.onnx.data"));
            e
        };
        let model = len_field(7, &len_field(5, &len_field(13, &entry)));
        let onnx_path = dir.join("model_stan.onnx");
        std::fs::write(&onnx_path, &model).unwrap();
        assert!(ensure_external_data_present(&onnx_path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_bytes_yield_none() {
        assert_eq!(onnx_first_input_name(&[]), None);
        assert_eq!(onnx_first_input_name(&[0xff, 0xff, 0xff]), None);
        // Graph present but with no inputs.
        let model = len_field(7, &[]);
        assert_eq!(onnx_first_input_name(&model), None);
    }

    /// GPU-set grammar: unset/empty/garbage → single device 0; a bare count → a
    /// 0-based dense range; an explicit comma list → those ids (dedup, ordered).
    #[test]
    fn gpu_set_parses_grammar() {
        assert_eq!(parse_gpu_set(None), vec![0]);
        assert_eq!(parse_gpu_set(Some("")), vec![0]);
        assert_eq!(parse_gpu_set(Some("   ")), vec![0]);
        // Bare count → devices 0..N.
        assert_eq!(parse_gpu_set(Some("2")), vec![0, 1]);
        assert_eq!(parse_gpu_set(Some(" 4 ")), vec![0, 1, 2, 3]);
        // Explicit list → exactly those ids, order preserved.
        assert_eq!(parse_gpu_set(Some("0,2,3")), vec![0, 2, 3]);
        assert_eq!(parse_gpu_set(Some(" 3 , 1 ")), vec![3, 1]);
        // Dedup within a list.
        assert_eq!(parse_gpu_set(Some("1,1,2")), vec![1, 2]);
        // Garbage / non-positive count / negatives → safe single-GPU default.
        assert_eq!(parse_gpu_set(Some("not-a-number")), vec![0]);
        assert_eq!(parse_gpu_set(Some("0")), vec![0]);
        assert_eq!(parse_gpu_set(Some("-3")), vec![0]);
        // A list of only-garbage collapses to the default; valid ids survive.
        assert_eq!(parse_gpu_set(Some("x,y")), vec![0]);
        assert_eq!(parse_gpu_set(Some("x,1,-2,3")), vec![1, 3]);
        // Count clamps to MAX_VISION_GPUS.
        assert_eq!(parse_gpu_set(Some("9999")).len(), super::MAX_VISION_GPUS);
    }

    /// Session→device round-robin: 8 sessions over 2 GPUs alternate 0,1,0,1,…;
    /// over an explicit `[0,2,3]` set they cycle 0,2,3,0,2,…. This is exactly the
    /// `gpus[i % gpus.len()]` mapping `build_session_pool_from_file` applies.
    #[test]
    fn sessions_map_round_robin_to_devices() {
        let gpus = [0, 1];
        let assigned: Vec<i32> = (0..8).map(|i| gpus[i % gpus.len()]).collect();
        assert_eq!(assigned, vec![0, 1, 0, 1, 0, 1, 0, 1]);

        let gpus = [0, 2, 3];
        let assigned: Vec<i32> = (0..7).map(|i| gpus[i % gpus.len()]).collect();
        assert_eq!(assigned, vec![0, 2, 3, 0, 2, 3, 0]);
    }

    /// Cache subdir keeps the historical `s<i>` for device 0 (no cache
    /// invalidation for single-GPU deployments) and namespaces other devices.
    #[test]
    fn cache_subdir_preserves_device0_and_namespaces_others() {
        assert_eq!(session_cache_subdir(0, 0), "s0");
        assert_eq!(session_cache_subdir(0, 3), "s3");
        assert_eq!(session_cache_subdir(1, 0), "d1_s0");
        assert_eq!(session_cache_subdir(2, 5), "d2_s5");
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

    /// Fake session standing in for `ort::session::Session` in the pool tests:
    /// it records the OS thread that "ran" a forward and carries a generation id
    /// so a rebuild is observable (the rebuilt session has a higher generation).
    struct FakeSession {
        generation: u64,
    }

    /// The dedicated-thread invariant: every forward for a given session MUST run
    /// on that session's ONE owning thread (never on the caller thread, never
    /// scattered across threads). This is exactly what stops ORT from caching
    /// per-OS-thread CUDA resources across many `spawn_blocking` threads.
    #[test]
    fn forward_runs_only_on_the_owning_thread() {
        let pool: WorkerPool<FakeSession> =
            WorkerPool::from_sessions("test", vec![FakeSession { generation: 0 }], None);

        let caller_thread = std::thread::current().id();
        // Every forward on the single-session pool must report the SAME worker
        // thread id, and it must differ from the calling thread.
        let mut seen: Vec<std::thread::ThreadId> = Vec::new();
        for _ in 0..8 {
            let tid = pool
                .run(|_sess: &mut FakeSession| Ok(std::thread::current().id()))
                .expect("forward runs");
            assert_ne!(tid, caller_thread, "forward must not run on the caller thread");
            seen.push(tid);
        }
        assert!(
            seen.windows(2).all(|w| w[0] == w[1]),
            "all forwards for one session must run on its single owning thread, saw {seen:?}"
        );

        // Two sessions → two distinct owning threads (N threads = N concurrent
        // forwards), and each slot is stable across repeated round-robin hits.
        let pool2: WorkerPool<FakeSession> = WorkerPool::from_sessions(
            "test",
            vec![FakeSession { generation: 0 }, FakeSession { generation: 0 }],
            None,
        );
        let a1 = pool2.run(|_| Ok(std::thread::current().id())).unwrap();
        let b1 = pool2.run(|_| Ok(std::thread::current().id())).unwrap();
        let a2 = pool2.run(|_| Ok(std::thread::current().id())).unwrap();
        let b2 = pool2.run(|_| Ok(std::thread::current().id())).unwrap();
        assert_ne!(a1, b1, "two sessions must own two distinct threads");
        assert_eq!(a1, a2, "slot 0 always runs on its one owning thread");
        assert_eq!(b1, b2, "slot 1 always runs on its one owning thread");
    }

    /// Self-heal: a forced forward error (and a panic) on a rebuilder-backed pool
    /// must NOT permanently disable the slot — the owning thread rebuilds its
    /// session in place from the same source and keeps serving. Observed via the
    /// generation counter (each rebuild yields a higher generation).
    #[test]
    fn slot_self_heals_after_forward_error_and_panic() {
        let gen = Arc::new(AtomicUsize::new(0));
        let rebuilder: Rebuilder<FakeSession> = {
            let gen = Arc::clone(&gen);
            Arc::new(move |_idx| {
                let g = gen.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                Ok(FakeSession { generation: g })
            })
        };
        let pool: WorkerPool<FakeSession> =
            WorkerPool::from_sessions("test", vec![FakeSession { generation: 0 }], Some(rebuilder));

        // Healthy forward on the original (generation 0) session.
        let g0 = pool
            .run(|s: &mut FakeSession| Ok(s.generation))
            .expect("first forward ok");
        assert_eq!(g0, 0);

        // Force a forward ERROR → the worker rebuilds the session in place.
        let err = pool.run(|_s: &mut FakeSession| -> anyhow::Result<u64> {
            anyhow::bail!("forced forward error")
        });
        assert!(err.is_err(), "forced error surfaces to the caller");

        // The slot is NOT dead: the next forward runs on a REBUILT session
        // (generation bumped by the rebuilder).
        let g1 = pool
            .run(|s: &mut FakeSession| Ok(s.generation))
            .expect("slot recovered after error");
        assert!(g1 > g0, "session must have been rebuilt (gen {g1} > {g0})");
        assert!(!pool.is_poisoned(), "rebuilder-backed pool never latches poisoned");

        // Force a PANIC mid-forward → catch_unwind keeps the thread alive and the
        // worker rebuilds again; the slot still serves afterwards.
        let panicked = pool.run(|_s: &mut FakeSession| -> anyhow::Result<u64> {
            panic!("forced forward panic");
        });
        assert!(panicked.is_err(), "panicked forward surfaces as an error, not a hang");
        let g2 = pool
            .run(|s: &mut FakeSession| Ok(s.generation))
            .expect("slot recovered after panic");
        assert!(g2 > g1, "session rebuilt again after the panic (gen {g2} > {g1})");
    }

    /// A rebuilder-less pool (the `onnx-cv` path) latches `poisoned` on a panicked
    /// forward so the owner reloads the whole entry, while a plain forward error
    /// leaves the pool usable (a shape/registry error does not corrupt the runtime).
    #[test]
    fn rebuilderless_pool_latches_poisoned_only_on_panic() {
        let pool: WorkerPool<FakeSession> =
            WorkerPool::from_sessions("test", vec![FakeSession { generation: 0 }], None);

        // A plain forward error does not poison the pool.
        let _ = pool.run(|_s: &mut FakeSession| -> anyhow::Result<u64> {
            anyhow::bail!("shape mismatch")
        });
        assert!(!pool.is_poisoned(), "a forward error must not poison an onnx-cv pool");
        assert!(pool.run(|s: &mut FakeSession| Ok(s.generation)).is_ok());

        // A panic poisons it; subsequent runs short-circuit with "reload required".
        let _ = pool.run(|_s: &mut FakeSession| -> anyhow::Result<u64> {
            panic!("boom");
        });
        // The worker sets the flag right after catch_unwind; give it a moment.
        for _ in 0..100 {
            if pool.is_poisoned() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(pool.is_poisoned(), "a panicked forward must poison a rebuilder-less pool");
        assert!(
            pool.run(|s: &mut FakeSession| Ok(s.generation)).is_err(),
            "a poisoned pool short-circuits further forwards"
        );
    }

    /// After a panic a rebuilder-less pool must NEVER run a later job on the torn
    /// session — the worker drops the session on panic, so any subsequent forward
    /// errors out WITHOUT its closure ever touching a (possibly corrupt) session.
    #[test]
    fn rebuilderless_pool_never_reuses_torn_session_after_panic() {
        let pool: WorkerPool<FakeSession> =
            WorkerPool::from_sessions("test", vec![FakeSession { generation: 0 }], None);

        let _ = pool.run(|_s: &mut FakeSession| -> anyhow::Result<u64> {
            panic!("torn");
        });
        for _ in 0..100 {
            if pool.is_poisoned() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // The closure must not run at all (short-circuited by the poison latch or
        // dropped by the None-rebuilder branch), and the caller must see an error.
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_probe = Arc::clone(&ran);
        let out = pool.run(move |s: &mut FakeSession| {
            ran_probe.fetch_add(1, Ordering::SeqCst);
            Ok(s.generation)
        });
        assert!(out.is_err(), "a forward after a panic must error, not reuse the session");
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the forward closure must never execute on a torn session"
        );
    }

    /// A dropped pool JOINS its worker threads, so every owned session is fully
    /// released before the drop returns — the guarantee that stops a replacement
    /// pool from building fresh sessions while the old ones still reside on the GPU
    /// (transient double-VRAM). Observed via a session whose Drop bumps a counter.
    #[test]
    fn dropping_pool_joins_workers_and_frees_sessions() {
        struct CountedSession {
            dropped: Arc<AtomicUsize>,
        }
        impl Drop for CountedSession {
            fn drop(&mut self) {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let pool: WorkerPool<CountedSession> = WorkerPool::from_sessions(
            "test",
            (0..3)
                .map(|_| CountedSession {
                    dropped: Arc::clone(&dropped),
                })
                .collect(),
            None,
        );
        // Exercise the workers so the sessions live on their threads.
        for _ in 0..6 {
            pool.run(|_s: &mut CountedSession| Ok(())).unwrap();
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 0, "sessions alive while pool is alive");

        // Dropping the pool must synchronously join all workers; the moment drop
        // returns, all three owned sessions have been dropped (no lingering thread
        // still holding one).
        drop(pool);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            3,
            "all worker threads must be joined (sessions freed) before drop returns"
        );
    }
}
