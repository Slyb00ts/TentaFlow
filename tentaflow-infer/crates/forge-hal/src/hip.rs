// ===== File: hip.rs — forge-hal: HIP/ROCm backend (device, memory, module load, launch) =====
// Pełny odpowiednik backendu CUDA dla AMD: wykrycie urządzenia, trzy pule VRAM
// na tych samych arenach (bump/slab/ring), streamy, eventy z pomiarem czasu,
// ładowanie code objectu (HSACO), uruchamianie kerneli oraz przechwytywanie
// i odtwarzanie grafów.

use std::any::Any;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use forge_types::{DeviceCaps, ForgeError, MemKind, Result, Vendor};

use crate::arena::{BumpArena, RingArena, SlabArena};
use crate::{
    BufferImpl, DevBuffer, Device, Event, EventImpl, ExecGraph, GraphImpl, KernelHandle,
    KernelImpl, LaunchArgs, LaunchConfig, Module, ModuleImpl, Pool, PoolSizes, Stream, StreamImpl,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct ForgeHipProps {
    name: [c_char; 256],
    arch: [c_char; 64],
    total_mem: u64,
    warp_size: c_int,
    cu_count: c_int,
    max_threads_per_block: c_int,
    max_shared_mem_per_block: c_int,
}

type HipStreamRaw = *mut c_void;
type HipEventRaw = *mut c_void;
type HipModuleRaw = *mut c_void;
type HipFunctionRaw = *mut c_void;
type HipGraphRaw = *mut c_void;
type HipGraphExecRaw = *mut c_void;

extern "C" {
    fn forge_hip_props(device: c_int, out: *mut ForgeHipProps) -> c_int;

    fn hipInit(flags: c_uint) -> c_int;
    fn hipGetDeviceCount(count: *mut c_int) -> c_int;
    fn hipSetDevice(device: c_int) -> c_int;
    fn hipDeviceCanAccessPeer(can: *mut c_int, device: c_int, peer: c_int) -> c_int;
    fn hipDeviceEnablePeerAccess(peer: c_int, flags: c_uint) -> c_int;
    fn hipDeviceSynchronize() -> c_int;
    fn hipMemGetInfo(free: *mut usize, total: *mut usize) -> c_int;
    fn hipMalloc(ptr: *mut *mut c_void, size: usize) -> c_int;
    fn hipFree(ptr: *mut c_void) -> c_int;
    fn hipHostMalloc(ptr: *mut *mut c_void, size: usize, flags: c_uint) -> c_int;
    fn hipHostFree(ptr: *mut c_void) -> c_int;
    /// Kierunek rozpoznawany z adresow (UVA) — jedyny wariant poprawny, gdy
    /// jednym z buforow jest przypieta pamiec hosta.
    fn hipMemcpyAsync(
        dst: *mut c_void,
        src: *const c_void,
        size: usize,
        kind: c_int,
        stream: HipStreamRaw,
    ) -> c_int;
    fn hipStreamCreateWithFlags(stream: *mut HipStreamRaw, flags: c_uint) -> c_int;
    fn hipStreamDestroy(stream: HipStreamRaw) -> c_int;
    fn hipStreamSynchronize(stream: HipStreamRaw) -> c_int;
    fn hipStreamWaitEvent(stream: HipStreamRaw, event: HipEventRaw, flags: c_uint) -> c_int;
    fn hipEventCreate(event: *mut HipEventRaw) -> c_int;
    fn hipEventCreateWithFlags(event: *mut HipEventRaw, flags: c_uint) -> c_int;
    fn hipEventDestroy(event: HipEventRaw) -> c_int;
    fn hipEventRecord(event: HipEventRaw, stream: HipStreamRaw) -> c_int;
    fn hipEventSynchronize(event: HipEventRaw) -> c_int;
    fn hipEventQuery(event: HipEventRaw) -> c_int;
    fn hipEventElapsedTime(ms: *mut f32, start: HipEventRaw, end: HipEventRaw) -> c_int;
    fn hipModuleLoadData(module: *mut HipModuleRaw, image: *const c_void) -> c_int;
    fn hipModuleUnload(module: HipModuleRaw) -> c_int;
    fn hipModuleGetFunction(
        func: *mut HipFunctionRaw,
        module: HipModuleRaw,
        name: *const c_char,
    ) -> c_int;
    #[allow(clippy::too_many_arguments)]
    fn hipModuleLaunchKernel(
        func: HipFunctionRaw,
        grid_x: c_uint,
        grid_y: c_uint,
        grid_z: c_uint,
        block_x: c_uint,
        block_y: c_uint,
        block_z: c_uint,
        shared_bytes: c_uint,
        stream: HipStreamRaw,
        kernel_params: *mut *mut c_void,
        extra: *mut *mut c_void,
    ) -> c_int;
    fn hipStreamBeginCapture(stream: HipStreamRaw, mode: c_uint) -> c_int;
    fn hipStreamEndCapture(stream: HipStreamRaw, graph: *mut HipGraphRaw) -> c_int;
    fn hipGraphInstantiate(
        exec: *mut HipGraphExecRaw,
        graph: HipGraphRaw,
        error_node: *mut c_void,
        log: *mut c_char,
        log_size: usize,
    ) -> c_int;
    fn hipGraphLaunch(exec: HipGraphExecRaw, stream: HipStreamRaw) -> c_int;
    fn hipGraphDestroy(graph: HipGraphRaw) -> c_int;
    fn hipGraphExecDestroy(exec: HipGraphExecRaw) -> c_int;
    fn hipGetErrorString(status: c_int) -> *const c_char;
}

const HIP_SUCCESS: c_int = 0;
const HIP_ERROR_NOT_READY: c_int = 34;
const HIP_EVENT_DISABLE_TIMING: c_uint = 2;
/// `hipStreamCaptureModeThreadLocal` — tak samo jak backend CUDA.
///
/// Tryb globalny unieważnia przechwytywanie przy KAŻDEJ ryzykownej operacji w
/// całym procesie, także wykonanej przez inny wątek, który o grafie nic nie
/// wie. Backend CUDA używa trybu wątkowego, więc HIP w trybie globalnym dawał
/// błąd 906/901 tam, gdzie ta sama praca na NVIDII przechodziła.
const HIP_CAPTURE_MODE_THREAD_LOCAL: c_uint = 1;
/// `hipMemcpyDefault` — kierunek wyprowadzany z adresow.
const HIP_MEMCPY_DEFAULT: c_int = 4;
/// `hipStreamNonBlocking` — strumień nie synchronizuje się ze strumieniem
/// domyślnym, tak jak strumienie backendu CUDA. Strumień blokujący sprawiał,
/// że kopia na strumieniu domyślnym wywracała trwające przechwytywanie grafu.
const HIP_STREAM_NON_BLOCKING: c_uint = 1;

/// Zamienia kod HIP na `ForgeError` z komunikatem ze sterownika.
fn check(status: c_int, what: &str) -> Result<()> {
    if status == HIP_SUCCESS {
        return Ok(());
    }
    let text = unsafe {
        let raw = hipGetErrorString(status);
        if raw.is_null() {
            String::from("nieznany błąd HIP")
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    };
    Err(ForgeError::Device(format!("{what}: {text} (kod {status})")))
}

fn cstr_field(bytes: &[c_char]) -> String {
    unsafe { CStr::from_ptr(bytes.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub struct HipDevice {
    caps: DeviceCaps,
    ordinal: c_int,
    /// Wlasny strumien kopii host<->GPU. `hipMemcpyHtoD`/`DtoH` chodza po
    /// strumieniu domyslnym, a ten koliduje z trwajacym przechwytywaniem grafu
    /// na innym strumieniu (kod 906). Kopie ida wiec po strumieniu jawnym,
    /// zsynchronizowanym od razu — z zewnatrz nadal sa synchroniczne.
    transfer: HipStream,
    weights: Arc<HipPool>,
    kv_cache: Arc<HipPool>,
    activations: Arc<HipPool>,
}

impl HipDevice {
    /// Otwiera urządzenie HIP i zajmuje z góry trzy pule VRAM o podanych
    /// budżetach — ten sam układ, co backend CUDA.
    pub fn new(ordinal: usize, pools: PoolSizes) -> Result<Arc<Self>> {
        let ordinal = c_int::try_from(ordinal)
            .map_err(|_| ForgeError::Device("numer urządzenia HIP poza zakresem".into()))?;
        unsafe {
            check(hipInit(0), "hipInit")?;
            let mut count: c_int = 0;
            check(hipGetDeviceCount(&mut count), "hipGetDeviceCount")?;
            if ordinal >= count {
                return Err(ForgeError::Device(format!(
                    "urządzenie HIP {ordinal} nie istnieje (widocznych: {count})"
                )));
            }
            check(hipSetDevice(ordinal), "hipSetDevice")?;
        }
        let mut raw = ForgeHipProps {
            name: [0; 256],
            arch: [0; 64],
            total_mem: 0,
            warp_size: 0,
            cu_count: 0,
            max_threads_per_block: 0,
            max_shared_mem_per_block: 0,
        };
        check(unsafe { forge_hip_props(ordinal, &mut raw) }, "hipGetDeviceProperties")?;
        let arch = cstr_field(&raw.arch);
        // `gcnArchName` niesie sufiksy cech (np. "gfx1030:xnack-"), a artefakty
        // adresujemy samą nazwą architektury.
        let arch = arch.split(':').next().unwrap_or(&arch).to_string();
        let rdna4 = arch.starts_with("gfx12");
        if raw.warp_size != 32 && raw.warp_size != 64 {
            return Err(ForgeError::Device(format!(
                "nieoczekiwany rozmiar wavefrontu {} dla {arch}",
                raw.warp_size
            )));
        }
        if raw.cu_count <= 0 {
            return Err(ForgeError::Device(format!("{arch} zgłasza {} CU", raw.cu_count)));
        }
        let caps = DeviceCaps {
            name: cstr_field(&raw.name),
            vendor: Vendor::Amd,
            arch,
            total_memory: raw.total_mem as usize,
            max_shared_mem_per_block: raw.max_shared_mem_per_block as usize,
            max_threads_per_block: raw.max_threads_per_block as u32,
            warp_size: raw.warp_size as u32,
            // UWAGA: na RDNA `multiProcessorCount` liczy WGP, nie CU — 6900 XT
            // zgłasza tu 40, a `rocminfo` 80 CU (RDNA łączy 2 CU w jedno WGP).
            // Dla dobierania siatki WGP jest właściwym odpowiednikiem SM, bo
            // workgroup ląduje na jednym WGP; przy przenoszeniu heurystyk
            // liczących „bloki na SM" trzeba o tym pamiętać.
            sm_count: raw.cu_count as u32,
            // RDNA2/RDNA3 nie mają potoku FP8 ani FP4. RDNA4 ma
            // `v_wmma_f32_16x16x16_fp8_fp8` — zmierzone na R9700 378 TFLOPS
            // wobec 179 dla f16 — i kernel `gemm_fp8_wmma` na niej stoi.
            // Rodzina wystarcza: `gfx12` to cała RDNA4, a brak artefaktu i tak
            // zatrzyma launcher.
            fp8_native: rdna4,
            // Blokowo skalowane FP4, wgmma, tcgen05 i TMA to instrukcje NVIDII;
            // ten backend nie ma zadnego z tych rdzeni.
            fp4_block_scale_ue8m0: false,
            fp4_block_scale_e4m3: false,
            wgmma: false,
            tcgen05: false,
            tma: false,
            bf16_native: false,
            supports_p2p: false,
            supports_graph_capture: true,
        };
        let requested = pools.total()?;
        let free = mem_get_info()?.0;
        if requested > free {
            return Err(ForgeError::OutOfMemory {
                requested,
                available: free,
            });
        }
        let kv_page = if pools.kv_page_size == 0 {
            PoolSizes::DEFAULT_KV_PAGE
        } else {
            pools.kv_page_size
        };
        let weights = HipPool::new(pools.weights, PoolArena::Bump(BumpArena::new(pools.weights)))?;
        let kv_cache = HipPool::new(
            pools.kv_cache,
            PoolArena::Slab(SlabArena::new(pools.kv_cache, kv_page)?),
        )?;
        let activations = HipPool::new(
            pools.activations,
            PoolArena::Ring(RingArena::new(pools.activations)),
        )?;
        let mut transfer_raw: HipStreamRaw = std::ptr::null_mut();
        check(
            unsafe { hipStreamCreateWithFlags(&mut transfer_raw, HIP_STREAM_NON_BLOCKING) },
            "hipStreamCreateWithFlags transfer",
        )?;
        let transfer = HipStream(transfer_raw);
        Ok(Arc::new(Self {
            caps,
            ordinal,
            transfer,
            weights,
            kv_cache,
            activations,
        }))
    }

    /// Otwiera urządzenie z domyślnym budżetem: 90% wolnego VRAM w chwili
    /// tworzenia (patrz `PoolSizes::auto_from_free`).
    pub fn with_default_pools(ordinal: usize) -> Result<Arc<Self>> {
        let free = Self::free_vram(ordinal)?;
        Self::new(ordinal, PoolSizes::auto_from_free(free))
    }

    /// `(wolne, całkowite)` w bajtach dla już otwartego urządzenia.
    pub fn mem_info(&self) -> Result<(usize, usize)> {
        self.bind()?;
        mem_get_info()
    }

    /// Wolny VRAM w bajtach bez trzymania urządzenia — do wymiarowania pul,
    /// które zajmują cały swój budżet od razu przy tworzeniu.
    pub fn free_vram(ordinal: usize) -> Result<usize> {
        let ordinal = c_int::try_from(ordinal)
            .map_err(|_| ForgeError::Device("numer urządzenia HIP poza zakresem".into()))?;
        unsafe {
            check(hipInit(0), "hipInit")?;
            check(hipSetDevice(ordinal), "hipSetDevice")?;
        }
        Ok(mem_get_info()?.0)
    }

    fn pool(&self, pool: Pool) -> &Arc<HipPool> {
        match pool {
            Pool::Weights => &self.weights,
            Pool::KvCache => &self.kv_cache,
            Pool::Activations => &self.activations,
        }
    }

    fn bind(&self) -> Result<()> {
        check(unsafe { hipSetDevice(self.ordinal) }, "hipSetDevice")
    }
}

/// `(wolne, całkowite)` dla urządzenia aktualnie ustawionego w tym wątku.
fn mem_get_info() -> Result<(usize, usize)> {
    let mut free = 0usize;
    let mut total = 0usize;
    check(
        unsafe { hipMemGetInfo(&mut free, &mut total) },
        "hipMemGetInfo",
    )?;
    Ok((free, total))
}

enum PoolArena {
    Bump(BumpArena),
    Slab(SlabArena),
    Ring(RingArena),
}

/// Jeden zajęty z góry obszar VRAM plus polityka podpodziału. Bufory trzymają
/// `Arc<HipPool>`, więc baza przeżywa każdą podalokację.
struct HipPool {
    base: *mut c_void,
    arena: Mutex<PoolArena>,
}

// `base` jest adresem urządzenia, nie wskaźnikiem hosta; mutacje idą przez Mutex.
unsafe impl Send for HipPool {}
unsafe impl Sync for HipPool {}

impl HipPool {
    fn new(capacity: usize, arena: PoolArena) -> Result<Arc<Self>> {
        // Pula zerowa jest poprawna (np. silnik bez KV) — nie zajmuje VRAM,
        // a każda alokacja z niej zgłasza OutOfMemory.
        let base = if capacity == 0 {
            std::ptr::null_mut()
        } else {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            check(unsafe { hipMalloc(&mut ptr, capacity) }, "hipMalloc puli")?;
            ptr
        };
        Ok(Arc::new(Self {
            base,
            arena: Mutex::new(arena),
        }))
    }

}

impl Drop for HipPool {
    fn drop(&mut self) {
        if !self.base.is_null() {
            unsafe {
                let _ = hipFree(self.base);
            }
        }
    }
}

enum Backing {
    Pooled {
        pool: Arc<HipPool>,
        offset: usize,
        reserved: usize,
        generation: Option<u64>,
    },
    Pinned {
        ptr: *mut c_void,
    },
    /// Okno wewnątrz innej alokacji; zatrzymuje ją, ale niczego nie zwalnia.
    Borrowed {
        /// Trzymany wyłącznie po to, żeby rodzic przeżył pod-bufor.
        #[allow(dead_code)]
        parent: Arc<dyn BufferImpl>,
    },
}

struct HipBuffer {
    backing: Backing,
    ptr: *mut c_void,
    len: usize,
    kind: MemKind,
}

unsafe impl Send for HipBuffer {}
unsafe impl Sync for HipBuffer {}

impl Drop for HipBuffer {
    fn drop(&mut self) {
        match &self.backing {
            Backing::Pooled {
                pool,
                offset,
                reserved,
                generation,
            } => {
                let mut arena = pool.arena.lock().expect("arena puli zatruta");
                match (&mut *arena, generation) {
                    (PoolArena::Slab(slab), _) => slab.free(*offset, *reserved),
                    (PoolArena::Ring(ring), Some(generation)) => ring.on_drop(*generation),
                    // Wagi nie są zwalniane pojedynczo — pula oddaje całość przy
                    // rozbiórce modelu.
                    (PoolArena::Bump(_), _) => {}
                    (PoolArena::Ring(_), None) => {
                        unreachable!("pierścień bez numeru generacji")
                    }
                }
            }
            Backing::Pinned { ptr } => unsafe {
                let _ = hipHostFree(*ptr);
            },
            Backing::Borrowed { .. } => {}
        }
    }
}

impl BufferImpl for HipBuffer {
    fn len(&self) -> usize {
        self.len
    }
    fn kind(&self) -> MemKind {
        self.kind
    }
    fn device_ptr(&self) -> u64 {
        self.ptr as u64
    }
    fn host_ptr(&self) -> Option<*mut u8> {
        matches!(self.kind, MemKind::PinnedHost).then_some(self.ptr as *mut u8)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HipStream(HipStreamRaw);
unsafe impl Send for HipStream {}
unsafe impl Sync for HipStream {}

impl Drop for HipStream {
    fn drop(&mut self) {
        unsafe {
            let _ = hipStreamDestroy(self.0);
        }
    }
}

impl StreamImpl for HipStream {
    fn synchronize(&self) -> Result<()> {
        check(unsafe { hipStreamSynchronize(self.0) }, "hipStreamSynchronize")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HipEvent(HipEventRaw);
unsafe impl Send for HipEvent {}
unsafe impl Sync for HipEvent {}

impl Drop for HipEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = hipEventDestroy(self.0);
        }
    }
}

impl EventImpl for HipEvent {
    fn synchronize(&self) -> Result<()> {
        check(unsafe { hipEventSynchronize(self.0) }, "hipEventSynchronize")
    }
    fn is_complete(&self) -> Result<bool> {
        let status = unsafe { hipEventQuery(self.0) };
        match status {
            HIP_SUCCESS => Ok(true),
            HIP_ERROR_NOT_READY => Ok(false),
            other => {
                check(other, "hipEventQuery")?;
                Ok(false)
            }
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HipModule {
    raw: HipModuleRaw,
    /// Własna kopia obrazu code objectu. `hipModuleLoadData` nie gwarantuje, że
    /// skopiuje bufor wołającego (leniwe ładowanie code objectów v5 potrafi go
    /// trzymać), a rejestr kerneli zwalnia swój bufor od razu po załadowaniu.
    _image: Vec<u8>,
}
unsafe impl Send for HipModule {}
unsafe impl Sync for HipModule {}

impl Drop for HipModule {
    fn drop(&mut self) {
        unsafe {
            let _ = hipModuleUnload(self.raw);
        }
    }
}

impl ModuleImpl for HipModule {
    fn kernel(self: Arc<Self>, name: &str) -> Result<KernelHandle> {
        let symbol = CString::new(name)
            .map_err(|_| ForgeError::Device(format!("nazwa kernela {name} zawiera zero")))?;
        let mut func: HipFunctionRaw = std::ptr::null_mut();
        check(
            unsafe { hipModuleGetFunction(&mut func, self.raw, symbol.as_ptr()) },
            &format!("hipModuleGetFunction({name})"),
        )?;
        Ok(KernelHandle::from_impl(Arc::new(HipKernel {
            func,
            name: name.to_string(),
            _module: self,
        })))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HipKernel {
    func: HipFunctionRaw,
    name: String,
    /// Trzyma moduł żywym: `func` jest wskaźnikiem w jego obrazie kodu, a
    /// `hipModuleUnload` w `Drop` modułu unieważniłby go.
    _module: Arc<HipModule>,
}

unsafe impl Send for HipKernel {}
unsafe impl Sync for HipKernel {}

impl KernelImpl for HipKernel {
    fn name(&self) -> &str {
        &self.name
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct HipGraph(HipGraphExecRaw);
unsafe impl Send for HipGraph {}
unsafe impl Sync for HipGraph {}

impl Drop for HipGraph {
    fn drop(&mut self) {
        unsafe {
            let _ = hipGraphExecDestroy(self.0);
        }
    }
}

impl GraphImpl for HipGraph {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Zachowuje typowany `OutOfMemory` z areny i dokłada nazwę puli do logu —
/// tak samo jak backend CUDA, bo admission i tiering rozpoznają ten wariant.
fn pool_alloc_error(pool: Pool, error: ForgeError) -> ForgeError {
    if let ForgeError::OutOfMemory {
        requested,
        available,
    } = &error
    {
        tracing::debug!(?pool, requested, available, "pula HIP wyczerpana");
        return error;
    }
    ForgeError::Device(format!("pula {pool:?}: {error}"))
}

impl Device for HipDevice {
    fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    fn alloc(&self, bytes: usize, kind: MemKind, pool: Pool) -> Result<DevBuffer> {
        self.bind()?;
        let bytes = bytes.max(1);
        if matches!(kind, MemKind::Managed) {
            // Pamięć zarządzana nie jest zaimplementowana; bez tego warunku
            // żądanie dostałoby zwykły bufor urządzenia i milcząco zgubiło
            // dostęp z hosta.
            return Err(ForgeError::Unsupported(
                "backend HIP nie obsługuje pamięci zarządzanej".into(),
            ));
        }
        if matches!(kind, MemKind::PinnedHost) {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            check(unsafe { hipHostMalloc(&mut ptr, bytes, 0) }, "hipHostMalloc")?;
            return Ok(DevBuffer::from_impl(Arc::new(HipBuffer {
                backing: Backing::Pinned { ptr },
                ptr,
                len: bytes,
                kind,
            })));
        }
        let pool_kind = pool;
        let pool = self.pool(pool_kind).clone();
        let (offset, reserved, generation) = {
            let mut arena = pool.arena.lock().expect("arena puli zatruta");
            match &mut *arena {
                PoolArena::Bump(bump) => {
                    let offset = bump
                        .alloc(bytes)
                        .map_err(|error| pool_alloc_error(pool_kind, error))?;
                    (offset, 0, None)
                }
                PoolArena::Slab(slab) => {
                    let (offset, reserved) = slab
                        .alloc(bytes)
                        .map_err(|error| pool_alloc_error(pool_kind, error))?;
                    (offset, reserved, None)
                }
                PoolArena::Ring(ring) => {
                    let (offset, generation) = ring
                        .alloc(bytes)
                        .map_err(|error| pool_alloc_error(pool_kind, error))?;
                    (offset, 0, Some(generation))
                }
            }
        };
        let ptr = unsafe { pool.base.add(offset) };
        Ok(DevBuffer::from_impl(Arc::new(HipBuffer {
            backing: Backing::Pooled {
                pool,
                offset,
                reserved,
                generation,
            },
            ptr,
            len: bytes,
            kind,
        })))
    }

    fn sub_buffer(&self, parent: &DevBuffer, offset: usize, len: usize) -> Result<DevBuffer> {
        let base = parent.downcast::<HipBuffer>()?;
        crate::check_sub_range(base.len, offset, len)?;
        let ptr = unsafe { (base.ptr as *mut u8).add(offset) as *mut c_void };
        let kind = base.kind;
        Ok(DevBuffer::from_impl(Arc::new(HipBuffer {
            backing: Backing::Borrowed {
                parent: parent.impl_arc(),
            },
            ptr,
            len,
            kind,
        })))
    }

    fn pool_available(&self, pool: Pool) -> Option<usize> {
        let arena = self.pool(pool).arena.lock().expect("arena puli zatruta");
        match &*arena {
            PoolArena::Bump(bump) => Some(bump.available()),
            PoolArena::Ring(ring) => Some(ring.available()),
            PoolArena::Slab(_) => None,
        }
    }

    fn create_stream(&self) -> Result<Stream> {
        self.bind()?;
        let mut raw: HipStreamRaw = std::ptr::null_mut();
        check(
            unsafe { hipStreamCreateWithFlags(&mut raw, HIP_STREAM_NON_BLOCKING) },
            "hipStreamCreateWithFlags",
        )?;
        Ok(Stream::from_impl(Arc::new(HipStream(raw))))
    }

    fn create_event(&self) -> Result<Event> {
        self.bind()?;
        let mut raw: HipEventRaw = std::ptr::null_mut();
        check(
            unsafe { hipEventCreateWithFlags(&mut raw, HIP_EVENT_DISABLE_TIMING) },
            "hipEventCreateWithFlags",
        )?;
        Ok(Event::from_impl(Arc::new(HipEvent(raw))))
    }

    fn create_timing_event(&self) -> Result<Event> {
        self.bind()?;
        let mut raw: HipEventRaw = std::ptr::null_mut();
        check(unsafe { hipEventCreate(&mut raw) }, "hipEventCreate")?;
        Ok(Event::from_impl(Arc::new(HipEvent(raw))))
    }

    fn record_event(&self, event: &Event, stream: &Stream) -> Result<()> {
        let event = event.downcast::<HipEvent>()?;
        let stream = stream.downcast::<HipStream>()?;
        check(unsafe { hipEventRecord(event.0, stream.0) }, "hipEventRecord")
    }

    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
        let event = event.downcast::<HipEvent>()?;
        let stream = stream.downcast::<HipStream>()?;
        check(
            unsafe { hipStreamWaitEvent(stream.0, event.0, 0) },
            "hipStreamWaitEvent",
        )
    }

    fn elapsed_event_ms(&self, start: &Event, end: &Event) -> Result<Option<f32>> {
        let start = start.downcast::<HipEvent>()?;
        let end = end.downcast::<HipEvent>()?;
        let mut ms = 0.0f32;
        check(
            unsafe { hipEventElapsedTime(&mut ms, start.0, end.0) },
            "hipEventElapsedTime",
        )?;
        Ok(Some(ms))
    }

    fn ordinal(&self) -> usize {
        self.ordinal as usize
    }

    /// Włącza P2P w kierunku `peer`. HIP zgłasza błąd przy powtórnym włączeniu
    /// tej samej pary, więc traktujemy to jako sukces — stan docelowy jest ten
    /// sam, a wołający nie musi śledzić, kto już otworzył dostęp.
    fn enable_peer_access(&self, peer_ordinal: usize) -> Result<()> {
        let peer = c_int::try_from(peer_ordinal)
            .map_err(|_| ForgeError::Device("numer karty poza zakresem".into()))?;
        if peer == self.ordinal {
            return Ok(());
        }
        self.bind()?;
        let mut can: c_int = 0;
        check(
            unsafe { hipDeviceCanAccessPeer(&mut can, self.ordinal, peer) },
            "hipDeviceCanAccessPeer",
        )?;
        if can == 0 {
            return Err(ForgeError::Unsupported(format!(
                "karty {} i {peer} nie widzą swojej pamięci",
                self.ordinal
            )));
        }
        const ALREADY_ENABLED: c_int = 704;
        let status = unsafe { hipDeviceEnablePeerAccess(peer, 0) };
        if status == 0 || status == ALREADY_ENABLED {
            Ok(())
        } else {
            check(status, "hipDeviceEnablePeerAccess")
        }
    }

    fn copy(
        &self,
        src: &DevBuffer,
        src_offset: usize,
        dst: &DevBuffer,
        dst_offset: usize,
        bytes: usize,
        stream: &Stream,
    ) -> Result<()> {
        let source = src.downcast::<HipBuffer>()?;
        let target = dst.downcast::<HipBuffer>()?;
        let stream = stream.downcast::<HipStream>()?;
        if src_offset + bytes > source.len || dst_offset + bytes > target.len {
            return Err(ForgeError::Device(
                "kopia HIP wychodzi poza bufor".into(),
            ));
        }
        check(
            unsafe {
                hipMemcpyAsync(
                    target.ptr.add(dst_offset),
                    source.ptr.add(src_offset) as *const c_void,
                    bytes,
                    HIP_MEMCPY_DEFAULT,
                    stream.0,
                )
            },
            "hipMemcpyAsync",
        )
    }

    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
        let target = dst.downcast::<HipBuffer>()?;
        if dst_offset + src.len() > target.len {
            return Err(ForgeError::Device("zapis HIP wychodzi poza bufor".into()));
        }
        self.bind()?;
        check(
            unsafe {
                hipMemcpyAsync(
                    target.ptr.add(dst_offset),
                    src.as_ptr() as *const c_void,
                    src.len(),
                    HIP_MEMCPY_DEFAULT,
                    self.transfer.0,
                )
            },
            "hipMemcpyAsync HtoD",
        )?;
        check(
            unsafe { hipStreamSynchronize(self.transfer.0) },
            "hipStreamSynchronize transfer",
        )
    }

    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
        let source = src.downcast::<HipBuffer>()?;
        if src_offset + dst.len() > source.len {
            return Err(ForgeError::Device("odczyt HIP wychodzi poza bufor".into()));
        }
        self.bind()?;
        check(
            unsafe {
                hipMemcpyAsync(
                    dst.as_mut_ptr() as *mut c_void,
                    source.ptr.add(src_offset) as *const c_void,
                    dst.len(),
                    HIP_MEMCPY_DEFAULT,
                    self.transfer.0,
                )
            },
            "hipMemcpyAsync DtoH",
        )?;
        check(
            unsafe { hipStreamSynchronize(self.transfer.0) },
            "hipStreamSynchronize transfer",
        )
    }

    fn load_module(&self, image: &[u8]) -> Result<Module> {
        self.bind()?;
        let owned = image.to_vec();
        let mut raw: HipModuleRaw = std::ptr::null_mut();
        check(
            unsafe { hipModuleLoadData(&mut raw, owned.as_ptr() as *const c_void) },
            "hipModuleLoadData",
        )?;
        Ok(Module::from_impl(Arc::new(HipModule {
            raw,
            _image: owned,
        })))
    }

    fn launch(
        &self,
        kernel: &KernelHandle,
        cfg: &LaunchConfig,
        args: &LaunchArgs,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = kernel.downcast::<HipKernel>()?;
        let stream = stream.downcast::<HipStream>()?;
        // Wybór urządzenia w HIP jest STANEM WĄTKU, a silnik uruchamia kernele
        // z wątku roboczego, który nigdy nie wołał `hipSetDevice`.
        self.bind()?;
        // Parametry to wskaźniki na 8-bajtowe sloty zebrane przez builder;
        // `&args` gwarantuje stabilność adresów na czas uruchomienia.
        let mut params: Vec<*mut c_void> = args
            .slots()
            .iter()
            .map(|slot| slot as *const u64 as *mut c_void)
            .collect();
        check(
            unsafe {
                hipModuleLaunchKernel(
                    kernel.func,
                    cfg.grid.0,
                    cfg.grid.1,
                    cfg.grid.2,
                    cfg.block.0,
                    cfg.block.1,
                    cfg.block.2,
                    cfg.shared_mem_bytes as c_uint,
                    stream.0,
                    params.as_mut_ptr(),
                    std::ptr::null_mut(),
                )
            },
            &format!("hipModuleLaunchKernel({})", kernel.name),
        )
    }

    fn synchronize(&self) -> Result<()> {
        self.bind()?;
        check(unsafe { hipDeviceSynchronize() }, "hipDeviceSynchronize")
    }

    fn begin_capture(&self, stream: &Stream) -> Result<()> {
        let stream = stream.downcast::<HipStream>()?;
        check(
            unsafe { hipStreamBeginCapture(stream.0, HIP_CAPTURE_MODE_THREAD_LOCAL) },
            "hipStreamBeginCapture",
        )
    }

    fn end_capture(&self, stream: &Stream) -> Result<ExecGraph> {
        let stream = stream.downcast::<HipStream>()?;
        let mut graph: HipGraphRaw = std::ptr::null_mut();
        check(
            unsafe { hipStreamEndCapture(stream.0, &mut graph) },
            "hipStreamEndCapture",
        )?;
        let mut exec: HipGraphExecRaw = std::ptr::null_mut();
        let status = unsafe {
            hipGraphInstantiate(
                &mut exec,
                graph,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        // Sam graf nie jest już potrzebny po instancjacji; egzemplarz wykonawczy
        // trzyma własną kopię.
        unsafe {
            let _ = hipGraphDestroy(graph);
        }
        check(status, "hipGraphInstantiate")?;
        Ok(ExecGraph::from_impl(Arc::new(HipGraph(exec))))
    }

    fn launch_graph(&self, graph: &ExecGraph, stream: &Stream) -> Result<()> {
        let exec = graph.downcast::<HipGraph>()?;
        let stream = stream.downcast::<HipStream>()?;
        check(unsafe { hipGraphLaunch(exec.0, stream.0) }, "hipGraphLaunch")
    }

    fn reset_activations(&self) -> Result<u64> {
        let mut arena = self.activations.arena.lock().expect("arena puli zatruta");
        match &mut *arena {
            PoolArena::Ring(ring) => ring.reset(),
            _ => unreachable!("pula aktywacji jest zawsze pierścieniem"),
        }
    }
}
