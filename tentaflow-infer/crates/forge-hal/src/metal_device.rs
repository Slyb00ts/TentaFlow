// ===== File: metal_device.rs — the Device trait over the Metal context =====
//
// What the rest of FORGE sees. Three contract points differ from the CUDA
// backend and each difference is deliberate:
//
//   * `alloc` returns HOST-VISIBLE memory. On unified memory there is no
//     transfer, so `write` and `read` are memcpy on the same address and a
//     model load moves nothing.
//   * A stream owns ONE open command buffer that accumulates dispatches until
//     something forces a submission. This is the whole reason the backend is
//     shaped this way: 0.61 us per dispatch inside a buffer, 19.6 us for a
//     buffer of its own, ~94 us for a host round trip (EKS-A3).
//   * Graph capture is refused rather than emulated. It buys nothing here —
//     batching already removes what a graph would remove — and a fake capture
//     that silently does nothing is worse than an error.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use forge_types::{DeviceCaps, ForgeError, MemKind, Result, Vendor};

use crate::metal::{MetalArg, MetalBuffer, MetalCommandBuffer, MetalContext, MetalPipeline};
use crate::{
    ArgKind, BufferImpl, DevBuffer, Device, Event, EventImpl, ExecGraph, KernelHandle, KernelImpl,
    LaunchArgs, LaunchConfig, Module, ModuleImpl, Pool, Stream, StreamImpl,
};

pub struct MetalDevice {
    ctx: Arc<MetalContext>,
    caps: DeviceCaps,
    activations_generation: AtomicU64,
}

impl MetalDevice {
    pub fn new() -> Result<Arc<Self>> {
        let ctx = Arc::new(MetalContext::new()?);
        let m = ctx.caps();
        let caps = DeviceCaps {
            arch: m.name.to_lowercase().replace(' ', "-"),
            name: m.name,
            vendor: Vendor::Apple,
            total_memory: m.working_set_bytes as usize,
            max_shared_mem_per_block: m.threadgroup_memory_bytes as usize,
            max_threads_per_block: m.max_threads_per_group,
            // Apple executes in SIMD groups of 32 lanes.
            warp_size: 32,
            // Metal exposes no GPU-core count; leaving it 0 rather than
            // inventing one keeps every grid heuristic honest about not knowing.
            sm_count: 0,
            // Measured, not assumed: Metal 4.1 emulates fp8 at 0.94x fp16, and
            // there is no fp4 path at all on this generation. bf16 costs the
            // same as f16 (EKS-A2).
            fp8_native: false,
            fp4_native: false,
            bf16_native: true,
            supports_p2p: false,
            supports_graph_capture: false,
        };
        Ok(Arc::new(Self {
            ctx,
            caps,
            activations_generation: AtomicU64::new(0),
        }))
    }

    fn stream_of<'a>(&self, stream: &'a Stream) -> Result<&'a MetalStream> {
        stream.downcast::<MetalStream>()
    }
}

/// A buffer, or a window inside one. The parent is retained so a sub-buffer
/// cannot outlive the memory it points into.
struct MetalDevBuffer {
    parent: Arc<MetalBuffer>,
    offset: usize,
    len: usize,
    kind: MemKind,
}

impl MetalDevBuffer {
    fn host(&self) -> *mut u8 {
        unsafe { self.parent.as_ptr().add(self.offset) }
    }
}

impl BufferImpl for MetalDevBuffer {
    fn len(&self) -> usize {
        self.len
    }

    fn kind(&self) -> MemKind {
        self.kind
    }

    fn device_ptr(&self) -> u64 {
        // Unified memory: the host address IS the address the GPU sees. It is
        // informational here, because Metal binds buffers by handle and offset.
        self.host() as u64
    }

    fn host_ptr(&self) -> Option<*mut u8> {
        Some(self.host())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An ordered queue of work with one open command buffer.
struct MetalStream {
    ctx: Arc<MetalContext>,
    open: Mutex<Option<MetalCommandBuffer>>,
    submitted: Mutex<Vec<Arc<MetalCommandBuffer>>>,
}

impl MetalStream {
    /// Submits whatever is open and returns it, so an event can name it.
    /// Returns `None` when nothing was encoded.
    fn flush(&self) -> Result<Option<Arc<MetalCommandBuffer>>> {
        let taken = self.open.lock().expect("stream mutex").take();
        let Some(cb) = taken else {
            return Ok(None);
        };
        cb.commit();
        let cb = Arc::new(cb);
        self.submitted
            .lock()
            .expect("stream mutex")
            .push(cb.clone());
        Ok(Some(cb))
    }

    /// Runs `f` against the open command buffer, opening one if needed.
    fn with_open<R>(&self, f: impl FnOnce(&mut MetalCommandBuffer) -> Result<R>) -> Result<R> {
        let mut guard = self.open.lock().expect("stream mutex");
        if guard.is_none() {
            *guard = Some(self.ctx.command_buffer()?);
        }
        f(guard.as_mut().expect("just opened"))
    }
}

impl StreamImpl for MetalStream {
    fn synchronize(&self) -> Result<()> {
        self.flush()?;
        let pending: Vec<Arc<MetalCommandBuffer>> =
            std::mem::take(&mut *self.submitted.lock().expect("stream mutex"));
        for cb in pending {
            cb.wait()?;
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A completion marker naming the command buffer that was open when it was
/// recorded. Ordering between work items needs nothing more: a single command
/// queue runs its buffers in submission order.
struct MetalEvent {
    marked: Mutex<Option<Arc<MetalCommandBuffer>>>,
}

impl EventImpl for MetalEvent {
    fn synchronize(&self) -> Result<()> {
        let cb = self.marked.lock().expect("event mutex").clone();
        match cb {
            Some(cb) => cb.wait(),
            None => Ok(()),
        }
    }

    fn is_complete(&self) -> Result<bool> {
        let cb = self.marked.lock().expect("event mutex").clone();
        Ok(cb.is_none_or(|cb| cb.is_complete()))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MetalModule {
    ctx: Arc<MetalContext>,
    library: crate::metal::MetalLibrary,
}

impl ModuleImpl for MetalModule {
    fn kernel(self: Arc<Self>, name: &str) -> Result<KernelHandle> {
        let pipeline = self.ctx.pipeline(&self.library, name)?;
        Ok(KernelHandle::from_impl(Arc::new(MetalKernel {
            name: name.to_string(),
            pipeline,
            // Retaining the module keeps the library alive for as long as any
            // pipeline built from it — the HIP backend lost a day to exactly
            // this lifetime being implicit.
            _module: self,
        })))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MetalKernel {
    name: String,
    pipeline: MetalPipeline,
    _module: Arc<MetalModule>,
}

impl KernelImpl for MetalKernel {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Device for MetalDevice {
    fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    fn alloc(&self, bytes: usize, kind: MemKind, _pool: Pool) -> Result<DevBuffer> {
        // Pools are a VRAM-partitioning idea. With unified memory every
        // allocation comes from the same place, so the pool is recorded by the
        // caller and ignored here rather than faked.
        let buffer = Arc::new(self.ctx.alloc(bytes.max(1))?);
        Ok(DevBuffer::from_impl(Arc::new(MetalDevBuffer {
            parent: buffer,
            offset: 0,
            len: bytes,
            kind,
        })))
    }

    fn sub_buffer(&self, parent: &DevBuffer, offset: usize, len: usize) -> Result<DevBuffer> {
        crate::check_sub_range(parent.len(), offset, len)?;
        let p = parent.downcast::<MetalDevBuffer>()?;
        Ok(DevBuffer::from_impl(Arc::new(MetalDevBuffer {
            parent: p.parent.clone(),
            offset: p.offset + offset,
            len,
            kind: p.kind,
        })))
    }

    fn create_stream(&self) -> Result<Stream> {
        Ok(Stream::from_impl(Arc::new(MetalStream {
            ctx: self.ctx.clone(),
            open: Mutex::new(None),
            submitted: Mutex::new(Vec::new()),
        })))
    }

    fn create_event(&self) -> Result<Event> {
        Ok(Event::from_impl(Arc::new(MetalEvent {
            marked: Mutex::new(None),
        })))
    }

    fn record_event(&self, event: &Event, stream: &Stream) -> Result<()> {
        let s = self.stream_of(stream)?;
        let e = event.downcast::<MetalEvent>()?;
        let cb = s.flush()?;
        *e.marked.lock().expect("event mutex") = cb;
        Ok(())
    }

    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
        // Both sides live on one command queue, and a queue runs its buffers in
        // submission order, so the ordering the caller asks for already holds.
        // The downcasts stay: passing a handle from another backend must fail
        // here rather than at the first wrong result.
        self.stream_of(stream)?;
        event.downcast::<MetalEvent>()?;
        Ok(())
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
        crate::check_sub_range(src.len(), src_offset, bytes)?;
        crate::check_sub_range(dst.len(), dst_offset, bytes)?;
        let s = self.stream_of(stream)?;
        // Conservative: the copy is a host memcpy, so anything already encoded
        // has to have finished. A blit encoder would keep it on the GPU
        // timeline; that belongs with the kernels, not with the first slice.
        s.synchronize()?;
        let from = src.downcast::<MetalDevBuffer>()?;
        let to = dst.downcast::<MetalDevBuffer>()?;
        unsafe {
            std::ptr::copy(
                from.host().add(src_offset),
                to.host().add(dst_offset),
                bytes,
            );
        }
        Ok(())
    }

    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
        crate::check_sub_range(dst.len(), dst_offset, src.len())?;
        let to = dst.downcast::<MetalDevBuffer>()?;
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), to.host().add(dst_offset), src.len()) };
        Ok(())
    }

    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
        crate::check_sub_range(src.len(), src_offset, dst.len())?;
        let from = src.downcast::<MetalDevBuffer>()?;
        unsafe {
            std::ptr::copy_nonoverlapping(from.host().add(src_offset), dst.as_mut_ptr(), dst.len())
        };
        Ok(())
    }

    /// The module image is Metal Shading Language text, the way it is PTX text
    /// on CUDA. Compilation happens here, at load time, never in a hot path.
    fn load_module(&self, image: &[u8]) -> Result<Module> {
        let source = std::str::from_utf8(image).map_err(|_| {
            ForgeError::Format("Metal: obraz modułu nie jest tekstem UTF-8 (MSL)".into())
        })?;
        let library = self.ctx.library(source)?;
        Ok(Module::from_impl(Arc::new(MetalModule {
            ctx: self.ctx.clone(),
            library,
        })))
    }

    fn launch(
        &self,
        kernel: &KernelHandle,
        cfg: &LaunchConfig,
        args: &LaunchArgs,
        stream: &Stream,
    ) -> Result<()> {
        let k = kernel.downcast::<MetalKernel>()?;
        let s = self.stream_of(stream)?;

        // Grid z i oraz blok wielowymiarowy nie mają dziś kernela, który by ich
        // używał, więc są odrzucane zamiast po cichu spłaszczane.
        if cfg.grid.2 != 1 || cfg.block.1 != 1 || cfg.block.2 != 1 {
            return Err(ForgeError::Unsupported(format!(
                "Metal: dyspozycja {:?}/{:?} nie jest obsługiwana",
                cfg.grid, cfg.block
            )));
        }
        if cfg.shared_mem_bytes != 0 {
            return Err(ForgeError::Unsupported(
                "Metal: pamięć grupy roboczej deklaruje kernel, nie wywołanie".into(),
            ));
        }

        let retained = args.retained();
        let mut metal_args: Vec<MetalArg<'_>> = Vec::with_capacity(args.len());
        for (slot, kind) in args.slots().iter().zip(args.kinds()) {
            match kind {
                ArgKind::Scalar => metal_args.push(MetalArg::Scalar(*slot)),
                ArgKind::Buffer {
                    retained: idx,
                    byte_offset,
                } => {
                    let buf = retained
                        .get(*idx)
                        .ok_or_else(|| ForgeError::Device("Metal: brak bufora slotu".into()))?
                        .downcast::<MetalDevBuffer>()?;
                    metal_args.push(MetalArg::Buffer(
                        &buf.parent,
                        buf.offset as u64 + byte_offset,
                    ));
                }
            }
        }

        s.with_open(|cb| {
            cb.dispatch_2d(
                &k.pipeline,
                &metal_args,
                (cfg.grid.0, cfg.grid.1),
                cfg.block.0,
            )
        })
    }

    fn synchronize(&self) -> Result<()> {
        // An empty buffer submitted last on the queue completes only after
        // everything before it, so waiting on it waits for the device.
        self.ctx.command_buffer()?.commit_and_wait()
    }

    fn begin_capture(&self, _stream: &Stream) -> Result<()> {
        Err(ForgeError::Unsupported(
            "Metal: przechwytywanie grafu nie jest obsługiwane — dyspozycje pakuje \
             się do jednego bufora poleceń (EKS-A3)"
                .into(),
        ))
    }

    fn end_capture(&self, _stream: &Stream) -> Result<ExecGraph> {
        Err(ForgeError::Unsupported(
            "Metal: przechwytywanie grafu nie jest obsługiwane".into(),
        ))
    }

    fn launch_graph(&self, _graph: &ExecGraph, _stream: &Stream) -> Result<()> {
        Err(ForgeError::Unsupported(
            "Metal: przechwytywanie grafu nie jest obsługiwane".into(),
        ))
    }

    fn reset_activations(&self) -> Result<u64> {
        // Nothing to retire: allocations are direct, not carved out of an
        // arena, so a generation boundary frees nothing. The counter still
        // advances, because callers use it to tell steps apart.
        Ok(self.activations_generation.fetch_add(1, Ordering::SeqCst) + 1)
    }
}
