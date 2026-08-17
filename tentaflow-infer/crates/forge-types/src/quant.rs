// ===== File: quant.rs — quantization formats (GGUF quants, GPTQ/AWQ, FP8, NVFP4) =====

use serde::{Deserialize, Serialize};

/// Block-quantized weight formats. Each variant describes an on-disk /
/// in-memory block layout; dequant references live in `forge-formats`,
/// fused GPU dequant in `forge-kernels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantKind {
    /// Not quantized — plain `DType` elements.
    None,
    // GGML legacy blocks (32 elements)
    Q4_0,
    Q4_1,
    Q5_0,
    Q5_1,
    Q8_0,
    Q8_1,
    // K-quants (256-element superblocks)
    Q2K,
    Q3K,
    Q4K,
    Q5K,
    Q6K,
    Q8K,
    // I-quants
    IQ1S,
    IQ1M,
    IQ2XXS,
    IQ2XS,
    IQ2S,
    IQ3XXS,
    IQ3S,
    IQ4NL,
    IQ4XS,
    /// MXFP4 (OCP microscaling): FP4 e2m1 + shared E8M0 scale per 32.
    MXFP4,
    /// NVFP4 in compressed-tensors layout: FP4 e2m1 packed pairs in a separate
    /// packed tensor + FP8-E4M3 scale per 16-element block + one F32 tensor
    /// scale. Scales live in sibling tensors, so `block_bytes` covers packed
    /// data only.
    NVFP4,
    /// NVFP4 in GGML/GGUF self-contained layout: 64-element block carrying
    /// four FP8-E4M3 per-16 scales + 32 packed e2m1 bytes inline.
    NVFP4Gguf,
    /// GPTQ INT4 with group scales/zeros; group size is model metadata.
    Gptq4 {
        group: u16,
    },
    /// AWQ INT4 with group scales/zeros; group size is model metadata.
    Awq4 {
        group: u16,
    },
    /// Per-tensor/per-channel FP8 (compressed-tensors "fp8" scheme); scales in
    /// sibling tensors.
    Fp8Dynamic,
}

impl QuantKind {
    /// Elements per quantization block (1 for unquantized / per-channel schemes).
    pub const fn block_elems(self) -> usize {
        match self {
            QuantKind::None | QuantKind::Fp8Dynamic => 1,
            QuantKind::Q4_0
            | QuantKind::Q4_1
            | QuantKind::Q5_0
            | QuantKind::Q5_1
            | QuantKind::Q8_0
            | QuantKind::Q8_1
            | QuantKind::IQ4NL
            | QuantKind::MXFP4 => 32,
            QuantKind::NVFP4 => 16,
            QuantKind::NVFP4Gguf => 64,
            QuantKind::Gptq4 { group } | QuantKind::Awq4 { group } => group as usize,
            _ => 256,
        }
    }

    /// Bytes per block for GGML-style formats where the layout is fixed.
    /// Schemes with separate scale tensors (NVFP4/GPTQ/AWQ/FP8) return the
    /// packed-data bytes only.
    pub const fn block_bytes(self) -> usize {
        match self {
            QuantKind::None => 0,
            QuantKind::Fp8Dynamic => 1,
            QuantKind::Q4_0 => 18,
            QuantKind::Q4_1 => 20,
            QuantKind::Q5_0 => 22,
            QuantKind::Q5_1 => 24,
            QuantKind::Q8_0 => 34,
            QuantKind::Q8_1 => 36,
            QuantKind::Q2K => 84,
            QuantKind::Q3K => 110,
            QuantKind::Q4K => 144,
            QuantKind::Q5K => 176,
            QuantKind::Q6K => 210,
            QuantKind::Q8K => 292,
            QuantKind::IQ1S => 50,
            QuantKind::IQ1M => 56,
            QuantKind::IQ2XXS => 66,
            QuantKind::IQ2XS => 74,
            QuantKind::IQ2S => 82,
            QuantKind::IQ3XXS => 98,
            QuantKind::IQ3S => 110,
            QuantKind::IQ4NL => 18,
            QuantKind::IQ4XS => 136,
            QuantKind::MXFP4 => 17,
            QuantKind::NVFP4 => 8,
            QuantKind::NVFP4Gguf => 36,
            // INT4 packed pairs: group/2 data bytes; scales/zeros are siblings.
            QuantKind::Gptq4 { group } | QuantKind::Awq4 { group } => (group as usize) / 2,
        }
    }
}
