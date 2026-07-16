// ===== File: dequant.rs — CPU reference dequantization (golden source for GPU kernels) =====
//
// Semantics match llama.cpp `dequantize_row_*` exactly (verified against the
// pinned checkout used by TentaFlow native-libs). These functions are the
// correctness oracle for every fused GPU dequant kernel, so clarity beats
// speed here.

use forge_types::{DType, ForgeError, QuantKind, Result};
use half::f16;

fn fmt_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Format(msg.into())
}

const QK_K: usize = 256;

/// IQ4_NL / IQ4_XS non-linear 4-bit codebook (ggml `kvalues_iq4nl`).
const KVALUES_IQ4NL: [i8; 16] = [
    -127, -104, -83, -65, -49, -35, -22, -10, 1, 13, 25, 38, 53, 69, 89, 113,
];

/// MXFP4 codebook: 2 × E2M1 values as integers (ggml `kvalues_mxfp4`); the
/// block scale is therefore halved (`e8m0_to_fp32_half`).
const KVALUES_MXFP4: [i8; 16] = [0, 1, 2, 3, 4, 6, 8, 12, 0, -1, -2, -3, -4, -6, -8, -12];

fn f16_le(bytes: &[u8]) -> f32 {
    f16::from_bits(u16::from_le_bytes([bytes[0], bytes[1]])).to_f32()
}

fn bf16_le(bytes: &[u8]) -> f32 {
    f32::from_bits((u16::from_le_bytes([bytes[0], bytes[1]]) as u32) << 16)
}

/// ggml `ggml_e8m0_to_fp32_half`: 2^(e-127) / 2 with denormal handling.
fn e8m0_to_f32_half(e: u8) -> f32 {
    let bits: u32 = if e < 2 {
        0x0020_0000 << e
    } else {
        (e as u32 - 1) << 23
    };
    f32::from_bits(bits)
}

/// Dequantize a full tensor to f32. `data` must be exactly the packed bytes
/// for `numel` elements of the given format.
pub fn dequantize_to_f32(
    dtype: DType,
    quant: QuantKind,
    data: &[u8],
    numel: usize,
) -> Result<Vec<f32>> {
    if quant == QuantKind::None {
        return dequantize_plain(dtype, data, numel);
    }
    let be = quant.block_elems();
    let bb = quant.block_bytes();
    if numel % be != 0 {
        return Err(fmt_err(format!(
            "dequant: {numel} elements not divisible by {quant:?} block size {be}"
        )));
    }
    let nb = numel / be;
    let expected = nb
        .checked_mul(bb)
        .ok_or_else(|| fmt_err("dequant: size overflow"))?;
    if data.len() != expected {
        return Err(fmt_err(format!(
            "dequant: {quant:?} expects {expected} bytes for {numel} elements, got {}",
            data.len()
        )));
    }

    let mut out = vec![0.0f32; numel];
    for (i, block) in data.chunks_exact(bb).enumerate() {
        let y = &mut out[i * be..(i + 1) * be];
        match quant {
            QuantKind::Q4_0 => dq_q4_0(block, y),
            QuantKind::Q4_1 => dq_q4_1(block, y),
            QuantKind::Q5_0 => dq_q5_0(block, y),
            QuantKind::Q5_1 => dq_q5_1(block, y),
            QuantKind::Q8_0 => dq_q8_0(block, y),
            QuantKind::Q2K => dq_q2_k(block, y),
            QuantKind::Q3K => dq_q3_k(block, y),
            QuantKind::Q4K => dq_q4_k(block, y),
            QuantKind::Q5K => dq_q5_k(block, y),
            QuantKind::Q6K => dq_q6_k(block, y),
            QuantKind::Q8K => dq_q8_k(block, y),
            QuantKind::IQ4NL => dq_iq4_nl(block, y),
            QuantKind::IQ4XS => dq_iq4_xs(block, y),
            QuantKind::MXFP4 => dq_mxfp4(block, y),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "dequant: no CPU reference for {other:?}"
                )))
            }
        }
    }
    Ok(out)
}

fn dequantize_plain(dtype: DType, data: &[u8], numel: usize) -> Result<Vec<f32>> {
    let expected = numel
        .checked_mul(dtype.size())
        .ok_or_else(|| fmt_err("dequant: size overflow"))?;
    if data.len() != expected {
        return Err(fmt_err(format!(
            "dequant: {dtype} expects {expected} bytes for {numel} elements, got {}",
            data.len()
        )));
    }
    match dtype {
        DType::F32 => Ok(data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()),
        DType::F16 => Ok(data.chunks_exact(2).map(f16_le).collect()),
        DType::BF16 => Ok(data.chunks_exact(2).map(bf16_le).collect()),
        DType::I8 => Ok(data.iter().map(|&b| b as i8 as f32).collect()),
        other => Err(ForgeError::Unsupported(format!(
            "dequant: no f32 conversion for plain {other}"
        ))),
    }
}

// --- legacy 32-element blocks ---

fn dq_q4_0(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    let qs = &b[2..18];
    for j in 0..16 {
        y[j] = ((qs[j] & 0x0F) as i32 - 8) as f32 * d;
        y[j + 16] = ((qs[j] >> 4) as i32 - 8) as f32 * d;
    }
}

fn dq_q4_1(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    let m = f16_le(&b[2..4]);
    let qs = &b[4..20];
    for j in 0..16 {
        y[j] = (qs[j] & 0x0F) as f32 * d + m;
        y[j + 16] = (qs[j] >> 4) as f32 * d + m;
    }
}

fn dq_q5_0(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    let qh = u32::from_le_bytes([b[2], b[3], b[4], b[5]]);
    let qs = &b[6..22];
    for j in 0..16 {
        let xh0 = ((qh >> j) << 4) & 0x10;
        let xh1 = (qh >> (j + 12)) & 0x10;
        y[j] = (((qs[j] & 0x0F) as u32 | xh0) as i32 - 16) as f32 * d;
        y[j + 16] = (((qs[j] >> 4) as u32 | xh1) as i32 - 16) as f32 * d;
    }
}

fn dq_q5_1(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    let m = f16_le(&b[2..4]);
    let qh = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let qs = &b[8..24];
    for j in 0..16 {
        let xh0 = ((qh >> j) << 4) & 0x10;
        let xh1 = (qh >> (j + 12)) & 0x10;
        y[j] = ((qs[j] & 0x0F) as u32 | xh0) as f32 * d + m;
        y[j + 16] = ((qs[j] >> 4) as u32 | xh1) as f32 * d + m;
    }
}

fn dq_q8_0(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    for j in 0..32 {
        y[j] = (b[2 + j] as i8) as f32 * d;
    }
}

// --- K-quants (256-element superblocks) ---

fn dq_q2_k(b: &[u8], y: &mut [f32]) {
    // Layout: scales[16], qs[64], d f16, dmin f16.
    let scales = &b[0..16];
    let qs = &b[16..80];
    let d = f16_le(&b[80..82]);
    let min = f16_le(&b[82..84]);

    let mut is = 0usize;
    let mut yi = 0usize;
    for n in (0..QK_K).step_by(128) {
        let q = &qs[n / 4..n / 4 + 32];
        for shift in [0u8, 2, 4, 6] {
            for half in 0..2 {
                let sc = scales[is];
                is += 1;
                let dl = d * (sc & 0x0F) as f32;
                let ml = min * (sc >> 4) as f32;
                for l in 0..16 {
                    y[yi] = dl * ((q[l + 16 * half] >> shift) & 3) as f32 - ml;
                    yi += 1;
                }
            }
        }
    }
}

fn dq_q3_k(b: &[u8], y: &mut [f32]) {
    // Layout: hmask[32], qs[64], scales[12], d f16.
    let hm = &b[0..32];
    let qs = &b[32..96];
    let raw_scales = &b[96..108];
    let d_all = f16_le(&b[108..110]);

    // 6-bit scale unpack, identical bit shuffle to llama.cpp (kmask1/kmask2).
    const KMASK1: u32 = 0x0303_0303;
    const KMASK2: u32 = 0x0f0f_0f0f;
    let a0 = u32::from_le_bytes([raw_scales[0], raw_scales[1], raw_scales[2], raw_scales[3]]);
    let a1 = u32::from_le_bytes([raw_scales[4], raw_scales[5], raw_scales[6], raw_scales[7]]);
    let tmp = u32::from_le_bytes([raw_scales[8], raw_scales[9], raw_scales[10], raw_scales[11]]);
    let aux = [
        (a0 & KMASK2) | ((tmp & KMASK1) << 4),
        (a1 & KMASK2) | (((tmp >> 2) & KMASK1) << 4),
        ((a0 >> 4) & KMASK2) | (((tmp >> 4) & KMASK1) << 4),
        ((a1 >> 4) & KMASK2) | (((tmp >> 6) & KMASK1) << 4),
    ];
    let mut scales = [0i8; 16];
    for (i, a) in aux.iter().enumerate() {
        for (k, s) in a.to_le_bytes().iter().enumerate() {
            scales[i * 4 + k] = *s as i8;
        }
    }

    let mut is = 0usize;
    let mut m: u8 = 1;
    let mut yi = 0usize;
    for n in (0..QK_K).step_by(128) {
        let q = &qs[n / 4..n / 4 + 32];
        for shift in [0u8, 2, 4, 6] {
            for half in 0..2 {
                let dl = d_all * (scales[is] as f32 - 32.0);
                is += 1;
                for l in 0..16 {
                    let idx = l + 16 * half;
                    let lo = ((q[idx] >> shift) & 3) as i32;
                    let hi = if hm[idx] & m != 0 { 0 } else { 4 };
                    y[yi] = dl * (lo - hi) as f32;
                    yi += 1;
                }
            }
            m <<= 1;
        }
    }
}

/// llama.cpp `get_scale_min_k4`: 8 × (6-bit scale, 6-bit min) packed in 12 bytes.
fn get_scale_min_k4(j: usize, q: &[u8]) -> (u8, u8) {
    if j < 4 {
        (q[j] & 63, q[j + 4] & 63)
    } else {
        (
            (q[j + 4] & 0x0F) | ((q[j - 4] >> 6) << 4),
            (q[j + 4] >> 4) | ((q[j] >> 6) << 4),
        )
    }
}

fn dq_q4_k(b: &[u8], y: &mut [f32]) {
    // Layout: d f16, dmin f16, scales[12], qs[128].
    let d = f16_le(&b[0..2]);
    let min = f16_le(&b[2..4]);
    let scales = &b[4..16];
    let qs = &b[16..144];

    let mut yi = 0usize;
    let mut is = 0usize;
    for j in (0..QK_K).step_by(64) {
        let q = &qs[j / 2..j / 2 + 32];
        let (sc1, m1) = get_scale_min_k4(is, scales);
        let (sc2, m2) = get_scale_min_k4(is + 1, scales);
        let (d1, mm1) = (d * sc1 as f32, min * m1 as f32);
        let (d2, mm2) = (d * sc2 as f32, min * m2 as f32);
        for &qb in q {
            y[yi] = d1 * (qb & 0x0F) as f32 - mm1;
            yi += 1;
        }
        for &qb in q {
            y[yi] = d2 * (qb >> 4) as f32 - mm2;
            yi += 1;
        }
        is += 2;
    }
}

fn dq_q5_k(b: &[u8], y: &mut [f32]) {
    // Layout: d f16, dmin f16, scales[12], qh[32], qs[128].
    let d = f16_le(&b[0..2]);
    let min = f16_le(&b[2..4]);
    let scales = &b[4..16];
    let qh = &b[16..48];
    let qs = &b[48..176];

    let mut yi = 0usize;
    let mut is = 0usize;
    let mut u1: u8 = 1;
    let mut u2: u8 = 2;
    for j in (0..QK_K).step_by(64) {
        let ql = &qs[j / 2..j / 2 + 32];
        let (sc1, m1) = get_scale_min_k4(is, scales);
        let (sc2, m2) = get_scale_min_k4(is + 1, scales);
        let (d1, mm1) = (d * sc1 as f32, min * m1 as f32);
        let (d2, mm2) = (d * sc2 as f32, min * m2 as f32);
        for l in 0..32 {
            let hi = if qh[l] & u1 != 0 { 16 } else { 0 };
            y[yi] = d1 * ((ql[l] & 0x0F) as u32 + hi) as f32 - mm1;
            yi += 1;
        }
        for l in 0..32 {
            let hi = if qh[l] & u2 != 0 { 16 } else { 0 };
            y[yi] = d2 * ((ql[l] >> 4) as u32 + hi) as f32 - mm2;
            yi += 1;
        }
        is += 2;
        u1 <<= 2;
        u2 <<= 2;
    }
}

fn dq_q6_k(b: &[u8], y: &mut [f32]) {
    // Layout: ql[128], qh[64], scales[16] i8, d f16.
    let ql = &b[0..128];
    let qh = &b[128..192];
    let sc = &b[192..208];
    let d = f16_le(&b[208..210]);

    for n in 0..2 {
        let y = &mut y[n * 128..(n + 1) * 128];
        let ql = &ql[n * 64..(n + 1) * 64];
        let qh = &qh[n * 32..(n + 1) * 32];
        let sc = &sc[n * 8..(n + 1) * 8];
        for l in 0..32 {
            let is = l / 16;
            let q1 = ((ql[l] & 0x0F) | (((qh[l]) & 3) << 4)) as i32 - 32;
            let q2 = ((ql[l + 32] & 0x0F) | (((qh[l] >> 2) & 3) << 4)) as i32 - 32;
            let q3 = ((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) as i32 - 32;
            let q4 = ((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) as i32 - 32;
            y[l] = d * (sc[is] as i8) as f32 * q1 as f32;
            y[l + 32] = d * (sc[is + 2] as i8) as f32 * q2 as f32;
            y[l + 64] = d * (sc[is + 4] as i8) as f32 * q3 as f32;
            y[l + 96] = d * (sc[is + 6] as i8) as f32 * q4 as f32;
        }
    }
}

fn dq_q8_k(b: &[u8], y: &mut [f32]) {
    // Layout: d f32, qs[256] i8, bsums[16] i16 (bsums unused for dequant).
    let d = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    for j in 0..QK_K {
        y[j] = d * (b[4 + j] as i8) as f32;
    }
}

// --- I-quants / microscaling ---

fn dq_iq4_nl(b: &[u8], y: &mut [f32]) {
    let d = f16_le(&b[0..2]);
    let qs = &b[2..18];
    for j in 0..16 {
        y[j] = d * KVALUES_IQ4NL[(qs[j] & 0x0F) as usize] as f32;
        y[j + 16] = d * KVALUES_IQ4NL[(qs[j] >> 4) as usize] as f32;
    }
}

fn dq_iq4_xs(b: &[u8], y: &mut [f32]) {
    // Layout: d f16, scales_h u16, scales_l[4], qs[128].
    let d = f16_le(&b[0..2]);
    let scales_h = u16::from_le_bytes([b[2], b[3]]);
    let scales_l = &b[4..8];
    let qs = &b[8..136];

    for ib in 0..8 {
        let ls = ((scales_l[ib / 2] >> (4 * (ib % 2))) & 0x0F) as i32
            | ((((scales_h >> (2 * ib)) & 3) as i32) << 4);
        let dl = d * (ls - 32) as f32;
        let q = &qs[ib * 16..(ib + 1) * 16];
        let y = &mut y[ib * 32..(ib + 1) * 32];
        for j in 0..16 {
            y[j] = dl * KVALUES_IQ4NL[(q[j] & 0x0F) as usize] as f32;
            y[j + 16] = dl * KVALUES_IQ4NL[(q[j] >> 4) as usize] as f32;
        }
    }
}

fn dq_mxfp4(b: &[u8], y: &mut [f32]) {
    // Layout: e u8 (E8M0), qs[16].
    let d = e8m0_to_f32_half(b[0]);
    let qs = &b[1..17];
    for j in 0..16 {
        y[j] = KVALUES_MXFP4[(qs[j] & 0x0F) as usize] as f32 * d;
        y[j + 16] = KVALUES_MXFP4[(qs[j] >> 4) as usize] as f32 * d;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f16_bytes(v: f32) -> [u8; 2] {
        f16::from_f32(v).to_bits().to_le_bytes()
    }

    fn dq(quant: QuantKind, block: &[u8]) -> Vec<f32> {
        dequantize_to_f32(DType::U8, quant, block, quant.block_elems()).unwrap()
    }

    #[test]
    fn plain_f16_bf16() {
        let data = [f16_bytes(1.5), f16_bytes(-2.0)].concat();
        assert_eq!(
            dequantize_to_f32(DType::F16, QuantKind::None, &data, 2).unwrap(),
            vec![1.5, -2.0]
        );
        // bf16 1.5 = 0x3FC0, -2.0 = 0xC000
        let data = [0xC0u8, 0x3F, 0x00, 0xC0];
        assert_eq!(
            dequantize_to_f32(DType::BF16, QuantKind::None, &data, 2).unwrap(),
            vec![1.5, -2.0]
        );
    }

    #[test]
    fn q4_0_scale_and_offset() {
        let mut b = vec![0u8; 18];
        b[0..2].copy_from_slice(&f16_bytes(2.0));
        b[2] = 0x31; // low nibble 1, high nibble 3
        let y = dq(QuantKind::Q4_0, &b);
        assert_eq!(y[0], (1 - 8) as f32 * 2.0); // -14
        assert_eq!(y[16], (3 - 8) as f32 * 2.0); // -10
        assert_eq!(y[1], -16.0); // nibble 0 -> (0-8)*2
    }

    #[test]
    fn q4_1_min() {
        let mut b = vec![0u8; 20];
        b[0..2].copy_from_slice(&f16_bytes(0.5));
        b[2..4].copy_from_slice(&f16_bytes(1.0));
        b[4] = 0x21;
        let y = dq(QuantKind::Q4_1, &b);
        assert_eq!(y[0], 1.5); // 1*0.5 + 1
        assert_eq!(y[16], 2.0); // 2*0.5 + 1
    }

    #[test]
    fn q5_0_high_bit() {
        let mut b = vec![0u8; 22];
        b[0..2].copy_from_slice(&f16_bytes(1.0));
        // qh bit 0 -> element 0 gets +16; bit 16 -> element 16 (xh_1 = (qh>>12)&0x10)
        let qh: u32 = 1 | (1 << 16);
        b[2..6].copy_from_slice(&qh.to_le_bytes());
        b[6] = 0xFF; // low nibble 15, high nibble 15
        let y = dq(QuantKind::Q5_0, &b);
        assert_eq!(y[0], (15 + 16 - 16) as f32); // 15
        assert_eq!(y[16], (15 + 16 - 16) as f32); // 15
        assert_eq!(y[1], -16.0); // qs 0, qh 0 -> 0-16
    }

    #[test]
    fn q5_1_high_bit_and_min() {
        let mut b = vec![0u8; 24];
        b[0..2].copy_from_slice(&f16_bytes(2.0));
        b[2..4].copy_from_slice(&f16_bytes(-1.0));
        let qh: u32 = 1;
        b[4..8].copy_from_slice(&qh.to_le_bytes());
        b[8] = 0x03;
        let y = dq(QuantKind::Q5_1, &b);
        assert_eq!(y[0], (3 + 16) as f32 * 2.0 - 1.0); // 37
        assert_eq!(y[16], 0.0 * 2.0 - 1.0); // high nibble 0, no qh bit
    }

    #[test]
    fn q8_0_signed() {
        let mut b = vec![0u8; 34];
        b[0..2].copy_from_slice(&f16_bytes(0.5));
        for j in 0..32 {
            b[2 + j] = (j as i32 - 16) as i8 as u8;
        }
        let y = dq(QuantKind::Q8_0, &b);
        for (j, v) in y.iter().enumerate() {
            assert_eq!(*v, (j as f32 - 16.0) * 0.5);
        }
    }

    #[test]
    fn q2_k_scales_and_mins() {
        let mut b = vec![0u8; 84];
        b[0] = 0x21; // sub-block 0: scale 1, min 2
        b[16] = 0b0000_0011; // element 0 quant = 3 (shift 0)
        b[80..82].copy_from_slice(&f16_bytes(2.0)); // d
        b[82..84].copy_from_slice(&f16_bytes(0.5)); // dmin
        let y = dq(QuantKind::Q2K, &b);
        // y[0] = d*1*3 - min*2 = 6 - 1 = 5
        assert_eq!(y[0], 5.0);
        // element 1: quant 0 -> -min*2 = -1
        assert_eq!(y[1], -1.0);
        // sub-block 1 (elements 16..32) uses scales[1]=0 -> all zero
        assert_eq!(y[16], 0.0);
    }

    #[test]
    fn q3_k_scale_unpack_and_hmask() {
        let mut b = vec![0u8; 110];
        // hmask all set -> no -4 offset anywhere
        for h in b[0..32].iter_mut() {
            *h = 0xFF;
        }
        b[32] = 3; // element 0 quant (shift 0) = 3
        b[96] = 0x01; // scales[0] low 4 bits = 1, high bits (tmp) = 0 -> scale 1
        b[108..110].copy_from_slice(&f16_bytes(1.0));
        let y = dq(QuantKind::Q3K, &b);
        // dl = 1.0 * (1 - 32) = -31; y[0] = -31 * 3
        assert_eq!(y[0], -93.0);
        // clear hmask bit for element 1 (m=1): q -> 0 - 4 = -4; y[1] = -31 * -4
        b[1] = 0xFE;
        let y = dq(QuantKind::Q3K, &b);
        assert_eq!(y[1], 124.0);
    }

    #[test]
    fn q4_k_both_scale_branches() {
        let mut b = vec![0u8; 144];
        b[0..2].copy_from_slice(&f16_bytes(1.0)); // d
        b[2..4].copy_from_slice(&f16_bytes(0.25)); // dmin
        let scales = &mut b[4..16];
        scales[0] = 2; // sc0 = 2
        scales[4] = 4; // m0 = 4
        scales[1] = 3; // sc1 = 3
        scales[5] = 0; // m1 = 0
                       // j >= 4 branch: sc4 = (q[8]&0xF) | ((q[0]>>6)<<4); m4 = (q[8]>>4) | ((q[4]>>6)<<4)
        scales[8] = 0x21; // sc4 low = 1, m4 low = 2
        b[16] = 0x53; // qs[0]: elem0 = 3, elem32 = 5
        b[16 + 64] = 0x0F; // qs[64]: elem128 = 15 (sub-block is=4)
        let y = dq(QuantKind::Q4K, &b);
        assert_eq!(y[0], 1.0 * 2.0 * 3.0 - 0.25 * 4.0); // 5.0
        assert_eq!(y[32], 1.0 * 3.0 * 5.0 - 0.0); // 15.0
        assert_eq!(y[128], 1.0 * 1.0 * 15.0 - 0.25 * 2.0); // 14.5
    }

    #[test]
    fn q5_k_high_bits() {
        let mut b = vec![0u8; 176];
        b[0..2].copy_from_slice(&f16_bytes(1.0));
        b[2..4].copy_from_slice(&f16_bytes(1.0));
        b[4] = 1; // sc0 = 1
        b[8] = 2; // m0 = 2
        b[16] = 0x01; // qh[0] bit 0 set -> element 0 +16
        b[48] = 0x07; // ql elem0 = 7
        let y = dq(QuantKind::Q5K, &b);
        assert_eq!(y[0], (7 + 16) as f32 - 2.0); // 21
        assert_eq!(y[32], 0.0); // sc1=0, m1=0
    }

    #[test]
    fn q6_k_quadrants() {
        let mut b = vec![0u8; 210];
        b[0] = 0x05; // ql[0]: low nibble 5 (elem 0), high nibble 0 (elem 64)
        b[128] = 0x00; // qh[0] = 0
        b[192] = 2; // scales[0] = 2 (elem 0..16)
        b[196] = 1; // scales[4] = 1 (elem 64..80)
        b[208..210].copy_from_slice(&f16_bytes(1.0));
        let y = dq(QuantKind::Q6K, &b);
        assert_eq!(y[0], 2.0 * (5 - 32) as f32); // -54
        assert_eq!(y[64], 1.0 * (0 - 32) as f32); // -32
                                                  // set qh bits 4..5 for elem 64 quadrant: q3 gains ((qh>>4)&3)<<4
        b[128] = 0x30;
        let y = dq(QuantKind::Q6K, &b);
        assert_eq!(y[64], 1.0 * (48 - 32) as f32); // 16
    }

    #[test]
    fn q8_k_f32_scale() {
        let mut b = vec![0u8; 292];
        b[0..4].copy_from_slice(&0.5f32.to_le_bytes());
        b[4] = (-4i8) as u8;
        let y = dq(QuantKind::Q8K, &b);
        assert_eq!(y[0], -2.0);
    }

    #[test]
    fn iq4_nl_codebook() {
        let mut b = vec![0u8; 18];
        b[0..2].copy_from_slice(&f16_bytes(1.0));
        b[2] = 0x90; // low nibble 0 -> -127; high nibble 9 -> 13
        let y = dq(QuantKind::IQ4NL, &b);
        assert_eq!(y[0], -127.0);
        assert_eq!(y[16], 13.0);
    }

    #[test]
    fn iq4_xs_subblock_scales() {
        let mut b = vec![0u8; 136];
        b[0..2].copy_from_slice(&f16_bytes(1.0));
        b[2..4].copy_from_slice(&1u16.to_le_bytes()); // scales_h bit 0 -> +16 for ib 0
        b[4] = 0x05; // scales_l[0] low nibble = 5 -> ls0 = 21
        b[8] = 0x01; // qs[0]: low idx 1 -> -104, high idx 0 -> -127
        let y = dq(QuantKind::IQ4XS, &b);
        let dl = (21 - 32) as f32; // -11
        assert_eq!(y[0], dl * -104.0);
        assert_eq!(y[16], dl * -127.0);
        // ib 1: ls = 0 -> dl = -32; quant code 0 is codebook -127, not zero
        assert_eq!(y[32], -32.0 * -127.0);
    }

    #[test]
    fn mxfp4_e8m0_half_scale() {
        let mut b = vec![0u8; 17];
        b[0] = 127; // 2^(127-127) / 2 = 0.5
        b[1] = 0x91; // low idx 1 -> +1; high idx 9 -> -1
        let y = dq(QuantKind::MXFP4, &b);
        assert_eq!(y[0], 0.5);
        assert_eq!(y[16], -0.5);
        assert_eq!(e8m0_to_f32_half(128), 1.0);
        assert_eq!(e8m0_to_f32_half(1), f32::from_bits(0x0040_0000));
        assert_eq!(e8m0_to_f32_half(0), f32::from_bits(0x0020_0000));
    }

    #[test]
    fn size_mismatch_is_error() {
        assert!(dequantize_to_f32(DType::U8, QuantKind::Q8_0, &[0u8; 33], 32).is_err());
        assert!(dequantize_to_f32(DType::U8, QuantKind::Q4K, &[0u8; 144], 255).is_err());
        assert!(dequantize_to_f32(DType::F16, QuantKind::None, &[0u8; 3], 2).is_err());
    }
}
