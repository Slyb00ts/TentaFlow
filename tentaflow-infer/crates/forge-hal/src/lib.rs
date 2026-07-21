// ===== File: lib.rs — forge-hal: hardware abstraction layer traits (Device/Stream/Event/ExecGraph) and shared handle types =====
//
// The HAL exposes vendor-neutral, object-safe traits so the execution layer can
// hold `Arc<dyn Device>` and never mention CUDA/HIP/Metal directly. Backend
// resources (streams, events, buffers, graphs) are opaque handle structs that
// wrap `Arc<dyn ...Impl>`; backends downcast them via `Any` and reject handles
// created by a different backend instead of silently misbehaving.

// Offset arenas are consumed by GPU backends only; the CPU backend allocates
// per-buffer from the host allocator.
#[cfg(feature = "cuda")]
pub(crate) mod arena;
pub mod cpu;
#[cfg(feature = "cuda")]
pub mod cuda;

use std::any::Any;
use std::sync::Arc;

use forge_types::{DeviceCaps, ForgeError, MemKind, Result};

/// Logical VRAM sub-pool an allocation is served from (spec §3.2). Only
/// meaningful for `MemKind::Device`; host-side kinds ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pool {
    /// Bump-allocated, lives for the model lifetime; individual frees are no-ops.
    Weights,
    /// Fixed-size page slabs with a free list; alloc/free churns per sequence.
    KvCache,
    /// Per-iteration ring: freed wholesale via `Device::reset_activations`.
    Activations,
}

/// Kernel launch geometry. Mirrors CUDA semantics; other backends map or reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchConfig {
    pub grid: (u32, u32, u32),
    pub block: (u32, u32, u32),
    pub shared_mem_bytes: u32,
}

impl LaunchConfig {
    /// 1-D launch covering `n` elements with `block_size` threads per block.
    pub fn linear(n: u32, block_size: u32) -> Self {
        Self {
            grid: (n.div_ceil(block_size.max(1)), 1, 1),
            block: (block_size.max(1), 1, 1),
            shared_mem_bytes: 0,
        }
    }
}

/// Scalars that can be passed by value as kernel parameters. Sealed: the set
/// must match what backends know how to marshal into an 8-byte launch slot.
pub trait KernelScalar: sealed::Sealed + Copy {
    #[doc(hidden)]
    fn to_slot(self) -> u64;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! impl_kernel_scalar {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl KernelScalar for $t {
            // Little-endian low-bytes placement: cuLaunchKernel reads exactly
            // `sizeof(param)` bytes from the slot address, so narrower scalars
            // occupy the low bytes of the u64 slot.
            fn to_slot(self) -> u64 {
                let mut slot = [0u8; 8];
                let bytes = self.to_le_bytes();
                slot[..bytes.len()].copy_from_slice(&bytes);
                u64::from_le_bytes(slot)
            }
        }
    )*};
}

impl_kernel_scalar!(i32, u32, i64, u64, f32, f64);

impl sealed::Sealed for usize {}
impl KernelScalar for usize {
    fn to_slot(self) -> u64 {
        self as u64
    }
}

/// Typed kernel-argument builder. Each argument occupies one address-stable
/// 8-byte slot; buffers contribute their raw device address and are retained
/// (Arc clone) so the asynchronous launch cannot outlive the allocation.
#[derive(Default)]
pub struct LaunchArgs {
    slots: Vec<u64>,
    retained: Vec<DevBuffer>,
}

impl LaunchArgs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pass the buffer's base device address as a pointer argument.
    pub fn buf(mut self, buffer: &DevBuffer) -> Self {
        self.slots.push(buffer.device_ptr());
        self.retained.push(buffer.clone());
        self
    }

    /// Pass a pointer argument offset `byte_offset` into the buffer.
    /// Errors instead of silently producing an out-of-bounds device pointer.
    pub fn buf_at(mut self, buffer: &DevBuffer, byte_offset: usize) -> Result<Self> {
        if byte_offset > buffer.len() {
            return Err(ForgeError::Device(format!(
                "kernel arg offset {byte_offset} exceeds buffer size {}",
                buffer.len()
            )));
        }
        self.slots.push(buffer.device_ptr() + byte_offset as u64);
        self.retained.push(buffer.clone());
        Ok(self)
    }

    /// Pass a scalar by value.
    pub fn scalar<T: KernelScalar>(mut self, value: T) -> Self {
        self.slots.push(value.to_slot());
        self
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Raw parameter-slot values; backends build the `void*[]` kernel-param
    /// array by taking the address of each slot (slots are address-stable for
    /// the borrow's duration since `&self` prevents reallocation).
    pub fn slots(&self) -> &[u64] {
        &self.slots
    }

    /// Buffers referenced by pointer arguments; backends capturing a graph
    /// retain these so replays cannot dereference freed memory.
    pub fn retained(&self) -> &[DevBuffer] {
        &self.retained
    }
}

// --- Backend-implementation traits (crate-internal contract) -----------------
//
// Public because backend modules implement them, but the execution layer only
// consumes the opaque handle structs below.

// Emptiness is answered by the `DevBuffer` handle, not per backend.
#[allow(clippy::len_without_is_empty)]
pub trait BufferImpl: Send + Sync {
    /// Requested allocation size in bytes.
    fn len(&self) -> usize;
    fn kind(&self) -> MemKind;
    /// Address valid for kernel arguments on the owning device (UVA for
    /// pinned host memory; plain host address on CPU).
    fn device_ptr(&self) -> u64;
    /// Host-visible base address, when the memory is host-accessible.
    fn host_ptr(&self) -> Option<*mut u8>;
    fn as_any(&self) -> &dyn Any;
}

pub trait StreamImpl: Send + Sync {
    fn synchronize(&self) -> Result<()>;
    fn as_any(&self) -> &dyn Any;
}

pub trait EventImpl: Send + Sync {
    fn synchronize(&self) -> Result<()>;
    fn is_complete(&self) -> Result<bool>;
    fn as_any(&self) -> &dyn Any;
}

pub trait ModuleImpl: Send + Sync {
    fn kernel(&self, name: &str) -> Result<KernelHandle>;
    fn as_any(&self) -> &dyn Any;
}

pub trait KernelImpl: Send + Sync {
    fn name(&self) -> &str;
    fn as_any(&self) -> &dyn Any;
}

pub trait GraphImpl: Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

// --- Opaque handles -----------------------------------------------------------

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident, $imp:ident) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name(pub(crate) Arc<dyn $imp>);

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($name), "(backend impl @ {:p})"), Arc::as_ptr(&self.0))
            }
        }

        impl $name {
            pub fn from_impl(inner: Arc<dyn $imp>) -> Self {
                Self(inner)
            }

            // Uniform part of the handle contract; a given backend does not
            // necessarily need to downcast every handle type (e.g. Module is
            // consumed only through its own vtable), so per-type dead-code
            // analysis is suppressed.
            #[allow(dead_code)]
            pub(crate) fn downcast<T: 'static>(&self) -> Result<&T> {
                self.0.as_any().downcast_ref::<T>().ok_or_else(|| {
                    ForgeError::Device(format!(
                        concat!(stringify!($name), " belongs to a different backend ({})"),
                        std::any::type_name::<T>()
                    ))
                })
            }
        }
    };
}

handle!(
    /// Device memory allocation (RAII: dropping the last clone returns the
    /// memory to its pool / frees it).
    DevBuffer,
    BufferImpl
);
handle!(
    /// Ordered asynchronous work queue on a device.
    Stream,
    StreamImpl
);
handle!(
    /// Cross-stream synchronization / completion marker.
    Event,
    EventImpl
);
handle!(
    /// Loaded kernel module (e.g. a PTX image) from which kernels are resolved.
    Module,
    ModuleImpl
);
handle!(
    /// Resolved kernel entry point, launchable via `Device::launch`.
    KernelHandle,
    KernelImpl
);
handle!(
    /// Captured, replayable execution graph (CUDA Graphs or equivalent).
    ExecGraph,
    GraphImpl
);

impl DevBuffer {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    pub fn kind(&self) -> MemKind {
        self.0.kind()
    }

    pub fn device_ptr(&self) -> u64 {
        self.0.device_ptr()
    }

    /// Host-visible pointer for `PinnedHost`/`Managed`/CPU buffers, `None`
    /// for device-only memory.
    pub fn host_ptr(&self) -> Option<*mut u8> {
        self.0.host_ptr()
    }
}

impl Stream {
    /// Block until all work submitted to this stream has completed.
    pub fn synchronize(&self) -> Result<()> {
        self.0.synchronize()
    }
}

impl Event {
    /// Block until the recorded point has been reached.
    pub fn synchronize(&self) -> Result<()> {
        self.0.synchronize()
    }

    /// Non-blocking completion query.
    pub fn is_complete(&self) -> Result<bool> {
        self.0.is_complete()
    }
}

impl Module {
    /// Resolve a kernel entry point by its exported (extern "C") name.
    pub fn kernel(&self, name: &str) -> Result<KernelHandle> {
        self.0.kernel(name)
    }
}

impl KernelHandle {
    pub fn name(&self) -> &str {
        self.0.name()
    }
}

// --- The Device trait (spec §3.1) ----------------------------------------------

/// A single compute device (one GPU or the host CPU). Object-safe: the
/// execution layer holds `Arc<dyn Device>`.
///
/// Asynchrony contract: `copy` and `launch` are stream-ordered and return
/// before completion; `write`/`read` are synchronous staging helpers and must
/// not be called while a stream on this device is capturing a graph.
pub trait Device: Send + Sync {
    fn caps(&self) -> &DeviceCaps;

    /// Allocate `bytes` of memory. `pool` selects the VRAM sub-pool for
    /// `MemKind::Device`; host kinds allocate directly from the driver/OS.
    /// Freeing is RAII via `DevBuffer` drop.
    fn alloc(&self, bytes: usize, kind: MemKind, pool: Pool) -> Result<DevBuffer>;

    /// Liczba wolnych bajtów w puli urządzenia, gdy backend potrafi ją raportować.
    fn pool_available(&self, _pool: Pool) -> Option<usize> {
        None
    }

    fn create_stream(&self) -> Result<Stream>;

    fn create_event(&self) -> Result<Event>;

    /// Record `event` at the current tail of `stream`.
    fn record_event(&self, event: &Event, stream: &Stream) -> Result<()>;

    /// Make future work on `stream` wait until `event` completes.
    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()>;

    /// Stream-ordered copy between buffers; direction (H2D/D2H/D2D/H2H) is
    /// derived from the buffers' `MemKind`s. Host-side endpoints must be
    /// `PinnedHost` (or `Managed`) — pageable host memory never enters the
    /// async path.
    fn copy(
        &self,
        src: &DevBuffer,
        src_offset: usize,
        dst: &DevBuffer,
        dst_offset: usize,
        bytes: usize,
        stream: &Stream,
    ) -> Result<()>;

    /// Synchronous host→buffer staging write (init/load path, not hot path).
    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()>;

    /// Synchronous buffer→host staging read (debug/test path, not hot path).
    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()>;

    /// Load a kernel module from a PTX image (NUL-terminated text or binary
    /// fatbin/cubin, backend-defined). CPU backend returns `Unsupported`.
    fn load_module(&self, image: &[u8]) -> Result<Module>;

    /// Stream-ordered kernel launch. CPU backend returns `Unsupported`
    /// (host kernels are invoked natively by higher layers).
    fn launch(
        &self,
        kernel: &KernelHandle,
        cfg: &LaunchConfig,
        args: &LaunchArgs,
        stream: &Stream,
    ) -> Result<()>;

    /// Block until all work on the device has completed.
    fn synchronize(&self) -> Result<()>;

    /// Begin capturing work submitted to `stream` into a graph.
    fn begin_capture(&self, stream: &Stream) -> Result<()>;

    /// Finish capture and instantiate a replayable graph. Buffers referenced
    /// by launches recorded through this HAL are retained by the graph.
    fn end_capture(&self, stream: &Stream) -> Result<ExecGraph>;

    /// Replay a captured graph on `stream`.
    fn launch_graph(&self, graph: &ExecGraph, stream: &Stream) -> Result<()>;

    /// Retire the current activations generation: outstanding `Activations`
    /// buffers become logically dead and their memory is reused. Errors if
    /// live buffers from the current generation still exist. Returns the new
    /// generation number.
    fn reset_activations(&self) -> Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_slots_use_low_bytes() {
        assert_eq!(1.0f32.to_slot(), f32::to_bits(1.0) as u64);
        assert_eq!((-1i32).to_slot(), 0xFFFF_FFFFu64);
        assert_eq!(7usize.to_slot(), 7);
        assert_eq!((-1i64).to_slot(), u64::MAX);
    }

    #[test]
    fn launch_args_collects_slots() {
        let args = LaunchArgs::new().scalar(3i32).scalar(0.5f32);
        assert_eq!(args.len(), 2);
        assert_eq!(args.slots()[0], 3);
        assert_eq!(args.slots()[1], f32::to_bits(0.5) as u64);
    }
}
