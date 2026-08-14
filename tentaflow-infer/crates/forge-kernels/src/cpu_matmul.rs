// ===== File: cpu_matmul.rs — the CPU's share of a prefill matrix product =====
//
// Lives with the kernels because it IS one: the same product, computed by the
// other unit on the same chip. Which unit runs a piece of work is exactly the
// question this crate exists to answer, and exactly the question the layer
// above must not be asked.
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

use forge_formats::dequant::dequantize_to_f32;
use forge_types::{DType, ForgeError, QuantKind, Result};
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
    pub high: Option<&'a [u32]>,
    pub scales: &'a [u16],
    pub biases: &'a [u16],
    pub param_dtype: DType,
    pub x: &'a [u16],
    pub out: *mut u8,
    pub out_f16: bool,
    pub tokens: u32,
    pub rows: u32,
    pub cols: u32,
    pub group: u32,
    /// Code width of this weight. Carried so `check` can REFUSE anything but
    /// four, rather than trust that whoever chose the split knew.
    ///
    /// There is already a gate in the variant registry. It is not enough: when
    /// six-bit weights did reach here, the half of every code that lives in a
    /// second array was simply absent, and the result was fluent nonsense out
    /// of a checkpoint that decodes correctly at short prompts. A gate
    /// elsewhere and a check at the point of use are not the same thing.
    pub bits: u32,
}

/// A source that keeps a GGUF-style block layout. The CPU decodes only the
/// rows assigned to it, so the full model never needs a second expanded copy.
pub struct BlockOperands<'a> {
    pub blocks: &'a [u8],
    pub quant: QuantKind,
    pub global: Option<f32>,
    pub x: &'a [u16],
    pub out: *mut u8,
    pub out_f16: bool,
    pub tokens: u32,
    pub rows: u32,
    pub cols: u32,
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
    ///
    /// Depends on NOTHING the GPU produces — the weights are uploaded once at
    /// load and never written again — so the caller is free to run this before
    /// waiting for the activations, in the window where the CPU would otherwise
    /// be watching the GPU work.
    pub fn unpack(&mut self, op: &Operands<'_>, row0: usize, count: usize) {
        let cols = op.cols as usize;
        self.weights.resize(count * cols, f16::ZERO);
        let job = DequantJob {
            out: self.weights.as_mut_ptr(),
            packed: op.packed.as_ptr(),
            high: op.high.map_or(std::ptr::null(), |high| high.as_ptr()),
            scales: op.scales.as_ptr(),
            biases: op.biases.as_ptr(),
            bits: op.bits,
            param_dtype: op.param_dtype,
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

    /// Decodes rows from a source block layout into the same reusable scratch
    /// used by the affine path. Q4_K and Q6_K stay on specialized loops; the
    /// format decoder covers the remaining self-contained block formats.
    pub fn unpack_blocks(
        &mut self,
        op: &BlockOperands<'_>,
        row0: usize,
        count: usize,
    ) -> Result<()> {
        let cols = op.cols as usize;
        let block_elems = op.quant.block_elems();
        let block_bytes = op.quant.block_bytes();
        if block_elems == 0 || block_bytes == 0 || !cols.is_multiple_of(block_elems) {
            return Err(ForgeError::Unsupported(format!(
                "CPU: {:?} nie ma samodzielnego układu blokowego dla {} kolumn",
                op.quant, cols
            )));
        }
        let bytes_per_row = cols / block_elems * block_bytes;
        let start = row0
            .checked_mul(bytes_per_row)
            .ok_or_else(|| ForgeError::Format("CPU: przepełnienie offsetu bloków".into()))?;
        let bytes = count
            .checked_mul(bytes_per_row)
            .ok_or_else(|| ForgeError::Format("CPU: przepełnienie rozmiaru bloków".into()))?;
        let end = start
            .checked_add(bytes)
            .ok_or_else(|| ForgeError::Format("CPU: przepełnienie końca bloków".into()))?;
        let source = op.blocks.get(start..end).ok_or_else(|| {
            ForgeError::Format(format!(
                "CPU: {} B bloków nie mieści wierszy [{row0}, {})",
                op.blocks.len(),
                row0 + count
            ))
        })?;
        self.weights.resize(count * cols, f16::ZERO);
        match op.quant {
            QuantKind::Q4K => unpack_q4_k(source, &mut self.weights, cols),
            QuantKind::Q6K => unpack_q6_k(source, &mut self.weights, cols),
            _ => {
                let decoded = dequantize_to_f32(
                    DType::U8,
                    op.quant,
                    source,
                    count * cols,
                )?;
                for (dst, value) in self.weights.iter_mut().zip(decoded) {
                    *dst = f16::from_f32(value);
                }
            }
        }
        if let Some(global) = op.global {
            for value in &mut self.weights {
                *value = f16::from_f32(value.to_f32() * global);
            }
        }
        Ok(())
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
        self.check(op, row0, count)?;
        self.unpack(op, row0 as usize, count as usize);
        // SAFETY: forwarded from this function's own contract.
        unsafe { self.multiply(op, row0, count) }
    }

    /// The block-layout counterpart of `run`.
    pub unsafe fn run_blocks(
        &mut self,
        op: &BlockOperands<'_>,
        row0: u32,
        count: u32,
    ) -> Result<()> {
        self.check_blocks(op, row0, count)?;
        self.unpack_blocks(op, row0 as usize, count as usize)?;
        // SAFETY: forwarded from this function's contract.
        unsafe { self.multiply_blocks(op, row0, count) }
    }

    /// Shapes the caller has to get right before either half runs.
    pub fn check(&self, op: &Operands<'_>, row0: u32, count: u32) -> Result<()> {
        let (rows, cols, group) = (op.rows as usize, op.cols as usize, op.group as usize);
        let (row0, count) = (row0 as usize, count as usize);
        if !matches!(op.bits, 4 | 6) {
            return Err(ForgeError::Unsupported(format!(
                "CPU: {} bitów na wagę, a rozpakowanie zna cztery i sześć",
                op.bits
            )));
        }
        if op.bits == 6 && op.high.is_none() {
            return Err(ForgeError::Format(
                "CPU: sześciobitowa waga nie ma płaszczyzny high".into(),
            ));
        }
        if op.bits == 4 && op.high.is_some() {
            return Err(ForgeError::Format(
                "CPU: czterobitowa waga ma nieużywaną płaszczyznę high".into(),
            ));
        }
        if cols % group != 0 || cols % 8 != 0 || (op.bits == 6 && !cols.is_multiple_of(16)) {
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
        Ok(())
    }

    pub fn check_blocks(&self, op: &BlockOperands<'_>, row0: u32, count: u32) -> Result<()> {
        let rows = op.rows as usize;
        let (row0, count) = (row0 as usize, count as usize);
        let block_elems = op.quant.block_elems();
        let block_bytes = op.quant.block_bytes();
        if block_elems == 0 || block_bytes == 0 || !op.cols.is_multiple_of(block_elems as u32) {
            return Err(ForgeError::Unsupported(format!(
                "CPU: {:?} nie ma samodzielnego układu blokowego dla {} kolumn",
                op.quant, op.cols
            )));
        }
        if row0.checked_add(count).is_none_or(|end| end > rows) {
            return Err(ForgeError::Other(format!(
                "CPU: blokowy wycinek [{row0}, {}) wykracza poza {rows} wierszy",
                row0 + count
            )));
        }
        let expected = rows
            .checked_mul(op.cols as usize)
            .and_then(|n| n.checked_div(block_elems))
            .and_then(|n| n.checked_mul(block_bytes))
            .ok_or_else(|| ForgeError::Format("CPU: przepełnienie rozmiaru wagi blokowej".into()))?;
        if op.blocks.len() != expected {
            return Err(ForgeError::Format(format!(
                "CPU: {:?} ma {} B, oczekiwano {expected}",
                op.quant,
                op.blocks.len()
            )));
        }
        if op.x.len() < op.tokens as usize * op.cols as usize {
            return Err(ForgeError::Other("CPU: za krótki bufor aktywacji".into()));
        }
        Ok(())
    }

    /// Multiplies the already-unpacked rows into `out`. This one DOES need the
    /// activations, so it is what the wait for the GPU has to precede.
    ///
    /// # Safety
    ///
    /// As `run`, plus: `unpack` must have been called for the same slice.
    pub unsafe fn multiply(&mut self, op: &Operands<'_>, row0: u32, count: u32) -> Result<()> {
        // SAFETY: forwarded from `run`.
        unsafe {
            self.multiply_matrix(
                op.x,
                op.out,
                op.out_f16,
                op.tokens,
                op.rows,
                op.cols,
                row0,
                count,
            )
        }
    }

    /// Multiplies a block-layout source after its rows have been unpacked.
    pub unsafe fn multiply_blocks(
        &mut self,
        op: &BlockOperands<'_>,
        row0: u32,
        count: u32,
    ) -> Result<()> {
        // SAFETY: forwarded from `run_blocks`.
        unsafe {
            self.multiply_matrix(
                op.x,
                op.out,
                op.out_f16,
                op.tokens,
                op.rows,
                op.cols,
                row0,
                count,
            )
        }
    }

    unsafe fn multiply_matrix(
        &mut self,
        x: &[u16],
        out: *mut u8,
        out_f16: bool,
        tokens: u32,
        rows: u32,
        cols: u32,
        row0: u32,
        count: u32,
    ) -> Result<()> {
        let (rows, cols) = (rows as usize, cols as usize);
        let (tokens, row0, count) = (tokens as usize, row0 as usize, count as usize);
        let out_ty = if out_f16 { BNNS_F16 } else { BNNS_F32 };
        let elem = if out_f16 { 2 } else { 4 };
        let a = descriptor(cols, tokens, x.as_ptr() as *mut c_void, BNNS_F16, 0);
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
            unsafe { out.add(row0 * elem) } as *mut c_void,
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
    high: *const u32,
    scales: *const u16,
    biases: *const u16,
    bits: u32,
    param_dtype: DType,
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

#[inline(always)]
fn scalar(bits: u16, dtype: DType) -> f32 {
    match dtype {
        DType::F16 => f16::from_bits(bits).to_f32(),
        DType::BF16 => bf16(bits),
        _ => unreachable!("kwantyzacyjne parametry CPU są tylko F16 albo BF16"),
    }
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
            // SAFETY: indices are inside the shapes checked in `check`.
            let sc = scalar(unsafe { *job.scales.add(grow + g) }, job.param_dtype);
            let bi = scalar(unsafe { *job.biases.add(grow + g) }, job.param_dtype);

            // Sixteen values ARE the whole alphabet of a 4-bit weight, and a
            // group shares one scale and one bias — so the group's values are
            // built once and then only read, instead of a multiply, an add and
            // a rounding to half per element.
            let mut lut = [0u16; 64];
            for (n, slot) in lut.iter_mut().enumerate() {
                if n < (1usize << job.bits) {
                    *slot = f16::from_f32(n as f32 * sc + bi).to_bits();
                }
            }

            // SAFETY: `qrow + g * words_per_group` indexes inside `packed` and
            // the destination run of `group` values is inside the scratch.
            let src = unsafe { job.packed.add(qrow + g * words_per_group) } as *const u8;
            let dst = unsafe { job.out.add(orow + g * job.group) } as *mut u16;
            // SAFETY: both runs are `group` elements long, as established above.
            if job.bits == 4 {
                let lut4: &[u16; 16] = lut[..16].try_into().unwrap();
                // SAFETY: the four-bit source uses only the low packed plane.
                unsafe { fill_group4(dst, src, lut4, job.group) };
            } else {
                let high = unsafe {
                    job.high.add(row * job.cols / 16 + g * job.group / 16)
                };
                // SAFETY: `check` established the six-bit layout and bounds.
                unsafe { fill_group6(dst, src, high, &lut, job.group) };
            }
        }
    }
}

fn unpack_q4_k(source: &[u8], out: &mut [f16], cols: usize) {
    let superblocks = cols / 256;
    for (row, dst) in out.chunks_exact_mut(cols).enumerate() {
        for sb in 0..superblocks {
            let b = &source[(row * superblocks + sb) * 144..][..144];
            let d = f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
            let dmin = f16::from_bits(u16::from_le_bytes([b[2], b[3]])).to_f32();
            for sub in 0..8 {
                let (scale, min) = scale_min_k4(sub, &b[4..16]);
                let scale = d * scale as f32;
                let min = dmin * min as f32;
                for j in 0..32 {
                    let qbyte = b[16 + (sub / 2) * 32 + j];
                    let q = if sub % 2 == 0 { qbyte & 0x0F } else { qbyte >> 4 };
                    dst[sb * 256 + sub * 32 + j] =
                        f16::from_f32((q as f32).mul_add(scale, -min));
                }
            }
        }
    }
}

fn unpack_q6_k(source: &[u8], out: &mut [f16], cols: usize) {
    let superblocks = cols / 256;
    for (row, dst) in out.chunks_exact_mut(cols).enumerate() {
        for sb in 0..superblocks {
            let b = &source[(row * superblocks + sb) * 210..][..210];
            let ql = &b[..128];
            let qh = &b[128..192];
            let scales = &b[192..208];
            let d = f16::from_bits(u16::from_le_bytes([b[208], b[209]])).to_f32();
            for n in 0..2 {
                let ql = &ql[n * 64..n * 64 + 64];
                let qh = &qh[n * 32..n * 32 + 32];
                let scales = &scales[n * 8..n * 8 + 8];
                for l in 0..32 {
                    let is = l / 16;
                    let qs = [
                        (ql[l] & 0x0F) | ((qh[l] & 3) << 4),
                        (ql[l + 32] & 0x0F) | (((qh[l] >> 2) & 3) << 4),
                        (ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4),
                        (ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4),
                    ];
                    for (q, value) in qs.into_iter().enumerate() {
                        let col = sb * 256 + n * 128 + q * 32 + l;
                        let scale = d * (scales[is + q * 2] as i8) as f32;
                        dst[col] = f16::from_f32(scale * (value as i32 - 32) as f32);
                    }
                }
            }
        }
    }
}

fn scale_min_k4(j: usize, q: &[u8]) -> (u8, u8) {
    if j < 4 {
        (q[j] & 63, q[j + 4] & 63)
    } else {
        (
            (q[j + 4] & 0x0F) | ((q[j - 4] >> 6) << 4),
            (q[j + 4] >> 4) | ((q[j] >> 6) << 4),
        )
    }
}

/// Writes one group's worth of unpacked weights.
///
/// # Safety
///
/// `dst` must have room for `group` values and `src` for `group / 2` bytes.
#[inline(always)]
unsafe fn fill_group4(dst: *mut u16, src: *const u8, lut: &[u16; 16], group: usize) {
    #[cfg(target_arch = "aarch64")]
    if group % 32 == 0 {
        // SAFETY: forwarded, plus the multiple-of-32 shape this branch checks.
        unsafe { fill_group_neon(dst, src, lut, group) };
        return;
    }
    for j in 0..group {
        // SAFETY: forwarded.
        let byte = unsafe { *src.add(j / 2) };
        let nibble = if j % 2 == 0 { byte & 0xF } else { byte >> 4 };
        // SAFETY: forwarded.
        unsafe { *dst.add(j) = lut[nibble as usize] };
    }
}

/// Writes one six-bit affine group whose high pairs live in a second plane.
#[inline(always)]
unsafe fn fill_group6(
    dst: *mut u16,
    src: *const u8,
    high: *const u32,
    lut: &[u16; 64],
    group: usize,
) {
    for j in 0..group {
        let byte = unsafe { *src.add(j / 2) };
        let low = if j % 2 == 0 { byte & 0xF } else { byte >> 4 };
        let word = unsafe { *high.add(j / 16) };
        let q = low | ((((word >> ((j % 16) * 2)) & 0x3) as u8) << 4);
        unsafe { *dst.add(j) = lut[q as usize] };
    }
}

/// The same thing, sixteen nibbles at a time.
///
/// A table lookup is what `vqtbl1q_u8` does natively, and the alphabet is
/// exactly sixteen entries — the width the instruction indexes. Two tables,
/// one per byte of the half, then the halves are woven back together. Measured
/// 2,66x against the scalar loop and bit-identical over 13 million elements.
///
/// # Safety
///
/// As `fill_group`, and `group` must be a multiple of 32.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn fill_group_neon(dst: *mut u16, src: *const u8, lut: &[u16; 16], group: usize) {
    use std::arch::aarch64::*;

    let mut lo = [0u8; 16];
    let mut hi = [0u8; 16];
    for (n, &v) in lut.iter().enumerate() {
        lo[n] = v as u8;
        hi[n] = (v >> 8) as u8;
    }
    // SAFETY: both arrays are exactly the sixteen bytes the load reads.
    let (tlo, thi) = unsafe { (vld1q_u8(lo.as_ptr()), vld1q_u8(hi.as_ptr())) };

    for step in 0..group / 32 {
        // SAFETY: `group / 2` bytes exist, so 16 bytes per step of 32 values do.
        let bytes = unsafe { vld1q_u8(src.add(step * 16)) };
        // Nibble 2k of the group lives in the low half of byte k, 2k+1 in the
        // high half — so the two halves are gathered separately and interleaved.
        let (even, odd) = unsafe { (vandq_u8(bytes, vdupq_n_u8(0x0F)), vshrq_n_u8(bytes, 4)) };
        let (e_lo, e_hi) = unsafe { (vqtbl1q_u8(tlo, even), vqtbl1q_u8(thi, even)) };
        let (o_lo, o_hi) = unsafe { (vqtbl1q_u8(tlo, odd), vqtbl1q_u8(thi, odd)) };

        let mut e = [0u16; 16];
        let mut o = [0u16; 16];
        // SAFETY: each store writes exactly the 32 bytes of its array.
        unsafe {
            vst2q_u8(e.as_mut_ptr() as *mut u8, uint8x16x2_t(e_lo, e_hi));
            vst2q_u8(o.as_mut_ptr() as *mut u8, uint8x16x2_t(o_lo, o_hi));
        }
        // SAFETY: 32 values per step are inside the group.
        unsafe {
            let d = dst.add(step * 32);
            vst2q_u16(d, uint16x8x2_t(vld1q_u16(e.as_ptr()), vld1q_u16(o.as_ptr())));
            vst2q_u16(
                d.add(16),
                uint16x8x2_t(vld1q_u16(e.as_ptr().add(8)), vld1q_u16(o.as_ptr().add(8))),
            );
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

// Atrybut linkowania, nie deklaracje: `cargo check` ma przejść wszędzie, żeby
// zmiana wspólnego kontraktu odbiła się od kompilatora także na maszynie bez
// Apple. Budowana ta ścieżka jest wyłącznie na Apple, gdzie framework jest.
#[cfg_attr(target_vendor = "apple", link(name = "Accelerate", kind = "framework"))]
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
            high: None,
            scales: &scales,
            biases: &biases,
            param_dtype: DType::BF16,
            x: &x,
            out: out.as_mut_ptr() as *mut u8,
            out_f16: false,
            tokens: TOKENS as u32,
            rows: ROWS as u32,
            cols: COLS as u32,
            group: GROUP as u32,
        bits: 4,
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
    fn block_unpack_matches_the_format_reference_for_q4_and_q6() {
        for quant in [QuantKind::Q4K, QuantKind::Q6K] {
            let mut data: Vec<u8> = (0..quant.block_bytes())
                .map(|i| (i as u8).wrapping_mul(29).wrapping_add(7))
                .collect();
            if quant == QuantKind::Q4K {
                data[0..2].copy_from_slice(&0x3C00u16.to_le_bytes());
                data[2..4].copy_from_slice(&0x3800u16.to_le_bytes());
            } else {
                data[208..210].copy_from_slice(&0x3C00u16.to_le_bytes());
            }
            let op = BlockOperands {
                blocks: &data,
                quant,
                global: None,
                x: &[],
                out: std::ptr::null_mut(),
                out_f16: false,
                tokens: 1,
                rows: 1,
                cols: 256,
            };
            let mut cpu = CpuMatmul::new();
            cpu.unpack_blocks(&op, 0, 1).expect("dekoder bloków");
            let want = dequantize_to_f32(DType::U8, quant, &data, 256).expect("wzorzec");
            for (got, expected) in cpu.weights.iter().zip(want) {
                assert_eq!(
                    got.to_bits(),
                    f16::from_f32(expected).to_bits(),
                    "niezgodność dekodera {quant:?}"
                );
            }
        }
    }

    #[test]
    fn q4_k_block_unpack_matches_the_format_reference_for_multiple_rows() {
        const ROWS: usize = 64;
        const COLS: usize = 256;
        let mut data = vec![0u8; ROWS * 144];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(17).wrapping_add(3);
        }
        for (i, block) in data.chunks_exact_mut(144).enumerate() {
            block[0..2].copy_from_slice(
                &f16::from_f32(0.01 + (i % 31) as f32 * 0.037).to_le_bytes(),
            );
            block[2..4]
                .copy_from_slice(&f16::from_f32(0.02 + (i % 17) as f32 * 0.023).to_le_bytes());
        }
        let op = BlockOperands {
            blocks: &data,
            quant: QuantKind::Q4K,
            global: None,
            x: &[],
            out: std::ptr::null_mut(),
            out_f16: false,
            tokens: 1,
            rows: ROWS as u32,
            cols: COLS as u32,
        };
        let mut cpu = CpuMatmul::new();
        cpu.unpack_blocks(&op, 0, ROWS)
            .expect("CPU raw Q4_K multiple rows");
        let want = dequantize_to_f32(DType::U8, QuantKind::Q4K, &data, ROWS * COLS)
            .expect("wzorzec");
        for (got, expected) in cpu.weights.iter().zip(want) {
            assert_eq!(
                got.to_bits(),
                f16::from_f32(expected).to_bits(),
                "niezgodność dekodera Q4_K dla wielu wierszy"
            );
        }
    }

    #[test]
    fn affine_six_bit_unpack_matches_the_format_reference() {
        let mut data: Vec<u8> = (0..QuantKind::Q6K.block_bytes())
            .map(|i| (i as u8).wrapping_mul(23).wrapping_add(5))
            .collect();
        data[208..210].copy_from_slice(&0x3C00u16.to_le_bytes());
        let affine = forge_formats::affine::to_affine_triple(&data, QuantKind::Q6K, 1, 256)
            .expect("Q6_K affine");
        let op = Operands {
            packed: &affine.packed,
            high: Some(&affine.high),
            scales: bytemuck::cast_slice(&affine.scales),
            biases: bytemuck::cast_slice(&affine.biases),
            param_dtype: affine.param_dtype,
            x: &[],
            out: std::ptr::null_mut(),
            out_f16: false,
            tokens: 1,
            rows: 1,
            cols: 256,
            group: affine.group as u32,
            bits: affine.bits,
        };
        let mut cpu = CpuMatmul::new();
        cpu.check(&op, 0, 1).expect("kontrakt Q6_K");
        cpu.unpack(&op, 0, 1);
        let want = dequantize_to_f32(DType::U8, QuantKind::Q6K, &data, 256).expect("wzorzec");
        for (got, expected) in cpu.weights.iter().zip(want) {
            assert!(
                (got.to_f32() - expected).abs() < 0.001 * expected.abs().max(1.0),
                "niezgodność afinicznego dekodera Q6_K: {} != {expected}",
                got.to_f32()
            );
        }
    }

    /// Dwie implementacje tego samego muszą dawać ten sam bit — inaczej wynik
    /// zależałby od tego, na czym się akurat kompiluje.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn the_wide_unpack_matches_the_narrow_one_bit_for_bit() {
        let lut: [u16; 16] = std::array::from_fn(|n| {
            f16::from_f32(n as f32 * 0.013 - 0.11).to_bits()
        });
        for &group in &[32usize, 64, 128] {
            let src: Vec<u8> = (0..group / 2)
                .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
                .collect();
            let mut wide = vec![0u16; group];
            let mut narrow = vec![0u16; group];
            // SAFETY: obie tablice mają dokładnie `group` miejsc, `src` połowę tego.
            unsafe {
                fill_group_neon(wide.as_mut_ptr(), src.as_ptr(), &lut, group);
                for j in 0..group {
                    let byte = *src.as_ptr().add(j / 2);
                    let nib = if j % 2 == 0 { byte & 0xF } else { byte >> 4 };
                    narrow[j] = lut[nib as usize];
                }
            }
            assert_eq!(wide, narrow, "grupa {group}: szeroka ścieżka rozjeżdża się z wąską");
        }
    }

    #[test]
    fn a_slice_outside_the_matrix_is_refused_before_it_writes() {
        let mut cpu = CpuMatmul::new();
        let op = Operands {
            packed: &[0u32; 512],
            high: None,
            scales: &[0u16; 64],
            biases: &[0u16; 64],
            param_dtype: DType::BF16,
            x: &[0u16; 64],
            out: std::ptr::null_mut(),
            out_f16: false,
            tokens: 1,
            rows: 64,
            cols: 64,
            group: 64,
        bits: 4,
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
            high: None,
            scales: &scales,
            biases: &biases,
            param_dtype: DType::BF16,
            x: &x,
            out: out.as_mut_ptr() as *mut u8,
            out_f16: false,
            tokens: TOKENS as u32,
            rows: ROWS as u32,
            cols: COLS as u32,
            group: GROUP as u32,
        bits: 4,
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
