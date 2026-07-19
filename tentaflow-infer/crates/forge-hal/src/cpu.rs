// ===== File: cpu.rs — CPU backend: aligned host buffers, immediate "streams", reference/fallback device =====
//
// The CPU backend exists as the always-available reference target. Kernels are
// not launched through PTX here — higher layers call native host kernels
// directly — so `load_module`/`launch`/graph capture report `Unsupported`
// rather than pretending.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::any::Any;
use std::sync::Arc;

use forge_types::{DeviceCaps, ForgeError, MemKind, Result, Vendor};

use crate::{
    BufferImpl, DevBuffer, Device, Event, EventImpl, ExecGraph, KernelHandle, LaunchArgs,
    LaunchConfig, Module, Pool, Stream, StreamImpl,
};

/// Cache-line/SIMD-friendly alignment for host tensors (AVX-512 wants 64 B).
const HOST_ALIGN: usize = 64;

struct CpuBuffer {
    ptr: *mut u8,
    layout: Layout,
    kind: MemKind,
}

// The raw pointer is uniquely owned by this struct and freed on drop; access
// synchronization is the caller's responsibility exactly as with VRAM buffers.
unsafe impl Send for CpuBuffer {}
unsafe impl Sync for CpuBuffer {}

impl BufferImpl for CpuBuffer {
    fn len(&self) -> usize {
        self.layout.size()
    }

    fn kind(&self) -> MemKind {
        self.kind
    }

    fn device_ptr(&self) -> u64 {
        self.ptr as u64
    }

    fn host_ptr(&self) -> Option<*mut u8> {
        Some(self.ptr)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for CpuBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

/// Immediate-execution stream: every submitted operation completes before the
/// submitting call returns, so synchronization is a no-op.
struct CpuStream;

impl StreamImpl for CpuStream {
    fn synchronize(&self) -> Result<()> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// With immediate streams every recorded point is already complete.
struct CpuEvent;

impl EventImpl for CpuEvent {
    fn synchronize(&self) -> Result<()> {
        Ok(())
    }

    fn is_complete(&self) -> Result<bool> {
        Ok(true)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct CpuDevice {
    caps: DeviceCaps,
}

impl CpuDevice {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            caps: DeviceCaps {
                name: "host-cpu".to_string(),
                vendor: Vendor::Cpu,
                arch: std::env::consts::ARCH.to_string(),
                total_memory: host_total_memory(),
                max_shared_mem_per_block: 0,
                max_threads_per_block: 1,
                warp_size: 1,
                sm_count: 0,
                fp8_native: false,
                fp4_native: false,
                bf16_native: false,
                supports_p2p: false,
                supports_graph_capture: false,
            },
        })
    }
}

/// Total physical RAM. Linux reads /proc/meminfo; other hosts report 0
/// (meaning "unknown"), which callers must treat as "do not budget by caps".
fn host_total_memory() -> usize {
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            for line in meminfo.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Some(kb) = rest.split_whitespace().next() {
                        if let Ok(kb) = kb.parse::<usize>() {
                            return kb * 1024;
                        }
                    }
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

// Checked add: `offset + bytes` overflowing in release would pass the bound
// and feed an out-of-range offset into raw pointer arithmetic below.
fn buffer_bounds_check(buf: &DevBuffer, offset: usize, bytes: usize) -> Result<()> {
    match offset.checked_add(bytes) {
        Some(end) if end <= buf.len() => Ok(()),
        _ => Err(ForgeError::Device(format!(
            "copy range at offset {} for {} byte(s) exceeds buffer size {}",
            offset,
            bytes,
            buf.len()
        ))),
    }
}

impl Device for CpuDevice {
    fn caps(&self) -> &DeviceCaps {
        &self.caps
    }

    fn alloc(&self, bytes: usize, kind: MemKind, _pool: Pool) -> Result<DevBuffer> {
        // On the host all three MemKinds are ordinary RAM; pools carry no
        // meaning because there is no driver allocation to avoid.
        let layout = Layout::from_size_align(bytes.max(1), HOST_ALIGN)
            .map_err(|e| ForgeError::Device(format!("invalid layout: {e}")))?;
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err(ForgeError::OutOfMemory {
                requested: bytes,
                available: 0,
            });
        }
        Ok(DevBuffer::from_impl(Arc::new(CpuBuffer {
            ptr,
            layout,
            kind,
        })))
    }

    fn create_stream(&self) -> Result<Stream> {
        Ok(Stream::from_impl(Arc::new(CpuStream)))
    }

    fn create_event(&self) -> Result<Event> {
        Ok(Event::from_impl(Arc::new(CpuEvent)))
    }

    fn record_event(&self, event: &Event, _stream: &Stream) -> Result<()> {
        event.downcast::<CpuEvent>()?;
        Ok(())
    }

    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
        stream.downcast::<CpuStream>()?;
        event.downcast::<CpuEvent>()?;
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
        stream.downcast::<CpuStream>()?;
        let src_buf = src.downcast::<CpuBuffer>()?;
        let dst_buf = dst.downcast::<CpuBuffer>()?;
        buffer_bounds_check(src, src_offset, bytes)?;
        buffer_bounds_check(dst, dst_offset, bytes)?;
        if Arc::ptr_eq(&src.0, &dst.0) {
            return Err(ForgeError::Device(
                "overlapping copy within the same buffer".to_string(),
            ));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                src_buf.ptr.add(src_offset),
                dst_buf.ptr.add(dst_offset),
                bytes,
            );
        }
        Ok(())
    }

    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
        let dst_buf = dst.downcast::<CpuBuffer>()?;
        buffer_bounds_check(dst, dst_offset, src.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst_buf.ptr.add(dst_offset), src.len());
        }
        Ok(())
    }

    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
        let src_buf = src.downcast::<CpuBuffer>()?;
        buffer_bounds_check(src, src_offset, dst.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(src_buf.ptr.add(src_offset), dst.as_mut_ptr(), dst.len());
        }
        Ok(())
    }

    fn load_module(&self, _image: &[u8]) -> Result<Module> {
        Err(ForgeError::Unsupported(
            "CPU backend has no kernel modules; host kernels are called natively".to_string(),
        ))
    }

    fn launch(
        &self,
        _kernel: &KernelHandle,
        _cfg: &LaunchConfig,
        _args: &LaunchArgs,
        _stream: &Stream,
    ) -> Result<()> {
        Err(ForgeError::Unsupported(
            "CPU backend has no kernel launch; host kernels are called natively".to_string(),
        ))
    }

    fn synchronize(&self) -> Result<()> {
        Ok(())
    }

    fn begin_capture(&self, _stream: &Stream) -> Result<()> {
        Err(ForgeError::Unsupported(
            "CPU backend does not support graph capture".to_string(),
        ))
    }

    fn end_capture(&self, _stream: &Stream) -> Result<ExecGraph> {
        Err(ForgeError::Unsupported(
            "CPU backend does not support graph capture".to_string(),
        ))
    }

    fn launch_graph(&self, _graph: &ExecGraph, _stream: &Stream) -> Result<()> {
        Err(ForgeError::Unsupported(
            "CPU backend does not support graph capture".to_string(),
        ))
    }

    fn reset_activations(&self) -> Result<u64> {
        // Host buffers are individually owned; there is no shared ring to
        // retire, so the generation is a constant.
        Ok(0)
    }
}
