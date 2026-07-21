// ===== File: cuda.rs — CUDA backend: driver-API device with VRAM arena pools, PTX modules, CUDA Graphs =====
//
// Built on cudarc's driver layer with dynamic loading (libcuda is dlopen'ed at
// first use, the binary carries no hard link). VRAM is claimed once at device
// construction into three logical pools (weights/KV/activations) and
// sub-allocated by the arenas in `arena.rs`, so no cuMemAlloc happens in the
// hot path. Graph capture piggybacks on stream capture; buffers and kernels
// referenced by captured launches are retained by the resulting `ExecGraph`
// so replays can never dereference freed VRAM or unloaded module code.

use std::any::Any;
use std::ffi::{c_void, CString};
use std::sync::{Arc, Mutex};

use cudarc::driver::safe::{CudaContext, CudaEvent, CudaStream};
use cudarc::driver::{result, sys, DriverError};
use forge_types::{DeviceCaps, ForgeError, MemKind, Result, Vendor};

use crate::arena::{BumpArena, RingArena, SlabArena, ALLOC_ALIGN};
use crate::{
    BufferImpl, DevBuffer, Device, Event, EventImpl, ExecGraph, GraphImpl, KernelHandle,
    KernelImpl, LaunchArgs, LaunchConfig, Module, ModuleImpl, Pool, Stream, StreamImpl,
};

fn cu_err(context: &str, err: DriverError) -> ForgeError {
    ForgeError::Device(format!("{context}: {err}"))
}

/// Explicit byte budgets for the three VRAM pools. Constructing a
/// `CudaDevice` claims exactly these amounts up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizes {
    pub weights: usize,
    pub kv_cache: usize,
    /// Slab granularity of the KV pool; KV allocations round up to pages.
    pub kv_page_size: usize,
    pub activations: usize,
}

impl PoolSizes {
    /// Default KV page: 256 KiB holds a 16-token page of a 7B-class layer's
    /// K+V in fp16 with headroom, while keeping free-list churn negligible.
    pub const DEFAULT_KV_PAGE: usize = 256 * 1024;

    /// Opt-in sizing from currently free VRAM: claims 90% of `free_bytes`,
    /// split 60/30/10 between weights, KV cache and activations.
    pub fn auto_from_free(free_bytes: usize) -> Self {
        let budget = free_bytes / 10 * 9;
        let weights = align_down(budget / 10 * 6);
        let kv_cache = align_down(budget / 10 * 3);
        let activations = align_down(budget - weights - kv_cache);
        Self {
            weights,
            kv_cache,
            kv_page_size: Self::DEFAULT_KV_PAGE,
            activations,
        }
    }

    fn total(&self) -> usize {
        self.weights + self.kv_cache + self.activations
    }
}

fn align_down(bytes: usize) -> usize {
    bytes / ALLOC_ALIGN * ALLOC_ALIGN
}

// --- Pools ----------------------------------------------------------------------

enum PoolArena {
    Bump(BumpArena),
    Slab(SlabArena),
    Ring(RingArena),
}

/// One pre-claimed VRAM region plus its sub-allocation policy. Buffers hold an
/// `Arc<CudaPool>`, so the base allocation outlives every sub-allocation; the
/// region itself is released only when the last user (device or buffer) drops.
struct CudaPool {
    ctx: Arc<CudaContext>,
    base: sys::CUdeviceptr,
    arena: Mutex<PoolArena>,
}

// CUdeviceptr is an address, not a host pointer; all mutation goes through the
// internal Mutex.
unsafe impl Send for CudaPool {}
unsafe impl Sync for CudaPool {}

impl CudaPool {
    fn new(ctx: &Arc<CudaContext>, capacity: usize, arena: PoolArena) -> Result<Arc<Self>> {
        ctx.bind_to_thread()
            .map_err(|e| cu_err("bind context", e))?;
        // A zero-capacity pool is valid (e.g. an embeddings-only engine with
        // no KV budget); it owns no VRAM and every alloc reports OOM.
        let base = if capacity == 0 {
            0
        } else {
            unsafe { result::malloc_sync(capacity) }
                .map_err(|e| cu_err("cuMemAlloc pool", e))?
        };
        Ok(Arc::new(Self {
            ctx: ctx.clone(),
            base,
            arena: Mutex::new(arena),
        }))
    }
}

impl Drop for CudaPool {
    fn drop(&mut self) {
        if self.base != 0 {
            let _ = self.ctx.bind_to_thread();
            let _ = unsafe { result::memory_free(self.base) };
        }
    }
}

// --- Buffers --------------------------------------------------------------------

enum Backing {
    Pooled {
        pool: Arc<CudaPool>,
        offset: usize,
        reserved: usize,
        /// Ring generation stamp; `None` for bump/slab pools.
        generation: Option<u64>,
    },
    Pinned {
        ctx: Arc<CudaContext>,
        ptr: *mut c_void,
    },
    Managed {
        ctx: Arc<CudaContext>,
        dptr: sys::CUdeviceptr,
    },
}

struct CudaBuffer {
    bytes: usize,
    kind: MemKind,
    backing: Backing,
}

impl CudaBuffer {
    fn ctx(&self) -> &Arc<CudaContext> {
        match &self.backing {
            Backing::Pooled { pool, .. } => &pool.ctx,
            Backing::Pinned { ctx, .. } => ctx,
            Backing::Managed { ctx, .. } => ctx,
        }
    }
}

// Raw pointers are uniquely owned; concurrent GPU access is stream-ordered by
// the caller exactly as raw CUDA requires.
unsafe impl Send for CudaBuffer {}
unsafe impl Sync for CudaBuffer {}

impl BufferImpl for CudaBuffer {
    fn len(&self) -> usize {
        self.bytes
    }

    fn kind(&self) -> MemKind {
        self.kind
    }

    fn device_ptr(&self) -> u64 {
        match &self.backing {
            Backing::Pooled { pool, offset, .. } => pool.base + *offset as u64,
            // Pinned host memory is UVA-mapped: the host address doubles as
            // the device-visible address on every 64-bit CUDA platform.
            Backing::Pinned { ptr, .. } => *ptr as u64,
            Backing::Managed { dptr, .. } => *dptr,
        }
    }

    fn host_ptr(&self) -> Option<*mut u8> {
        match &self.backing {
            Backing::Pooled { .. } => None,
            Backing::Pinned { ptr, .. } => Some(*ptr as *mut u8),
            Backing::Managed { dptr, .. } => Some(*dptr as *mut u8),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for CudaBuffer {
    fn drop(&mut self) {
        match &self.backing {
            Backing::Pooled {
                pool,
                offset,
                reserved,
                generation,
            } => {
                let mut arena = pool.arena.lock().expect("pool arena poisoned");
                match (&mut *arena, generation) {
                    (PoolArena::Slab(slab), _) => slab.free(*offset, *reserved),
                    (PoolArena::Ring(ring), Some(generation)) => ring.on_drop(*generation),
                    // Weights are never freed individually; the pool reclaims
                    // everything when the model is torn down.
                    (PoolArena::Bump(_), _) => {}
                    (PoolArena::Ring(_), None) => unreachable!("ring buffer without generation"),
                }
            }
            Backing::Pinned { ctx, ptr } => {
                let _ = ctx.bind_to_thread();
                let _ = unsafe { result::free_host(*ptr) };
            }
            Backing::Managed { ctx, dptr } => {
                let _ = ctx.bind_to_thread();
                let _ = unsafe { result::memory_free(*dptr) };
            }
        }
    }
}

// --- Streams / events -----------------------------------------------------------

/// Resources referenced by work recorded into a graph capture. Replay
/// dereferences buffer addresses and jumps into kernel code, so the resulting
/// graph must keep both alive: buffers pin their pool sub-ranges, kernels pin
/// their module image (unloading a CUmodule frees the code a captured launch
/// points at).
#[derive(Default)]
struct CaptureSet {
    buffers: Vec<DevBuffer>,
    kernels: Vec<KernelHandle>,
}

struct CudaStreamImpl {
    stream: Arc<CudaStream>,
    /// `Some` while this HAL is capturing a graph on the stream; collects
    /// resources referenced by captured work so the graph can retain them.
    capture_retained: Mutex<Option<CaptureSet>>,
}

impl StreamImpl for CudaStreamImpl {
    fn synchronize(&self) -> Result<()> {
        self.stream
            .synchronize()
            .map_err(|e| cu_err("cuStreamSynchronize", e))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct CudaEventImpl {
    event: CudaEvent,
}

impl EventImpl for CudaEventImpl {
    fn synchronize(&self) -> Result<()> {
        self.event
            .synchronize()
            .map_err(|e| cu_err("cuEventSynchronize", e))
    }

    fn is_complete(&self) -> Result<bool> {
        Ok(self.event.is_complete())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// --- Modules / kernels ----------------------------------------------------------

/// Sole owner of a loaded CUmodule. Shared (`Arc`) between the `Module`
/// handle and every `KernelHandle` resolved from it: a CUfunction is a
/// pointer into the module image, so dropping the module while a kernel
/// handle (or a captured graph referencing it) is alive would leave that
/// function dangling.
struct RawCudaModule {
    ctx: Arc<CudaContext>,
    module: sys::CUmodule,
}

// CUmodule handles are usable from any thread once the context is current.
unsafe impl Send for RawCudaModule {}
unsafe impl Sync for RawCudaModule {}

impl Drop for RawCudaModule {
    fn drop(&mut self) {
        let _ = self.ctx.bind_to_thread();
        let _ = unsafe { result::module::unload(self.module) };
    }
}

struct CudaModuleImpl {
    raw: Arc<RawCudaModule>,
}

impl ModuleImpl for CudaModuleImpl {
    fn kernel(&self, name: &str) -> Result<KernelHandle> {
        self.raw
            .ctx
            .bind_to_thread()
            .map_err(|e| cu_err("bind context", e))?;
        let c_name = CString::new(name)
            .map_err(|_| ForgeError::Kernel(format!("kernel name contains NUL: {name:?}")))?;
        let func = unsafe { result::module::get_function(self.raw.module, c_name) }
            .map_err(|e| cu_err(&format!("cuModuleGetFunction({name})"), e))?;
        Ok(KernelHandle::from_impl(Arc::new(CudaKernelImpl {
            func,
            name: name.to_string(),
            module: self.raw.clone(),
        })))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct CudaKernelImpl {
    func: sys::CUfunction,
    name: String,
    /// Keeps the module image loaded for as long as this function handle exists.
    module: Arc<RawCudaModule>,
}

// CUfunction is a handle into the loaded module image; launches are guarded by
// context binding.
unsafe impl Send for CudaKernelImpl {}
unsafe impl Sync for CudaKernelImpl {}

impl KernelImpl for CudaKernelImpl {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// --- Graphs ---------------------------------------------------------------------

struct CudaGraphImpl {
    ctx: Arc<CudaContext>,
    graph: sys::CUgraph,
    exec: sys::CUgraphExec,
    /// CUDA graph objects are not internally synchronized; serialize access.
    launch_lock: Mutex<()>,
    /// Buffers and kernels referenced by captured work; replay dereferences
    /// their addresses/code, so they must live as long as the graph.
    _retained: CaptureSet,
}

unsafe impl Send for CudaGraphImpl {}
unsafe impl Sync for CudaGraphImpl {}

impl GraphImpl for CudaGraphImpl {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for CudaGraphImpl {
    fn drop(&mut self) {
        let _ = self.ctx.bind_to_thread();
        let _ = unsafe { result::graph::exec_destroy(self.exec) };
        let _ = unsafe { result::graph::destroy(self.graph) };
    }
}

// --- Device ---------------------------------------------------------------------

/// A graph launch whose replay may still be in flight on the GPU. Holding the
/// graph impl keeps its exec graph and retained buffers alive even if the
/// caller drops the `ExecGraph` handle right after `launch_graph` — otherwise
/// the retained pool ranges would be recycled while the replay still reads them.
struct PendingGraphLaunch {
    event: CudaEvent,
    _graph: Arc<dyn GraphImpl>,
}

pub struct CudaDevice {
    ctx: Arc<CudaContext>,
    caps: DeviceCaps,
    weights: Arc<CudaPool>,
    kv_cache: Arc<CudaPool>,
    activations: Arc<CudaPool>,
    /// Completed entries are pruned (non-blocking event query) on subsequent
    /// graph launches and on `synchronize`.
    pending_graph_launches: Mutex<Vec<PendingGraphLaunch>>,
}

impl CudaDevice {
    /// Open device `ordinal` and claim exactly the given pool budgets.
    pub fn new(ordinal: usize, pools: PoolSizes) -> Result<Arc<Self>> {
        let ctx = CudaContext::new(ordinal)
            .map_err(|e| cu_err(&format!("CudaContext::new({ordinal})"), e))?;
        let (free, _total) = ctx
            .mem_get_info()
            .map_err(|e| cu_err("cuMemGetInfo", e))?;
        if pools.total() > free {
            return Err(ForgeError::OutOfMemory {
                requested: pools.total(),
                available: free,
            });
        }
        let caps = detect_caps(&ctx)?;
        let weights = CudaPool::new(&ctx, pools.weights, PoolArena::Bump(BumpArena::new(pools.weights)))?;
        let kv_cache = CudaPool::new(
            &ctx,
            pools.kv_cache,
            PoolArena::Slab(SlabArena::new(pools.kv_cache, pools.kv_page_size)?),
        )?;
        let activations = CudaPool::new(
            &ctx,
            pools.activations,
            PoolArena::Ring(RingArena::new(pools.activations)),
        )?;
        Ok(Arc::new(Self {
            ctx,
            caps,
            weights,
            kv_cache,
            activations,
            pending_graph_launches: Mutex::new(Vec::new()),
        }))
    }

    /// Open device `ordinal` with the opt-in default budget: 90% of the VRAM
    /// free at construction time (see `PoolSizes::auto_from_free`).
    pub fn with_default_pools(ordinal: usize) -> Result<Arc<Self>> {
        let ctx = CudaContext::new(ordinal)
            .map_err(|e| cu_err(&format!("CudaContext::new({ordinal})"), e))?;
        let (free, _total) = ctx
            .mem_get_info()
            .map_err(|e| cu_err("cuMemGetInfo", e))?;
        Self::new(ordinal, PoolSizes::auto_from_free(free))
    }

    /// `(free, total)` device memory in bytes.
    pub fn mem_info(&self) -> Result<(usize, usize)> {
        self.ctx
            .mem_get_info()
            .map_err(|e| cu_err("cuMemGetInfo", e))
    }

    /// Free VRAM in bytes without retaining a device — for sizing pools before
    /// the arenas (which grab their whole budget up front) are created.
    pub fn free_vram(ordinal: usize) -> Result<usize> {
        let ctx = CudaContext::new(ordinal)
            .map_err(|e| cu_err(&format!("CudaContext::new({ordinal})"), e))?;
        let (free, _total) = ctx
            .mem_get_info()
            .map_err(|e| cu_err("cuMemGetInfo", e))?;
        Ok(free)
    }

    fn pool(&self, pool: Pool) -> &Arc<CudaPool> {
        match pool {
            Pool::Weights => &self.weights,
            Pool::KvCache => &self.kv_cache,
            Pool::Activations => &self.activations,
        }
    }

    fn bind(&self) -> Result<()> {
        self.ctx
            .bind_to_thread()
            .map_err(|e| cu_err("bind context", e))
    }

    /// Rejects handles whose owning context is a different physical device.
    /// The `Any` downcast only proves the backend type; two `CudaDevice`s
    /// would otherwise silently accept each other's streams/buffers.
    fn check_same_device(&self, other: &Arc<CudaContext>, what: &str) -> Result<()> {
        if other.ordinal() != self.ctx.ordinal() {
            return Err(ForgeError::Device(format!(
                "{what} belongs to CUDA device {} but was used on device {}",
                other.ordinal(),
                self.ctx.ordinal()
            )));
        }
        Ok(())
    }
}

fn detect_caps(ctx: &Arc<CudaContext>) -> Result<DeviceCaps> {
    let name = ctx.name().map_err(|e| cu_err("cuDeviceGetName", e))?;
    let (major, minor) = ctx
        .compute_capability()
        .map_err(|e| cu_err("compute capability", e))?;
    let sm = major * 10 + minor;
    let attr = |a: sys::CUdevice_attribute| -> Result<i32> {
        ctx.attribute(a)
            .map_err(|e| cu_err("cuDeviceGetAttribute", e))
    };
    let max_shared_mem_per_block = attr(
        sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN,
    )? as usize;
    let max_threads_per_block =
        attr(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK)? as u32;
    let warp_size = attr(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_WARP_SIZE)? as u32;
    let sm_count =
        attr(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT)? as u32;
    let supports_p2p = detect_p2p(ctx)?;
    Ok(DeviceCaps {
        name,
        vendor: Vendor::Nvidia,
        arch: format!("sm_{major}{minor}"),
        total_memory: ctx.total_mem().map_err(|e| cu_err("cuDeviceTotalMem", e))?,
        max_shared_mem_per_block,
        max_threads_per_block,
        warp_size,
        sm_count,
        // FP8 matmul is native from Ada (sm_89); FP4 tensor cores from
        // Blackwell (sm_100). Below those, NVFP4/FP8 take the software
        // fused-dequant path.
        fp8_native: sm >= 89,
        fp4_native: sm >= 100,
        bf16_native: sm >= 80,
        supports_p2p,
        supports_graph_capture: true,
    })
}

fn detect_p2p(ctx: &Arc<CudaContext>) -> Result<bool> {
    let count = CudaContext::device_count().map_err(|e| cu_err("cuDeviceGetCount", e))? as usize;
    for peer in 0..count {
        if peer == ctx.ordinal() {
            continue;
        }
        let mut accessible: i32 = 0;
        unsafe {
            sys::cuDeviceCanAccessPeer(&mut accessible, ctx.cu_device(), peer as i32)
        }
        .result()
        .map_err(|e| cu_err("cuDeviceCanAccessPeer", e))?;
        if accessible != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

// Checked add: `offset + bytes` overflowing in release would pass the bound
// and issue an out-of-range CUDA copy.
fn bounds_check(buf: &DevBuffer, offset: usize, bytes: usize) -> Result<()> {
    match offset.checked_add(bytes) {
        Some(end) if end <= buf.len() => Ok(()),
        _ => Err(ForgeError::Device(format!(
            "range at offset {} for {} byte(s) exceeds buffer size {}",
            offset,
            bytes,
            buf.len()
        ))),
    }
}

impl Device for CudaDevice {
    fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    fn alloc(&self, bytes: usize, kind: MemKind, pool: Pool) -> Result<DevBuffer> {
        self.bind()?;
        let backing = match kind {
            MemKind::Device => {
                let pool = self.pool(pool).clone();
                let (offset, reserved, generation) = {
                    let mut arena = pool.arena.lock().expect("pool arena poisoned");
                    match &mut *arena {
                        PoolArena::Bump(bump) => {
                            let offset = bump.alloc(bytes)?;
                            (offset, 0, None)
                        }
                        PoolArena::Slab(slab) => {
                            let (offset, reserved) = slab.alloc(bytes)?;
                            (offset, reserved, None)
                        }
                        PoolArena::Ring(ring) => {
                            let (offset, generation) = ring.alloc(bytes)?;
                            (offset, 0, Some(generation))
                        }
                    }
                };
                Backing::Pooled {
                    pool,
                    offset,
                    reserved,
                    generation,
                }
            }
            MemKind::PinnedHost => {
                let ptr = unsafe { result::malloc_host(bytes.max(1), 0) }
                    .map_err(|e| cu_err("cuMemHostAlloc", e))?;
                Backing::Pinned {
                    ctx: self.ctx.clone(),
                    ptr,
                }
            }
            MemKind::Managed => {
                let dptr = unsafe {
                    result::malloc_managed(
                        bytes.max(1),
                        sys::CUmemAttach_flags::CU_MEM_ATTACH_GLOBAL,
                    )
                }
                .map_err(|e| cu_err("cuMemAllocManaged", e))?;
                Backing::Managed {
                    ctx: self.ctx.clone(),
                    dptr,
                }
            }
        };
        Ok(DevBuffer::from_impl(Arc::new(CudaBuffer {
            bytes,
            kind,
            backing,
        })))
    }

    fn create_stream(&self) -> Result<Stream> {
        let stream = self
            .ctx
            .new_stream()
            .map_err(|e| cu_err("cuStreamCreate", e))?;
        Ok(Stream::from_impl(Arc::new(CudaStreamImpl {
            stream,
            capture_retained: Mutex::new(None),
        })))
    }

    fn create_event(&self) -> Result<Event> {
        let event = self
            .ctx
            .new_event(None)
            .map_err(|e| cu_err("cuEventCreate", e))?;
        Ok(Event::from_impl(Arc::new(CudaEventImpl { event })))
    }

    fn record_event(&self, event: &Event, stream: &Stream) -> Result<()> {
        let event = event.downcast::<CudaEventImpl>()?;
        let stream = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(event.event.context(), "Event")?;
        self.check_same_device(stream.stream.context(), "Stream")?;
        event
            .event
            .record(&stream.stream)
            .map_err(|e| cu_err("cuEventRecord", e))
    }

    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
        let event = event.downcast::<CudaEventImpl>()?;
        let stream = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(event.event.context(), "Event")?;
        self.check_same_device(stream.stream.context(), "Stream")?;
        stream
            .stream
            .wait(&event.event)
            .map_err(|e| cu_err("cuStreamWaitEvent", e))
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
        self.check_same_device(src.downcast::<CudaBuffer>()?.ctx(), "source DevBuffer")?;
        self.check_same_device(dst.downcast::<CudaBuffer>()?.ctx(), "destination DevBuffer")?;
        let stream_impl = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(stream_impl.stream.context(), "Stream")?;
        bounds_check(src, src_offset, bytes)?;
        bounds_check(dst, dst_offset, bytes)?;
        self.bind()?;
        // Every HAL buffer address is UVA (device pools, pinned host, managed),
        // so the direction-agnostic cuMemcpyAsync covers H2D/D2H/D2D/H2H with
        // stream ordering in one call.
        unsafe {
            sys::cuMemcpyAsync(
                dst.device_ptr() + dst_offset as u64,
                src.device_ptr() + src_offset as u64,
                bytes,
                stream_impl.stream.cu_stream(),
            )
        }
        .result()
        .map_err(|e| cu_err("cuMemcpyAsync", e))?;
        // Keep both endpoints alive if this copy is being captured into a graph.
        if let Some(retained) = stream_impl
            .capture_retained
            .lock()
            .expect("capture state poisoned")
            .as_mut()
        {
            retained.buffers.push(src.clone());
            retained.buffers.push(dst.clone());
        }
        Ok(())
    }

    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
        self.check_same_device(dst.downcast::<CudaBuffer>()?.ctx(), "DevBuffer")?;
        bounds_check(dst, dst_offset, src.len())?;
        self.bind()?;
        unsafe {
            result::memcpy_htod_sync(dst.device_ptr() + dst_offset as u64, src)
                .map_err(|e| cu_err("cuMemcpyHtoD", e))?;
            // cuMemcpyHtoD from PAGEABLE memory returns once the source is
            // staged; the DMA to the device is still in flight on the legacy
            // default stream. Streams created by this HAL are NON_BLOCKING,
            // so a kernel launched right after would NOT wait for that DMA
            // and could read (or be overwritten by) a half-arrived buffer.
            // Draining the legacy stream makes `write` truly synchronous.
            result::stream::synchronize(std::ptr::null_mut())
                .map_err(|e| cu_err("cuStreamSynchronize(legacy)", e))
        }
    }

    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
        self.check_same_device(src.downcast::<CudaBuffer>()?.ctx(), "DevBuffer")?;
        bounds_check(src, src_offset, dst.len())?;
        self.bind()?;
        unsafe {
            result::memcpy_dtoh_sync(dst, src.device_ptr() + src_offset as u64)
                .map_err(|e| cu_err("cuMemcpyDtoH", e))
        }
    }

    fn load_module(&self, image: &[u8]) -> Result<Module> {
        self.bind()?;
        // cuModuleLoadData requires PTX text to be NUL-terminated; normalize
        // here so callers can pass `include_bytes!`/`as_bytes` directly.
        let module = if image.last() == Some(&0) {
            unsafe { result::module::load_data(image.as_ptr() as *const c_void) }
        } else {
            let mut owned = Vec::with_capacity(image.len() + 1);
            owned.extend_from_slice(image);
            owned.push(0);
            unsafe { result::module::load_data(owned.as_ptr() as *const c_void) }
        }
        .map_err(|e| cu_err("cuModuleLoadData", e))?;
        Ok(Module::from_impl(Arc::new(CudaModuleImpl {
            raw: Arc::new(RawCudaModule {
                ctx: self.ctx.clone(),
                module,
            }),
        })))
    }

    fn launch(
        &self,
        kernel: &KernelHandle,
        cfg: &LaunchConfig,
        args: &LaunchArgs,
        stream: &Stream,
    ) -> Result<()> {
        let kernel_impl = kernel.downcast::<CudaKernelImpl>()?;
        let stream_impl = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(&kernel_impl.module.ctx, "KernelHandle")?;
        self.check_same_device(stream_impl.stream.context(), "Stream")?;
        self.bind()?;
        // Kernel params are pointers to the (address-stable) 8-byte slots the
        // LaunchArgs builder collected; cuLaunchKernel copies the values out
        // before returning, so slot lifetime only needs to span this call.
        let mut params: Vec<*mut c_void> = args
            .slots()
            .iter()
            .map(|slot| slot as *const u64 as *mut c_void)
            .collect();
        // Blocks requesting more than the 48 KB default dynamic shared memory
        // (e.g. the vendored Q4_K MMQ kernel, up to ~58 KB) must opt in per
        // function via CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES. Setting it
        // is idempotent and immediate (not a stream op), so it is safe to repeat
        // and safe under graph capture.
        const DEFAULT_SMEM_LIMIT: u32 = 48 * 1024;
        if cfg.shared_mem_bytes > DEFAULT_SMEM_LIMIT {
            unsafe {
                result::function::set_function_attribute(
                    kernel_impl.func,
                    sys::CUfunction_attribute_enum::CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES,
                    cfg.shared_mem_bytes as i32,
                )
            }
            .map_err(|e| {
                cu_err(&format!("cuFuncSetAttribute({})", kernel_impl.name), e)
            })?;
        }
        unsafe {
            result::launch_kernel(
                kernel_impl.func,
                cfg.grid,
                cfg.block,
                cfg.shared_mem_bytes,
                stream_impl.stream.cu_stream(),
                &mut params,
            )
        }
        .map_err(|e| cu_err(&format!("cuLaunchKernel({})", kernel_impl.name), e))?;
        if let Some(retained) = stream_impl
            .capture_retained
            .lock()
            .expect("capture state poisoned")
            .as_mut()
        {
            retained.buffers.extend(args.retained().iter().cloned());
            retained.kernels.push(kernel.clone());
        }
        Ok(())
    }

    fn synchronize(&self) -> Result<()> {
        self.ctx
            .synchronize()
            .map_err(|e| cu_err("cuCtxSynchronize", e))?;
        // Everything submitted has completed, so no graph replay can still be
        // reading its retained buffers.
        self.pending_graph_launches
            .lock()
            .expect("pending graph launches poisoned")
            .clear();
        Ok(())
    }

    fn begin_capture(&self, stream: &Stream) -> Result<()> {
        let stream_impl = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(stream_impl.stream.context(), "Stream")?;
        let mut retained = stream_impl
            .capture_retained
            .lock()
            .expect("capture state poisoned");
        if retained.is_some() {
            return Err(ForgeError::Device(
                "stream is already capturing a graph".to_string(),
            ));
        }
        // Thread-local mode keeps capture from poisoning unrelated CUDA work
        // on other threads (global mode would fail their API calls).
        stream_impl
            .stream
            .begin_capture(sys::CUstreamCaptureMode::CU_STREAM_CAPTURE_MODE_THREAD_LOCAL)
            .map_err(|e| cu_err("cuStreamBeginCapture", e))?;
        *retained = Some(CaptureSet::default());
        Ok(())
    }

    fn end_capture(&self, stream: &Stream) -> Result<ExecGraph> {
        let stream_impl = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(stream_impl.stream.context(), "Stream")?;
        let retained = stream_impl
            .capture_retained
            .lock()
            .expect("capture state poisoned")
            .take()
            .ok_or_else(|| {
                ForgeError::Device("end_capture without begin_capture on this stream".to_string())
            })?;
        self.bind()?;
        let graph = unsafe { result::stream::end_capture(stream_impl.stream.cu_stream()) }
            .map_err(|e| cu_err("cuStreamEndCapture", e))?;
        if graph.is_null() {
            return Err(ForgeError::Device(
                "stream capture produced no graph (capture was invalidated)".to_string(),
            ));
        }
        // Instantiate with flags=0: no auto-free (the HAL owns memory), no
        // device launch. Raw sys call because cudarc's enum wrapper cannot
        // express an empty flag set.
        let mut exec = std::ptr::null_mut();
        let instantiate =
            unsafe { sys::cuGraphInstantiateWithFlags(&mut exec, graph, 0) }.result();
        if let Err(e) = instantiate {
            let _ = unsafe { result::graph::destroy(graph) };
            return Err(cu_err("cuGraphInstantiateWithFlags", e));
        }
        Ok(ExecGraph::from_impl(Arc::new(CudaGraphImpl {
            ctx: self.ctx.clone(),
            graph,
            exec,
            launch_lock: Mutex::new(()),
            _retained: retained,
        })))
    }

    fn launch_graph(&self, graph: &ExecGraph, stream: &Stream) -> Result<()> {
        let graph_impl = graph.downcast::<CudaGraphImpl>()?;
        let stream_impl = stream.downcast::<CudaStreamImpl>()?;
        self.check_same_device(&graph_impl.ctx, "ExecGraph")?;
        self.check_same_device(stream_impl.stream.context(), "Stream")?;
        self.bind()?;
        {
            let _guard = graph_impl.launch_lock.lock().expect("graph lock poisoned");
            unsafe { result::graph::launch(graph_impl.exec, stream_impl.stream.cu_stream()) }
                .map_err(|e| cu_err("cuGraphLaunch", e))?;
        }
        // Replay is asynchronous: pin the graph impl (and thus its retained
        // buffers/kernels) until this launch completes on the stream, so
        // dropping the caller's ExecGraph handle right away cannot recycle
        // memory the GPU is still reading. Pruning is a non-blocking query.
        let event = self
            .ctx
            .new_event(None)
            .map_err(|e| cu_err("cuEventCreate", e))?;
        event
            .record(&stream_impl.stream)
            .map_err(|e| cu_err("cuEventRecord", e))?;
        let mut pending = self
            .pending_graph_launches
            .lock()
            .expect("pending graph launches poisoned");
        pending.retain(|p| !p.event.is_complete());
        pending.push(PendingGraphLaunch {
            event,
            _graph: graph.0.clone(),
        });
        Ok(())
    }

    fn reset_activations(&self) -> Result<u64> {
        let mut arena = self.activations.arena.lock().expect("pool arena poisoned");
        match &mut *arena {
            PoolArena::Ring(ring) => ring.reset(),
            _ => unreachable!("activations pool is always a ring"),
        }
    }
}
