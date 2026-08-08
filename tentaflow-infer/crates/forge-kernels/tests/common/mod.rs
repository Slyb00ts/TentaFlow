// ===== File: tests/common/mod.rs — wspólne wzorce wag dla testów golden =====
//
// Bloki GGUF budowane tu deterministycznie z indeksu: ten sam wzorzec musi
// wejść i do kernela na GPU, i do dekwantyzacji wzorcowej na CPU, więc żyje
// w jednym miejscu zamiast po kopii na plik testu.
#![allow(dead_code)]

use half::f16;

/// Deterministic Q8_0 stream (34-byte blocks: f16 scale + 32 i8 codes).
pub fn build_q8_0(rows: usize, cols: usize) -> Vec<u8> {
    let mut wq = Vec::with_capacity(rows * cols / 32 * 34);
    for r in 0..rows {
        for b in 0..cols / 32 {
            let scale = f16::from_f32(0.01 + ((r + b) % 5) as f32 * 0.01);
            wq.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                wq.push((((r * 31 + b * 17 + k * 13) % 255) as i32 - 127) as i8 as u8);
            }
        }
    }
    wq
}
/// Deterministic Q4_K stream (144-byte superblocks) exercising both
/// get_scale_min_k4 branches including the high 2 bits of every scale byte.
pub fn build_q4k(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 256;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 144);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let d = f16::from_f32(0.008 + ((r + b) % 7) as f32 * 0.004);
            let dmin = f16::from_f32(0.005 + ((r + 2 * b) % 5) as f32 * 0.003);
            wq.extend_from_slice(&d.to_le_bytes());
            wq.extend_from_slice(&dmin.to_le_bytes());
            for i in 0..12 {
                wq.push(((r * 53 + b * 19 + i * 41 + 7) % 256) as u8);
            }
            for i in 0..128 {
                wq.push(((r * 31 + b * 17 + i * 13) % 256) as u8);
            }
        }
    }
    wq
}
/// Deterministic Q6_K stream (210-byte superblocks: ql[128], qh[64],
/// 16 int8 scales, d f16) exercising every qh shift and both nibble halves.
pub fn build_q6k(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 256;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 210);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            for i in 0..208 {
                wq.push(((r * 37 + b * 23 + i * 11 + 5) % 256) as u8);
            }
            let d = f16::from_f32(0.006 + ((r + b) % 7) as f32 * 0.003);
            wq.extend_from_slice(&d.to_le_bytes());
        }
    }
    wq
}
pub fn e2m1_reference(code: u8) -> f32 {
    const MAGNITUDES: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
    let magnitude = MAGNITUDES[(code & 0x07) as usize];
    if code & 0x08 == 0 {
        magnitude
    } else {
        -magnitude
    }
}
pub fn make_nvfp4_gguf(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 64;
    let mut weights = vec![0u8; rows * blocks_per_row * 36];
    for row in 0..rows {
        for block in 0..blocks_per_row {
            let base = (row * blocks_per_row + block) * 36;
            for subblock in 0..4 {
                weights[base + subblock] = 0x20 + ((row + block * 3 + subblock * 5) % 25) as u8;
                for j in 0..8 {
                    let low = ((row * 7 + block * 11 + subblock * 3 + j * 5) % 16) as u8;
                    let high = ((row * 13 + block * 5 + subblock * 7 + j * 3 + 1) % 16) as u8;
                    weights[base + 4 + subblock * 8 + j] = low | (high << 4);
                }
            }
        }
    }
    weights
}
pub fn nvfp4_gguf_dot(weights: &[u8], row: usize, cols: usize, x: &[f32], output_scale: f32) -> f32 {
    let blocks_per_row = cols / 64;
    let row_base = row * blocks_per_row * 36;
    let mut sum = 0.0f32;
    for group in 0..cols / 16 {
        let block = group / 4;
        let subblock = group % 4;
        let block_base = row_base + block * 36;
        let scale_byte = weights[block_base + subblock];
        let scale = if scale_byte == 0x7f {
            0.0
        } else {
            forge_formats::nvfp4::f8e4m3_to_f32(scale_byte)
        };
        let packed_base = block_base + 4 + subblock * 8;
        let x_base = group * 16;
        let mut dot = 0.0f32;
        for j in 0..8 {
            let code = weights[packed_base + j];
            dot += e2m1_reference(code & 0x0f) * x[x_base + j];
            dot += e2m1_reference(code >> 4) * x[x_base + j + 8];
        }
        sum += scale * dot;
    }
    sum * output_scale
}
pub fn make_q8_0(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 32;
    let mut weights = Vec::with_capacity(rows * blocks_per_row * 34);
    for row in 0..rows {
        for block in 0..blocks_per_row {
            let scale = f16::from_f32(0.003 + ((row + block) % 7) as f32 * 0.001);
            weights.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                weights.push((((row * 17 + block * 13 + k * 7) % 63) as i32 - 31) as i8 as u8);
            }
        }
    }
    weights
}
pub fn fp32_to_ue4m3(mut value: f32) -> u8 {
    if value <= 0.0 {
        return 0;
    }
    value = value.min(448.0);
    let bits = value.to_bits();
    let fp32_exp = ((bits >> 23) & 0xff) as i32 - 127;
    let fp32_man = ((bits >> 20) & 7) as i32;
    let mut exponent = fp32_exp + 7;
    if exponent <= 0 {
        return (value.mul_add(512.0, 0.5) as i32).clamp(0, 7) as u8;
    }
    if exponent >= 15 {
        return 0x7e;
    }
    let mut mantissa = fp32_man + ((bits >> 19) & 1) as i32;
    if mantissa > 7 {
        mantissa = 0;
        exponent += 1;
        if exponent >= 15 {
            return 0x7e;
        }
    }
    ((exponent << 3) | mantissa) as u8
}
pub fn ue4m3(value: u8) -> f32 {
    if value == 0 || value == 0x7f {
        return 0.0;
    }
    let exponent = (value >> 3) & 0x0f;
    let mantissa = (value & 7) as f32;
    if exponent == 0 {
        mantissa / 512.0
    } else {
        (1.0 + mantissa / 8.0) * 2.0f32.powi(exponent as i32 - 7)
    }
}
pub fn best_e2m1(value: f32, scale: f32) -> u8 {
    const VALUES: [f32; 16] = [
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    ];
    let mut best = 0;
    let mut error = value.abs();
    for (index, candidate) in VALUES.iter().enumerate().skip(1) {
        let next = (candidate * scale - value).abs();
        if next < error {
            best = index as u8;
            error = next;
        }
    }
    best
}
