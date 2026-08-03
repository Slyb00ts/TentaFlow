// ===== File: cpu_matmul.rs — the CPU's share of a prefill matrix product =====
//
// Prefill leaves the GPU at 77% of its matrix ceiling (3,02 of 3,94 TFLOPS) and
// barely touches memory, because it reads each weight once per tile instead of
// once per token. That is the one condition under which a second compute unit
// adds throughput instead of taking it: measured concurrently, the GPU lost
// 0,3% and the CPU 3%, so 3,02 + 1,47 TFLOPS were available where the GPU alone
// gave 3,02 (docs/pomiary/eks-a7-cpu-gpu-wspolbieznie-m4.md).
//
// DECODE MUST NEVER COME HERE. It runs at 86% of the bandwidth ceiling, the
// memory is shared, and adding compute cost 14% — 20,9 down to 17,9 tok/s in
// the same measurement. The caller decides, and the registry entry says so.
//
// The weights are NOT duplicated. Accelerate cannot read our 4-bit affine
// layout, so the slice is unpacked on every call into a scratch buffer that is
// allocated once and reused. That costs 27% against keeping a second f16 copy
// of the CPU's rows, which is what the 27% buys: 4,5 GB not spent.
//
// The unpacking here MIRRORS `qmg_affine_4bit_source`: nibbles LSB-first inside
// each u32, scale and bias per (row, group) in bf16, value = nibble * scale +
// bias. It is not bit-identical — the GPU rounds the multiply-add to half and
// this does it in f32 — which is why the gate compares against the GPU path
// with a tolerance rather than for equality.

use std::ffi::c_void;

use forge_types::{ForgeError, Result};
use half::f16;

/// Reusable scratch for the CPU half. One per model, not one per call: the
/// buffer is the largest slice any layer asks for, and allocating it per
/// product would put a multi-megabyte allocation on the critical path.
pub struct CpuMatmul {
    weights: Vec<f16>,
    tiles: usize,
}

/// What the CPU half needs to find its inputs. Raw pointers because they come
/// from Metal buffers the GPU is reading at the same moment — shared memory,
/// no copy, and the disjointness that makes it safe is in `run`'s contract.
pub struct Operands<'a> {
    pub packed: &'a [u32],
    pub scales: &'a [u16],
    pub biases: &'a [u16],
    pub x: &'a [u16],
    pub out: *mut u8,
    pub out_f16: bool,
    pub tokens: u32,
    pub rows: u32,
    pub cols: u32,
    pub group: u32,
}

impl CpuMatmul {
    pub fn new() -> Self {
        // Work is handed out in tiles rather than per row: `dispatch_apply_f`
        // costs a barrier per iteration, and a row of 4096 nibbles is far too
        // little to pay for one.
        Self {
            weights: Vec::new(),
            tiles: 16,
        }
    }

    /// Unpacks rows `[row0, row0 + count)` of the weight into the scratch.
    fn unpack(&mut self, op: &Operands<'_>, row0: usize, count: usize) {
        let cols = op.cols as usize;
        self.weights.resize(count * cols, f16::ZERO);
        let job = DequantJob {
            out: self.weights.as_mut_ptr(),
            packed: op.packed.as_ptr(),
            scales: op.scales.as_ptr(),
            biases: op.biases.as_ptr(),
            row0,
            count,
            cols,
            group: op.group as usize,
            tiles: self.tiles,
        };
        // SAFETY: every tile writes a disjoint row range of `self.weights` and
        // reads only immutable inputs, so the pointers may cross threads.
        unsafe {
            dispatch_apply_f(
                self.tiles,
                dispatch_get_global_queue(QOS_USER_INITIATED, 0),
                &job as *const DequantJob as *mut c_void,
                dequant_tile,
            );
        }
    }

    /// Computes rows `[row0, row0 + count)` of `out = x * wᵀ`.
    ///
    /// # Safety
    ///
    /// `out` must be a buffer of `tokens * rows` values of the type `out_f16`
    /// names, and the caller must guarantee that nothing else writes rows
    /// `[row0, row0 + count)` while this runs. The GPU half writes rows below
    /// `row0` in the same buffer at the same time, which is disjoint and
    /// therefore fine on unified memory.
    pub unsafe fn run(&mut self, op: &Operands<'_>, row0: u32, count: u32) -> Result<()> {
        let (rows, cols, group) = (op.rows as usize, op.cols as usize, op.group as usize);
        let (tokens, row0, count) = (op.tokens as usize, row0 as usize, count as usize);
        if cols % group != 0 || cols % 8 != 0 {
            return Err(ForgeError::Unsupported(format!(
                "CPU: kolumny {cols} nie dzielą się na grupę {group} i słowa po 8"
            )));
        }
        if row0 + count > rows {
            return Err(ForgeError::Other(format!(
                "CPU: wycinek [{row0}, {}) wykracza poza {rows} wierszy",
                row0 + count
            )));
        }

        self.unpack(op, row0, count);
        let out_ty = if op.out_f16 { BNNS_F16 } else { BNNS_F32 };
        let elem = if op.out_f16 { 2 } else { 4 };
        let a = descriptor(cols, tokens, op.x.as_ptr() as *mut c_void, BNNS_F16, 0);
        let b = descriptor(
            cols,
            count,
            self.weights.as_mut_ptr() as *mut c_void,
            BNNS_F16,
            0,
        );
        // The output is a WINDOW on the full result: `count` rows starting at
        // `row0`, with the stride of the whole matrix, so BNNS writes straight
        // into the buffer the GPU is filling the rest of.
        let c = descriptor(
            count,
            tokens,
            // SAFETY: `row0 * elem` is inside the buffer by the check above.
            unsafe { op.out.add(row0 * elem) } as *mut c_void,
            out_ty,
            rows,
        );

        // SAFETY: three descriptors of matching inner dimension, all pointing
        // at live buffers for the duration of the call.
        let rc = unsafe { BNNSMatMul(false, true, 1.0, &a, &b, &c, std::ptr::null_mut(), std::ptr::null()) };
        if rc != 0 {
            return Err(ForgeError::Other(format!(
                "CPU: BNNSMatMul odmówił ({rc}) dla [{count} x {cols}] przy {tokens} tokenach"
            )));
        }
        Ok(())
    }
}

impl Default for CpuMatmul {
    fn default() -> Self {
        Self::new()
    }
}

struct DequantJob {
    out: *mut f16,
    packed: *const u32,
    scales: *const u16,
    biases: *const u16,
    row0: usize,
    count: usize,
    cols: usize,
    group: usize,
    tiles: usize,
}

/// bf16 is the top half of an f32, so widening is a shift — no table, no branch.
#[inline(always)]
fn bf16(bits: u16) -> f32 {
    f32::from_bits(u32::from(bits) << 16)
}

extern "C" fn dequant_tile(ctx: *mut c_void, tile: usize) {
    // SAFETY: `ctx` is the `DequantJob` this dispatch was given, alive for the
    // whole call, and this tile touches only its own rows.
    let job = unsafe { &*(ctx as *const DequantJob) };
    let per_tile = job.count.div_ceil(job.tiles);
    let first = tile * per_tile;
    if first >= job.count {
        return;
    }
    let last = (first + per_tile).min(job.count);

    let words_per_row = job.cols / 8;
    let groups_per_row = job.cols / job.group;
    let words_per_group = job.group / 8;

    for local in first..last {
        let row = job.row0 + local;
        let (qrow, grow, orow) = (row * words_per_row, row * groups_per_row, local * job.cols);
        for g in 0..groups_per_row {
            // SAFETY: indices are inside the shapes checked in `run`.
            let sc = bf16(unsafe { *job.scales.add(grow + g) });
            let bi = bf16(unsafe { *job.biases.add(grow + g) });

            // Sixteen values ARE the whole alphabet of a 4-bit weight, and a
            // group shares one scale and one bias — so the group's values can
            // be built once and then only read, instead of a multiply, an add
            // and a rounding to half per element. Sixty-four elements per group
            // means the arithmetic is done a quarter as often.
            let mut lut = [f16::ZERO; 16];
            for (n, slot) in lut.iter_mut().enumerate() {
                *slot = f16::from_f32(n as f32 * sc + bi);
            }

            for w in 0..words_per_group {
                // SAFETY: as above.
                let bits = unsafe { *job.packed.add(qrow + g * words_per_group + w) };
                let base = orow + g * job.group + w * 8;
                for j in 0..8 {
                    // SAFETY: `base + j` is inside the resized scratch.
                    unsafe { *job.out.add(base + j) = lut[((bits >> (j * 4)) & 0xF) as usize] };
                }
            }
        }
    }
}

// ===== Accelerate =====

const BNNS_F16: u32 = 0x1_0010;
const BNNS_F32: u32 = 0x1_0020;
const BNNS_ROW_MAJOR_MATRIX: u32 = 0x2_0000;
const QOS_USER_INITIATED: isize = 0x19;

/// A row-major matrix as BNNS names one: `size[0]` varies fastest.
///
/// `stride` of 0 means contiguous, which is what every operand but the output
/// window wants.
fn descriptor(fast: usize, slow: usize, data: *mut c_void, dtype: u32, row_stride: usize) -> BnnsNdArray {
    let mut size = [0usize; 8];
    let mut stride = [0usize; 8];
    size[0] = fast;
    size[1] = slow;
    if row_stride != 0 {
        stride[0] = 1;
        stride[1] = row_stride;
    }
    BnnsNdArray {
        flags: 0,
        layout: BNNS_ROW_MAJOR_MATRIX,
        size,
        stride,
        data,
        data_type: dtype,
        table_data: std::ptr::null_mut(),
        table_data_type: BNNS_F32,
        data_scale: 1.0,
        data_bias: 0.0,
    }
}

#[repr(C)]
struct BnnsNdArray {
    flags: u32,
    layout: u32,
    size: [usize; 8],
    stride: [usize; 8],
    data: *mut c_void,
    data_type: u32,
    table_data: *mut c_void,
    table_data_type: u32,
    data_scale: f32,
    data_bias: f32,
}

#[link(name = "Accelerate", kind = "framework")]
unsafe extern "C" {
    fn BNNSMatMul(
        trans_a: bool,
        trans_b: bool,
        alpha: f32,
        a: *const BnnsNdArray,
        b: *const BnnsNdArray,
        c: *const BnnsNdArray,
        workspace: *mut c_void,
        params: *const c_void,
    ) -> i32;
}

unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
    fn dispatch_apply_f(
        iterations: usize,
        queue: *mut c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void, usize),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bf16_widens_by_shifting() {
        // 0x3F80 to 1,0 w bf16. Gdyby to poszło przez złą połowę słowa, wagi
        // byłyby ciche i błędne — a wyjście wciąż wyglądałoby jak tekst.
        assert_eq!(bf16(0x3F80), 1.0);
        assert_eq!(bf16(0x4000), 2.0);
        assert_eq!(bf16(0xBF80), -1.0);
    }

    /// Naive reference: unpack and multiply the plainest way there is.
    fn reference(
        packed: &[u32],
        scales: &[u16],
        biases: &[u16],
        x: &[u16],
        cols: usize,
        group: usize,
        row: usize,
        t: usize,
    ) -> f32 {
        let mut acc = 0.0f32;
        for c in 0..cols {
            let bits = packed[row * (cols / 8) + c / 8];
            let nib = ((bits >> ((c % 8) * 4)) & 0xF) as f32;
            let g = row * (cols / group) + c / group;
            let w = f16::from_f32(nib * bf16(scales[g]) + bf16(biases[g]));
            acc += f32::from(f16::from_bits(x[t * cols + c])) * f32::from(w);
        }
        acc
    }

    #[test]
    fn the_cpu_half_computes_its_rows_and_touches_nothing_else() {
        // Zły krok wyjścia nadpisałby wiersze GPU i wyglądałby jak „model liczy
        // trochę inaczej", a nie jak błąd. Dlatego wartownik na cudzych
        // wierszach jest tu równie ważny jak sama arytmetyka.
        const ROWS: usize = 128;
        const COLS: usize = 128;
        const GROUP: usize = 64;
        const TOKENS: usize = 8;
        const ROW0: usize = 64;

        let packed: Vec<u32> = (0..ROWS * COLS / 8)
            .map(|i| (i as u32).wrapping_mul(2_654_435_761))
            .collect();
        let groups = ROWS * COLS / GROUP;
        let scales: Vec<u16> = (0..groups).map(|i| 0x3C00u16.wrapping_add(i as u16)).collect();
        let biases: Vec<u16> = (0..groups).map(|i| 0xBC00u16.wrapping_add(i as u16)).collect();
        let x: Vec<u16> = (0..TOKENS * COLS)
            .map(|i| f16::from_f32((i % 7) as f32 * 0.125 - 0.5).to_bits())
            .collect();

        const SENTINEL: f32 = -12345.0;
        let mut out = vec![SENTINEL; TOKENS * ROWS];
        let op = Operands {
            packed: &packed,
            scales: &scales,
            biases: &biases,
            x: &x,
            out: out.as_mut_ptr() as *mut u8,
            out_f16: false,
            tokens: TOKENS as u32,
            rows: ROWS as u32,
            cols: COLS as u32,
            group: GROUP as u32,
        };
        // SAFETY: `out` holds TOKENS * ROWS f32 and nothing else writes it here.
        unsafe {
            CpuMatmul::new()
                .run(&op, ROW0 as u32, (ROWS - ROW0) as u32)
                .expect("połowa CPU");
        }

        for t in 0..TOKENS {
            for r in 0..ROW0 {
                assert_eq!(
                    out[t * ROWS + r],
                    SENTINEL,
                    "wiersz {r} należy do GPU, a został nadpisany"
                );
            }
            for r in ROW0..ROWS {
                let want = reference(&packed, &scales, &biases, &x, COLS, GROUP, r, t);
                let got = out[t * ROWS + r];
                let tol = 1e-2 * want.abs().max(1.0);
                assert!(
                    (got - want).abs() <= tol,
                    "token {t}, wiersz {r}: {got} zamiast {want}"
                );
            }
        }
    }

    #[test]
    fn a_slice_outside_the_matrix_is_refused_before_it_writes() {
        let mut cpu = CpuMatmul::new();
        let op = Operands {
            packed: &[0u32; 512],
            scales: &[0u16; 64],
            biases: &[0u16; 64],
            x: &[0u16; 64],
            out: std::ptr::null_mut(),
            out_f16: false,
            tokens: 1,
            rows: 64,
            cols: 64,
            group: 64,
        };
        // SAFETY: odrzucenie następuje przed jakimkolwiek dostępem do `out`.
        let err = unsafe { cpu.run(&op, 32, 64) };
        assert!(err.is_err(), "wycinek poza macierzą musi być odrzucony");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Ile z czasu CPU zjada samo rozpakowanie, na realnym kształcie.
    ///
    /// To ono decyduje, ile wierszy CPU może wziąć i od jakiego wsadu podział w
    /// ogóle się opłaca, więc jest mierzone osobno od mnożenia.
    #[test]
    #[ignore]
    fn how_much_of_the_cpu_half_is_unpacking() {
        const ROWS: usize = 3264; // przydział CPU z gate_proj
        const COLS: usize = 4096;
        const GROUP: usize = 64;
        const TOKENS: usize = 512;

        let packed: Vec<u32> = (0..ROWS * COLS / 8)
            .map(|i| (i as u32).wrapping_mul(2_654_435_761))
            .collect();
        let groups = ROWS * COLS / GROUP;
        let scales = vec![0x3F00u16; groups];
        let biases = vec![0xBB80u16; groups];
        let x = vec![f16::from_f32(0.01).to_bits(); TOKENS * COLS];
        let mut out = vec![0f32; TOKENS * ROWS];

        let op = Operands {
            packed: &packed,
            scales: &scales,
            biases: &biases,
            x: &x,
            out: out.as_mut_ptr() as *mut u8,
            out_f16: false,
            tokens: TOKENS as u32,
            rows: ROWS as u32,
            cols: COLS as u32,
            group: GROUP as u32,
        };
        let mut cpu = CpuMatmul::new();
        // SAFETY: `out` mieści TOKENS * ROWS f32 i nikt inny go nie dotyka.
        unsafe { cpu.run(&op, 0, ROWS as u32).expect("rozgrzewka") };

        let reps = 10;
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            // SAFETY: jak wyżej.
            unsafe { cpu.run(&op, 0, ROWS as u32).expect("całość") };
        }
        let whole = t0.elapsed().as_secs_f64() / f64::from(reps);

        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            cpu.unpack(&op, 0, ROWS);
        }
        let unpack = t0.elapsed().as_secs_f64() / f64::from(reps);

        let flops = 2.0 * ROWS as f64 * COLS as f64 * TOKENS as f64;
        eprintln!(
            "[{ROWS} x {COLS}] x {TOKENS}: całość {:.0} us ({:.2} TFLOPS), \
             rozpakowanie {:.0} us ({:.0}%), samo mnożenie {:.0} us ({:.2} TFLOPS)",
            whole * 1e6,
            flops / whole / 1e12,
            unpack * 1e6,
            100.0 * unpack / whole,
            (whole - unpack) * 1e6,
            flops / (whole - unpack) / 1e12
        );
    }
}
