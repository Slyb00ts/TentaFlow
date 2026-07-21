// Standalone cudarc harness: load Modular's AOT-exported multistage fp8 GEMM
// PTX and launch it WITHOUT the Mojo runtime, replicating the host launch
// geometry (grid/block/dynamic-smem) that `multistage_gemm` computes.
//
// Kernel: multistage_gemm_kernel[c=f32, a=e4m3, b=e4m3, transpose_b=True],
//   config block=128x128x64 warp=64x64x64 stages=4 kpart=1
//   -> block_dim = 128 threads, grid = (ceil(N/128), ceil(M/128), 1),
//      dynamic smem = 2 * (128*64*4 stages) * 1 byte = 65536 bytes.
// Params (from PTX .entry, 3 pointer slots): param_0=c, param_1=a, param_2=b.

use std::ffi::c_void;
use std::time::Instant;

use cudarc::driver::sys::CUfunction_attribute_enum;
use cudarc::driver::{result, CudaContext};

// e4m3 exact byte encodings for the {0.5,-0.5,1.0,-1.0} pattern.
fn fp8_byte(m: usize) -> u8 {
    match m & 3 {
        0 => 0x30, // 0.5
        1 => 0xB0, // -0.5
        2 => 0x38, // 1.0
        _ => 0xB8, // -1.0
    }
}
fn fp8_val(m: usize) -> f32 {
    match m & 3 {
        0 => 0.5,
        1 => -0.5,
        2 => 1.0,
        _ => -1.0,
    }
}

fn entry_name(ptx: &str) -> String {
    let marker = ".visible .entry ";
    let i = ptx.find(marker).expect("no .entry") + marker.len();
    let j = ptx[i..].find('(').expect("malformed entry") + i;
    ptx[i..j].trim().to_string()
}

struct Shape {
    file: &'static str,
    m: usize,
    n: usize,
    k: usize,
    correctness: bool,
}

fn main() {
    let ptx_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../kernels/mojo/scratch/modular_ptx");
    let shapes = [
        Shape { file: "modular_fp8_corr_256_512_512.ptx", m: 256, n: 512, k: 512, correctness: true },
        Shape { file: "modular_fp8_2048_4096_14336.ptx", m: 2048, n: 4096, k: 14336, correctness: false },
        Shape { file: "modular_fp8_2048_14336_4096.ptx", m: 2048, n: 14336, k: 4096, correctness: false },
        Shape { file: "modular_fp8_512_14336_4096.ptx", m: 512, n: 14336, k: 4096, correctness: false },
        Shape { file: "modular_fp8_512_4096_14336.ptx", m: 512, n: 4096, k: 14336, correctness: false },
    ];

    let ctx = CudaContext::new(0).expect("CudaContext::new");
    ctx.bind_to_thread().expect("bind");
    println!("device: {}", ctx.name().unwrap_or_default());

    const SMEM: u32 = 65536;
    const BLOCK: u32 = 128;

    for s in &shapes {
        let path = format!("{ptx_dir}/{}", s.file);
        let ptx = std::fs::read_to_string(&path).expect("read ptx");
        let name = entry_name(&ptx);
        let mut ptx_z = ptx.into_bytes();
        ptx_z.push(0);
        let module = unsafe { result::module::load_data(ptx_z.as_ptr() as *const c_void) }
            .expect("load_data");
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        let func = unsafe { result::module::get_function(module, cname) }.expect("get_function");

        // Opt in to >48KB dynamic shared memory (mirrors forge-hal).
        unsafe {
            result::function::set_function_attribute(
                func,
                CUfunction_attribute_enum::CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES,
                SMEM as i32,
            )
        }
        .expect("set smem attr");

        let (m, n, k) = (s.m, s.n, s.k);
        // Host buffers (fp8 bytes for a[MxK], b[NxK]; f32 for c[MxN]).
        let mut a_h = vec![0u8; m * k];
        let mut b_h = vec![0u8; n * k];
        for i in 0..m * k {
            a_h[i] = fp8_byte(i * 7 + 1);
        }
        for i in 0..n * k {
            b_h[i] = fp8_byte(i * 3 + 2);
        }

        let a_d = unsafe { result::malloc_sync(m * k) }.expect("malloc a");
        let b_d = unsafe { result::malloc_sync(n * k) }.expect("malloc b");
        let c_d = unsafe { result::malloc_sync(m * n * 4) }.expect("malloc c");
        unsafe {
            result::memcpy_htod_sync(a_d, &a_h).expect("h2d a");
            result::memcpy_htod_sync(b_d, &b_h).expect("h2d b");
        }

        // param slots: c, a, b
        let slots: [u64; 3] = [c_d, a_d, b_d];
        let mut params: Vec<*mut c_void> =
            slots.iter().map(|p| p as *const u64 as *mut c_void).collect();

        let grid = (((n as u32) + BLOCK - 1) / BLOCK, ((m as u32) + BLOCK - 1) / BLOCK, 1u32);
        let block = (BLOCK, 1u32, 1u32);

        let launch = |params: &mut Vec<*mut c_void>| unsafe {
            result::launch_kernel(func, grid, block, SMEM, std::ptr::null_mut(), params)
                .expect("launch");
        };

        // Warmup + correctness.
        launch(&mut params);
        unsafe { result::stream::synchronize(std::ptr::null_mut()) }.expect("sync");

        if s.correctness {
            let mut c_h = vec![0f32; m * n];
            unsafe { result::memcpy_dtoh_sync(&mut c_h, c_d) }.expect("d2h c");
            let mut max_rel = 0f32;
            let mut max_abs = 0f32;
            for mm in 0..m {
                for nn in 0..n {
                    let mut acc = 0f32;
                    for kk in 0..k {
                        acc += fp8_val((mm * k + kk) * 7 + 1) * fp8_val((nn * k + kk) * 3 + 2);
                    }
                    let got = c_h[mm * n + nn];
                    let d = (got - acc).abs();
                    if d > max_abs {
                        max_abs = d;
                    }
                    let denom = acc.abs().max(1.0);
                    let rel = d / denom;
                    if rel > max_rel {
                        max_rel = rel;
                    }
                }
            }
            println!(
                "[correctness] {}x{}x{}  max_rel_err={:.3e}  max_abs_err={:.3e}",
                m, n, k, max_rel, max_abs
            );
        } else {
            // Steady-state timing: warmup then best-of-40.
            for _ in 0..200 {
                launch(&mut params);
            }
            unsafe { result::stream::synchronize(std::ptr::null_mut()) }.expect("sync");
            const ITERS: usize = 40;
            let t0 = Instant::now();
            for _ in 0..ITERS {
                launch(&mut params);
            }
            unsafe { result::stream::synchronize(std::ptr::null_mut()) }.expect("sync");
            let ms = t0.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
            let ops = 2.0 * m as f64 * n as f64 * k as f64;
            let tflops = ops / (ms / 1e3) / 1e12;
            println!(
                "[bench] T={:5} N={:6} K={:6}  {:7.1} TFLOPS  ({:.4} ms)",
                m, n, k, tflops, ms
            );
        }

        unsafe {
            let _ = result::free_sync(a_d);
            let _ = result::free_sync(b_d);
            let _ = result::free_sync(c_d);
            let _ = result::module::unload(module);
        }
    }
}
