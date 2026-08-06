// ===== File: mma_fp4.rs — the block-scaled FP4 op, before anything uses it =====
//
// GB10 executes `mma.sync.aligned.m16n8k64...kind::mxf4.block_scale` natively.
// Nothing in the catalogue emitted it, because the capability check in the HAL
// said the part did not have it — it does, on the `a`-suffixed target.
//
// A wrong fragment layout does not fail: it produces numbers. So the layout is
// established here, against arithmetic done on the host, before any tile is
// built on the instruction. The comparison is EXACT and can be: every e2m1
// value is a multiple of 0.5 with magnitude at most 6, so 64 products summed in
// f32 land on representable numbers whatever order they are added in.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;

/// The instruction is PTX, so this runs on CUDA or it runs nowhere.
fn device() -> Option<Arc<dyn Device>> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 16 << 20,
            kv_cache: 4 << 20,
            activations: 16 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .map_err(|e| eprintln!("pomijam MMA mxf4: {e}"))
    .ok()
}

const M: usize = 16;
const N: usize = 8;
const K: usize = 64;

/// The eight magnitudes E2M1 can hold, indexed by the low three bits of a code.
const E2M1: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];

fn decode(code: u8) -> f32 {
    let v = E2M1[(code & 0x7) as usize];
    if code & 0x8 != 0 {
        -v
    } else {
        v
    }
}

/// Where lane `l`'s A nibble `(reg, slot)` sits in the 16x64 operand.
///
/// Byte layout is the m16n8k32 E4M3 one already working next door — sixteen
/// bytes per lane in four registers — with each byte now holding two values,
/// low nibble first. That is what halving the element width and doubling k
/// means, and it is the assumption this file exists to confirm.
fn a_index(lane: usize, reg: usize, slot: usize) -> (usize, usize) {
    let (group, in_group) = (lane / 4, lane % 4);
    (group + 8 * (reg % 2), 32 * (reg / 2) + 8 * in_group + slot)
}

/// Where lane `l`'s B nibble `(reg, slot)` sits in the 64x8 operand.
///
/// `.row.col` makes B column-major, so a lane's contiguous nibbles walk k for
/// one column rather than walking columns.
fn b_index(lane: usize, reg: usize, slot: usize) -> (usize, usize) {
    let (group, in_group) = (lane / 4, lane % 4);
    (32 * reg + 8 * in_group + slot, group)
}

/// Where lane `l`'s accumulator element `i` sits in the 16x8 result.
fn d_index(lane: usize, i: usize) -> (usize, usize) {
    let (group, in_group) = (lane / 4, lane % 4);
    (group + 8 * (i / 2), 2 * in_group + i % 2)
}

fn pack_a(a: &[[u8; K]; M]) -> Vec<u32> {
    let mut regs = vec![0u32; 32 * 4];
    for lane in 0..32 {
        for reg in 0..4 {
            let mut word = 0u32;
            for slot in 0..8 {
                let (row, k) = a_index(lane, reg, slot);
                word |= u32::from(a[row][k] & 0xF) << (4 * slot);
            }
            regs[lane * 4 + reg] = word;
        }
    }
    regs
}

fn pack_b(b: &[[u8; N]; K]) -> Vec<u32> {
    let mut regs = vec![0u32; 32 * 2];
    for lane in 0..32 {
        for reg in 0..2 {
            let mut word = 0u32;
            for slot in 0..8 {
                let (k, col) = b_index(lane, reg, slot);
                word |= u32::from(b[k][col] & 0xF) << (4 * slot);
            }
            regs[lane * 2 + reg] = word;
        }
    }
    regs
}

fn unpack_d(raw: &[f32]) -> [[f32; N]; M] {
    let mut d = [[0f32; N]; M];
    for lane in 0..32 {
        for i in 0..4 {
            let (row, col) = d_index(lane, i);
            d[row][col] = raw[lane * 4 + i];
        }
    }
    d
}

fn upload(dev: &dyn Device, words: &[u32], pool: Pool) -> DevBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(words);
    let buf = dev.alloc(bytes.len(), MemKind::Device, pool).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn run(
    dev: &Arc<dyn Device>,
    kernels: &Kernels,
    a: &[[u8; K]; M],
    b: &[[u8; N]; K],
    scale_a: &[u32; 32],
    scale_b: &[u32; 32],
) -> [[f32; N]; M] {
    let stream = dev.create_stream().unwrap();
    let a_dev = upload(dev.as_ref(), &pack_a(a), Pool::Weights);
    let b_dev = upload(dev.as_ref(), &pack_b(b), Pool::Weights);
    let sa_dev = upload(dev.as_ref(), scale_a, Pool::Weights);
    let sb_dev = upload(dev.as_ref(), scale_b, Pool::Weights);
    let d_dev = dev
        .alloc(M * N * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .mma_mxf4_probe(&d_dev, &a_dev, &b_dev, &sa_dev, &sb_dev, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let mut bytes = vec![0u8; M * N * 4];
    dev.read(&d_dev, 0, &mut bytes).unwrap();
    unpack_d(bytemuck::cast_slice(&bytes))
}

/// Every scale byte set to UE8M0 127, which is 2^0.
const UNIT_SCALES: [u32; 32] = [0x7F7F_7F7F; 32];

/// The product the instruction is supposed to compute, on the host in f32.
fn reference(
    a: &[[u8; K]; M],
    b: &[[u8; N]; K],
    scale: impl Fn(usize, usize) -> f32,
) -> [[f32; N]; M] {
    let mut d = [[0f32; N]; M];
    for (row, out) in d.iter_mut().enumerate() {
        for (col, cell) in out.iter_mut().enumerate() {
            *cell = (0..K)
                .map(|k| decode(a[row][k]) * decode(b[k][col]) * scale(row, k))
                .sum();
        }
    }
    d
}

/// Random operands, unit scales: does the assumed fragment layout hold?
///
/// This is the whole question. A layout wrong by one register or one nibble
/// still fills the accumulator, so nothing downstream would notice; a
/// full-rank comparison against the host notices immediately, and because the
/// arithmetic is exact the threshold is equality rather than a tolerance.
#[test]
fn the_block_scaled_op_computes_the_product() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.supports_mxf4_block_scale() {
        eprintln!("pomijam: brak artefaktu mma_mxf4_probe");
        return;
    }

    // Codes chosen so no row, column or k-slice repeats a pattern: an operand
    // filled with one value agrees with a transposed layout as readily as with
    // the right one.
    let mut a = [[0u8; K]; M];
    for (row, line) in a.iter_mut().enumerate() {
        for (k, cell) in line.iter_mut().enumerate() {
            *cell = ((row * 7 + k * 3 + row * k) % 16) as u8;
        }
    }
    let mut b = [[0u8; N]; K];
    for (k, line) in b.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            *cell = ((k * 5 + col * 11 + 1) % 16) as u8;
        }
    }

    let got = run(&dev, &kernels, &a, &b, &UNIT_SCALES, &UNIT_SCALES);
    let want = reference(&a, &b, |_, _| 1.0);
    for row in 0..M {
        assert_eq!(
            got[row], want[row],
            "wiersz {row} rozjechał się z wzorcem: {:?} vs {:?}",
            got[row], want[row]
        );
    }
    assert!(
        got.iter().flatten().any(|v| *v != 0.0),
        "instrukcja zwróciła same zera"
    );
}

/// The scale operands: which lane's which byte reaches which row and k-block.
///
/// Derived, not assumed. A one-at-a-time sweep over all 32 lanes and all four
/// bytes of both scale words, against operands of all ones so any departure
/// from 64 per cell is the scale and nothing else, gave exactly this: the
/// selector `{0, 0}` reads BYTES 0 AND 1 — the pair `scale_vec::2X` asks for,
/// one per 32 columns — and reads them from the threads of each quad whose
/// index is 0 (B) or 0 and 1 (A, which needs sixteen rows rather than eight).
/// Bytes 2 and 3 of the word are never read under this selector.
fn a_scale_lane(row: usize) -> usize {
    4 * (row % 8) + row / 8
}

fn b_scale_lane(col: usize) -> usize {
    4 * col
}

/// Packs UE8M0 exponents into the words each lane hands the instruction.
fn pack_scales(
    per_block: impl Fn(usize, usize) -> u8,
    count: usize,
    lane_of: fn(usize) -> usize,
) -> [u32; 32] {
    let mut words = UNIT_SCALES;
    for i in 0..count {
        let lane = lane_of(i);
        let mut word = 0x7F7F_7F7Fu32;
        for half in 0..2 {
            word &= !(0xFFu32 << (8 * half));
            word |= u32::from(per_block(i, half)) << (8 * half);
        }
        words[lane] = word;
    }
    words
}

/// The whole instruction, scales included, against the host.
///
/// The first gate held both scale operands at 2^0, which leaves the largest
/// part of a block-scaled mma untested — a scale routed to the wrong row is
/// exactly the failure that produces plausible logits. Here every row of A and
/// every column of B carries its own pair of exponents, and the comparison is
/// still exact: powers of two times multiples of 0.5 stay representable.
#[test]
fn the_block_scaled_op_applies_per_32_scales() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.supports_mxf4_block_scale() {
        eprintln!("pomijam: brak artefaktu mma_mxf4_probe");
        return;
    }

    let mut a = [[0u8; K]; M];
    for (row, line) in a.iter_mut().enumerate() {
        for (k, cell) in line.iter_mut().enumerate() {
            *cell = ((row * 3 + k * 5 + 2) % 16) as u8;
        }
    }
    let mut b = [[0u8; N]; K];
    for (k, line) in b.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            *cell = ((k * 9 + col * 7) % 16) as u8;
        }
    }

    // Exponents spread over six orders of magnitude, and different for the two
    // halves of k, so a k-block swapped with its neighbour cannot pass.
    let exp_a = |row: usize, half: usize| (127 + (row as i32 % 5) - 2 + half as i32) as u8;
    let exp_b = |col: usize, half: usize| (127 - (col as i32 % 4) + 2 * half as i32) as u8;
    let scale_a = pack_scales(exp_a, M, a_scale_lane);
    let scale_b = pack_scales(exp_b, N, b_scale_lane);

    let got = run(&dev, &kernels, &a, &b, &scale_a, &scale_b);
    let mut want = [[0f32; N]; M];
    for (row, out) in want.iter_mut().enumerate() {
        for (col, cell) in out.iter_mut().enumerate() {
            *cell = (0..K)
                .map(|k| {
                    let half = k / 32;
                    decode(a[row][k])
                        * decode(b[k][col])
                        * ue8m0(exp_a(row, half))
                        * ue8m0(exp_b(col, half))
                })
                .sum();
        }
    }
    for row in 0..M {
        assert_eq!(
            got[row], want[row],
            "wiersz {row} rozjechał się ze skalowanym wzorcem: {:?} vs {:?}",
            got[row], want[row]
        );
    }
}

fn ue8m0(exponent: u8) -> f32 {
    f32::from_bits(u32::from(exponent) << 23)
}
