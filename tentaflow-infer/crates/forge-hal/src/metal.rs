// ===== File: metal.rs — Metal backend: unified-memory buffers, batched command buffers =====
//
// The layer between the Objective-C shim and the rest of FORGE. Two properties
// are deliberate and both come from measurement, not taste
// (docs/pomiary/eks-a1-a3-apple-m4.md):
//
//   * Buffers are `Shared`, so a "device" allocation IS host memory. There is
//     no host-to-device copy on Apple and pretending otherwise would move
//     gigabytes for nothing when a model loads.
//   * A command buffer is an object with a lifetime, not a hidden detail of
//     "launch a kernel". One dispatch inside an open buffer costs 0.61 us,
//     a buffer of its own costs 19.6 us and a host round trip ~94 us — so the
//     API makes batching the easy thing and the round trip explicit.
//
// The `Device` trait implementation builds on this and is the next step; what
// is here is complete, tested, and does not pretend to be more.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use forge_types::{ForgeError, Result};

#[link(name = "forge_metal_shim", kind = "static")]
extern "C" {
    fn fm_device_create() -> *mut c_void;
    fn fm_device_name(device: *mut c_void) -> *const c_char;
    fn fm_device_working_set(device: *mut c_void) -> u64;
    fn fm_device_threadgroup_memory(device: *mut c_void) -> u64;
    fn fm_device_has_unified_memory(device: *mut c_void) -> c_int;
    fn fm_device_max_threads(device: *mut c_void) -> u64;
    fn fm_queue_create(device: *mut c_void) -> *mut c_void;
    fn fm_buffer_new(device: *mut c_void, length: u64) -> *mut c_void;
    fn fm_buffer_contents(buffer: *mut c_void) -> *mut c_void;
    fn fm_library_new(
        device: *mut c_void,
        source: *const c_char,
        err: *mut c_char,
        err_len: c_int,
    ) -> *mut c_void;
    fn fm_pipeline_new(
        device: *mut c_void,
        library: *mut c_void,
        name: *const c_char,
        err: *mut c_char,
        err_len: c_int,
    ) -> *mut c_void;
    fn fm_pipeline_max_threads(pipeline: *mut c_void) -> u64;
    fn fm_cmdbuf_new(queue: *mut c_void) -> *mut c_void;
    fn fm_dispatch(
        cmdbuf: *mut c_void,
        pipeline: *mut c_void,
        buffers: *const *mut c_void,
        offsets: *const u64,
        scalars: *const u64,
        is_buffer: *const c_int,
        arg_count: c_int,
        threadgroups_x: u32,
        threadgroups_y: u32,
        threads_per_group: u32,
    );
    fn fm_commit(cmdbuf: *mut c_void);
    fn fm_wait(cmdbuf: *mut c_void);
    fn fm_cmdbuf_completed(cmdbuf: *mut c_void) -> c_int;
    fn fm_cmdbuf_failed(cmdbuf: *mut c_void, err: *mut c_char, err_len: c_int) -> c_int;
    fn fm_release(object: *mut c_void);
}

const ERR_LEN: usize = 1024;

fn take_error(buf: &[c_char; ERR_LEN]) -> String {
    // Safe: the shim always writes a NUL-terminated string into the buffer.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// An owned Objective-C object. Releasing is the only thing every handle needs.
#[derive(Debug)]
struct Owned(*mut c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { fm_release(self.0) };
    }
}

// The handles are used behind `&` from one thread at a time; Metal objects are
// themselves thread-safe for the operations exposed here.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

pub struct MetalContext {
    device: Owned,
    queue: Owned,
}

/// What the device reports about itself. Fed into the capability registry
/// rather than consulted ad hoc, so no heuristic reads it twice differently.
#[derive(Debug, Clone)]
pub struct MetalCaps {
    pub name: String,
    pub unified_memory: bool,
    pub working_set_bytes: u64,
    pub threadgroup_memory_bytes: u64,
    pub max_threads_per_group: u32,
}

impl MetalContext {
    pub fn new() -> Result<Self> {
        let device = unsafe { fm_device_create() };
        if device.is_null() {
            return Err(ForgeError::Unsupported(
                "Metal: brak urządzenia systemowego".into(),
            ));
        }
        let queue = unsafe { fm_queue_create(device) };
        if queue.is_null() {
            unsafe { fm_release(device) };
            return Err(ForgeError::Unsupported(
                "Metal: nie udało się utworzyć kolejki poleceń".into(),
            ));
        }
        Ok(Self {
            device: Owned(device),
            queue: Owned(queue),
        })
    }

    pub fn caps(&self) -> MetalCaps {
        let name = unsafe { CStr::from_ptr(fm_device_name(self.device.0)) }
            .to_string_lossy()
            .into_owned();
        MetalCaps {
            name,
            unified_memory: unsafe { fm_device_has_unified_memory(self.device.0) } != 0,
            working_set_bytes: unsafe { fm_device_working_set(self.device.0) },
            threadgroup_memory_bytes: unsafe { fm_device_threadgroup_memory(self.device.0) },
            max_threads_per_group: unsafe { fm_device_max_threads(self.device.0) } as u32,
        }
    }

    /// Allocates shared memory. The returned buffer is readable and writable by
    /// the host without any transfer.
    pub fn alloc(&self, bytes: usize) -> Result<MetalBuffer> {
        let raw = unsafe { fm_buffer_new(self.device.0, bytes as u64) };
        if raw.is_null() {
            return Err(ForgeError::Other(format!(
                "Metal: alokacja {bytes} B nie powiodła się"
            )));
        }
        let host = unsafe { fm_buffer_contents(raw) } as *mut u8;
        Ok(MetalBuffer {
            handle: Owned(raw),
            host,
            len: bytes,
        })
    }

    /// Compiles a Metal Shading Language source at runtime. Compilation is
    /// cheap on Apple and this is what replaces a prebuilt kernel catalogue.
    pub fn library(&self, source: &str) -> Result<MetalLibrary> {
        let src = CString::new(source)
            .map_err(|_| ForgeError::Other("Metal: źródło zawiera bajt zerowy".into()))?;
        let mut err = [0 as c_char; ERR_LEN];
        let raw = unsafe {
            fm_library_new(
                self.device.0,
                src.as_ptr(),
                err.as_mut_ptr(),
                ERR_LEN as c_int,
            )
        };
        if raw.is_null() {
            return Err(ForgeError::Other(format!(
                "Metal: kompilacja MSL nie powiodła się: {}",
                take_error(&err)
            )));
        }
        Ok(MetalLibrary(Owned(raw)))
    }

    pub fn pipeline(&self, library: &MetalLibrary, function: &str) -> Result<MetalPipeline> {
        let name = CString::new(function)
            .map_err(|_| ForgeError::Other("Metal: nazwa funkcji z bajtem zerowym".into()))?;
        let mut err = [0 as c_char; ERR_LEN];
        let raw = unsafe {
            fm_pipeline_new(
                self.device.0,
                (library.0).0,
                name.as_ptr(),
                err.as_mut_ptr(),
                ERR_LEN as c_int,
            )
        };
        if raw.is_null() {
            return Err(ForgeError::Other(format!(
                "Metal: pipeline '{function}': {}",
                take_error(&err)
            )));
        }
        let max_threads = unsafe { fm_pipeline_max_threads(raw) } as u32;
        Ok(MetalPipeline {
            handle: Owned(raw),
            max_threads_per_group: max_threads,
        })
    }

    /// Opens a command buffer. Every dispatch added to it before `commit`
    /// pays 0.61 us instead of the 19.6 us a fresh buffer costs.
    pub fn command_buffer(&self) -> Result<MetalCommandBuffer> {
        let raw = unsafe { fm_cmdbuf_new(self.queue.0) };
        if raw.is_null() {
            return Err(ForgeError::Other("Metal: brak bufora poleceń".into()));
        }
        Ok(MetalCommandBuffer {
            handle: Owned(raw),
            dispatches: 0,
        })
    }
}

pub struct MetalBuffer {
    handle: Owned,
    host: *mut u8,
    len: usize,
}

impl MetalBuffer {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Host view of the same memory the GPU reads. No copy is involved.
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.host, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.host, self.len) }
    }

    pub(crate) fn raw(&self) -> *mut c_void {
        self.handle.0
    }

    /// Base host pointer of the whole allocation.
    pub(crate) fn as_ptr(&self) -> *mut u8 {
        self.host
    }
}

// The host pointer is the buffer's own mapping and is valid for as long as the
// buffer lives; synchronizing access is the caller's job, exactly as it is for
// VRAM in the CUDA and HIP backends.
unsafe impl Send for MetalBuffer {}
unsafe impl Sync for MetalBuffer {}

#[derive(Debug)]
pub struct MetalLibrary(Owned);

pub struct MetalPipeline {
    handle: Owned,
    pub max_threads_per_group: u32,
}

/// One kernel argument: a bound buffer with a byte offset, or a value passed
/// inline. Mirrors `LaunchArgs::kinds`.
pub enum MetalArg<'a> {
    Buffer(&'a MetalBuffer, u64),
    Scalar(u64),
}

pub struct MetalCommandBuffer {
    handle: Owned,
    dispatches: u32,
}

impl MetalCommandBuffer {
    /// Number of dispatches encoded so far — the quantity EKS-A3 prices, so it
    /// is observable rather than guessed at.
    pub fn dispatch_count(&self) -> u32 {
        self.dispatches
    }

    /// Adds one dispatch over a one-dimensional grid.
    pub fn dispatch(
        &mut self,
        pipeline: &MetalPipeline,
        args: &[MetalArg<'_>],
        threadgroups: u32,
        threads_per_group: u32,
    ) -> Result<()> {
        self.dispatch_2d(pipeline, args, (threadgroups, 1), threads_per_group)
    }

    /// Adds one dispatch. Argument `i` binds to Metal index `i`, which is the
    /// order the kernels declare `[[buffer(n)]]` in.
    ///
    /// The second grid dimension exists for kernels that tile two independent
    /// axes at once — a batched matmul tiles output rows and tokens — where
    /// folding them into one index would make the kernel divide to get them back.
    pub fn dispatch_2d(
        &mut self,
        pipeline: &MetalPipeline,
        args: &[MetalArg<'_>],
        threadgroups: (u32, u32),
        threads_per_group: u32,
    ) -> Result<()> {
        if threads_per_group == 0 || threads_per_group > pipeline.max_threads_per_group {
            return Err(ForgeError::Other(format!(
                "Metal: {threads_per_group} wątków na grupę, limit tego kernela to {}",
                pipeline.max_threads_per_group
            )));
        }
        let mut handles: Vec<*mut c_void> = Vec::with_capacity(args.len());
        let mut offsets: Vec<u64> = Vec::with_capacity(args.len());
        let mut scalars: Vec<u64> = Vec::with_capacity(args.len());
        let mut is_buffer: Vec<c_int> = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                MetalArg::Buffer(buf, offset) => {
                    handles.push(buf.raw());
                    offsets.push(*offset);
                    scalars.push(0);
                    is_buffer.push(1);
                }
                MetalArg::Scalar(value) => {
                    handles.push(std::ptr::null_mut());
                    offsets.push(0);
                    scalars.push(*value);
                    is_buffer.push(0);
                }
            }
        }
        unsafe {
            fm_dispatch(
                self.handle.0,
                pipeline.handle.0,
                handles.as_ptr(),
                offsets.as_ptr(),
                scalars.as_ptr(),
                is_buffer.as_ptr(),
                args.len() as c_int,
                threadgroups.0,
                threadgroups.1,
                threads_per_group,
            );
        }
        self.dispatches += 1;
        Ok(())
    }

    /// Submits the work without blocking. Ordering against later buffers on the
    /// same queue is guaranteed by the queue itself.
    pub fn commit(&self) {
        unsafe { fm_commit(self.handle.0) };
    }

    /// Blocks until this buffer finishes. This is the host round trip the
    /// measurement prices at ~94 us, so it is a separate, named call.
    pub fn wait(&self) -> Result<()> {
        unsafe { fm_wait(self.handle.0) };
        let mut err = [0 as c_char; ERR_LEN];
        if unsafe { fm_cmdbuf_failed(self.handle.0, err.as_mut_ptr(), ERR_LEN as c_int) } != 0 {
            return Err(ForgeError::Other(format!(
                "Metal: bufor poleceń zakończył się błędem: {}",
                take_error(&err)
            )));
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        unsafe { fm_cmdbuf_completed(self.handle.0) != 0 }
    }

    pub fn commit_and_wait(self) -> Result<()> {
        self.commit();
        self.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
        #include <metal_stdlib>
        using namespace metal;
        kernel void scale_add(device float* out [[buffer(0)]],
                              device const float* in [[buffer(1)]],
                              constant float& k [[buffer(2)]],
                              uint gid [[thread_position_in_grid]]) {
            out[gid] = in[gid] * k + 1.0f;
        }
    "#;

    #[test]
    fn runs_a_real_kernel_through_the_shim() {
        let ctx = MetalContext::new().expect("urządzenie Metal");
        let caps = ctx.caps();
        assert!(caps.unified_memory, "Apple GPU ma pamięć unified");
        assert!(caps.working_set_bytes > 0);

        let n = 1024usize;
        let mut input = ctx.alloc(n * 4).unwrap();
        let output = ctx.alloc(n * 4).unwrap();
        for (i, chunk) in input.as_mut_slice().chunks_exact_mut(4).enumerate() {
            chunk.copy_from_slice(&(i as f32).to_le_bytes());
        }

        let lib = ctx.library(SRC).unwrap();
        let pipe = ctx.pipeline(&lib, "scale_add").unwrap();

        let mut cb = ctx.command_buffer().unwrap();
        cb.dispatch(
            &pipe,
            &[
                MetalArg::Buffer(&output, 0),
                MetalArg::Buffer(&input, 0),
                MetalArg::Scalar(3.0f32.to_bits() as u64),
            ],
            4,
            256,
        )
        .unwrap();
        assert_eq!(cb.dispatch_count(), 1);
        cb.commit_and_wait().unwrap();

        for (i, chunk) in output.as_slice().chunks_exact(4).enumerate() {
            let got = f32::from_le_bytes(chunk.try_into().unwrap());
            assert_eq!(got, i as f32 * 3.0 + 1.0, "element {i}");
        }
    }

    #[test]
    fn many_dispatches_share_one_command_buffer() {
        // The batching property the whole backend is designed around: several
        // dispatches, one commit, one host round trip.
        let ctx = MetalContext::new().unwrap();
        let n = 256usize;
        let mut a = ctx.alloc(n * 4).unwrap();
        let b = ctx.alloc(n * 4).unwrap();
        for chunk in a.as_mut_slice().chunks_exact_mut(4) {
            chunk.copy_from_slice(&1.0f32.to_le_bytes());
        }

        let lib = ctx.library(SRC).unwrap();
        let pipe = ctx.pipeline(&lib, "scale_add").unwrap();
        let mut cb = ctx.command_buffer().unwrap();
        // b = a*2+1 = 3, then a = b*2+1 = 7, then b = a*2+1 = 15.
        let two = MetalArg::Scalar(2.0f32.to_bits() as u64);
        cb.dispatch(
            &pipe,
            &[MetalArg::Buffer(&b, 0), MetalArg::Buffer(&a, 0), two],
            1,
            256,
        )
        .unwrap();
        let two = MetalArg::Scalar(2.0f32.to_bits() as u64);
        cb.dispatch(
            &pipe,
            &[MetalArg::Buffer(&a, 0), MetalArg::Buffer(&b, 0), two],
            1,
            256,
        )
        .unwrap();
        let two = MetalArg::Scalar(2.0f32.to_bits() as u64);
        cb.dispatch(
            &pipe,
            &[MetalArg::Buffer(&b, 0), MetalArg::Buffer(&a, 0), two],
            1,
            256,
        )
        .unwrap();
        assert_eq!(cb.dispatch_count(), 3);
        cb.commit_and_wait().unwrap();

        for chunk in b.as_slice().chunks_exact(4) {
            assert_eq!(f32::from_le_bytes(chunk.try_into().unwrap()), 15.0);
        }
    }

    #[test]
    fn a_broken_kernel_reports_the_compiler_message() {
        let ctx = MetalContext::new().unwrap();
        let err = ctx.library("kernel void nope(} {").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("MSL"), "komunikat bez kontekstu: {msg}");
    }

    #[test]
    fn an_oversized_threadgroup_is_refused_before_dispatch() {
        let ctx = MetalContext::new().unwrap();
        let out = ctx.alloc(64).unwrap();
        let lib = ctx.library(SRC).unwrap();
        let pipe = ctx.pipeline(&lib, "scale_add").unwrap();
        let mut cb = ctx.command_buffer().unwrap();
        let err = cb
            .dispatch(
                &pipe,
                &[MetalArg::Buffer(&out, 0), MetalArg::Buffer(&out, 0)],
                1,
                100_000,
            )
            .unwrap_err();
        assert!(format!("{err}").contains("limit"));
    }
}
