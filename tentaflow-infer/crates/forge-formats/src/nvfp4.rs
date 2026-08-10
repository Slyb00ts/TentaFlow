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

/// Kanoniczny S0E5M3 NaN używany przez repacker GPU dla błędnego wejścia.
pub const NVFP4_CT_S0_NAN: u8 = 0xF9;

/// Koduje legalną dodatnią skalę E4M3 do S0E5M3 z kompensacją x128.
///
/// Ujemne skale i NaN nie należą do kontraktu compressed-tensors i są
/// odrzucane. Repacker GPU zapisuje dla nich `NVFP4_CT_S0_NAN`, aby błąd
/// pozostał widoczny również bez walidacji hosta.
pub fn nvfp4_ct_s0_from_e4m3(value: u8) -> Option<u8> {
    if value & 0x80 != 0 || value == 0x7f {
        return None;
    }
    let exponent = (value >> 3) & 0x0f;
    let mantissa = value & 0x07;
    if exponent != 0 {
        return Some((exponent + 15) << 3 | mantissa);
    }
    Some(match mantissa {
        0 => 0x00,
        1 => 0x68,
        2 => 0x70,
        3 => 0x74,
        4 => 0x78,
        5 => 0x7a,
        6 => 0x7c,
        7 => 0x7e,
        _ => unreachable!(),
    })
}

/// Dekoduje S0E5M3 tak samo jak `UInt16(value) << 7` w kernelu Mojo.
pub fn nvfp4_ct_s0_to_f32(value: u8) -> f32 {
    half::f16::from_bits((value as u16) << 7).to_f32()
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

pub fn e2m1_to_f32(nibble: u8) -> f32 {
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

/// Przepakowuje NVFP4 compressed-tensors do jednobuforowego układu GGUF
/// (36 bajtów na 64 elementy: cztery skale UE4M3, potem 32 bajty par E2M1).
///
/// Sens jest jeden: rezydencja ekspertów wymaga wagi o JEDNYM buforze bajtów,
/// a układ źródłowy trzyma pakiety i skale osobno. Po przepakowaniu ekspert
/// jest samodzielnym blokiem, który da się położyć w VRAM, w pamięci hosta albo
/// na dysku niezależnie od sąsiadów — i przy okazji wchodzi we wszystkie
/// istniejące kernele NVFP4 bez zmian.
///
/// Operacja jest czystym przestawieniem bajtów: te same skale E4M3 i te same
/// nibble E2M1, tylko w innej kolejności. Zmienia się wyłącznie parowanie
/// nibbli — źródło trzyma elementy `2i` i `2i+1` w jednym bajcie, układ GGUF
/// elementy `i` oraz `i+8` w obrębie szesnastki.
///
/// Skala globalna NIE jest tu zaszywana: wędruje osobno jako `output_scale`,
/// przez który kernel mnoży wynik.
///
/// * `packed`: `rows * cols/2` bajtów, niski nibble = element parzysty
/// * `scales`: `rows * cols/16` bajtów E4M3, po jednej skali na 16 elementów
pub fn nvfp4_ct_to_gguf_blocks(
    packed: &[u8],
    scales: &[u8],
    rows: usize,
    cols: usize,
) -> Result<Vec<u8>> {
    const GROUP: usize = 16;
    const BLOCK_ELEMS: usize = 64;
    const BLOCK_BYTES: usize = 36;

    if !cols.is_multiple_of(BLOCK_ELEMS) {
        return Err(fmt_err(format!(
            "nvfp4: przepakowanie do GGUF wymaga cols wielokrotności {BLOCK_ELEMS}, jest {cols}"
        )));
    }
    let row_bytes = cols / 2;
    let row_groups = cols / GROUP;
    let expect_packed = rows
        .checked_mul(row_bytes)
        .ok_or_else(|| fmt_err("nvfp4: przepełnienie rozmiaru"))?;
    let expect_scales = rows
        .checked_mul(row_groups)
        .ok_or_else(|| fmt_err("nvfp4: przepełnienie rozmiaru"))?;
    if packed.len() != expect_packed {
        return Err(fmt_err(format!(
            "nvfp4: pakiet ma {} bajtów, oczekiwano {expect_packed} dla [{rows}, {cols}]",
            packed.len()
        )));
    }
    if scales.len() != expect_scales {
        return Err(fmt_err(format!(
            "nvfp4: skale mają {} bajtów, oczekiwano {expect_scales} dla [{rows}, {cols}]",
            scales.len()
        )));
    }
    // Dwa kody skali, których układ GGUF nie oddaje wiernie, a które przeszłyby
    // cicho: bit znaku (skale są tam bez znaku, więc szesnastka wag zmieniłaby
    // znak) oraz 0x7F, czyli NaN w E4M3, mapowany tam na zero — co wyzerowałoby
    // całą szesnastkę zamiast zasygnalizować uszkodzony checkpoint.
    if let Some(bad) = scales
        .iter()
        .position(|scale| scale & 0x80 != 0 || *scale == 0x7F)
    {
        return Err(fmt_err(format!(
            "nvfp4: skala bloku {bad} (0x{:02X}) nie ma wiernego odpowiednika w układzie GGUF",
            scales[bad]
        )));
    }

    let blocks_per_row = cols / BLOCK_ELEMS;
    let mut out = vec![0u8; rows * blocks_per_row * BLOCK_BYTES];
    for row in 0..rows {
        let src_packed = &packed[row * row_bytes..(row + 1) * row_bytes];
        let src_scales = &scales[row * row_groups..(row + 1) * row_groups];
        for block in 0..blocks_per_row {
            let dst = &mut out[(row * blocks_per_row + block) * BLOCK_BYTES..][..BLOCK_BYTES];
            for sub in 0..4 {
                dst[sub] = src_scales[block * 4 + sub];
                for i in 0..8 {
                    // Element o indeksie lokalnym `i` i `i + 8` w szesnastce.
                    let base = block * BLOCK_ELEMS + sub * GROUP;
                    let lo = nibble(src_packed, base + i);
                    let hi = nibble(src_packed, base + i + 8);
                    dst[4 + sub * 8 + i] = lo | (hi << 4);
                }
            }
        }
    }
    Ok(out)
}

/// Dekoduje skalę FP8 E8M0 — sam wykładnik, bez mantysy i bez znaku.
/// `0xFF` to NaN, `0x00` to `2^-127`.
pub fn f8e8m0_to_f32(byte: u8) -> Option<f32> {
    if byte == 0xFF {
        return None;
    }
    Some(2f32.powi(byte as i32 - 127))
}

/// Przenosi wagę FP8 DeepSeeka z kafelkowej skali E8M0 na skalę NA WIERSZ,
/// wtapiając różnicę wykładników w same bajty E4M3.
///
/// Motyw: FORGE ma kernel FP8 ze skalą na wiersz, a nie ma z kafelkową. Skala
/// E8M0 jest czystą potęgą dwójki, więc przesunięcie wagi o `2^-k` zmienia
/// wyłącznie pole wykładnika — bez straty, dopóki nie zejdzie poniżej zakresu
/// normalnego. Normalizujemy do MAKSIMUM wiersza, więc przesunięcia idą zawsze
/// w dół i nic nie przepełnia się w górę.
///
/// Zmierzone na tym checkpoincie: rozrzut skal w obrębie wiersza wynosi
/// najwyżej jedną potęgę dwójki, więc przesunięcie dotyka najwyżej jednego bitu
/// wykładnika. Wartości najmniejsze schodzą przy tym do zakresu subnormalnego i
/// tracą bit mantysy — to jedyna strata tej ścieżki i jest mierzona w teście.
///
/// Zwraca bajty E4M3 wiersz-major oraz jedną skalę f32 na wiersz.
pub fn deepseek_fp8_to_row_scaled(
    weight: &[u8],
    scales: &[u8],
    rows: usize,
    cols: usize,
    tile: usize,
) -> Result<(Vec<u8>, Vec<f32>)> {
    if tile == 0 {
        return Err(fmt_err("fp8: kafel skali nie może być zerowy"));
    }
    if weight.len() != rows * cols {
        return Err(fmt_err(format!(
            "fp8: waga ma {} bajtów, oczekiwano {} dla [{rows}, {cols}]",
            weight.len(),
            rows * cols
        )));
    }
    let scale_rows = rows.div_ceil(tile);
    let scale_cols = cols.div_ceil(tile);
    if scales.len() != scale_rows * scale_cols {
        return Err(fmt_err(format!(
            "fp8: skale mają {} bajtów, oczekiwano {scale_rows}x{scale_cols}",
            scales.len()
        )));
    }

    let mut out = vec![0u8; rows * cols];
    let mut row_scales = vec![0f32; rows];
    for row in 0..rows {
        let tile_row = &scales[(row / tile) * scale_cols..(row / tile + 1) * scale_cols];
        let mut max_code = 0u8;
        for &code in tile_row {
            if code == 0xFF {
                return Err(fmt_err(format!(
                    "fp8: skala kafla w wierszu {row} jest NaN"
                )));
            }
            max_code = max_code.max(code);
        }
        row_scales[row] = 2f32.powi(max_code as i32 - 127);
        for col in 0..cols {
            let shift = max_code - tile_row[col / tile];
            out[row * cols + col] = e4m3_shift_down(weight[row * cols + col], shift)?;
        }
    }
    Ok((out, row_scales))
}

/// Dzieli wartość E4M3 przez `2^shift`, z zaokrągleniem do najbliższej parzystej
/// przy zejściu w zakres subnormalny.
fn e4m3_shift_down(byte: u8, shift: u8) -> Result<u8> {
    if shift == 0 {
        return Ok(byte);
    }
    let sign = byte & 0x80;
    let exponent = ((byte >> 3) & 0x0F) as i32;
    let mantissa = (byte & 0x07) as i32;
    if exponent == 0x0F && mantissa == 0x07 {
        return Err(fmt_err("fp8: waga jest NaN"));
    }
    if exponent == 0 && mantissa == 0 {
        return Ok(sign);
    }
    let shifted = exponent - shift as i32;
    if shifted >= 1 {
        return Ok(sign | ((shifted as u8) << 3) | mantissa as u8);
    }
    // Zejście poniżej zakresu normalnego: mantysa dostaje wiodącą jedynkę i
    // jest przesuwana, z zaokrągleniem do najbliższej parzystej.
    let significand = if exponent == 0 {
        mantissa
    } else {
        mantissa + 8
    };
    let drop = 1 - shifted;
    if drop > 4 {
        return Ok(sign);
    }
    let half = 1 << (drop - 1);
    let rounded = (significand + half - i32::from(significand & (2 * half - 1) == half)) >> drop;
    Ok(sign | rounded.min(7) as u8)
}

/// Nazwy trójki tensorów jednego eksperta NVFP4 w checkpoincie DeepSeek V4.
///
/// Układ różni się od compressed-tensors dwiema rzeczami, z których druga jest
/// pułapką: waga nazywa się `weight` (nie `weight_packed`), a skala globalna
/// `weight_scale_2` MNOŻY wynik, podczas gdy `weight_global_scale`
/// compressed-tensors przez wynik DZIELI. Podstawienie jednego pod drugie nie
/// wywala się — daje wagi rzędu 10^6 zamiast 10^-2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepseekNvFp4Names {
    pub packed: String,
    pub scale: String,
    pub global_scale: String,
}

impl DeepseekNvFp4Names {
    pub fn for_weight(weight_name: &str) -> Result<Self> {
        let base = weight_name.strip_suffix(".weight").ok_or_else(|| {
            fmt_err(format!(
                "nvfp4: '{weight_name}' nie jest nazwą tensora '.weight'"
            ))
        })?;
        Ok(Self {
            packed: weight_name.to_string(),
            scale: format!("{base}.weight_scale"),
            global_scale: format!("{base}.weight_scale_2"),
        })
    }
}

/// Jeden ekspert NVFP4 DeepSeeka przepakowany do jednobuforowego układu GGUF.
///
/// `output_scale` to `weight_scale_2` przeniesione bez zmian — kernele NVFP4
/// mnożą przez nie wynik GEMV, więc konwencja się zgadza.
pub struct DeepseekNvFp4Weight {
    pub blocks: Vec<u8>,
    pub output_scale: f32,
    pub rows: usize,
    pub cols: usize,
}

/// Przepakowuje eksperta DeepSeeka: `[rows, cols/2]` bajtów pakietu, skale
/// E4M3 co 16 elementów i skalarną skalę globalną.
pub fn deepseek_expert_to_gguf(
    packed: &[u8],
    packed_shape: &[usize],
    scales: &[u8],
    global_scale: f32,
) -> Result<DeepseekNvFp4Weight> {
    if packed_shape.len() != 2 {
        return Err(fmt_err(format!(
            "nvfp4: ekspert DeepSeeka ma kształt {packed_shape:?}, oczekiwano dwóch wymiarów"
        )));
    }
    if !global_scale.is_finite() || global_scale <= 0.0 {
        return Err(fmt_err(format!(
            "nvfp4: weight_scale_2 = {global_scale} nie jest dodatnią liczbą skończoną"
        )));
    }
    let rows = packed_shape[0];
    let cols = packed_shape[1] * 2;
    let blocks = nvfp4_ct_to_gguf_blocks(packed, scales, rows, cols)?;
    Ok(DeepseekNvFp4Weight {
        blocks,
        output_scale: global_scale,
        rows,
        cols,
    })
}

/// Nibble elementu `index` w wierszu spakowanym po dwa na bajt.
fn nibble(packed_row: &[u8], index: usize) -> u8 {
    let byte = packed_row[index / 2];
    if index % 2 == 0 {
        byte & 0x0F
    } else {
        byte >> 4
    }
}

#[cfg(test)]
mod tests {
    /// Przepakowanie do bloków GGUF musi dać TE SAME liczby co dekod wprost.
    ///
    /// Obie drogi czytają ten sam tensor: jedna dekoduje kody i skale osobno,
    /// druga składa je w blok GGUF i dekoduje go. Jeżeli się rozjeżdżają, to
    /// model wczytany przez konwersję liczy innymi wagami niż ten sam model
    /// wczytany wprost — a różnica rzędu procenta wygląda w wyniku jak
    /// zaokrąglenie i nie jest nim.
    #[test]
    fn the_gguf_repack_decodes_to_the_same_numbers() {
        let (rows, cols) = (2usize, 128usize);
        let packed: Vec<u8> = (0..rows * cols / 2)
            .map(|i| ((i * 37 + 11) % 256) as u8)
            .collect();
        // Skale E4M3 bez bitu znaku i bez NaN — konwersja odrzuca oba jawnie.
        let scales: Vec<u8> = (0..rows * cols / 16)
            .map(|i| (0x30 + (i % 16)) as u8)
            .collect();
        let global = 0.125f32;

        let direct = dequantize_nvfp4(&packed, &scales, global, rows, cols, 16).expect("wprost");
        let blocks = nvfp4_ct_to_gguf_blocks(&packed, &scales, rows, cols).expect("przepakowanie");
        let repacked = crate::dequant::dequantize_to_f32(
            forge_types::DType::U8,
            forge_types::QuantKind::NVFP4Gguf,
            &blocks,
            rows * cols,
        )
        .expect("dekod bloków");

        // Skalar tensora nie mieści się w bloku GGUF, więc dekod bloków go nie
        // zna — porównanie musi go dołożyć po tej stronie i to jest jedyna
        // dozwolona różnica. DZIELI, bo `weight_global_scale` jest dzielnikiem
        // użytym przy kwantyzacji; pomnożenie zamiast podzielenia daje rozjazd
        // o KWADRAT skalara i wygląda jak zepsuty konwerter, którym nie jest.
        let mut worst = (0usize, 0.0f32, 0.0f32, 0.0f32);
        for (i, (&a, &b)) in direct.iter().zip(&repacked).enumerate() {
            let scaled = b / global;
            let diff = (a - scaled).abs();
            if diff > worst.3 {
                worst = (i, a, scaled, diff);
            }
        }
        assert!(
            worst.3 == 0.0,
            "element {}: wprost {}, przez bloki {} (roznica {})",
            worst.0,
            worst.1,
            worst.2,
            worst.3
        );
    }

    use super::*;

    /// Przepakowanie musi być przestawieniem bajtów, nie przybliżeniem: te same
    /// wartości, tylko w układzie jednobuforowym. Porównanie idzie przez
    /// dekwantyzację obu układów i wymaga zgodności CO DO BITU.
    #[test]
    fn gguf_repack_dequantizes_identically() {
        let (rows, cols) = (5usize, 128usize);
        let packed: Vec<u8> = (0..rows * cols / 2)
            .map(|i| ((i * 37 + 11) % 256) as u8)
            .collect();
        // Skale dodatnie E4M3 (bit znaku zerowany).
        // Kody skali bez znaku i bez NaN (0x7F).
        let scales: Vec<u8> = (0..rows * cols / 16)
            .map(|i| (((i * 53 + 7) % 127) as u8).max(1))
            .collect();

        let repacked = nvfp4_ct_to_gguf_blocks(&packed, &scales, rows, cols).unwrap();
        assert_eq!(repacked.len(), rows * (cols / 64) * 36);
        let from_gguf = crate::dequant::dequantize_to_f32(
            forge_types::DType::U8,
            forge_types::QuantKind::NVFP4Gguf,
            &repacked,
            rows * cols,
        )
        .unwrap();

        // Referencja układu źródłowego: skala bloku razy wartość E2M1, bez
        // skali globalnej (ta wędruje osobno).
        let mut reference = vec![0f32; rows * cols];
        for row in 0..rows {
            for col in 0..cols {
                let byte = packed[row * cols / 2 + col / 2];
                let code = if col % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                let scale = f8e4m3_to_f32(scales[row * cols / 16 + col / 16]);
                reference[row * cols + col] = e2m1_to_f32(code) * scale;
            }
        }
        assert_eq!(from_gguf, reference, "przepakowanie zmieniło wartości");
    }

    /// Skala ujemna i NaN nie mają wiernej reprezentacji w układzie GGUF —
    /// muszą być błędem, a nie cicho obciętym bitem znaku ani cichym zerem.
    #[test]
    fn gguf_repack_rejects_unrepresentable_scales() {
        let (rows, cols) = (1usize, 64usize);
        let packed = vec![0u8; cols / 2];
        for bad in [0x80 | 0x30, 0x7F] {
            let mut scales = vec![1u8; cols / 16];
            scales[2] = bad;
            assert!(
                nvfp4_ct_to_gguf_blocks(&packed, &scales, rows, cols).is_err(),
                "skala 0x{bad:02X} powinna zostać odrzucona"
            );
        }
    }

    /// Dowód, że przepakowanie zachowuje WARTOŚCI, a nie tylko układ bajtów:
    /// oba dekodery muszą dać to samo dla każdej pary (kod skali, kod E2M1).
    /// Układy różnią się wewnętrznie — GGUF trzyma skalę jako UE4M3 połówkowe —
    /// więc zgodność wyniku jest własnością złożenia, nie oczywistością.
    #[test]
    fn gguf_repack_preserves_every_scale_and_value_code() {
        for scale_code in 1u8..0x7F {
            let cols = 64usize;
            // Jeden wiersz, wszystkie 16 kodów E2M1 po kolei.
            let packed: Vec<u8> = (0..cols / 2)
                .map(|i| {
                    let lo = (2 * i % 16) as u8;
                    let hi = ((2 * i + 1) % 16) as u8;
                    lo | (hi << 4)
                })
                .collect();
            let scales = vec![scale_code; cols / 16];
            let repacked = nvfp4_ct_to_gguf_blocks(&packed, &scales, 1, cols).unwrap();
            let got = forge_types_dequant(&repacked, cols);
            for col in 0..cols {
                let code = if col % 2 == 0 {
                    packed[col / 2] & 0x0F
                } else {
                    packed[col / 2] >> 4
                };
                let want = e2m1_to_f32(code) * f8e4m3_to_f32(scale_code);
                assert_eq!(
                    got[col], want,
                    "kod skali 0x{scale_code:02X}, element {col}, kod E2M1 {code}"
                );
            }
        }
    }

    fn forge_types_dequant(blocks: &[u8], numel: usize) -> Vec<f32> {
        crate::dequant::dequantize_to_f32(
            forge_types::DType::U8,
            forge_types::QuantKind::NVFP4Gguf,
            blocks,
            numel,
        )
        .unwrap()
    }

    /// Układ GGUF ma bloki po 64 elementy; wiersz niebędący ich wielokrotnością
    /// nie da się zapisać bez dopełnienia zmieniającego kształt.
    #[test]
    fn gguf_repack_requires_full_blocks() {
        assert!(nvfp4_ct_to_gguf_blocks(&vec![0u8; 24], &vec![1u8; 3], 1, 48).is_err());
    }

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
    fn nvfp4_ct_s0_sprawdza_wszystkie_kody_e4m3() {
        for raw in 0u8..=u8::MAX {
            match nvfp4_ct_s0_from_e4m3(raw) {
                Some(encoded) => {
                    assert!(raw <= 0x7e);
                    assert_eq!(
                        nvfp4_ct_s0_to_f32(encoded),
                        f8e4m3_to_f32(raw) * 128.0,
                        "kod E4M3 0x{raw:02x}"
                    );
                }
                None => assert!(raw >= 0x7f),
            }
        }
        assert_eq!(
            [
                nvfp4_ct_s0_from_e4m3(0x01).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x02).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x03).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x04).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x05).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x06).unwrap(),
                nvfp4_ct_s0_from_e4m3(0x07).unwrap(),
            ],
            [0x68, 0x70, 0x74, 0x78, 0x7a, 0x7c, 0x7e]
        );
        assert!(nvfp4_ct_s0_to_f32(NVFP4_CT_S0_NAN).is_nan());
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
