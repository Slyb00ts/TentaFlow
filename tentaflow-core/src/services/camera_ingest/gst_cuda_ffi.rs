// =============================================================================
// File: services/camera_ingest/gst_cuda_ffi.rs — zero-copy NVDEC CUDA buffer map
// =============================================================================
//
// Hand-rolled FFI to borrow the DEVICE NV12 surface an `nvh26Xdec` element
// produces (`video/x-raw(memory:CUDAMemory), format=NV12`) WITHOUT downloading
// it to host memory. gstreamer-rs 0.25 has no CUDA bindings, so we map the
// underlying `GstCudaMemory` directly:
//
//   * `gst_buffer_peek_memory(buf, 0)` → the single `GstMemory` backing the
//     decoded frame (nvcodec packs NV12 Y+UV into ONE allocation at plane
//     offsets — planar-split buffers make the single-memory map fail and the
//     caller falls back to the host-download path).
//   * `gst_memory_map(mem, &info, GST_MAP_READ | GST_MAP_CUDA)` — the magic
//     `GST_MAP_CUDA = GST_MAP_FLAG_LAST << 1 = 0x20000` flag makes GstCudaMemory
//     hand back the raw `CUdeviceptr` in `info.data` (mapping WITHOUT it copies
//     to host, which is exactly what we are avoiding). A non-CUDA memory rejects
//     the flag → map fails → fallback.
//   * `cudaPointerGetAttributes` validates the pointer lives on CUDA DEVICE 0 —
//     the same device ORT's CUDA/TRT execution provider runs on. nvcodec and ORT
//     both bind device 0's CUDA *primary* context, so a device-0 device pointer
//     is dereferenceable by our fused kernel and by ORT `from_raw`. A pointer on
//     another device (multi-GPU box, decoder pinned elsewhere) fails validation
//     → fallback (never a cross-device deref).
//
// Plane strides/offsets come from the buffer's `GstVideoMeta` when present
// (padded decode surfaces carry real strides there) and fall back to the caps
// `VideoInfo` otherwise.
//
// LIFETIME: the returned [`CudaNv12Map`] borrows the `&BufferRef` it was mapped
// from; the device pointer is valid ONLY while the map (and the buffer's ref)
// live. `Drop` unmaps. The caller MUST keep the `GstBuffer` referenced (it holds
// a decode surface from the decoder's finite pool) for the whole time it reads
// the pointer, and drop the map as soon as the reading kernel has synced — see
// `preprocess_nv12_device_gpu`.
//
// Gated on the GPU inference features: without them nvcc/cudart are not linked
// (so `cudaPointerGetAttributes` would not resolve) and no device-tensor path
// exists to consume the pointer.

#![cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]

use std::marker::PhantomData;
use std::os::raw::{c_int, c_void};

use gstreamer as gst;
use gstreamer_video as gst_video;

/// `GST_MAP_CUDA` — the GstCudaMemory map flag (`GST_MAP_FLAG_LAST << 1`). Not in
/// gstreamer-sys (it is defined by the out-of-tree `gstcuda` plugin), so we
/// derive it from the exported `GST_MAP_FLAG_LAST` the same way the C header does.
const GST_MAP_CUDA: u32 = (gst::ffi::GST_MAP_FLAG_LAST as u32) << 1;

/// `cudaMemoryType::cudaMemoryTypeDevice` — a pointer backed by GPU device memory.
const CUDA_MEMORY_TYPE_DEVICE: c_int = 2;

/// Runtime-API pointer-attributes query (cudart, already linked by the GPU
/// preprocess). Stable layout across CUDA 11/12/13: `{ type, device,
/// devicePointer, hostPointer }`. Device pointers share one unified virtual
/// address space per device, so this resolves the owning device ordinal
/// regardless of which context/stream allocated the surface.
#[repr(C)]
struct CudaPointerAttributes {
    type_: c_int,
    device: c_int,
    device_ptr: *mut c_void,
    host_ptr: *mut c_void,
}

extern "C" {
    fn cudaPointerGetAttributes(attr: *mut CudaPointerAttributes, ptr: *const c_void) -> c_int;
}

/// Why a zero-copy map could not be used for this frame — every variant routes
/// the caller to the guaranteed host-download fallback (a camera never breaks).
#[derive(Debug)]
pub enum CudaMapError {
    /// Buffer had no `GstMemory` (should never happen for a decoded frame).
    NoMemory,
    /// `gst_memory_map(GST_MAP_CUDA)` failed — the memory is not GstCudaMemory
    /// (e.g. the decoder handed host memory, or a build without gstcuda).
    NotCudaMemory,
    /// Mapped device pointer was null.
    NullPointer,
    /// `cudaPointerGetAttributes` failed or the pointer is not device-0 device
    /// memory (`device`, `type_`) — cross-device deref would be unsafe.
    WrongDevice {
        device: i32,
        memory_type: i32,
        rc: i32,
    },
    /// The frame is not a 2-plane NV12 surface we can describe.
    UnexpectedLayout,
}

impl std::fmt::Display for CudaMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CudaMapError::NoMemory => write!(f, "buffer has no GstMemory"),
            CudaMapError::NotCudaMemory => write!(f, "GST_MAP_CUDA rejected (not GstCudaMemory)"),
            CudaMapError::NullPointer => write!(f, "mapped CUDA pointer is null"),
            CudaMapError::WrongDevice {
                device,
                memory_type,
                rc,
            } => write!(
                f,
                "pointer not on CUDA device 0 (device={device}, type={memory_type}, rc={rc})"
            ),
            CudaMapError::UnexpectedLayout => write!(f, "frame is not a 2-plane NV12 surface"),
        }
    }
}

/// A borrowed, device-resident NV12 frame: the decoder's `CUdeviceptr` plus the
/// two plane pointers/strides, valid only while this guard (and the underlying
/// `GstBuffer` ref) live. Constructed by [`map_nv12_device`]; `Drop` unmaps the
/// `GstMemory`. The pointers are raw device addresses on CUDA device 0.
pub struct CudaNv12Map<'a> {
    mem: *mut gst::ffi::GstMemory,
    info: gst::ffi::GstMapInfo,
    y_ptr: u64,
    uv_ptr: u64,
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
    _borrow: PhantomData<&'a gst::BufferRef>,
}

impl<'a> CudaNv12Map<'a> {
    /// Device pointer (as `u64`, i.e. `CUdeviceptr`) to the Y plane.
    pub fn y_device_ptr(&self) -> u64 {
        self.y_ptr
    }
    /// Device pointer (as `u64`) to the interleaved UV plane.
    pub fn uv_device_ptr(&self) -> u64 {
        self.uv_ptr
    }
    pub fn y_stride(&self) -> usize {
        self.y_stride
    }
    pub fn uv_stride(&self) -> usize {
        self.uv_stride
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
}

impl<'a> Drop for CudaNv12Map<'a> {
    fn drop(&mut self) {
        // SAFETY: `mem`/`info` were paired by a successful `gst_memory_map`; the
        // memory is still ref-held by the caller's live buffer. Unmap balances it.
        unsafe {
            gst::ffi::gst_memory_unmap(self.mem, &mut self.info);
        }
    }
}

/// Maps the NVDEC device NV12 surface of `buffer` in place (no host download) and
/// validates it lives on CUDA device 0. `info` is the caps-derived `VideoInfo`
/// used only as a stride/offset fallback when the buffer carries no
/// `GstVideoMeta`. On any error the caller falls back to the host-download path.
///
/// SAFETY / lifetime: the returned map borrows `buffer`; keep the buffer (and its
/// decode surface) alive for the whole time the device pointers are read.
pub fn map_nv12_device<'a>(
    buffer: &'a gst::BufferRef,
    info: &gst_video::VideoInfo,
) -> Result<CudaNv12Map<'a>, CudaMapError> {
    let raw = buffer.as_ptr() as *mut gst::ffi::GstBuffer;

    // Single-memory contract: nvcodec packs NV12 into one GstCudaMemory. Anything
    // else (0 or planar-split) → fallback.
    let n_mem = unsafe { gst::ffi::gst_buffer_n_memory(raw) };
    if n_mem == 0 {
        return Err(CudaMapError::NoMemory);
    }
    let mem = unsafe { gst::ffi::gst_buffer_peek_memory(raw, 0) };
    if mem.is_null() {
        return Err(CudaMapError::NoMemory);
    }

    // Map READ|CUDA. GstCudaMemory returns the CUdeviceptr in `info.data`; a
    // non-CUDA memory rejects the flag and the map fails.
    let mut map_info: gst::ffi::GstMapInfo = unsafe { std::mem::zeroed() };
    let flags = (gst::ffi::GST_MAP_READ as u32 | GST_MAP_CUDA) as gst::ffi::GstMapFlags;
    let ok = unsafe { gst::ffi::gst_memory_map(mem, &mut map_info, flags) };
    if ok == 0 {
        return Err(CudaMapError::NotCudaMemory);
    }
    let base = map_info.data as u64;
    if base == 0 {
        unsafe { gst::ffi::gst_memory_unmap(mem, &mut map_info) };
        return Err(CudaMapError::NullPointer);
    }

    // Validate the device pointer is CUDA device 0 device memory. Any mismatch or
    // query failure → unmap and fall back (never a cross-device deref).
    let mut attr = CudaPointerAttributes {
        type_: 0,
        device: -1,
        device_ptr: std::ptr::null_mut(),
        host_ptr: std::ptr::null_mut(),
    };
    let rc = unsafe { cudaPointerGetAttributes(&mut attr, base as *const c_void) };
    if rc != 0 || attr.device != 0 || attr.type_ != CUDA_MEMORY_TYPE_DEVICE {
        unsafe { gst::ffi::gst_memory_unmap(mem, &mut map_info) };
        return Err(CudaMapError::WrongDevice {
            device: attr.device,
            memory_type: attr.type_,
            rc,
        });
    }

    // Plane strides/offsets: prefer GstVideoMeta (real padded strides), else the
    // caps VideoInfo. NV12 has exactly 2 planes (Y, interleaved UV).
    let (y_stride, uv_stride, y_off, uv_off, width, height) =
        match buffer.meta::<gst_video::VideoMeta>() {
            Some(vm) if vm.n_planes() >= 2 => {
                let strides = vm.stride();
                let offsets = vm.offset();
                (
                    strides[0] as usize,
                    strides[1] as usize,
                    offsets[0],
                    offsets[1],
                    vm.width(),
                    vm.height(),
                )
            }
            _ => {
                if info.format() != gst_video::VideoFormat::Nv12 {
                    unsafe { gst::ffi::gst_memory_unmap(mem, &mut map_info) };
                    return Err(CudaMapError::UnexpectedLayout);
                }
                (
                    info.stride()[0] as usize,
                    info.stride()[1] as usize,
                    info.offset()[0],
                    info.offset()[1],
                    info.width(),
                    info.height(),
                )
            }
        };

    Ok(CudaNv12Map {
        mem,
        info: map_info,
        y_ptr: base + y_off as u64,
        uv_ptr: base + uv_off as u64,
        y_stride,
        uv_stride,
        width,
        height,
        _borrow: PhantomData,
    })
}
