// =============================================================================
// File: vision/gpu_preprocess.rs — GPU-resident preprocess + NV12 mosaic (CUDA)
// =============================================================================
//
// Minimal CUDA FFI + safe wrapper that runs the state-classifier crop
// preprocessing (bilinear resize + /255 + per-channel ImageNet normalize +
// HWC->CHW) entirely on the GPU via the fused kernel in
// `cuda/crop_resize_normalize.cu`, leaving the NCHW `[n,3,S,S]` f32 result in
// DEVICE memory. That device buffer is handed straight to ONNX Runtime as a
// CUDA-memory input tensor (`TensorRefMut::from_raw`), so inference reads it
// with ZERO host->device copy — the GPU stops idling on CPU preprocessing.
//
// Concurrency design (why this scales under N parallel camera batchers):
//   * Each worker thread gets its OWN non-blocking CUDA stream + its own
//     grow-only device scratch buffers, held in a `thread_local`. H2D copies and
//     the kernel launch run on that stream, and we sync with
//     `cudaStreamSynchronize` (NOT `cudaDeviceSynchronize`) so one preprocess
//     never stalls another thread's stream.
//   * The per-call `cudaMalloc`/`cudaFree` are gone: staging (packed raw crops),
//     the descriptor arrays and the `[n,3,S,S]` f32 output are reused across
//     calls and only reallocated when a bigger batch needs more capacity.
//
// The zero-copy NVDEC path adds two primitives that run on a CALLER-owned
// [`GpuStream`] and never synchronize on their own: the single-frame
// preprocess into a persistent tensor ([`preprocess_nv12_device_into`]) and the
// irreversible in-place privacy mosaic ([`mosaic_nv12_device`], kernel
// `cuda/nv12_mosaic_regions.cu`) with its bit-exact host oracle
// ([`mosaic_nv12_host`]). The caller decides where the stream is synchronized,
// which is what lets one probe chain map → preprocess → mosaic → unmap with a
// single `cudaStreamSynchronize`.
//
// Gated on both features because it only exists to feed the ort device-tensor
// path; a build without either never links CUDA and never invokes nvcc.

#![cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort"
))]

use anyhow::{bail, Result};
use std::cell::RefCell;

use crate::vision::preprocessing::{fitted_content, ChannelOrder, FrameFit};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

/// Opaque CUDA stream handle (`cudaStream_t` is a pointer in the CUDA runtime).
type CudaStream = *mut c_void;

// Minimal CUDA runtime + kernel-launcher FFI. All H2D copies and the kernel run
// on a caller-provided stream; `cudaStreamSynchronize` waits only for that
// stream, so concurrent worker threads on distinct streams don't serialize.
extern "C" {
    fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> c_int;
    fn cudaFree(dev_ptr: *mut c_void) -> c_int;
    fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: c_int) -> c_int;
    fn cudaMemcpy2D(
        dst: *mut c_void,
        dpitch: usize,
        src: *const c_void,
        spitch: usize,
        width: usize,
        height: usize,
        kind: c_int,
    ) -> c_int;
    fn cudaMemcpyAsync(
        dst: *mut c_void,
        src: *const c_void,
        count: usize,
        kind: c_int,
        stream: CudaStream,
    ) -> c_int;
    fn cudaStreamCreateWithFlags(stream: *mut CudaStream, flags: c_uint) -> c_int;
    fn cudaStreamDestroy(stream: CudaStream) -> c_int;
    fn cudaStreamSynchronize(stream: CudaStream) -> c_int;
    fn cudaDeviceSynchronize() -> c_int;
    fn cudaGetErrorString(err: c_int) -> *const c_char;

    fn launch_crop_resize_normalize(
        crop_ptrs: *const *const u8,
        crop_ws: *const c_int,
        crop_hs: *const c_int,
        n: c_int,
        s: c_int,
        mean: *const f32,
        stdv: *const f32,
        out: *mut f32,
        stream: CudaStream,
    ) -> c_int;

    fn launch_nv12_frame_to_rgb_resize_normalize(
        y_ptr: *const u8,
        y_stride: c_int,
        uv_ptr: *const u8,
        uv_stride: c_int,
        w: c_int,
        h: c_int,
        dw: c_int,
        dh: c_int,
        s: c_int,
        pad: c_int,
        mean: *const f32,
        stdv: *const f32,
        kr: f32,
        kb: f32,
        full_range: c_int,
        bgr: c_int,
        out: *mut f32,
        stream: CudaStream,
    ) -> c_int;

    fn launch_nv12_mosaic_regions(
        y_plane: *mut u8,
        y_pitch: c_int,
        uv_plane: *mut u8,
        uv_pitch: c_int,
        table: *const MosaicRegionTable,
        means: *mut u8,
        stream: CudaStream,
    ) -> c_int;

    fn launch_nv12_to_rgb_resize_normalize(
        y_ptrs: *const *const u8,
        y_strides: *const c_int,
        uv_ptrs: *const *const u8,
        uv_strides: *const c_int,
        widths: *const c_int,
        heights: *const c_int,
        content_ws: *const c_int,
        content_hs: *const c_int,
        n: c_int,
        s: c_int,
        pad: c_int,
        mean: *const f32,
        stdv: *const f32,
        kr: f32,
        kb: f32,
        full_range: c_int,
        bgr: c_int,
        out: *mut f32,
        stream: CudaStream,
    ) -> c_int;
}

/// `cudaMemcpyKind::cudaMemcpyHostToDevice`.
const CUDA_MEMCPY_HOST_TO_DEVICE: c_int = 1;
/// `cudaMemcpyKind::cudaMemcpyDeviceToHost`.
const CUDA_MEMCPY_DEVICE_TO_HOST: c_int = 2;
/// `cudaStreamNonBlocking` — the stream does not implicitly synchronize with the
/// default (`NULL`) stream, so per-thread streams stay independent.
const CUDA_STREAM_NON_BLOCKING: c_uint = 0x01;

/// Maps a non-zero CUDA return code to a descriptive error.
fn cuda_check(code: c_int, ctx: &str) -> Result<()> {
    if code == 0 {
        return Ok(());
    }
    // SAFETY: cudaGetErrorString returns a static NUL-terminated string for any
    // code (an "unrecognized error code" string for unknown values), never null.
    let msg = unsafe {
        let p = cudaGetErrorString(code);
        if p.is_null() {
            "unknown".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    bail!("{ctx}: CUDA error {code} ({msg})");
}

/// Grow-only device scratch buffer. `cudaMalloc` runs only when a request
/// exceeds the current capacity (then the old block is freed and a bigger one
/// allocated); otherwise the existing block is reused, so the steady-state has
/// zero per-call allocation.
struct GrowBuf {
    ptr: *mut c_void,
    cap: usize,
}

impl GrowBuf {
    const fn new() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            cap: 0,
        }
    }

    /// Ensures at least `size` bytes are available, reallocating only on growth.
    fn ensure(&mut self, size: usize) -> Result<()> {
        let want = size.max(1);
        if want <= self.cap {
            return Ok(());
        }
        if !self.ptr.is_null() {
            // Ignore free errors: the pointer is device memory we own; a failure
            // here would only leak, and the realloc below reports real errors.
            unsafe {
                cudaFree(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
            self.cap = 0;
        }
        let mut p: *mut c_void = std::ptr::null_mut();
        cuda_check(
            unsafe { cudaMalloc(&mut p as *mut *mut c_void, want) },
            "cudaMalloc grow",
        )?;
        self.ptr = p;
        self.cap = want;
        Ok(())
    }

    /// Async H2D copy of `size` bytes into this buffer at `offset` on `stream`.
    fn h2d_at(
        &self,
        offset: usize,
        src: *const c_void,
        size: usize,
        stream: CudaStream,
    ) -> Result<()> {
        cuda_check(
            unsafe {
                cudaMemcpyAsync(
                    (self.ptr as *mut u8).add(offset) as *mut c_void,
                    src,
                    size,
                    CUDA_MEMCPY_HOST_TO_DEVICE,
                    stream,
                )
            },
            "cudaMemcpyAsync H2D",
        )
    }
}

impl Drop for GrowBuf {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                cudaFree(self.ptr);
            }
        }
    }
}

/// An owned non-blocking CUDA stream on the current device (device 0, the one
/// nvcodec and ORT bind). Work enqueued on it is ordered only against itself, so
/// independent pipelines (one per worker thread, one per privacy probe) never
/// serialize on the legacy default stream. `Drop` destroys the stream; pending
/// work still completes (CUDA defers the release until the stream drains).
pub struct GpuStream {
    raw: CudaStream,
}

// The handle is an opaque driver object; the CUDA runtime is thread-safe for
// enqueueing onto and synchronizing a stream from any host thread.
unsafe impl Send for GpuStream {}
unsafe impl Sync for GpuStream {}

impl GpuStream {
    /// Creates a `cudaStreamNonBlocking` stream.
    pub fn new() -> Result<Self> {
        let mut raw: CudaStream = std::ptr::null_mut();
        cuda_check(
            unsafe {
                cudaStreamCreateWithFlags(&mut raw as *mut CudaStream, CUDA_STREAM_NON_BLOCKING)
            },
            "cudaStreamCreateWithFlags",
        )?;
        Ok(Self { raw })
    }

    /// Raw `cudaStream_t`, e.g. for ORT's `user_compute_stream`.
    pub fn raw(&self) -> *mut c_void {
        self.raw
    }

    /// Blocks the calling host thread until everything enqueued on this stream
    /// has completed (never a device-wide barrier).
    pub fn synchronize(&self) -> Result<()> {
        cuda_check(
            unsafe { cudaStreamSynchronize(self.raw) },
            "cudaStreamSynchronize",
        )
    }
}

impl Drop for GpuStream {
    fn drop(&mut self) {
        // Errors at teardown are ignored: the handle is ours and a failed destroy
        // would only leak it.
        unsafe {
            cudaStreamDestroy(self.raw);
        }
    }
}

/// Per-thread CUDA scratch: one non-blocking stream + grow-only device buffers
/// reused across every `preprocess_batch_gpu` call on this thread. Lives in a
/// `thread_local`; created lazily on first use. Dropping it releases the stream
/// and buffers at thread exit (a leak on abrupt teardown is acceptable — the process
/// is ending — but the explicit teardown keeps long-lived pools tidy).
struct ThreadScratch {
    stream: GpuStream,
    staging: GrowBuf,   // packed raw RGB24 crop bytes, contiguous
    crop_ptrs: GrowBuf, // device array of `const u8*` (one per crop)
    crop_ws: GrowBuf,   // device array of c_int crop widths
    crop_hs: GrowBuf,   // device array of c_int crop heights
    mean: GrowBuf,      // 3 f32
    stdv: GrowBuf,      // 3 f32
    output: GrowBuf,    // [n,3,S,S] f32 — handed to ORT via from_raw
    // NV12 path: packed Y and UV planes + their per-frame descriptor arrays.
    nv12_y: GrowBuf,          // packed Y planes, contiguous
    nv12_uv: GrowBuf,         // packed interleaved UV planes, contiguous
    nv12_y_ptrs: GrowBuf,     // device array of `const u8*` Y-plane pointers
    nv12_uv_ptrs: GrowBuf,    // device array of `const u8*` UV-plane pointers
    nv12_y_strides: GrowBuf,  // device array of c_int Y strides
    nv12_uv_strides: GrowBuf, // device array of c_int UV strides
    nv12_ws: GrowBuf,         // device array of c_int frame widths
    nv12_hs: GrowBuf,         // device array of c_int frame heights
    nv12_dws: GrowBuf,        // device array of c_int fitted content widths
    nv12_dhs: GrowBuf,        // device array of c_int fitted content heights
}

impl ThreadScratch {
    fn new() -> Result<Self> {
        Ok(Self {
            stream: GpuStream::new()?,
            staging: GrowBuf::new(),
            crop_ptrs: GrowBuf::new(),
            crop_ws: GrowBuf::new(),
            crop_hs: GrowBuf::new(),
            mean: GrowBuf::new(),
            stdv: GrowBuf::new(),
            output: GrowBuf::new(),
            nv12_y: GrowBuf::new(),
            nv12_uv: GrowBuf::new(),
            nv12_y_ptrs: GrowBuf::new(),
            nv12_uv_ptrs: GrowBuf::new(),
            nv12_y_strides: GrowBuf::new(),
            nv12_uv_strides: GrowBuf::new(),
            nv12_ws: GrowBuf::new(),
            nv12_hs: GrowBuf::new(),
            nv12_dws: GrowBuf::new(),
            nv12_dhs: GrowBuf::new(),
        })
    }
}

thread_local! {
    // RefCell so the single-threaded borrow inside a call is checked; the scratch
    // is only ever touched by its owning thread. Option because creation can fail
    // (no CUDA device) and is retried lazily.
    static SCRATCH: RefCell<Option<ThreadScratch>> = const { RefCell::new(None) };
}

/// Handle to the fused-preprocess OUTPUT: the device pointer + dims for the NCHW
/// `[n,3,S,S]` f32 tensor. The memory is NOT owned here — it lives in the calling
/// thread's `thread_local` output scratch and is reused by the next call. It is
/// valid until the next `preprocess_batch_gpu` on the SAME thread, which is after
/// the (synchronous, blocking) ORT run that borrows it has completed. See the
/// lifetime invariant on `preprocess_batch_gpu`.
pub struct DeviceBatch {
    ptr: *mut f32,
    n: usize,
    s: usize,
}

// The handle is just a device pointer + dims; it carries no thread-affine host
// state, so it is safe to move the raw pointer value across to the pooled
// session thread (the buffer lives on the GPU regardless of host thread).
unsafe impl Send for DeviceBatch {}

impl DeviceBatch {
    /// Raw device pointer to the `[n,3,S,S]` f32 output.
    pub fn device_ptr(&self) -> *mut f32 {
        self.ptr
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn s(&self) -> usize {
        self.s
    }

    /// Total f32 element count (`n * 3 * S * S`).
    pub fn elements(&self) -> usize {
        self.n * 3 * self.s * self.s
    }

    /// Synchronous device->host copy of the whole `[n,3,S,S]` tensor into a fresh
    /// host `Vec<f32>`. Used by parity/smoke tooling to read the GPU result back
    /// (the ORT hot path consumes the device pointer directly and never copies).
    pub fn copy_to_host(&self) -> Result<Vec<f32>> {
        let count = self.elements();
        let mut host = vec![0f32; count];
        cuda_check(
            unsafe {
                cudaMemcpy(
                    host.as_mut_ptr() as *mut c_void,
                    self.ptr as *const c_void,
                    count * std::mem::size_of::<f32>(),
                    CUDA_MEMCPY_DEVICE_TO_HOST,
                )
            },
            "cudaMemcpy D2H (DeviceBatch::copy_to_host)",
        )?;
        Ok(host)
    }
}

/// Preprocesses a batch of tightly-packed RGB24 crops on the GPU and returns a
/// [`DeviceBatch`] borrowing the calling thread's reused output buffer. Each crop
/// is `(&[u8], cw, ch)` with `len == cw*ch*3`.
///
/// Packs all crops into one reused staging buffer (async H2D on the thread's
/// stream), runs the fused resize+normalize kernel into the reused `[n,3,S,S]`
/// f32 output on that stream, then `cudaStreamSynchronize`s — no device-wide sync
/// and no per-call `cudaMalloc`.
///
/// LIFETIME INVARIANT: the returned buffer is the thread_local output scratch,
/// reused (and possibly reallocated) by the NEXT call on this thread. Callers
/// hand its `device_ptr()` to ORT via `TensorRefMut::from_raw` and must complete
/// the (synchronous, blocking) `run` BEFORE issuing another
/// `preprocess_batch_gpu` on the same thread. `classify_batch_gpu` does exactly
/// this: it calls preprocess, then blocks in `pool.run`, so a second call cannot
/// clobber the buffer mid-run (the calls are serial on the worker thread).
pub fn preprocess_batch_gpu(
    crops: &[(&[u8], u32, u32)],
    s: usize,
    mean: [f32; 3],
    stdv: [f32; 3],
) -> Result<DeviceBatch> {
    if crops.is_empty() {
        bail!("preprocess_batch_gpu: empty batch");
    }
    if s == 0 {
        bail!("preprocess_batch_gpu: S must be > 0");
    }
    let n = crops.len();

    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = Some(ThreadScratch::new()?);
        }
        let sc = guard.as_mut().expect("scratch initialized above");
        let stream = sc.stream.raw();

        // Validate crops and compute contiguous packing offsets into staging.
        let mut offsets: Vec<usize> = Vec::with_capacity(n);
        let mut host_ws: Vec<c_int> = Vec::with_capacity(n);
        let mut host_hs: Vec<c_int> = Vec::with_capacity(n);
        let mut total_bytes = 0usize;
        for (i, (bytes, cw, ch)) in crops.iter().enumerate() {
            if *cw == 0 || *ch == 0 {
                bail!("preprocess_batch_gpu: crop {i} has a zero dimension");
            }
            let expected = (*cw as usize) * (*ch as usize) * 3;
            if bytes.len() != expected {
                bail!(
                    "preprocess_batch_gpu: crop {i} len {} != {cw}*{ch}*3 = {expected}",
                    bytes.len()
                );
            }
            offsets.push(total_bytes);
            total_bytes += expected;
            host_ws.push(*cw as c_int);
            host_hs.push(*ch as c_int);
        }

        // Reused device buffers: grow only when this batch needs more room.
        sc.staging.ensure(total_bytes)?;
        let ptr_bytes = n * std::mem::size_of::<*const u8>();
        sc.crop_ptrs.ensure(ptr_bytes)?;
        let int_bytes = n * std::mem::size_of::<c_int>();
        sc.crop_ws.ensure(int_bytes)?;
        sc.crop_hs.ensure(int_bytes)?;
        sc.mean.ensure(3 * 4)?;
        sc.stdv.ensure(3 * 4)?;
        let out_bytes = n * 3 * s * s * std::mem::size_of::<f32>();
        sc.output.ensure(out_bytes)?;

        // Upload each crop into its slot; the device pointer of each crop is the
        // staging base + its packing offset (recomputed each call because the
        // staging base can move across a grow-realloc).
        let staging_base = sc.staging.ptr as *const u8;
        let mut host_ptrs: Vec<*const u8> = Vec::with_capacity(n);
        for (i, (bytes, _, _)) in crops.iter().enumerate() {
            sc.staging.h2d_at(
                offsets[i],
                bytes.as_ptr() as *const c_void,
                bytes.len(),
                stream,
            )?;
            host_ptrs.push(unsafe { staging_base.add(offsets[i]) });
        }

        // Descriptor arrays + mean/std to their reused buffers (async on stream).
        sc.crop_ptrs
            .h2d_at(0, host_ptrs.as_ptr() as *const c_void, ptr_bytes, stream)?;
        sc.crop_ws
            .h2d_at(0, host_ws.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.crop_hs
            .h2d_at(0, host_hs.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.mean
            .h2d_at(0, mean.as_ptr() as *const c_void, 3 * 4, stream)?;
        sc.stdv
            .h2d_at(0, stdv.as_ptr() as *const c_void, 3 * 4, stream)?;

        let out_ptr = sc.output.ptr as *mut f32;
        let rc = unsafe {
            launch_crop_resize_normalize(
                sc.crop_ptrs.ptr as *const *const u8,
                sc.crop_ws.ptr as *const c_int,
                sc.crop_hs.ptr as *const c_int,
                n as c_int,
                s as c_int,
                sc.mean.ptr as *const f32,
                sc.stdv.ptr as *const f32,
                out_ptr,
                stream,
            )
        };
        cuda_check(rc, "launch_crop_resize_normalize")?;
        // Stream-scoped sync: wait for THIS thread's H2D + kernel only. Also keeps
        // the local host arrays (host_ptrs/ws/hs) alive until their async copies
        // finish, since they drop after this returns.
        cuda_check(
            unsafe { cudaStreamSynchronize(stream) },
            "cudaStreamSynchronize",
        )?;

        Ok(DeviceBatch { ptr: out_ptr, n, s })
    })
}

/// YUV->RGB conversion parameters for the NV12 kernel. `kr`/`kb` are the luma
/// coefficients (Kr, Kb) of the color matrix; `kg = 1 - kr - kb` is derived.
/// `full_range` picks full (0..255) vs limited (16..235 luma / 16..240 chroma)
/// range. In Stage 1 the caller sets these from the GStreamer caps colorimetry.
#[derive(Debug, Clone, Copy)]
pub struct ColorCoeffs {
    pub kr: f32,
    pub kb: f32,
    pub full_range: bool,
}

impl ColorCoeffs {
    /// BT.709 limited-range — the usual decode for H.264 camera streams.
    pub fn bt709_limited() -> Self {
        Self {
            kr: 0.2126,
            kb: 0.0722,
            full_range: false,
        }
    }

    /// BT.601 limited-range — SD content / some cameras.
    pub fn bt601_limited() -> Self {
        Self {
            kr: 0.299,
            kb: 0.114,
            full_range: false,
        }
    }
}

/// One NV12 (4:2:0) input frame: a Y plane and an interleaved UV plane, each with
/// its own row stride (GStreamer frames may pad rows beyond `w`). `y` must cover
/// `y_stride*h` bytes and `uv` must cover `uv_stride*ceil(h/2)` bytes.
#[derive(Debug, Clone, Copy)]
pub struct Nv12Frame<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub uv: &'a [u8],
    pub uv_stride: usize,
    pub w: u32,
    pub h: u32,
}

/// Preprocesses a batch of NV12 frames on the GPU (YUV->RGB + the SAME Q8
/// bilinear resize as [`preprocess_batch_gpu`] + /255 + per-channel normalize)
/// and returns a [`DeviceBatch`] borrowing the calling thread's reused output
/// buffer. `s` is the square output side (e.g. 560 for detect). The common detect
/// case is `n = 1`. `fit` and `order` apply to every frame of the batch; each
/// frame's fitted content size is [`fitted_content`] of its own dimensions.
///
/// Uploads each frame's Y and UV planes into the thread's reused pool buffers
/// (async H2D on the thread stream), launches the fused kernel on that stream,
/// then `cudaStreamSynchronize`s. Shares the SAME lifetime invariant as
/// `preprocess_batch_gpu`: the returned buffer is thread-local scratch, valid
/// until the next preprocess call on this thread.
pub fn preprocess_nv12_batch_gpu(
    frames: &[Nv12Frame<'_>],
    s: usize,
    mean: [f32; 3],
    stdv: [f32; 3],
    fit: FrameFit,
    order: ChannelOrder,
    color: ColorCoeffs,
) -> Result<DeviceBatch> {
    if frames.is_empty() {
        bail!("preprocess_nv12_batch_gpu: empty batch");
    }
    if s == 0 {
        bail!("preprocess_nv12_batch_gpu: S must be > 0");
    }
    let n = frames.len();

    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = Some(ThreadScratch::new()?);
        }
        let sc = guard.as_mut().expect("scratch initialized above");
        let stream = sc.stream.raw();

        // Validate frames and compute contiguous packing offsets for each plane.
        let mut y_offsets: Vec<usize> = Vec::with_capacity(n);
        let mut uv_offsets: Vec<usize> = Vec::with_capacity(n);
        let mut host_y_strides: Vec<c_int> = Vec::with_capacity(n);
        let mut host_uv_strides: Vec<c_int> = Vec::with_capacity(n);
        let mut host_ws: Vec<c_int> = Vec::with_capacity(n);
        let mut host_hs: Vec<c_int> = Vec::with_capacity(n);
        let mut host_dws: Vec<c_int> = Vec::with_capacity(n);
        let mut host_dhs: Vec<c_int> = Vec::with_capacity(n);
        let mut y_total = 0usize;
        let mut uv_total = 0usize;
        for (i, f) in frames.iter().enumerate() {
            if f.w == 0 || f.h == 0 {
                bail!("preprocess_nv12_batch_gpu: frame {i} has a zero dimension");
            }
            let (w, h) = (f.w as usize, f.h as usize);
            if f.y_stride < w {
                bail!("preprocess_nv12_batch_gpu: frame {i} y_stride {} < width {w}", f.y_stride);
            }
            // Interleaved UV row holds 2 bytes per chroma column; ceil(w/2) columns.
            if f.uv_stride < ((w + 1) / 2) * 2 {
                bail!(
                    "preprocess_nv12_batch_gpu: frame {i} uv_stride {} too small for width {w}",
                    f.uv_stride
                );
            }
            let y_need = f.y_stride * h;
            let uv_rows = (h + 1) / 2;
            let uv_need = f.uv_stride * uv_rows;
            if f.y.len() < y_need {
                bail!(
                    "preprocess_nv12_batch_gpu: frame {i} Y plane len {} < y_stride*h = {y_need}",
                    f.y.len()
                );
            }
            if f.uv.len() < uv_need {
                bail!(
                    "preprocess_nv12_batch_gpu: frame {i} UV plane len {} < uv_stride*ceil(h/2) = {uv_need}",
                    f.uv.len()
                );
            }
            y_offsets.push(y_total);
            y_total += y_need;
            uv_offsets.push(uv_total);
            uv_total += uv_need;
            host_y_strides.push(f.y_stride as c_int);
            host_uv_strides.push(f.uv_stride as c_int);
            host_ws.push(f.w as c_int);
            host_hs.push(f.h as c_int);
            let (dw, dh) = fitted_content(fit, f.w, f.h, s as u32);
            host_dws.push(dw as c_int);
            host_dhs.push(dh as c_int);
        }

        // Reused device buffers: grow only when this batch needs more room.
        sc.nv12_y.ensure(y_total)?;
        sc.nv12_uv.ensure(uv_total)?;
        let ptr_bytes = n * std::mem::size_of::<*const u8>();
        sc.nv12_y_ptrs.ensure(ptr_bytes)?;
        sc.nv12_uv_ptrs.ensure(ptr_bytes)?;
        let int_bytes = n * std::mem::size_of::<c_int>();
        sc.nv12_y_strides.ensure(int_bytes)?;
        sc.nv12_uv_strides.ensure(int_bytes)?;
        sc.nv12_ws.ensure(int_bytes)?;
        sc.nv12_hs.ensure(int_bytes)?;
        sc.nv12_dws.ensure(int_bytes)?;
        sc.nv12_dhs.ensure(int_bytes)?;
        sc.mean.ensure(3 * 4)?;
        sc.stdv.ensure(3 * 4)?;
        let out_bytes = n * 3 * s * s * std::mem::size_of::<f32>();
        sc.output.ensure(out_bytes)?;

        // Upload each frame's planes; the device pointer of each plane is its
        // packed base + offset (recomputed each call because a grow-realloc can
        // move the staging base).
        let y_base = sc.nv12_y.ptr as *const u8;
        let uv_base = sc.nv12_uv.ptr as *const u8;
        let mut host_y_ptrs: Vec<*const u8> = Vec::with_capacity(n);
        let mut host_uv_ptrs: Vec<*const u8> = Vec::with_capacity(n);
        for (i, f) in frames.iter().enumerate() {
            let y_len = f.y_stride * f.h as usize;
            let uv_len = f.uv_stride * ((f.h as usize + 1) / 2);
            sc.nv12_y
                .h2d_at(y_offsets[i], f.y.as_ptr() as *const c_void, y_len, stream)?;
            sc.nv12_uv
                .h2d_at(uv_offsets[i], f.uv.as_ptr() as *const c_void, uv_len, stream)?;
            host_y_ptrs.push(unsafe { y_base.add(y_offsets[i]) });
            host_uv_ptrs.push(unsafe { uv_base.add(uv_offsets[i]) });
        }

        // Descriptor arrays + mean/std to their reused buffers (async on stream).
        sc.nv12_y_ptrs
            .h2d_at(0, host_y_ptrs.as_ptr() as *const c_void, ptr_bytes, stream)?;
        sc.nv12_uv_ptrs
            .h2d_at(0, host_uv_ptrs.as_ptr() as *const c_void, ptr_bytes, stream)?;
        sc.nv12_y_strides
            .h2d_at(0, host_y_strides.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_uv_strides
            .h2d_at(0, host_uv_strides.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_ws
            .h2d_at(0, host_ws.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_hs
            .h2d_at(0, host_hs.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_dws
            .h2d_at(0, host_dws.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_dhs
            .h2d_at(0, host_dhs.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.mean
            .h2d_at(0, mean.as_ptr() as *const c_void, 3 * 4, stream)?;
        sc.stdv
            .h2d_at(0, stdv.as_ptr() as *const c_void, 3 * 4, stream)?;

        let out_ptr = sc.output.ptr as *mut f32;
        let rc = unsafe {
            launch_nv12_to_rgb_resize_normalize(
                sc.nv12_y_ptrs.ptr as *const *const u8,
                sc.nv12_y_strides.ptr as *const c_int,
                sc.nv12_uv_ptrs.ptr as *const *const u8,
                sc.nv12_uv_strides.ptr as *const c_int,
                sc.nv12_ws.ptr as *const c_int,
                sc.nv12_hs.ptr as *const c_int,
                sc.nv12_dws.ptr as *const c_int,
                sc.nv12_dhs.ptr as *const c_int,
                n as c_int,
                s as c_int,
                letterbox_pad(fit) as c_int,
                sc.mean.ptr as *const f32,
                sc.stdv.ptr as *const f32,
                color.kr,
                color.kb,
                if color.full_range { 1 } else { 0 },
                matches!(order, ChannelOrder::Bgr) as c_int,
                out_ptr,
                stream,
            )
        };
        cuda_check(rc, "launch_nv12_to_rgb_resize_normalize")?;
        // Stream-scoped sync: wait for THIS thread's H2D + kernel only, and keep
        // the local host descriptor arrays alive until their async copies finish.
        cuda_check(unsafe { cudaStreamSynchronize(stream) }, "cudaStreamSynchronize")?;

        Ok(DeviceBatch { ptr: out_ptr, n, s })
    })
}

/// An OWNED `[1,3,S,S]` f32 device tensor: the result of the zero-copy device
/// preprocess ([`preprocess_nv12_device_gpu`]). Unlike [`DeviceBatch`] (which
/// borrows the thread-local scratch, valid only until the next call on that
/// thread) this owns a dedicated `cudaMalloc` and frees it on `Drop`, so it can
/// travel from the appsink callback thread that produced it to the ORT worker
/// thread that consumes it — the input NVDEC surface is unmapped immediately
/// after the kernel, but the small preprocessed tensor lives on until ORT ran.
/// `Send + Sync`: the buffer is on the GPU (not thread-affine) and callers only
/// read its device pointer; `Drop` (`cudaFree`) is valid from any thread against
/// the process-wide primary context.
pub struct OwnedDeviceTensor {
    ptr: *mut f32,
    n: usize,
    s: usize,
}

unsafe impl Send for OwnedDeviceTensor {}
unsafe impl Sync for OwnedDeviceTensor {}

impl OwnedDeviceTensor {
    /// Allocates an uninitialized `[1,3,S,S]` f32 tensor on device 0 — the
    /// persistent input a caller keeps across frames for
    /// [`preprocess_nv12_device_into`].
    pub fn alloc(s: usize) -> Result<Self> {
        if s == 0 {
            bail!("OwnedDeviceTensor::alloc: S must be > 0");
        }
        let bytes = 3 * s * s * std::mem::size_of::<f32>();
        let mut ptr: *mut c_void = std::ptr::null_mut();
        cuda_check(
            unsafe { cudaMalloc(&mut ptr as *mut *mut c_void, bytes) },
            "cudaMalloc device tensor",
        )?;
        Ok(Self {
            ptr: ptr as *mut f32,
            n: 1,
            s,
        })
    }

    /// Raw device pointer to the `[n,3,S,S]` f32 tensor (device 0).
    pub fn device_ptr(&self) -> *mut f32 {
        self.ptr
    }
    pub fn n(&self) -> usize {
        self.n
    }
    pub fn s(&self) -> usize {
        self.s
    }
    pub fn elements(&self) -> usize {
        self.n * 3 * self.s * self.s
    }

    /// Synchronous device→host copy of the whole tensor (verify/parity tooling
    /// only — the ORT hot path reads the device pointer directly).
    pub fn copy_to_host(&self) -> Result<Vec<f32>> {
        let count = self.elements();
        let mut host = vec![0f32; count];
        cuda_check(
            unsafe {
                cudaMemcpy(
                    host.as_mut_ptr() as *mut c_void,
                    self.ptr as *const c_void,
                    count * std::mem::size_of::<f32>(),
                    CUDA_MEMCPY_DEVICE_TO_HOST,
                )
            },
            "cudaMemcpy D2H (OwnedDeviceTensor::copy_to_host)",
        )?;
        Ok(host)
    }
}

impl Drop for OwnedDeviceTensor {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // Ignore errors at teardown: the pointer is device memory we own; a
            // failed free would only leak.
            unsafe {
                cudaFree(self.ptr as *mut c_void);
            }
        }
    }
}

/// Synchronous device→host copy from a RAW device pointer (`CUdeviceptr` as
/// `u64`) into `host`. Verify/tooling only — copies an NVDEC device NV12 plane to
/// host to build the download-path reference tensor.
pub fn device_to_host_copy(device_ptr: u64, host: &mut [u8]) -> Result<()> {
    cuda_check(
        unsafe {
            cudaMemcpy(
                host.as_mut_ptr() as *mut c_void,
                device_ptr as *const c_void,
                host.len(),
                CUDA_MEMCPY_DEVICE_TO_HOST,
            )
        },
        "cudaMemcpy D2H (device_to_host_copy)",
    )
}

/// A downloaded NV12 sub-frame: exactly the crop rectangle of a device NV12
/// frame copied to host as a packed `[Y_sub | UV_sub]` buffer. Produced by
/// [`download_nv12_crop_rect`] for the zero-copy CROPS enrichment path — the crop
/// is downloaded ON ITS OWN (a few KB), never the full 4K frame. Feed it to the
/// host `crop_nv12` at origin `(0, 0)` with these strides to reproduce the exact
/// bytes the full-frame host crop would produce (bit-identical parity).
pub struct Nv12CropDownload {
    /// Packed `[Y_sub | UV_sub]` host bytes.
    pub data: Vec<u8>,
    /// Sub-frame width (the even-snapped, frame-clamped crop width `ecw`).
    pub width: u32,
    /// Sub-frame height (`ech`).
    pub height: u32,
    /// Tight Y stride of the sub-frame (`= width`).
    pub y_stride: u32,
    /// Tight interleaved-UV stride of the sub-frame (`ceil(width/2) * 2`).
    pub uv_stride: u32,
    /// Byte offset of the UV plane inside `data` (`= y_stride * height`).
    pub uv_offset: u32,
    /// Even-snapped crop origin in the ORIGINAL frame (mirrors `crop_nv12`).
    pub ex0: u32,
    pub ey0: u32,
}

/// Downloads ONLY the crop sub-rectangle of a device NV12 frame to host via two
/// strided `cudaMemcpy2D` copies (Y sub-plane + UV sub-plane), returning a packed
/// `[Y_sub | UV_sub]` buffer plus the sub-frame geometry. The origin is snapped
/// EVEN and the size clamped to the frame — IDENTICAL rect math to
/// [`super::super::services::camera_ingest::fakefile::crop_nv12`] — so passing
/// the result to that `crop_nv12` at origin `(0, 0)` yields the exact same RGB
/// bytes as cropping the full host frame would. Transfers ~`crop_w*crop_h*1.5`
/// bytes, never the full frame.
///
/// SYNCHRONIZATION: the NVDEC decode is asynchronous. By default we
/// `cudaDeviceSynchronize()` before the copies so the decoder has finished
/// writing the surface (the enrichment crop runs off the mailbox's latest frame,
/// usually already complete, but the barrier is correct against any stream
/// setup). `[vision] zerocopy_map_sync = true` trusts the map already synced and
/// skips it (lower latency once confirmed on the target GStreamer build).
pub fn download_nv12_crop_rect(
    planes: Nv12DevicePlanes,
    x0: u32,
    y0: u32,
    cw: u32,
    ch: u32,
) -> Result<Nv12CropDownload> {
    // Mirror `crop_nv12`'s rect math EXACTLY: snap origin even (2×2 chroma
    // alignment) then clamp width/height to the frame.
    let ex0 = x0 & !1;
    let ey0 = y0 & !1;
    let ecw = cw.min(planes.w.saturating_sub(ex0));
    let ech = ch.min(planes.h.saturating_sub(ey0));
    if ecw == 0 || ech == 0 {
        bail!("download_nv12_crop_rect: empty crop after clamp (ecw={ecw}, ech={ech})");
    }
    let (ecw_u, ech_u) = (ecw as usize, ech as usize);
    let y_stride = ecw_u;
    let chroma_cols = (ecw_u + 1) / 2;
    let uv_stride = chroma_cols * 2;
    let chroma_rows = (ech_u + 1) / 2;
    let y_bytes = y_stride * ech_u;
    let uv_bytes = uv_stride * chroma_rows;
    let mut data = vec![0u8; y_bytes + uv_bytes];

    wait_for_decoded_surface()?;

    // Y sub-plane: rows [ey0, ey0+ech), cols [ex0, ex0+ecw) → tight dst.
    let y_src = planes.y_ptr + (ey0 as u64) * (planes.y_stride as u64) + ex0 as u64;
    cuda_check(
        unsafe {
            cudaMemcpy2D(
                data.as_mut_ptr() as *mut c_void,
                y_stride,
                y_src as *const c_void,
                planes.y_stride,
                ecw_u,
                ech_u,
                CUDA_MEMCPY_DEVICE_TO_HOST,
            )
        },
        "cudaMemcpy2D crop Y",
    )?;

    // UV sub-plane: chroma rows start ey0/2, chroma cols start ex0/2 (byte offset
    // ex0, since ex0 is even). `uv_stride` bytes per row, `chroma_rows` rows.
    let uv_src = planes.uv_ptr + ((ey0 / 2) as u64) * (planes.uv_stride as u64) + ex0 as u64;
    cuda_check(
        unsafe {
            cudaMemcpy2D(
                data[y_bytes..].as_mut_ptr() as *mut c_void,
                uv_stride,
                uv_src as *const c_void,
                planes.uv_stride,
                uv_stride,
                chroma_rows,
                CUDA_MEMCPY_DEVICE_TO_HOST,
            )
        },
        "cudaMemcpy2D crop UV",
    )?;

    Ok(Nv12CropDownload {
        data,
        width: ecw,
        height: ech,
        y_stride: y_stride as u32,
        uv_stride: uv_stride as u32,
        uv_offset: y_bytes as u32,
        ex0,
        ey0,
    })
}

/// One NV12 frame whose planes ALREADY live in CUDA device 0 memory (the raw
/// `CUdeviceptr`s of an NVDEC decode surface, obtained via the zero-copy
/// `gst_cuda_ffi` map). `y_ptr`/`uv_ptr` are device addresses (as `u64`), not
/// host slices — there is no host copy of this frame.
#[derive(Debug, Clone, Copy)]
pub struct Nv12DevicePlanes {
    pub y_ptr: u64,
    pub y_stride: usize,
    pub uv_ptr: u64,
    pub uv_stride: usize,
    pub w: u32,
    pub h: u32,
}

/// Validates the geometry of a device NV12 frame the kernels are about to read
/// or write; `ctx` names the caller in the error.
fn check_device_planes(planes: &Nv12DevicePlanes, ctx: &str) -> Result<()> {
    if planes.w == 0 || planes.h == 0 {
        bail!("{ctx}: frame has a zero dimension");
    }
    if planes.y_ptr == 0 || planes.uv_ptr == 0 {
        bail!("{ctx}: null plane pointer");
    }
    let w = planes.w as usize;
    if planes.y_stride < w {
        bail!("{ctx}: y_stride {} < width {w}", planes.y_stride);
    }
    if planes.uv_stride < ((w + 1) / 2) * 2 {
        bail!(
            "{ctx}: uv_stride {} too small for width {w}",
            planes.uv_stride
        );
    }
    if planes.y_stride > c_int::MAX as usize || planes.uv_stride > c_int::MAX as usize {
        bail!("{ctx}: stride exceeds the kernel's int range");
    }
    Ok(())
}

/// Zero-copy device preprocess into a CALLER-OWNED persistent `[1,3,S,S]` f32
/// tensor (`S = out.s()`): the SAME fused NV12→RGB + Q8 resize + normalize math
/// as [`preprocess_nv12_batch_gpu`] (bit-identical, the kernels share one device
/// function), reading the NVDEC surface in place. It only ENQUEUES the kernel on
/// `stream` and returns; nothing is uploaded (the descriptors are kernel
/// parameters) and nothing is synchronized.
///
/// ORDERING CONTRACT (the caller's responsibility, because only the caller knows
/// the producer of the surface and the consumer of the tensor):
///   * the decoder must have finished writing the surface before work on
///     `stream` executes — e.g. the map already synchronized the memory's stream,
///     or the caller made `stream` wait on an event recorded after the decode;
///   * the surface must stay mapped and ref-held, and `out` must not be read
///     (by ORT on another stream) or overwritten, until this kernel has
///     completed — synchronize `stream`, or make the consumer's stream wait on
///     it. Enqueuing ORT on the same `stream` satisfies the second point by
///     stream order.
pub fn preprocess_nv12_device_into(
    planes: Nv12DevicePlanes,
    out: &mut OwnedDeviceTensor,
    mean: [f32; 3],
    stdv: [f32; 3],
    fit: FrameFit,
    order: ChannelOrder,
    color: ColorCoeffs,
    stream: &GpuStream,
) -> Result<(u32, u32)> {
    check_device_planes(&planes, "preprocess_nv12_device_into")?;
    if out.n() != 1 {
        bail!(
            "preprocess_nv12_device_into: output must be [1,3,S,S], got n={}",
            out.n()
        );
    }
    let side = out.s() as u32;
    let (dw, dh) = fitted_content(fit, planes.w, planes.h, side);
    let pad = letterbox_pad(fit);
    let rc = unsafe {
        launch_nv12_frame_to_rgb_resize_normalize(
            planes.y_ptr as *const u8,
            planes.y_stride as c_int,
            planes.uv_ptr as *const u8,
            planes.uv_stride as c_int,
            planes.w as c_int,
            planes.h as c_int,
            dw as c_int,
            dh as c_int,
            side as c_int,
            pad as c_int,
            mean.as_ptr(),
            stdv.as_ptr(),
            color.kr,
            color.kb,
            if color.full_range { 1 } else { 0 },
            matches!(order, ChannelOrder::Bgr) as c_int,
            out.device_ptr(),
            stream.raw(),
        )
    };
    cuda_check(rc, "launch_nv12_frame_to_rgb_resize_normalize")?;
    Ok((dw, dh))
}

/// Pad byte the kernels write outside the fitted content (unused by a stretch,
/// whose content covers the whole plane).
fn letterbox_pad(fit: FrameFit) -> u8 {
    match fit {
        FrameFit::Stretch => 0,
        FrameFit::Letterbox { pad } => pad,
    }
}

/// Zero-copy device preprocess returning a fresh OWNED `[1,3,S,S]` tensor: a
/// synchronous wrapper over [`preprocess_nv12_device_into`] on the calling
/// thread's stream, for callers that hand the tensor to another thread and
/// unmap the surface right after this returns.
///
/// SYNCHRONIZATION: the decode is asynchronous, so the decoder may still be
/// writing the surface when we map it. Before launching the kernel we ensure the
/// decode has completed: by default `cudaDeviceSynchronize()` (correct against
/// ANY nvcodec stream configuration, the safe choice for the opt-in path). Set
/// `[vision] zerocopy_map_sync = true` to trust that `gst_memory_map(GST_MAP_CUDA)`
/// already synced the surface's stream and skip the device sync (lower latency,
/// only once confirmed on the target GStreamer build).
///
/// LIFETIME: the caller MUST keep the source `GstBuffer` mapped + ref-held across
/// this whole call (the kernel reads its device memory); it may unmap
/// immediately AFTER this returns (the kernel has synced and consumed the NV12).
/// The returned [`OwnedDeviceTensor`] is independent of the source surface.
pub fn preprocess_nv12_device_gpu(
    planes: Nv12DevicePlanes,
    s: usize,
    mean: [f32; 3],
    stdv: [f32; 3],
    color: ColorCoeffs,
) -> Result<OwnedDeviceTensor> {
    check_device_planes(&planes, "preprocess_nv12_device_gpu")?;
    let mut out = OwnedDeviceTensor::alloc(s)?;

    wait_for_decoded_surface()?;

    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = Some(ThreadScratch::new()?);
        }
        let sc = guard.as_mut().expect("scratch initialized above");
        preprocess_nv12_device_into(
            planes,
            &mut out,
            mean,
            stdv,
            FrameFit::Stretch,
            ChannelOrder::Rgb,
            color,
            &sc.stream,
        )?;
        // The caller unmaps the surface as soon as this returns.
        sc.stream.synchronize()
    })?;
    Ok(out)
}

// =============================================================================
// Privacy mosaic (in-place, irreversible) — `cuda/nv12_mosaic_regions.cu`
// =============================================================================

/// Most rectangles one mosaic launch takes; the region table rides in the
/// kernel parameters, and 64 entries keep it far below the 4 KiB limit.
pub const MOSAIC_MAX_RECTS: usize = 64;
/// Smallest block side (luma px): below this a mosaic over a small face keeps
/// enough structure to be recognisable.
pub const MOSAIC_BLOCK_MIN: u32 = 12;
/// Largest block side (luma px), so a big region still reads as a mosaic.
pub const MOSAIC_BLOCK_MAX: u32 = 48;
/// Block side of the whole-frame (fail-closed) mosaic.
pub const MOSAIC_WHOLE_FRAME_BLOCK: u32 = 32;

/// Make sure an NVDEC surface that was just mapped is fully decoded before a
/// kernel reads (or rewrites) it: the decode runs asynchronously on nvcodec's own
/// stream. By default a `cudaDeviceSynchronize()`, correct against any nvcodec
/// stream configuration; `[vision] zerocopy_map_sync = true` trusts that
/// `gst_memory_map(GST_MAP_CUDA)` already synced the surface's stream and skips
/// it (lower latency, only once confirmed on the target GStreamer build).
pub fn wait_for_decoded_surface() -> Result<()> {
    if crate::vision::settings::get().zerocopy_map_sync {
        return Ok(());
    }
    cuda_check(
        unsafe { cudaDeviceSynchronize() },
        "cudaDeviceSynchronize decode-wait",
    )
}

/// A rectangle to pixelate, in luma pixels of the full frame. It may extend past
/// the frame (it is clamped) and may sit on odd coordinates (it is widened to
/// even ones so the 2x2 chroma grid is covered exactly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// What to pixelate.
#[derive(Debug, Clone, Copy)]
pub enum MosaicMode<'a> {
    /// The given rectangles; where they overlap, the later one wins.
    Regions(&'a [BlurRect]),
    /// The whole frame with [`MOSAIC_WHOLE_FRAME_BLOCK`] blocks.
    WholeFrame,
}

/// One normalized region. Layout mirrors `struct MosaicRegion` in the CUDA file.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MosaicRegion {
    x0: c_int,
    y0: c_int,
    x1: c_int,
    y1: c_int,
    b: c_int,
    nbx: c_int,
    nby: c_int,
    first_block: c_int,
}

/// Layout mirrors `struct MosaicRegionTable` in the CUDA file; passed by value
/// as a kernel parameter.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MosaicRegionTable {
    n: c_int,
    total_blocks: c_int,
    r: [MosaicRegion; MOSAIC_MAX_RECTS],
}

impl MosaicRegionTable {
    fn regions(&self) -> &[MosaicRegion] {
        &self.r[..self.n as usize]
    }
}

/// The ONE geometry definition shared by the kernel launch and the host oracle:
/// clamps each rect to the frame, snaps its origin down and its far edge up to
/// even coordinates (then clamps again), drops rects that end up empty, picks the
/// block side `b = clamp(max(w, h) / 6, MIN, MAX)` rounded down to even (so a
/// luma block maps onto whole chroma samples) and numbers the blocks row-major,
/// region after region.
fn mosaic_regions(w: u32, h: u32, mode: MosaicMode<'_>) -> Result<MosaicRegionTable> {
    if w == 0 || h == 0 {
        bail!("mosaic: frame has a zero dimension");
    }
    if w > c_int::MAX as u32 / 2 || h > c_int::MAX as u32 / 2 {
        bail!("mosaic: frame {w}x{h} exceeds the kernel's int range");
    }
    let mut table = MosaicRegionTable {
        n: 0,
        total_blocks: 0,
        r: [MosaicRegion::default(); MOSAIC_MAX_RECTS],
    };
    let whole = [BlurRect { x: 0, y: 0, w, h }];
    let (rects, fixed_block) = match mode {
        MosaicMode::Regions(rects) => (rects, None),
        MosaicMode::WholeFrame => (&whole[..], Some(MOSAIC_WHOLE_FRAME_BLOCK)),
    };
    if rects.len() > MOSAIC_MAX_RECTS {
        bail!(
            "mosaic: {} rects exceed the per-launch limit of {MOSAIC_MAX_RECTS}",
            rects.len()
        );
    }
    let mut total: u64 = 0;
    for r in rects {
        let x0 = r.x.min(w) & !1;
        let y0 = r.y.min(h) & !1;
        let x1 = (r.x.saturating_add(r.w).saturating_add(1) & !1).min(w);
        let y1 = (r.y.saturating_add(r.h).saturating_add(1) & !1).min(h);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let b = fixed_block.unwrap_or_else(|| {
            ((x1 - x0).max(y1 - y0) / 6).clamp(MOSAIC_BLOCK_MIN, MOSAIC_BLOCK_MAX) & !1
        });
        let nbx = (x1 - x0).div_ceil(b);
        let nby = (y1 - y0).div_ceil(b);
        let slot = &mut table.r[table.n as usize];
        *slot = MosaicRegion {
            x0: x0 as c_int,
            y0: y0 as c_int,
            x1: x1 as c_int,
            y1: y1 as c_int,
            b: b as c_int,
            nbx: nbx as c_int,
            nby: nby as c_int,
            first_block: total as c_int,
        };
        table.n += 1;
        total += u64::from(nbx) * u64::from(nby);
    }
    // The grid dimension of both passes is the block count.
    if total > i32::MAX as u64 {
        bail!("mosaic: {total} blocks exceed the launch grid");
    }
    table.total_blocks = total as c_int;
    Ok(table)
}

/// Caller-owned device scratch for [`mosaic_nv12_device`]: the per-block means
/// buffer (3 bytes per block). It grows only when a launch needs more blocks than
/// any previous one, so steady state does no allocation.
///
/// Reuse is stream-ordered: keep one scratch per stream (or synchronize the old
/// stream before handing the scratch to another one), otherwise two launches can
/// overwrite each other's means.
pub struct MosaicScratch {
    means: GrowBuf,
}

// Owns a plain device allocation; freeing it from any host thread is valid.
unsafe impl Send for MosaicScratch {}

impl MosaicScratch {
    /// Pre-sizes the means buffer for the whole-frame launch of a `w`×`h`
    /// stream, so the fail-closed path never allocates mid-stream. A region set
    /// with more blocks grows the buffer once.
    pub fn new(w: u32, h: u32) -> Result<Self> {
        let whole = mosaic_regions(w, h, MosaicMode::WholeFrame)?;
        let mut means = GrowBuf::new();
        means.ensure(3 * whole.total_blocks as usize)?;
        Ok(Self { means })
    }
}

/// Pixelates an NV12 frame in device memory IN PLACE (see the CUDA file for the
/// block and overlap semantics). Enqueues two kernels on `stream` and returns
/// without synchronizing; an empty region list (after clamping) enqueues nothing.
///
/// ORDERING CONTRACT: the surface must be mapped for WRITE and stay mapped until
/// the kernels completed — synchronize `stream` before unmapping or pushing the
/// buffer downstream. Any earlier reader of the ORIGINAL pixels (e.g. the
/// detector preprocess) must be enqueued before this call on the same stream,
/// or be complete, because the frame is overwritten.
pub fn mosaic_nv12_device(
    planes: Nv12DevicePlanes,
    mode: MosaicMode<'_>,
    scratch: &mut MosaicScratch,
    stream: &GpuStream,
) -> Result<()> {
    check_device_planes(&planes, "mosaic_nv12_device")?;
    let table = mosaic_regions(planes.w, planes.h, mode)?;
    if table.n == 0 {
        return Ok(());
    }
    // Growth frees the old block with `cudaFree`, which waits for the device, so
    // a still-running launch on this stream never reads freed memory.
    scratch.means.ensure(3 * table.total_blocks as usize)?;
    let rc = unsafe {
        launch_nv12_mosaic_regions(
            planes.y_ptr as *mut u8,
            planes.y_stride as c_int,
            planes.uv_ptr as *mut u8,
            planes.uv_stride as c_int,
            &table,
            scratch.means.ptr as *mut u8,
            stream.raw(),
        )
    };
    cuda_check(rc, "launch_nv12_mosaic_regions")
}

/// Host oracle of [`mosaic_nv12_device`]: same geometry (`mosaic_regions`), same
/// integer rounded means over the ORIGINAL pixels, same "later region wins"
/// overlap rule — bit-exact against the kernel. `y` must hold `y_stride * h`
/// bytes and `uv` `uv_stride * ceil(h/2)` bytes; row padding is never touched.
pub fn mosaic_nv12_host(
    y: &mut [u8],
    y_stride: usize,
    uv: &mut [u8],
    uv_stride: usize,
    w: u32,
    h: u32,
    mode: MosaicMode<'_>,
) -> Result<()> {
    let table = mosaic_regions(w, h, mode)?;
    let (wu, hu) = (w as usize, h as usize);
    if y_stride < wu || uv_stride < wu.div_ceil(2) * 2 {
        bail!("mosaic_nv12_host: stride too small for width {w}");
    }
    if y.len() < y_stride * hu || uv.len() < uv_stride * hu.div_ceil(2) {
        bail!("mosaic_nv12_host: plane buffers too small for {w}x{h}");
    }

    struct Block {
        luma: (usize, usize, usize, usize),
        chroma: (usize, usize, usize, usize),
    }
    let mut blocks: Vec<Block> = Vec::with_capacity(table.total_blocks as usize);
    for g in table.regions() {
        let (x0, y0, x1, y1, b) = (
            g.x0 as usize,
            g.y0 as usize,
            g.x1 as usize,
            g.y1 as usize,
            g.b as usize,
        );
        let hb = b / 2;
        for by in 0..g.nby as usize {
            for bx in 0..g.nbx as usize {
                let lx0 = x0 + bx * b;
                let ly0 = y0 + by * b;
                let cx0 = x0 / 2 + bx * hb;
                let cy0 = y0 / 2 + by * hb;
                blocks.push(Block {
                    luma: (lx0, ly0, (lx0 + b).min(x1), (ly0 + b).min(y1)),
                    chroma: (
                        cx0,
                        cy0,
                        (cx0 + hb).min(x1.div_ceil(2)),
                        (cy0 + hb).min(y1.div_ceil(2)),
                    ),
                });
            }
        }
    }

    // Means over the untouched frame first, exactly like pass 1 finishing before
    // pass 2 starts on the GPU.
    let means: Vec<[u8; 3]> = blocks
        .iter()
        .map(|blk| {
            let (lx0, ly0, lx1, ly1) = blk.luma;
            let (cx0, cy0, cx1, cy1) = blk.chroma;
            let mut sy = 0u32;
            for py in ly0..ly1 {
                sy += y[py * y_stride + lx0..py * y_stride + lx1]
                    .iter()
                    .map(|&v| u32::from(v))
                    .sum::<u32>();
            }
            let (mut su, mut sv) = (0u32, 0u32);
            for py in cy0..cy1 {
                for px in cx0..cx1 {
                    su += u32::from(uv[py * uv_stride + 2 * px]);
                    sv += u32::from(uv[py * uv_stride + 2 * px + 1]);
                }
            }
            let nl = ((lx1 - lx0) * (ly1 - ly0)) as u32;
            let nc = ((cx1 - cx0) * (cy1 - cy0)) as u32;
            [
                ((sy + nl / 2) / nl) as u8,
                ((su + nc / 2) / nc) as u8,
                ((sv + nc / 2) / nc) as u8,
            ]
        })
        .collect();

    // Writing in region order lets a later region overwrite an earlier one — the
    // kernel's "highest covering index is the only writer" rule.
    for (blk, m) in blocks.iter().zip(&means) {
        let (lx0, ly0, lx1, ly1) = blk.luma;
        for py in ly0..ly1 {
            y[py * y_stride + lx0..py * y_stride + lx1].fill(m[0]);
        }
        let (cx0, cy0, cx1, cy1) = blk.chroma;
        for py in cy0..cy1 {
            for px in cx0..cx1 {
                uv[py * uv_stride + 2 * px] = m[1];
                uv[py * uv_stride + 2 * px + 1] = m[2];
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise so parity failures reproduce.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 56) as u8
            })
            .collect()
    }

    #[test]
    fn regions_are_clamped_even_snapped_and_sized() {
        let rects = [
            BlurRect {
                x: 3,
                y: 5,
                w: 40,
                h: 30,
            },
            BlurRect {
                x: 1270,
                y: 700,
                w: 100,
                h: 100,
            },
            BlurRect {
                x: 2000,
                y: 0,
                w: 10,
                h: 10,
            },
            BlurRect {
                x: 0,
                y: 0,
                w: 1000,
                h: 700,
            },
        ];
        let t = mosaic_regions(1280, 720, MosaicMode::Regions(&rects)).unwrap();
        let r = t.regions();
        assert_eq!(r.len(), 3, "the rect fully outside the frame is dropped");
        assert_eq!(
            (r[0].x0, r[0].y0, r[0].x1, r[0].y1, r[0].b),
            (2, 4, 44, 36, 12)
        );
        assert_eq!((r[1].x0, r[1].y0, r[1].x1, r[1].y1), (1270, 700, 1280, 720));
        assert_eq!(r[2].b as u32, MOSAIC_BLOCK_MAX);
        assert_eq!(r[1].first_block, r[0].nbx * r[0].nby);
        let whole = mosaic_regions(1280, 720, MosaicMode::WholeFrame).unwrap();
        assert_eq!(whole.regions()[0].b as u32, MOSAIC_WHOLE_FRAME_BLOCK);
        assert_eq!(whole.total_blocks, 40 * 23);
        let many = vec![
            BlurRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            };
            MOSAIC_MAX_RECTS + 1
        ];
        assert!(mosaic_regions(1280, 720, MosaicMode::Regions(&many)).is_err());
    }

    #[test]
    fn host_mosaic_collapses_blocks_and_keeps_the_rest() {
        let (w, h, ys, uvs) = (64u32, 48u32, 72usize, 72usize);
        let orig_y = noise(ys * 48, 1);
        let orig_uv = noise(uvs * 24, 2);
        let (mut y, mut uv) = (orig_y.clone(), orig_uv.clone());
        let rects = [BlurRect {
            x: 8,
            y: 8,
            w: 24,
            h: 24,
        }];
        mosaic_nv12_host(&mut y, ys, &mut uv, uvs, w, h, MosaicMode::Regions(&rects)).unwrap();
        // b = 12: block (0,0) covers luma [8,20)x[8,20).
        let v = y[8 * ys + 8];
        for py in 8..20 {
            assert!(y[py * ys + 8..py * ys + 20].iter().all(|&p| p == v));
        }
        let sum: u32 = (8..20)
            .flat_map(|py| orig_y[py * ys + 8..py * ys + 20].to_vec())
            .map(u32::from)
            .sum();
        assert_eq!(u32::from(v), (sum + 72) / 144);
        for py in 0..48 {
            for px in 0..ys {
                if !(8..32).contains(&px) || !(8..32).contains(&py) {
                    assert_eq!(y[py * ys + px], orig_y[py * ys + px]);
                }
            }
        }
        assert_eq!(&uv[..4 * uvs], &orig_uv[..4 * uvs]);
    }

    /// Uploads a host NV12 frame into a fresh device allocation.
    fn upload(frame_y: &[u8], frame_uv: &[u8]) -> (GrowBuf, GrowBuf) {
        let mut dy = GrowBuf::new();
        let mut duv = GrowBuf::new();
        dy.ensure(frame_y.len()).unwrap();
        duv.ensure(frame_uv.len()).unwrap();
        for (buf, src) in [(&dy, frame_y), (&duv, frame_uv)] {
            cuda_check(
                unsafe {
                    cudaMemcpy(
                        buf.ptr,
                        src.as_ptr() as *const c_void,
                        src.len(),
                        CUDA_MEMCPY_HOST_TO_DEVICE,
                    )
                },
                "test upload",
            )
            .unwrap();
        }
        (dy, duv)
    }

    fn download(buf: &GrowBuf, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        device_to_host_copy(buf.ptr as u64, &mut out).unwrap();
        out
    }

    #[test]
    #[ignore = "needs a CUDA device; run with --ignored"]
    fn mosaic_device_matches_host_oracle_bit_exact() {
        // Padded pitches, like NVDEC surfaces.
        let (w, h, ys, uvs) = (1280u32, 720u32, 1536usize, 1536usize);
        let base_y = noise(ys * h as usize, 7);
        let base_uv = noise(uvs * (h as usize / 2), 8);
        let rects = [
            BlurRect {
                x: 101,
                y: 57,
                w: 90,
                h: 120,
            },
            BlurRect {
                x: 150,
                y: 100,
                w: 400,
                h: 300,
            },
            BlurRect {
                x: 1200,
                y: 650,
                w: 200,
                h: 200,
            },
            BlurRect {
                x: 640,
                y: 0,
                w: 33,
                h: 17,
            },
        ];
        let stream = GpuStream::new().unwrap();
        let mut scratch = MosaicScratch::new(w, h).unwrap();

        for (label, mode) in [
            ("4 rects", MosaicMode::Regions(&rects)),
            ("whole frame", MosaicMode::WholeFrame),
        ] {
            let (dy, duv) = upload(&base_y, &base_uv);
            let planes = Nv12DevicePlanes {
                y_ptr: dy.ptr as u64,
                y_stride: ys,
                uv_ptr: duv.ptr as u64,
                uv_stride: uvs,
                w,
                h,
            };
            mosaic_nv12_device(planes, mode, &mut scratch, &stream).unwrap();
            stream.synchronize().unwrap();
            let got_y = download(&dy, base_y.len());
            let got_uv = download(&duv, base_uv.len());

            let (mut want_y, mut want_uv) = (base_y.clone(), base_uv.clone());
            mosaic_nv12_host(&mut want_y, ys, &mut want_uv, uvs, w, h, mode).unwrap();
            let dy_diff = got_y.iter().zip(&want_y).filter(|(a, b)| a != b).count();
            let duv_diff = got_uv.iter().zip(&want_uv).filter(|(a, b)| a != b).count();
            let changed = got_y.iter().zip(&base_y).filter(|(a, b)| a != b).count();
            println!("{label}: Y diff {dy_diff}, UV diff {duv_diff}, luma px changed {changed}");
            assert_eq!(dy_diff, 0, "{label}: Y plane differs from the oracle");
            assert_eq!(duv_diff, 0, "{label}: UV plane differs from the oracle");
            assert!(changed > 0);

            // Launch + completion latency as the probe sees it (enqueue, then sync).
            const ITERS: u32 = 500;
            for _ in 0..20 {
                mosaic_nv12_device(planes, mode, &mut scratch, &stream).unwrap();
            }
            stream.synchronize().unwrap();
            let mut samples: Vec<f64> = Vec::with_capacity(ITERS as usize);
            for _ in 0..ITERS {
                let t0 = std::time::Instant::now();
                mosaic_nv12_device(planes, mode, &mut scratch, &stream).unwrap();
                stream.synchronize().unwrap();
                samples.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            samples.sort_by(|a, b| a.total_cmp(b));
            println!(
                "{label} 1280x720: p50 {:.4} ms, p95 {:.4} ms, p99 {:.4} ms (n={ITERS})",
                samples[samples.len() / 2],
                samples[samples.len() * 95 / 100],
                samples[samples.len() * 99 / 100],
            );
        }
    }

    #[test]
    #[ignore = "needs a CUDA device; run with --ignored"]
    fn preprocess_into_matches_batch_path_bit_exact() {
        let (w, h, ys, uvs) = (1280u32, 720u32, 1536usize, 1536usize);
        let y = noise(ys * h as usize, 11);
        let uv = noise(uvs * (h as usize / 2), 12);
        const S: usize = 640;
        const MEAN: [f32; 3] = [0.0, 0.0, 0.0];
        const STD: [f32; 3] = [1.0, 1.0, 1.0];
        let color = ColorCoeffs::bt709_limited();

        let frame = Nv12Frame {
            y: &y,
            y_stride: ys,
            uv: &uv,
            uv_stride: uvs,
            w,
            h,
        };
        let want = preprocess_nv12_batch_gpu(
            &[frame],
            S,
            MEAN,
            STD,
            FrameFit::Stretch,
            ChannelOrder::Rgb,
            color,
        )
        .unwrap()
        .copy_to_host()
        .unwrap();

        let (dy, duv) = upload(&y, &uv);
        let planes = Nv12DevicePlanes {
            y_ptr: dy.ptr as u64,
            y_stride: ys,
            uv_ptr: duv.ptr as u64,
            uv_stride: uvs,
            w,
            h,
        };
        let stream = GpuStream::new().unwrap();
        let mut out = OwnedDeviceTensor::alloc(S).unwrap();
        preprocess_nv12_device_into(
            planes,
            &mut out,
            MEAN,
            STD,
            FrameFit::Stretch,
            ChannelOrder::Rgb,
            color,
            &stream,
        )
        .unwrap();
        stream.synchronize().unwrap();
        let got = out.copy_to_host().unwrap();
        let diff = got
            .iter()
            .zip(&want)
            .filter(|(a, b)| a.to_bits() != b.to_bits())
            .count();
        println!(
            "preprocess_into vs batch: {diff} differing of {}",
            got.len()
        );
        assert_eq!(diff, 0);

        // Letterbox + BGR: the batch kernel must fill content and pad exactly
        // like the single-frame kernel (the YOLOX host/NV12 paths rely on it).
        let lb = FrameFit::Letterbox { pad: 114 };
        let want_lb =
            preprocess_nv12_batch_gpu(&[frame], S, MEAN, STD, lb, ChannelOrder::Bgr, color)
                .unwrap()
                .copy_to_host()
                .unwrap();
        let content = preprocess_nv12_device_into(
            planes,
            &mut out,
            MEAN,
            STD,
            lb,
            ChannelOrder::Bgr,
            color,
            &stream,
        )
        .unwrap();
        stream.synchronize().unwrap();
        assert_eq!(content, (640, 360));
        let got_lb = out.copy_to_host().unwrap();
        assert!(got_lb
            .iter()
            .zip(&want_lb)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
        let pad_v = 114.0 / 255.0;
        assert_eq!(
            got_lb[639 * S + 5],
            pad_v,
            "row below the content is padded"
        );

        let owned = preprocess_nv12_device_gpu(planes, S, MEAN, STD, color)
            .unwrap()
            .copy_to_host()
            .unwrap();
        assert!(owned
            .iter()
            .zip(&want)
            .all(|(a, b)| a.to_bits() == b.to_bits()));

        let mut samples: Vec<f64> = Vec::new();
        for _ in 0..200 {
            let t0 = std::time::Instant::now();
            preprocess_nv12_device_into(
                planes,
                &mut out,
                MEAN,
                STD,
                FrameFit::Stretch,
                ChannelOrder::Rgb,
                color,
                &stream,
            )
            .unwrap();
            stream.synchronize().unwrap();
            samples.push(t0.elapsed().as_secs_f64() * 1e3);
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        println!(
            "preprocess_into 1280x720 -> 640: p50 {:.4} ms, p95 {:.4} ms",
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100]
        );
    }
}
