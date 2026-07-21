// ===== File: nvfp4.rs — NVFP4 (compressed-tensors nvfp4-pack-quantized) decode + reference dequant =====
//
// llm-compressor / compressed-tensors layout, as produced for e.g.
// Bielik-1.5B-NVFP4: for each Linear weight of shape [rows, cols]
//   `<base>.weight_packed`        U8  [rows, cols/2]   two E2M1 codes per byte
//   `<base>.weight_scale`         F8_E4M3 [rows, cols/group_size] per-block scales
//   `<base>.weight_global_scale`  F32 [1]              tensor scale
// Element 2i is the LOW nibble, 2i+1 the HIGH nibble (vLLM/compressed-tensors
// unpack order). Block scales were multiplied by the global scale before FP8
// rounding, so dequant divides it back out:
//   w = e2m1(code) * fp8(scale) / global_scale

use forge_types::{ForgeError, Result};

use crate::hf_config::HfConfig;

fn fmt_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Format(msg.into())
}

/// E2M1 magnitude codebook; bit 3 of the nibble is the sign.
const E2M1_LUT: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];

/// FP8 E4M3FN (no infinities; 0x7F/0xFF are NaN) to f32.
pub fn f8e4m3_to_f32(b: u8) -> f32 {
    let sign = if b & 0x80 != 0 { -1.0f32 } else { 1.0f32 };
    let exp = (b >> 3) & 0x0F;
    let man = (b & 0x07) as f32;
    if exp == 0 {
        // Subnormal: man/8 * 2^-6
        sign * man * (1.0 / 512.0)
    } else if exp == 15 && (b & 0x07) == 0x07 {
        f32::NAN
    } else {
        sign * (1.0 + man / 8.0) * 2f32.powi(exp as i32 - 7)
    }
}

/// f32 → FP8 E4M3FN byte, round-to-nearest-even, saturating to ±448 (e4m3fn
/// has no infinities; the max finite is 448). NaN maps to the canonical NaN
/// 0x7F. Mirrors the hardware `cvt.rn.satfinite.e4m3` used by the GPU quantizer,
/// so the CPU weight pack and the on-device activation quant agree bit-for-bit.
pub fn f32_to_f8e4m3(v: f32) -> u8 {
    if v.is_nan() {
        return 0x7F;
    }
    let sign: u8 = if v.is_sign_negative() { 0x80 } else { 0x00 };
    let a = v.abs();
    if a == 0.0 {
        return sign;
    }
    // Saturate to the largest finite magnitude (448 = 1.75 * 2^8).
    if a >= 448.0 {
        return sign | 0x7E;
    }
    // Round mantissa in the f32 domain, then re-extract the e4m3 fields.
    let bits = a.to_bits();
    let e = ((bits >> 23) & 0xFF) as i32 - 127; // unbiased f32 exponent
    if e < -9 {
        // Below the smallest subnormal (2^-9); flushes to zero (RNE: half of
        // the smallest subnormal rounds to even = 0).
        return sign;
    }
    if e < -6 {
        // Subnormal e4m3: value = man/8 * 2^-6, man in 0..=7.
        let scaled = a / (1.0 / 64.0); // a * 2^6 → man/8 domain (× further below)
        // man = round(a * 2^6 * 8) = round(a * 2^9), ties to even.
        let man = round_ties_even(a * 512.0);
        let man = man.clamp(0.0, 8.0) as u32;
        if man == 8 {
            // Rounded up into the smallest normal (exp=1, man=0).
            return sign | (1 << 3);
        }
        let _ = scaled;
        return sign | (man as u8 & 0x07);
    }
    // Normal e4m3: exp field = e + 7 (1..=15), 3 mantissa bits (RNE).
    let mant_f = (a / 2f32.powi(e)) - 1.0; // in [0,1)
    let man = round_ties_even(mant_f * 8.0);
    let (mut exp_field, man) = if man >= 8.0 {
        (e + 7 + 1, 0u32) // mantissa carry bumps the exponent
    } else {
        (e + 7, man as u32)
    };
    if exp_field >= 15 && !(exp_field == 15 && man <= 6) {
        // Overflow into the NaN slot → saturate to max finite.
        return sign | 0x7E;
    }
    if exp_field > 15 {
        exp_field = 15;
    }
    sign | ((exp_field as u8) << 3) | (man as u8 & 0x07)
}

fn round_ties_even(x: f32) -> f32 {
    let r = x.round(); // rounds halves away from zero
    if (x - x.floor() - 0.5).abs() < f32::EPSILON {
        // Exact tie: pick the even neighbor.
        let lo = x.floor();
        if (lo as i64) % 2 == 0 {
            lo
        } else {
            lo + 1.0
        }
    } else {
        r
    }
}

fn e2m1_to_f32(nibble: u8) -> f32 {
    let mag = E2M1_LUT[(nibble & 0x07) as usize];
    if nibble & 0x08 != 0 {
        -mag
    } else {
        mag
    }
}

/// Detected NVFP4 weight scheme from `quantization_config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvFp4Scheme {
    pub group_size: usize,
    /// Module names excluded from quantization (kept in plain dtype).
    pub ignore: Vec<String>,
}

impl NvFp4Scheme {
    /// Detect the compressed-tensors NVFP4 scheme from an HF config.
    /// Returns None when the model is not NVFP4-quantized.
    pub fn detect(config: &HfConfig) -> Option<Self> {
        let qc = config.quantization_config.as_ref()?;
        if qc.get("quant_method")?.as_str()? != "compressed-tensors" {
            return None;
        }
        if qc.get("format")?.as_str()? != "nvfp4-pack-quantized" {
            return None;
        }
        // group_size lives in the per-group weights spec; 16 is the NVFP4
        // definition, but read it from the config to catch drift.
        let group_size = qc
            .get("config_groups")
            .and_then(|g| g.as_object())
            .and_then(|g| g.values().next())
            .and_then(|g| g.get("weights"))
            .and_then(|w| w.get("group_size"))
            .and_then(|v| v.as_u64())
            .unwrap_or(16) as usize;
        let ignore = qc
            .get("ignore")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Some(NvFp4Scheme { group_size, ignore })
    }
}

/// The three on-disk tensor names for one NVFP4-quantized `<base>.weight`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvFp4TensorNames {
    pub packed: String,
    pub scale: String,
    pub global_scale: String,
}

impl NvFp4TensorNames {
    /// Derive the packed-tensor triple from a canonical `*.weight` name.
    pub fn for_weight(weight_name: &str) -> Result<Self> {
        let base = weight_name.strip_suffix(".weight").ok_or_else(|| {
            fmt_err(format!(
                "nvfp4: '{weight_name}' is not a '.weight' tensor name"
            ))
        })?;
        Ok(NvFp4TensorNames {
            packed: format!("{base}.weight_packed"),
            scale: format!("{base}.weight_scale"),
            global_scale: format!("{base}.weight_global_scale"),
        })
    }
}

/// Reference NVFP4 → f32 dequantization of a [rows, cols] weight.
///
/// * `packed`: rows × cols/2 bytes (row-major, low nibble = even element)
/// * `scales`: rows × cols/group_size FP8-E4M3 bytes (row-major)
pub fn dequantize_nvfp4(
    packed: &[u8],
    scales: &[u8],
    global_scale: f32,
    rows: usize,
    cols: usize,
    group_size: usize,
) -> Result<Vec<f32>> {
    if group_size == 0 || group_size % 2 != 0 {
        return Err(fmt_err(format!("nvfp4: invalid group size {group_size}")));
    }
    if cols % group_size != 0 {
        return Err(fmt_err(format!(
            "nvfp4: cols {cols} not divisible by group size {group_size}"
        )));
    }
    if !global_scale.is_finite() || global_scale == 0.0 {
        return Err(fmt_err(format!(
            "nvfp4: invalid global scale {global_scale}"
        )));
    }
    let row_bytes = cols / 2;
    let row_groups = cols / group_size;
    let expect_packed = rows
        .checked_mul(row_bytes)
        .ok_or_else(|| fmt_err("nvfp4: size overflow"))?;
    let expect_scales = rows
        .checked_mul(row_groups)
        .ok_or_else(|| fmt_err("nvfp4: size overflow"))?;
    if packed.len() != expect_packed {
        return Err(fmt_err(format!(
            "nvfp4: packed data is {} bytes, expected {expect_packed} for [{rows}, {cols}]",
            packed.len()
        )));
    }
    if scales.len() != expect_scales {
        return Err(fmt_err(format!(
            "nvfp4: scale data is {} bytes, expected {expect_scales} for [{rows}, {cols}] / {group_size}",
            scales.len()
        )));
    }

    let numel = rows
        .checked_mul(cols)
        .ok_or_else(|| fmt_err("nvfp4: size overflow"))?;
    let inv_global = 1.0 / global_scale;
    let mut out = vec![0.0f32; numel];
    for r in 0..rows {
        let prow = &packed[r * row_bytes..(r + 1) * row_bytes];
        let srow = &scales[r * row_groups..(r + 1) * row_groups];
        let orow = &mut out[r * cols..(r + 1) * cols];
        for (c2, &byte) in prow.iter().enumerate() {
            let c = c2 * 2;
            let scale = f8e4m3_to_f32(srow[c / group_size]) * inv_global;
            orow[c] = e2m1_to_f32(byte & 0x0F) * scale;
            orow[c + 1] = e2m1_to_f32(byte >> 4) * scale;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp8_e4m3_decode() {
        assert_eq!(f8e4m3_to_f32(0x38), 1.0); // exp 7, man 0
        assert_eq!(f8e4m3_to_f32(0x40), 2.0); // exp 8, man 0
        assert_eq!(f8e4m3_to_f32(0xB8), -1.0);
        assert_eq!(f8e4m3_to_f32(0x00), 0.0);
        assert_eq!(f8e4m3_to_f32(0x01), 1.0 / 512.0); // smallest subnormal
        assert_eq!(f8e4m3_to_f32(0x7E), 448.0); // max finite
        assert!(f8e4m3_to_f32(0x7F).is_nan());
    }

    #[test]
    fn e2m1_codebook_and_sign() {
        assert_eq!(e2m1_to_f32(0x0), 0.0);
        assert_eq!(e2m1_to_f32(0x1), 0.5);
        assert_eq!(e2m1_to_f32(0x7), 6.0);
        assert_eq!(e2m1_to_f32(0x9), -0.5);
        assert_eq!(e2m1_to_f32(0xF), -6.0);
    }

    #[test]
    fn dequant_one_group() {
        // 1 row × 16 cols, group 16. Packed byte 0x2C: element 0 = low nibble
        // 0xC (sign|4 -> -2.0), element 1 = high nibble 2 (1.0).
        // Scale fp8 0x40 = 2.0, global = 4.0.
        let mut packed = vec![0u8; 8];
        packed[0] = 0x2C;
        let scales = vec![0x40u8];
        let y = dequantize_nvfp4(&packed, &scales, 4.0, 1, 16, 16).unwrap();
        assert_eq!(y[0], -2.0 * 2.0 / 4.0);
        assert_eq!(y[1], 1.0 * 2.0 / 4.0);
        assert_eq!(y[2], 0.0);
    }

    #[test]
    fn shape_validation() {
        assert!(dequantize_nvfp4(&[0u8; 8], &[0u8; 1], 0.0, 1, 16, 16).is_err());
        assert!(dequantize_nvfp4(&[0u8; 7], &[0u8; 1], 1.0, 1, 16, 16).is_err());
        assert!(dequantize_nvfp4(&[0u8; 8], &[0u8; 2], 1.0, 1, 16, 16).is_err());
        assert!(dequantize_nvfp4(&[0u8; 8], &[0u8; 1], 1.0, 1, 16, 10).is_err());
    }

    #[test]
    fn tensor_names() {
        let n = NvFp4TensorNames::for_weight("model.layers.0.self_attn.q_proj.weight").unwrap();
        assert_eq!(n.packed, "model.layers.0.self_attn.q_proj.weight_packed");
        assert_eq!(n.scale, "model.layers.0.self_attn.q_proj.weight_scale");
        assert_eq!(
            n.global_scale,
            "model.layers.0.self_attn.q_proj.weight_global_scale"
        );
        assert!(NvFp4TensorNames::for_weight("foo.bias").is_err());
    }
}
