// ===== File: mlx.rs — MLX quantization: config parsing and affine decoding =====
//
// MLX checkpoints are safetensors plus a `quantization` block in config.json.
// Four modes exist: affine, mxfp4, mxfp8 and nvfp4. Three of them are formats
// this engine already decodes elsewhere; only `affine` is new, and it is
// structurally the same shape as Q4_1 — an unsigned integer per weight plus a
// scale and a bias shared by a group of consecutive elements along K.
//
// Nothing here is Apple specific. An MLX checkpoint decoded by this module runs
// on every backend, which is the point: the format is a loader concern, not a
// platform one.

use std::collections::{BTreeMap, BTreeSet};

use half::{bf16, f16};
use serde::Deserialize;

use forge_types::{ForgeError, Result};

use crate::arch::ModelDescriptor;

/// Quantization mode declared by an MLX checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MlxMode {
    Affine,
    Mxfp4,
    Mxfp8,
    Nvfp4,
}

/// The `quantization` block of an MLX config.json.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MlxQuantConfig {
    pub group_size: usize,
    pub bits: u32,
    #[serde(default = "default_mode")]
    pub mode: MlxMode,
}

fn default_mode() -> MlxMode {
    // Checkpoints produced before the mode field existed are all affine.
    MlxMode::Affine
}

impl MlxQuantConfig {
    /// Reads the quantization block from a parsed config.json. MLX writes the
    /// same block under two keys; either is accepted, and they must agree.
    pub fn from_config(config: &serde_json::Value) -> Result<Option<Self>> {
        let primary = config.get("quantization");
        let secondary = config.get("quantization_config");
        let raw = match primary.or(secondary) {
            Some(v) => v,
            None => return Ok(None),
        };
        let parsed: MlxQuantConfig = serde_json::from_value(raw.clone())
            .map_err(|e| ForgeError::Format(format!("blok quantization: {e}")))?;
        parsed.validate()?;
        Ok(Some(parsed))
    }

    fn validate(&self) -> Result<()> {
        if !matches!(self.bits, 2 | 3 | 4 | 5 | 6 | 8) {
            return Err(ForgeError::Format(format!(
                "MLX: nieobsługiwana liczba bitów {}",
                self.bits
            )));
        }
        if self.group_size == 0 || self.group_size % 8 != 0 {
            return Err(ForgeError::Format(format!(
                "MLX: group_size {} musi być dodatnią wielokrotnością 8",
                self.group_size
            )));
        }
        // A bit width that does not divide 32 would split a weight across two
        // words; MLX does not emit such a layout and guessing one would be a
        // silent wrong answer rather than an error.
        if 32 % self.bits != 0 {
            return Err(ForgeError::Format(format!(
                "MLX: liczba bitów {} nie dzieli słowa 32-bitowego",
                self.bits
            )));
        }
        Ok(())
    }

    /// Weights packed per 32-bit word.
    pub fn per_word(&self) -> usize {
        (32 / self.bits) as usize
    }
}

/// Scales and zero points as stored in the file. The element type is not fixed
/// by the format: a text checkpoint converted by mlx-lm carries them in bf16,
/// while mlx-whisper writes f16. Hard-coding either one silently rejects half
/// the ecosystem, so the type travels with the data.
#[derive(Debug, Clone, Copy)]
pub enum MlxParams<'a> {
    Bf16(&'a [bf16]),
    F16(&'a [f16]),
}

impl MlxParams<'_> {
    pub fn len(&self) -> usize {
        match self {
            MlxParams::Bf16(v) => v.len(),
            MlxParams::F16(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, index: usize) -> f32 {
        match self {
            MlxParams::Bf16(v) => v[index].to_f32(),
            MlxParams::F16(v) => v[index].to_f32(),
        }
    }
}

/// One quantized MLX tensor as it sits in the file.
pub struct MlxAffineTensor<'a> {
    pub packed: &'a [u32],
    pub scales: MlxParams<'a>,
    pub biases: MlxParams<'a>,
    pub rows: usize,
    /// Logical column count, i.e. weights per row after unpacking.
    pub cols: usize,
}

impl MlxAffineTensor<'_> {
    fn validate(&self, cfg: &MlxQuantConfig) -> Result<()> {
        if self.cols % cfg.group_size != 0 {
            return Err(ForgeError::Format(format!(
                "MLX: {} kolumn nie dzieli się na grupy po {}",
                self.cols, cfg.group_size
            )));
        }
        let groups = self.cols / cfg.group_size;
        let want_packed = self.rows * self.cols / cfg.per_word();
        let want_params = self.rows * groups;
        if self.packed.len() != want_packed {
            return Err(ForgeError::Format(format!(
                "MLX: oczekiwano {want_packed} słów upakowanych, jest {}",
                self.packed.len()
            )));
        }
        if self.scales.len() != want_params || self.biases.len() != want_params {
            return Err(ForgeError::Format(format!(
                "MLX: oczekiwano {want_params} skal i przesunięć, jest {} i {}",
                self.scales.len(),
                self.biases.len()
            )));
        }
        Ok(())
    }
}

/// Decodes an MLX affine tensor into f32.
///
/// `w = q * scale + bias`, where `q` is the unsigned integer stored in the
/// packed word and `scale`/`bias` belong to the group of `group_size`
/// consecutive weights along the row. The bias is ADDED — the same field in
/// DeepSeek multiplies and in compressed-tensors divides, and substituting one
/// convention for another produces weights off by orders of magnitude with no
/// error at all, which is why the golden test compares against MLX itself.
pub fn dequantize_affine(
    tensor: &MlxAffineTensor<'_>,
    cfg: &MlxQuantConfig,
    out: &mut [f32],
) -> Result<()> {
    tensor.validate(cfg)?;
    if out.len() != tensor.rows * tensor.cols {
        return Err(ForgeError::Format(format!(
            "MLX: bufor wyjściowy ma {} elementów, potrzeba {}",
            out.len(),
            tensor.rows * tensor.cols
        )));
    }

    let per_word = cfg.per_word();
    let bits = cfg.bits;
    let mask: u32 = if bits == 32 {
        u32::MAX
    } else {
        (1u32 << bits) - 1
    };
    let words_per_row = tensor.cols / per_word;
    let groups_per_row = tensor.cols / cfg.group_size;

    for row in 0..tensor.rows {
        let words = &tensor.packed[row * words_per_row..(row + 1) * words_per_row];
        let param_base = row * groups_per_row;
        let dst = &mut out[row * tensor.cols..(row + 1) * tensor.cols];

        for (w_idx, word) in words.iter().enumerate() {
            let base = w_idx * per_word;
            for lane in 0..per_word {
                // Least significant bits hold the earliest weight of the word.
                let q = (word >> (lane as u32 * bits)) & mask;
                let col = base + lane;
                let g = param_base + col / cfg.group_size;
                dst[col] = q as f32 * tensor.scales.get(g) + tensor.biases.get(g);
            }
        }
    }
    Ok(())
}

/// Which of the three tensors an MLX name refers to. A quantized weight is
/// stored as a triple; the architecture registry names only the `.weight`, so
/// the other two would otherwise look like tensors nobody claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlxComponent {
    Weight,
    Scales,
    Biases,
}

/// Splits an MLX tensor name into the canonical name used by the architecture
/// registry and the component it carries.
pub fn split_component(name: &str) -> (String, MlxComponent) {
    for (suffix, component) in [
        (".scales", MlxComponent::Scales),
        (".biases", MlxComponent::Biases),
    ] {
        if let Some(base) = name.strip_suffix(suffix) {
            return (format!("{base}.weight"), component);
        }
    }
    (name.to_string(), MlxComponent::Weight)
}

/// Result of matching a checkpoint's tensor list against an architecture
/// descriptor. Both directions matter: a tensor nobody claims means the map is
/// incomplete, and a role with no tensor means the loader would reach for
/// something that is not there.
#[derive(Debug, Default)]
pub struct MlxLayout {
    /// Canonical names present with a complete `.weight` + `.scales` + `.biases` triple.
    pub quantized: Vec<String>,
    /// Canonical names present as a single unquantized tensor (norms).
    pub plain: Vec<String>,
    /// File tensors the descriptor does not claim.
    pub unknown: Vec<String>,
    /// Descriptor roles with no tensor in the file.
    pub missing: Vec<String>,
    /// Quantized weights whose triple is incomplete — a broken checkpoint that
    /// would otherwise decode to silent garbage.
    pub partial: Vec<String>,
}

impl MlxLayout {
    /// True when every tensor resolved and every declared role was found.
    pub fn is_complete(&self) -> bool {
        self.unknown.is_empty() && self.missing.is_empty() && self.partial.is_empty()
    }

    pub fn tensor_count(&self) -> usize {
        self.quantized.len() * 3 + self.plain.len()
    }
}

/// Matches a checkpoint's tensor names against a descriptor built from
/// config.json. Pure name work: no file is read here, so the same function
/// serves the loader and the validation gate.
pub fn map_checkpoint<'a>(
    desc: &ModelDescriptor,
    names: impl Iterator<Item = &'a str>,
) -> MlxLayout {
    let mut expected: BTreeSet<&str> = BTreeSet::new();
    for name in desc.globals.values() {
        expected.insert(name.as_str());
    }
    for layer in &desc.layers {
        for name in layer.values() {
            expected.insert(name.as_str());
        }
    }

    // Component presence per canonical name, in file order stability.
    let mut seen: BTreeMap<String, [bool; 3]> = BTreeMap::new();
    let mut out = MlxLayout::default();

    for name in names {
        let (canon, component) = split_component(name);
        if !expected.contains(canon.as_str()) {
            out.unknown.push(name.to_string());
            continue;
        }
        let slot = seen.entry(canon).or_insert([false; 3]);
        slot[component as usize] = true;
    }

    for (canon, [weight, scales, biases]) in seen {
        match (weight, scales, biases) {
            (true, true, true) => out.quantized.push(canon),
            (true, false, false) => out.plain.push(canon),
            _ => out.partial.push(canon),
        }
    }

    let resolved: BTreeSet<&str> = out
        .quantized
        .iter()
        .chain(out.plain.iter())
        .chain(out.partial.iter())
        .map(|s| s.as_str())
        .collect();
    for name in expected {
        if !resolved.contains(name) {
            out.missing.push(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(bits: u32, group_size: usize) -> MlxQuantConfig {
        MlxQuantConfig {
            group_size,
            bits,
            mode: MlxMode::Affine,
        }
    }

    #[test]
    fn config_rejects_shapes_it_cannot_decode() {
        assert!(cfg(7, 64).validate().is_err(), "7 bitów nie dzieli słowa");
        assert!(cfg(4, 0).validate().is_err());
        assert!(cfg(4, 12).validate().is_err());
        assert!(cfg(4, 64).validate().is_ok());
        assert!(cfg(8, 32).validate().is_ok());
    }

    #[test]
    fn mode_defaults_to_affine_and_parses_all_four() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"quantization":{"group_size":64,"bits":4}}"#).unwrap();
        let c = MlxQuantConfig::from_config(&v).unwrap().unwrap();
        assert_eq!(c.mode, MlxMode::Affine);

        for (name, want) in [
            ("affine", MlxMode::Affine),
            ("mxfp4", MlxMode::Mxfp4),
            ("mxfp8", MlxMode::Mxfp8),
            ("nvfp4", MlxMode::Nvfp4),
        ] {
            let src = format!(r#"{{"quantization":{{"group_size":32,"bits":4,"mode":"{name}"}}}}"#);
            let v: serde_json::Value = serde_json::from_str(&src).unwrap();
            assert_eq!(MlxQuantConfig::from_config(&v).unwrap().unwrap().mode, want);
        }
    }

    #[test]
    fn decodes_a_hand_built_word() {
        // One group of 8 weights, 4 bits each, values 0..7 in ascending lanes.
        let packed = [0x7654_3210u32];
        let scales = [bf16::from_f32(2.0)];
        let biases = [bf16::from_f32(-1.0)];
        let t = MlxAffineTensor {
            packed: &packed,
            scales: MlxParams::Bf16(&scales),
            biases: MlxParams::Bf16(&biases),
            rows: 1,
            cols: 8,
        };
        let mut out = [0f32; 8];
        dequantize_affine(&t, &cfg(4, 8), &mut out).unwrap();
        for (i, v) in out.iter().enumerate() {
            assert_eq!(*v, i as f32 * 2.0 - 1.0, "lane {i}");
        }
    }

    #[test]
    fn a_linear_bias_is_not_mistaken_for_quantization_zero_points() {
        // Live hazard: mlx-whisper stores BOTH `attn.out.bias` (the linear
        // bias, one vector) and `attn.out.biases` (the affine zero points, one
        // per group). Singular and plural differ by one letter and a decoder
        // that strips the wrong suffix reads the wrong tensor without any error.
        let (canon, comp) = split_component("encoder.blocks.0.attn.out.biases");
        assert_eq!(canon, "encoder.blocks.0.attn.out.weight");
        assert_eq!(comp, MlxComponent::Biases);

        let (canon, comp) = split_component("encoder.blocks.0.attn.out.bias");
        assert_eq!(canon, "encoder.blocks.0.attn.out.bias");
        assert_eq!(comp, MlxComponent::Weight);
    }

    #[test]
    fn scales_may_be_f16_or_bf16() {
        // mlx-lm writes bf16, mlx-whisper writes f16; both must decode.
        let packed = [0x7654_3210u32];
        let mut out = [0f32; 8];

        let s16 = [f16::from_f32(2.0)];
        let b16 = [f16::from_f32(-1.0)];
        let t = MlxAffineTensor {
            packed: &packed,
            scales: MlxParams::F16(&s16),
            biases: MlxParams::F16(&b16),
            rows: 1,
            cols: 8,
        };
        dequantize_affine(&t, &cfg(4, 8), &mut out).unwrap();
        assert_eq!(out[3], 3.0 * 2.0 - 1.0);

        let sb = [bf16::from_f32(2.0)];
        let bb = [bf16::from_f32(-1.0)];
        let t = MlxAffineTensor {
            packed: &packed,
            scales: MlxParams::Bf16(&sb),
            biases: MlxParams::Bf16(&bb),
            rows: 1,
            cols: 8,
        };
        dequantize_affine(&t, &cfg(4, 8), &mut out).unwrap();
        assert_eq!(out[3], 3.0 * 2.0 - 1.0);
    }

    #[test]
    fn rejects_mismatched_lengths_instead_of_reading_out_of_bounds() {
        let packed = [0u32; 4];
        let scales = [bf16::ONE; 1];
        let biases = [bf16::ZERO; 1];
        let t = MlxAffineTensor {
            packed: &packed,
            scales: MlxParams::Bf16(&scales),
            biases: MlxParams::Bf16(&biases),
            rows: 1,
            cols: 64,
        };
        let mut out = [0f32; 64];
        assert!(dequantize_affine(&t, &cfg(4, 64), &mut out).is_err());
    }
}
