// =============================================================================
// File: vision/gpu_preprocess.rs — GPU-resident crop preprocess (CUDA)
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
// Gated on both features because it only exists to feed the ort device-tensor
// path; a build without either never links CUDA and never invokes nvcc.

#![cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]

use anyhow::{bail, Result};
use std::cell::RefCell;
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

    fn launch_nv12_to_rgb_resize_normalize(
        y_ptrs: *const *const u8,
        y_strides: *const c_int,
        uv_ptrs: *const *const u8,
        uv_strides: *const c_int,
        widths: *const c_int,
        heights: *const c_int,
        n: c_int,
        s: c_int,
        mean: *const f32,
        stdv: *const f32,
        kr: f32,
        kb: f32,
        full_range: c_int,
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

/// Per-thread CUDA scratch: one non-blocking stream + grow-only device buffers
/// reused across every `preprocess_batch_gpu` call on this thread. Lives in a
/// `thread_local`; created lazily on first use. `Drop` releases the stream and
/// buffers at thread exit (a leak on abrupt teardown is acceptable — the process
/// is ending — but the explicit teardown keeps long-lived pools tidy).
struct ThreadScratch {
    stream: CudaStream,
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
}

impl ThreadScratch {
    fn new() -> Result<Self> {
        let mut stream: CudaStream = std::ptr::null_mut();
        cuda_check(
            unsafe {
                cudaStreamCreateWithFlags(&mut stream as *mut CudaStream, CUDA_STREAM_NON_BLOCKING)
            },
            "cudaStreamCreateWithFlags",
        )?;
        Ok(Self {
            stream,
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
        })
    }
}

impl Drop for ThreadScratch {
    fn drop(&mut self) {
        // GrowBuf fields drop (cudaFree) automatically; only the stream needs an
        // explicit destroy. Errors at teardown are ignored.
        if !self.stream.is_null() {
            unsafe {
                cudaStreamDestroy(self.stream);
            }
        }
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
        let stream = sc.stream;

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
/// case is `n = 1`.
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
        let stream = sc.stream;

        // Validate frames and compute contiguous packing offsets for each plane.
        let mut y_offsets: Vec<usize> = Vec::with_capacity(n);
        let mut uv_offsets: Vec<usize> = Vec::with_capacity(n);
        let mut host_y_strides: Vec<c_int> = Vec::with_capacity(n);
        let mut host_uv_strides: Vec<c_int> = Vec::with_capacity(n);
        let mut host_ws: Vec<c_int> = Vec::with_capacity(n);
        let mut host_hs: Vec<c_int> = Vec::with_capacity(n);
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
                n as c_int,
                s as c_int,
                sc.mean.ptr as *const f32,
                sc.stdv.ptr as *const f32,
                color.kr,
                color.kb,
                if color.full_range { 1 } else { 0 },
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

    let trust_map_sync = crate::vision::settings::get().zerocopy_map_sync;
    if !trust_map_sync {
        cuda_check(
            unsafe { cudaDeviceSynchronize() },
            "cudaDeviceSynchronize crop-decode-wait",
        )?;
    }

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

/// Zero-copy device preprocess: runs the SAME fused NV12→RGB + Q8 resize +
/// normalize kernel as [`preprocess_nv12_batch_gpu`], but reads the NVDEC decode
/// surface IN PLACE (no host download, no re-upload of the pixel data) and writes
/// an OWNED `[1,3,S,S]` device tensor. Only the tiny descriptor arrays (plane
/// pointers, strides, dims, mean/std — a few dozen bytes) are uploaded; the
/// full-frame H2D that dominated the download path is gone.
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
    if s == 0 {
        bail!("preprocess_nv12_device_gpu: S must be > 0");
    }
    if planes.w == 0 || planes.h == 0 {
        bail!("preprocess_nv12_device_gpu: frame has a zero dimension");
    }
    let (w, _h) = (planes.w as usize, planes.h as usize);
    if planes.y_stride < w {
        bail!(
            "preprocess_nv12_device_gpu: y_stride {} < width {w}",
            planes.y_stride
        );
    }
    if planes.uv_stride < ((w + 1) / 2) * 2 {
        bail!(
            "preprocess_nv12_device_gpu: uv_stride {} too small for width {w}",
            planes.uv_stride
        );
    }

    // Default-on device sync guarantees the decoder finished writing the surface
    // before the kernel reads it, regardless of nvcodec's internal stream setup.
    let trust_map_sync = crate::vision::settings::get().zerocopy_map_sync;

    SCRATCH.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = Some(ThreadScratch::new()?);
        }
        let sc = guard.as_mut().expect("scratch initialized above");
        let stream = sc.stream;

        // Tiny descriptor arrays (n = 1). No pixel H2D — the plane pointers ARE
        // the decoder's device addresses.
        let host_y_ptrs: [*const u8; 1] = [planes.y_ptr as *const u8];
        let host_uv_ptrs: [*const u8; 1] = [planes.uv_ptr as *const u8];
        let host_y_strides: [c_int; 1] = [planes.y_stride as c_int];
        let host_uv_strides: [c_int; 1] = [planes.uv_stride as c_int];
        let host_ws: [c_int; 1] = [planes.w as c_int];
        let host_hs: [c_int; 1] = [planes.h as c_int];

        let ptr_bytes = std::mem::size_of::<*const u8>();
        sc.nv12_y_ptrs.ensure(ptr_bytes)?;
        sc.nv12_uv_ptrs.ensure(ptr_bytes)?;
        let int_bytes = std::mem::size_of::<c_int>();
        sc.nv12_y_strides.ensure(int_bytes)?;
        sc.nv12_uv_strides.ensure(int_bytes)?;
        sc.nv12_ws.ensure(int_bytes)?;
        sc.nv12_hs.ensure(int_bytes)?;
        sc.mean.ensure(3 * 4)?;
        sc.stdv.ensure(3 * 4)?;

        sc.nv12_y_ptrs
            .h2d_at(0, host_y_ptrs.as_ptr() as *const c_void, ptr_bytes, stream)?;
        sc.nv12_uv_ptrs
            .h2d_at(0, host_uv_ptrs.as_ptr() as *const c_void, ptr_bytes, stream)?;
        sc.nv12_y_strides
            .h2d_at(0, host_y_strides.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_uv_strides.h2d_at(
            0,
            host_uv_strides.as_ptr() as *const c_void,
            int_bytes,
            stream,
        )?;
        sc.nv12_ws
            .h2d_at(0, host_ws.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.nv12_hs
            .h2d_at(0, host_hs.as_ptr() as *const c_void, int_bytes, stream)?;
        sc.mean
            .h2d_at(0, mean.as_ptr() as *const c_void, 3 * 4, stream)?;
        sc.stdv
            .h2d_at(0, stdv.as_ptr() as *const c_void, 3 * 4, stream)?;

        // OWNED output — its own allocation so it survives the source surface's
        // unmap and can cross to the ORT thread.
        let out_bytes = 3 * s * s * std::mem::size_of::<f32>();
        let mut out_ptr: *mut c_void = std::ptr::null_mut();
        cuda_check(
            unsafe { cudaMalloc(&mut out_ptr as *mut *mut c_void, out_bytes) },
            "cudaMalloc device-tensor output",
        )?;
        let out_ptr = out_ptr as *mut f32;

        // Ensure decode completion BEFORE the kernel reads the surface (see the
        // synchronization note). Descriptor H2D copies above are on `stream` and
        // ordered before the kernel on the same stream, so only the decode needs
        // an explicit barrier.
        if !trust_map_sync {
            cuda_check(unsafe { cudaDeviceSynchronize() }, "cudaDeviceSynchronize decode-wait")?;
        }

        let rc = unsafe {
            launch_nv12_to_rgb_resize_normalize(
                sc.nv12_y_ptrs.ptr as *const *const u8,
                sc.nv12_y_strides.ptr as *const c_int,
                sc.nv12_uv_ptrs.ptr as *const *const u8,
                sc.nv12_uv_strides.ptr as *const c_int,
                sc.nv12_ws.ptr as *const c_int,
                sc.nv12_hs.ptr as *const c_int,
                1,
                s as c_int,
                sc.mean.ptr as *const f32,
                sc.stdv.ptr as *const f32,
                color.kr,
                color.kb,
                if color.full_range { 1 } else { 0 },
                out_ptr,
                stream,
            )
        };
        if let Err(e) = cuda_check(rc, "launch_nv12_to_rgb_resize_normalize (device)") {
            unsafe { cudaFree(out_ptr as *mut c_void) };
            return Err(e);
        }
        // Sync our stream so the kernel has fully consumed the borrowed NV12
        // surface before this returns (the caller then unmaps it). Also keeps the
        // stack descriptor arrays alive until their async copies finish.
        if let Err(e) = cuda_check(unsafe { cudaStreamSynchronize(stream) }, "cudaStreamSynchronize") {
            unsafe { cudaFree(out_ptr as *mut c_void) };
            return Err(e);
        }

        Ok(OwnedDeviceTensor {
            ptr: out_ptr,
            n: 1,
            s,
        })
    })
}
