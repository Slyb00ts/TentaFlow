// ===== File: registry.rs — PTX artifact loading (embedded defaults + dir override) =====

use std::collections::HashMap;
use std::path::Path;

use forge_hal::{Device, KernelHandle, Module};
use forge_types::{ForgeError, Result};
use serde::Deserialize;

/// Parsed kernels/build/<arch>/manifest.json.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub arch: String,
    pub kernels: HashMap<String, ManifestEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub entry: String,
}

/// One embedded (name, ptx) pair for the default artifact set. The list is
/// kept in lockstep with build_kernels.mojo registrations; a mismatch against
/// the manifest fails loudly at load time rather than at first launch.
struct EmbeddedArtifact {
    name: &'static str,
    ptx: &'static [u8],
}

macro_rules! embedded {
    ($($name:literal),+ $(,)?) => {
        &[$(EmbeddedArtifact {
            name: $name,
            ptx: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../kernels/mojo/build/sm_89/",
                $name,
                ".ptx"
            )),
        }),+]
    };
}

const EMBEDDED_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/manifest.json"
));

/// The single raw-CUDA kernel family (ADR-0001 exception): the int8 MMQ prefill
/// GEMM, compiled by nvcc to a committed cubin (kernels/cuda/gemm_i8mma.cu,
/// docs/CODEGEN_PROOF.md). It loads through the SAME `load_module`/cuModuleLoadData
/// path as the Mojo PTX, but is embedded separately so it never has to appear in
/// the Mojo-owned manifest.json. Entry names are the `extern "C"` symbols; the
/// registry key mirrors the Mojo naming (`gemm_{q}_i8mma[_bn64]`) with a `_cuda`
/// suffix so the launcher can select the backend.
const EMBEDDED_CUDA_CUBIN_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/gemm_i8mma_cuda.cubin"
));

/// (registry key, cubin entry symbol) for every kernel in the CUDA cubin.
const CUDA_CUBIN_ENTRIES: &[(&str, &str)] = &[
    ("gemm_q4_k_i8mma_cuda", "forge_gemm_q4_k_i8mma_cuda"),
    ("gemm_q4_k_i8mma_cuda_bn64", "forge_gemm_q4_k_i8mma_cuda_bn64"),
    ("gemm_q8_0_i8mma_cuda", "forge_gemm_q8_0_i8mma_cuda"),
    ("gemm_q8_0_i8mma_cuda_bn64", "forge_gemm_q8_0_i8mma_cuda_bn64"),
];

/// W4A8 (int4-weight x int8-activation) prefill GEMM cubin (kernels/cuda/
/// w4a8_gemm.cu; QServe dense_kernel0, ADR-0001 exception). Non-default: routed
/// only under `FORGE_GEMM=w4a8`; the committed CUDA MMQ stays the default path.
const EMBEDDED_CUDA_CUBIN_W4A8_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/w4a8_gemm_cuda.cubin"
));

/// (registry key, cubin entry symbol) for each W4A8 CTA config (one per QServe
/// dispatch branch, selected in the launcher by token count / K).
const CUDA_W4A8_ENTRIES: &[(&str, &str)] = &[
    ("w4a8_gemm_m128", "forge_w4a8_gemm_m128"),
    ("w4a8_gemm_m64_ksm", "forge_w4a8_gemm_m64_ksm"),
    ("w4a8_gemm_m64_klg", "forge_w4a8_gemm_m64_klg"),
    ("w4a8_gemm_m32", "forge_w4a8_gemm_m32"),
    ("w4a8_quant_act", "forge_w4a8_quant_act_pertoken"),
];

/// Tensor-core flash-attention prefill cubin (kernels/cuda/fattn_prefill.cu;
/// ADR-0001 exception). f16 mma QK^T + online softmax + P·V over the paged KV
/// cache. Non-default: routed only under `FORGE_ATTN=fa`; the Mojo scalar
/// `attn_prefill` stays the default so the golden path is bit-exact.
const EMBEDDED_CUDA_CUBIN_FATTN_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/fattn_prefill_cuda.cubin"
));

/// (registry key, cubin entry symbol) for each flash-attention head_dim variant.
const CUDA_FATTN_ENTRIES: &[(&str, &str)] = &[
    ("attn_prefill_fa_f16_hd64", "forge_attn_prefill_fa_f16_hd64"),
    ("attn_prefill_fa_f16_hd128", "forge_attn_prefill_fa_f16_hd128"),
];

/// Vendored llama.cpp Q4_K + Q6_K MMQ (`mul_mat_q`) tensor-core GEMM cubin
/// (kernels/cuda/mmq_q4k.cu; ADR-0001 exception, MIT). ggml's ACTUAL compiled
/// Q4_K/Q6_K device code (~208 TOPS on the 4090; docs/CODEGEN_PROOF.md Exp 2),
/// writing f16 directly. Loaded through the same cuModuleLoadData path. DEFAULT
/// Q4_K/Q6_K prefill GEMM; `FORGE_GEMM=cuda|mojo` selects the other backends.
const EMBEDDED_CUDA_CUBIN_MMQ_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/mmq_q4k_cuda.cubin"
));

/// (registry key, cubin entry symbol) for the MMQ GEMM (one per weight type ×
/// mmq_x × need_check) plus the two q8_1 activation quant layouts (DS4 for Q4_K,
/// D4 for Q6_K). The GEMM writes f16 directly — no separate f32→f16 epilogue.
const CUDA_MMQ_ENTRIES: &[(&str, &str)] = &[
    ("mmq_q4k_x8_nc", "forge_mmq_q4k_x8_nc"),
    ("mmq_q4k_x8_c", "forge_mmq_q4k_x8_c"),
    ("mmq_q4k_x16_nc", "forge_mmq_q4k_x16_nc"),
    ("mmq_q4k_x16_c", "forge_mmq_q4k_x16_c"),
    ("mmq_q4k_x24_nc", "forge_mmq_q4k_x24_nc"),
    ("mmq_q4k_x24_c", "forge_mmq_q4k_x24_c"),
    ("mmq_q4k_x32_nc", "forge_mmq_q4k_x32_nc"),
    ("mmq_q4k_x32_c", "forge_mmq_q4k_x32_c"),
    ("mmq_q4k_x40_nc", "forge_mmq_q4k_x40_nc"),
    ("mmq_q4k_x40_c", "forge_mmq_q4k_x40_c"),
    ("mmq_q4k_x48_nc", "forge_mmq_q4k_x48_nc"),
    ("mmq_q4k_x48_c", "forge_mmq_q4k_x48_c"),
    ("mmq_q4k_x56_nc", "forge_mmq_q4k_x56_nc"),
    ("mmq_q4k_x56_c", "forge_mmq_q4k_x56_c"),
    ("mmq_q4k_x64_nc", "forge_mmq_q4k_x64_nc"),
    ("mmq_q4k_x64_c", "forge_mmq_q4k_x64_c"),
    ("mmq_q4k_x72_nc", "forge_mmq_q4k_x72_nc"),
    ("mmq_q4k_x72_c", "forge_mmq_q4k_x72_c"),
    ("mmq_q4k_x80_nc", "forge_mmq_q4k_x80_nc"),
    ("mmq_q4k_x80_c", "forge_mmq_q4k_x80_c"),
    ("mmq_q4k_x88_nc", "forge_mmq_q4k_x88_nc"),
    ("mmq_q4k_x88_c", "forge_mmq_q4k_x88_c"),
    ("mmq_q4k_x96_nc", "forge_mmq_q4k_x96_nc"),
    ("mmq_q4k_x96_c", "forge_mmq_q4k_x96_c"),
    ("mmq_q4k_x104_nc", "forge_mmq_q4k_x104_nc"),
    ("mmq_q4k_x104_c", "forge_mmq_q4k_x104_c"),
    ("mmq_q4k_x112_nc", "forge_mmq_q4k_x112_nc"),
    ("mmq_q4k_x112_c", "forge_mmq_q4k_x112_c"),
    ("mmq_q4k_x120_nc", "forge_mmq_q4k_x120_nc"),
    ("mmq_q4k_x120_c", "forge_mmq_q4k_x120_c"),
    ("mmq_q4k_x128_nc", "forge_mmq_q4k_x128_nc"),
    ("mmq_q4k_x128_c", "forge_mmq_q4k_x128_c"),
    ("mmq_q6k_x8_nc", "forge_mmq_q6k_x8_nc"),
    ("mmq_q6k_x8_c", "forge_mmq_q6k_x8_c"),
    ("mmq_q6k_x16_nc", "forge_mmq_q6k_x16_nc"),
    ("mmq_q6k_x16_c", "forge_mmq_q6k_x16_c"),
    ("mmq_q6k_x24_nc", "forge_mmq_q6k_x24_nc"),
    ("mmq_q6k_x24_c", "forge_mmq_q6k_x24_c"),
    ("mmq_q6k_x32_nc", "forge_mmq_q6k_x32_nc"),
    ("mmq_q6k_x32_c", "forge_mmq_q6k_x32_c"),
    ("mmq_q6k_x40_nc", "forge_mmq_q6k_x40_nc"),
    ("mmq_q6k_x40_c", "forge_mmq_q6k_x40_c"),
    ("mmq_q6k_x48_nc", "forge_mmq_q6k_x48_nc"),
    ("mmq_q6k_x48_c", "forge_mmq_q6k_x48_c"),
    ("mmq_q6k_x56_nc", "forge_mmq_q6k_x56_nc"),
    ("mmq_q6k_x56_c", "forge_mmq_q6k_x56_c"),
    ("mmq_q6k_x64_nc", "forge_mmq_q6k_x64_nc"),
    ("mmq_q6k_x64_c", "forge_mmq_q6k_x64_c"),
    ("mmq_q6k_x72_nc", "forge_mmq_q6k_x72_nc"),
    ("mmq_q6k_x72_c", "forge_mmq_q6k_x72_c"),
    ("mmq_q6k_x80_nc", "forge_mmq_q6k_x80_nc"),
    ("mmq_q6k_x80_c", "forge_mmq_q6k_x80_c"),
    ("mmq_q6k_x88_nc", "forge_mmq_q6k_x88_nc"),
    ("mmq_q6k_x88_c", "forge_mmq_q6k_x88_c"),
    ("mmq_q6k_x96_nc", "forge_mmq_q6k_x96_nc"),
    ("mmq_q6k_x96_c", "forge_mmq_q6k_x96_c"),
    ("mmq_q6k_x104_nc", "forge_mmq_q6k_x104_nc"),
    ("mmq_q6k_x104_c", "forge_mmq_q6k_x104_c"),
    ("mmq_q6k_x112_nc", "forge_mmq_q6k_x112_nc"),
    ("mmq_q6k_x112_c", "forge_mmq_q6k_x112_c"),
    ("mmq_q6k_x120_nc", "forge_mmq_q6k_x120_nc"),
    ("mmq_q6k_x120_c", "forge_mmq_q6k_x120_c"),
    ("mmq_q6k_x128_nc", "forge_mmq_q6k_x128_nc"),
    ("mmq_q6k_x128_c", "forge_mmq_q6k_x128_c"),
    ("quantize_mmq_q8_1_ds4", "forge_quantize_mmq_q8_1_ds4"),
    ("quantize_mmq_q8_1_d4", "forge_quantize_mmq_q8_1_d4"),
    ("rmsnorm_q8_1_ds4", "forge_rmsnorm_q8_1_ds4"),
    ("rmsnorm_residual_q8_1_ds4", "forge_rmsnorm_residual_q8_1_ds4"),
    ("mmq_sk_q4k_x8_nc", "forge_mmq_sk_q4k_x8_nc"),
    ("mmq_fix_q4k_x8_nc", "forge_mmq_fix_q4k_x8_nc"),
    ("mmq_sk_q4k_x8_c", "forge_mmq_sk_q4k_x8_c"),
    ("mmq_fix_q4k_x8_c", "forge_mmq_fix_q4k_x8_c"),
    ("mmq_sk_q4k_x16_nc", "forge_mmq_sk_q4k_x16_nc"),
    ("mmq_fix_q4k_x16_nc", "forge_mmq_fix_q4k_x16_nc"),
    ("mmq_sk_q4k_x16_c", "forge_mmq_sk_q4k_x16_c"),
    ("mmq_fix_q4k_x16_c", "forge_mmq_fix_q4k_x16_c"),
    ("mmq_sk_q4k_x24_nc", "forge_mmq_sk_q4k_x24_nc"),
    ("mmq_fix_q4k_x24_nc", "forge_mmq_fix_q4k_x24_nc"),
    ("mmq_sk_q4k_x24_c", "forge_mmq_sk_q4k_x24_c"),
    ("mmq_fix_q4k_x24_c", "forge_mmq_fix_q4k_x24_c"),
    ("mmq_sk_q4k_x32_nc", "forge_mmq_sk_q4k_x32_nc"),
    ("mmq_fix_q4k_x32_nc", "forge_mmq_fix_q4k_x32_nc"),
    ("mmq_sk_q4k_x32_c", "forge_mmq_sk_q4k_x32_c"),
    ("mmq_fix_q4k_x32_c", "forge_mmq_fix_q4k_x32_c"),
    ("mmq_sk_q4k_x40_nc", "forge_mmq_sk_q4k_x40_nc"),
    ("mmq_fix_q4k_x40_nc", "forge_mmq_fix_q4k_x40_nc"),
    ("mmq_sk_q4k_x40_c", "forge_mmq_sk_q4k_x40_c"),
    ("mmq_fix_q4k_x40_c", "forge_mmq_fix_q4k_x40_c"),
    ("mmq_sk_q4k_x48_nc", "forge_mmq_sk_q4k_x48_nc"),
    ("mmq_fix_q4k_x48_nc", "forge_mmq_fix_q4k_x48_nc"),
    ("mmq_sk_q4k_x48_c", "forge_mmq_sk_q4k_x48_c"),
    ("mmq_fix_q4k_x48_c", "forge_mmq_fix_q4k_x48_c"),
    ("mmq_sk_q4k_x56_nc", "forge_mmq_sk_q4k_x56_nc"),
    ("mmq_fix_q4k_x56_nc", "forge_mmq_fix_q4k_x56_nc"),
    ("mmq_sk_q4k_x56_c", "forge_mmq_sk_q4k_x56_c"),
    ("mmq_fix_q4k_x56_c", "forge_mmq_fix_q4k_x56_c"),
    ("mmq_sk_q4k_x64_nc", "forge_mmq_sk_q4k_x64_nc"),
    ("mmq_fix_q4k_x64_nc", "forge_mmq_fix_q4k_x64_nc"),
    ("mmq_sk_q4k_x64_c", "forge_mmq_sk_q4k_x64_c"),
    ("mmq_fix_q4k_x64_c", "forge_mmq_fix_q4k_x64_c"),
    ("mmq_sk_q4k_x72_nc", "forge_mmq_sk_q4k_x72_nc"),
    ("mmq_fix_q4k_x72_nc", "forge_mmq_fix_q4k_x72_nc"),
    ("mmq_sk_q4k_x72_c", "forge_mmq_sk_q4k_x72_c"),
    ("mmq_fix_q4k_x72_c", "forge_mmq_fix_q4k_x72_c"),
    ("mmq_sk_q4k_x80_nc", "forge_mmq_sk_q4k_x80_nc"),
    ("mmq_fix_q4k_x80_nc", "forge_mmq_fix_q4k_x80_nc"),
    ("mmq_sk_q4k_x80_c", "forge_mmq_sk_q4k_x80_c"),
    ("mmq_fix_q4k_x80_c", "forge_mmq_fix_q4k_x80_c"),
    ("mmq_sk_q4k_x88_nc", "forge_mmq_sk_q4k_x88_nc"),
    ("mmq_fix_q4k_x88_nc", "forge_mmq_fix_q4k_x88_nc"),
    ("mmq_sk_q4k_x88_c", "forge_mmq_sk_q4k_x88_c"),
    ("mmq_fix_q4k_x88_c", "forge_mmq_fix_q4k_x88_c"),
    ("mmq_sk_q4k_x96_nc", "forge_mmq_sk_q4k_x96_nc"),
    ("mmq_fix_q4k_x96_nc", "forge_mmq_fix_q4k_x96_nc"),
    ("mmq_sk_q4k_x96_c", "forge_mmq_sk_q4k_x96_c"),
    ("mmq_fix_q4k_x96_c", "forge_mmq_fix_q4k_x96_c"),
    ("mmq_sk_q4k_x104_nc", "forge_mmq_sk_q4k_x104_nc"),
    ("mmq_fix_q4k_x104_nc", "forge_mmq_fix_q4k_x104_nc"),
    ("mmq_sk_q4k_x104_c", "forge_mmq_sk_q4k_x104_c"),
    ("mmq_fix_q4k_x104_c", "forge_mmq_fix_q4k_x104_c"),
    ("mmq_sk_q4k_x112_nc", "forge_mmq_sk_q4k_x112_nc"),
    ("mmq_fix_q4k_x112_nc", "forge_mmq_fix_q4k_x112_nc"),
    ("mmq_sk_q4k_x112_c", "forge_mmq_sk_q4k_x112_c"),
    ("mmq_fix_q4k_x112_c", "forge_mmq_fix_q4k_x112_c"),
    ("mmq_sk_q4k_x120_nc", "forge_mmq_sk_q4k_x120_nc"),
    ("mmq_fix_q4k_x120_nc", "forge_mmq_fix_q4k_x120_nc"),
    ("mmq_sk_q4k_x120_c", "forge_mmq_sk_q4k_x120_c"),
    ("mmq_fix_q4k_x120_c", "forge_mmq_fix_q4k_x120_c"),
    ("mmq_sk_q4k_x128_nc", "forge_mmq_sk_q4k_x128_nc"),
    ("mmq_fix_q4k_x128_nc", "forge_mmq_fix_q4k_x128_nc"),
    ("mmq_sk_q4k_x128_c", "forge_mmq_sk_q4k_x128_c"),
    ("mmq_fix_q4k_x128_c", "forge_mmq_fix_q4k_x128_c"),
    ("mmq_sk_q6k_x8_nc", "forge_mmq_sk_q6k_x8_nc"),
    ("mmq_fix_q6k_x8_nc", "forge_mmq_fix_q6k_x8_nc"),
    ("mmq_sk_q6k_x8_c", "forge_mmq_sk_q6k_x8_c"),
    ("mmq_fix_q6k_x8_c", "forge_mmq_fix_q6k_x8_c"),
    ("mmq_sk_q6k_x16_nc", "forge_mmq_sk_q6k_x16_nc"),
    ("mmq_fix_q6k_x16_nc", "forge_mmq_fix_q6k_x16_nc"),
    ("mmq_sk_q6k_x16_c", "forge_mmq_sk_q6k_x16_c"),
    ("mmq_fix_q6k_x16_c", "forge_mmq_fix_q6k_x16_c"),
    ("mmq_sk_q6k_x24_nc", "forge_mmq_sk_q6k_x24_nc"),
    ("mmq_fix_q6k_x24_nc", "forge_mmq_fix_q6k_x24_nc"),
    ("mmq_sk_q6k_x24_c", "forge_mmq_sk_q6k_x24_c"),
    ("mmq_fix_q6k_x24_c", "forge_mmq_fix_q6k_x24_c"),
    ("mmq_sk_q6k_x32_nc", "forge_mmq_sk_q6k_x32_nc"),
    ("mmq_fix_q6k_x32_nc", "forge_mmq_fix_q6k_x32_nc"),
    ("mmq_sk_q6k_x32_c", "forge_mmq_sk_q6k_x32_c"),
    ("mmq_fix_q6k_x32_c", "forge_mmq_fix_q6k_x32_c"),
    ("mmq_sk_q6k_x40_nc", "forge_mmq_sk_q6k_x40_nc"),
    ("mmq_fix_q6k_x40_nc", "forge_mmq_fix_q6k_x40_nc"),
    ("mmq_sk_q6k_x40_c", "forge_mmq_sk_q6k_x40_c"),
    ("mmq_fix_q6k_x40_c", "forge_mmq_fix_q6k_x40_c"),
    ("mmq_sk_q6k_x48_nc", "forge_mmq_sk_q6k_x48_nc"),
    ("mmq_fix_q6k_x48_nc", "forge_mmq_fix_q6k_x48_nc"),
    ("mmq_sk_q6k_x48_c", "forge_mmq_sk_q6k_x48_c"),
    ("mmq_fix_q6k_x48_c", "forge_mmq_fix_q6k_x48_c"),
    ("mmq_sk_q6k_x56_nc", "forge_mmq_sk_q6k_x56_nc"),
    ("mmq_fix_q6k_x56_nc", "forge_mmq_fix_q6k_x56_nc"),
    ("mmq_sk_q6k_x56_c", "forge_mmq_sk_q6k_x56_c"),
    ("mmq_fix_q6k_x56_c", "forge_mmq_fix_q6k_x56_c"),
    ("mmq_sk_q6k_x64_nc", "forge_mmq_sk_q6k_x64_nc"),
    ("mmq_fix_q6k_x64_nc", "forge_mmq_fix_q6k_x64_nc"),
    ("mmq_sk_q6k_x64_c", "forge_mmq_sk_q6k_x64_c"),
    ("mmq_fix_q6k_x64_c", "forge_mmq_fix_q6k_x64_c"),
    ("mmq_sk_q6k_x72_nc", "forge_mmq_sk_q6k_x72_nc"),
    ("mmq_fix_q6k_x72_nc", "forge_mmq_fix_q6k_x72_nc"),
    ("mmq_sk_q6k_x72_c", "forge_mmq_sk_q6k_x72_c"),
    ("mmq_fix_q6k_x72_c", "forge_mmq_fix_q6k_x72_c"),
    ("mmq_sk_q6k_x80_nc", "forge_mmq_sk_q6k_x80_nc"),
    ("mmq_fix_q6k_x80_nc", "forge_mmq_fix_q6k_x80_nc"),
    ("mmq_sk_q6k_x80_c", "forge_mmq_sk_q6k_x80_c"),
    ("mmq_fix_q6k_x80_c", "forge_mmq_fix_q6k_x80_c"),
    ("mmq_sk_q6k_x88_nc", "forge_mmq_sk_q6k_x88_nc"),
    ("mmq_fix_q6k_x88_nc", "forge_mmq_fix_q6k_x88_nc"),
    ("mmq_sk_q6k_x88_c", "forge_mmq_sk_q6k_x88_c"),
    ("mmq_fix_q6k_x88_c", "forge_mmq_fix_q6k_x88_c"),
    ("mmq_sk_q6k_x96_nc", "forge_mmq_sk_q6k_x96_nc"),
    ("mmq_fix_q6k_x96_nc", "forge_mmq_fix_q6k_x96_nc"),
    ("mmq_sk_q6k_x96_c", "forge_mmq_sk_q6k_x96_c"),
    ("mmq_fix_q6k_x96_c", "forge_mmq_fix_q6k_x96_c"),
    ("mmq_sk_q6k_x104_nc", "forge_mmq_sk_q6k_x104_nc"),
    ("mmq_fix_q6k_x104_nc", "forge_mmq_fix_q6k_x104_nc"),
    ("mmq_sk_q6k_x104_c", "forge_mmq_sk_q6k_x104_c"),
    ("mmq_fix_q6k_x104_c", "forge_mmq_fix_q6k_x104_c"),
    ("mmq_sk_q6k_x112_nc", "forge_mmq_sk_q6k_x112_nc"),
    ("mmq_fix_q6k_x112_nc", "forge_mmq_fix_q6k_x112_nc"),
    ("mmq_sk_q6k_x112_c", "forge_mmq_sk_q6k_x112_c"),
    ("mmq_fix_q6k_x112_c", "forge_mmq_fix_q6k_x112_c"),
    ("mmq_sk_q6k_x120_nc", "forge_mmq_sk_q6k_x120_nc"),
    ("mmq_fix_q6k_x120_nc", "forge_mmq_fix_q6k_x120_nc"),
    ("mmq_sk_q6k_x120_c", "forge_mmq_sk_q6k_x120_c"),
    ("mmq_fix_q6k_x120_c", "forge_mmq_fix_q6k_x120_c"),
    ("mmq_sk_q6k_x128_nc", "forge_mmq_sk_q6k_x128_nc"),
    ("mmq_fix_q6k_x128_nc", "forge_mmq_fix_q6k_x128_nc"),
    ("mmq_sk_q6k_x128_c", "forge_mmq_sk_q6k_x128_c"),
    ("mmq_fix_q6k_x128_c", "forge_mmq_fix_q6k_x128_c"),
];

const EMBEDDED_SM89: &[EmbeddedArtifact] = embedded![
    "rmsnorm_f16",
    "rmsnorm_residual_f16",
    "silu_mul_f16",
    "sigmoid_mul_f16",
    "deinterleave_gate_f16",
    "rope_neox_f16",
    "rope_neox_partial_f16",
    "gemv_q8_0_f16",
    "gemv_f16",
    "attn_decode_f16_hd64",
    "attn_decode_f16_hd128",
    "attn_decode_f16_hd256",
    "deltanet_conv_silu_f16",
    "l2norm_heads_f16",
    "deltanet_gated_step_f16",
    "deltanet_gated_rmsnorm_f16",
    "deltanet_log_decay_f32",
    "deltanet_beta_sigmoid_f32",
    "gemv_nvfp4_f16",
    "gather_rows_f16",
    "gemv_f16_out_f32",
    "gemv_q8_0_out_f32",
    "layernorm_f16",
    "layernorm_residual_f16",
    "gelu_f16",
    "conv1d_k3_f16",
    "attn_full_f16_hd64",
    "attn_full_f16_hd128",
    "gemv_f16_bias",
    "kv_append_f16",
    "gemv_q8_0_f16_v2",
    "gemv_q8_0_out_f32_v2",
    "gemv_nvfp4_f16_v2",
    "gemv_f16_out_f32_v2",
    "gemm_q8_0_f16",
    "gemm_nvfp4_f16",
    "gemm_f16",
    "gemm_q8_0_f16_bm64",
    "gemm_nvfp4_f16_bm64",
    "gemm_f16_bm64",
    "gemm_f16_out_f32",
    "gemm_f16_out_f32_bm64",
    "gemm_q8_0_out_f32",
    "gemm_q8_0_out_f32_bm64",
    "kv_append_batch_f16",
    "kv_append_batch_fp8",
    "attn_prefill_f16_hd64",
    "attn_prefill_f16_hd128",
    "attn_prefill_f16_hd256",
    "attn_prefill_fp8_hd64",
    "attn_prefill_fp8_hd128",
    "qkv_post_f16",
    "gemv_q4_k_f16_v2",
    "gemv_q4_k_out_f32_v2",
    "gemm_q4_k_f16",
    "gemm_q4_k_f16_bm64",
    "quantize_act_q8_1",
    "gemm_q8_0_i8mma",
    "gemm_q8_0_i8mma_bm64",
    "gemm_q8_0_i8mma_big",
    "gemm_q4_k_i8mma",
    "gemm_q4_k_i8mma_bm64",
    "gemm_q4_k_i8mma_big",
    "attn_decode_split_f16_hd64",
    "attn_decode_split_f16_hd128",
    "attn_decode_split_fp8_hd64",
    "attn_decode_split_fp8_hd128",
    "attn_decode_combine_f16_hd64",
    "attn_decode_combine_f16_hd128",
    "gemv_norm_q8_0_f16",
    "gemv_norm_nvfp4_f16",
    "gemv_norm_f16",
    "gemv_norm_silu_q8_0_f16",
    "gemv_norm_silu_nvfp4_f16",
    "gemv_norm_silu_f16",
    "gemv_residual_q8_0_f16",
    "gemv_residual_nvfp4_f16",
    "gemv_residual_f16",
    "rmsnorm_h32_f16",
    "gemv_q6_k_f16_v2",
    "gemv_q6_k_out_f32_v2",
    "gemv_q6_k_f16_gidx",
    "gemm_q6_k_f16",
    "gemm_q6_k_f16_bm64",
    "gemv_norm_q4_k_f16",
    "gemv_norm_q6_k_f16",
    "gemv_norm_silu_q4_k_f16",
    "gemv_norm_silu_q6_k_f16",
    "gemv_residual_q4_k_f16",
    "gemv_residual_q6_k_f16",
    "gemv_q8_0_dp4a_f16",
    "gemv_q4_k_dp4a_f16",
    "gemv_q4_k_dp4a_out_f32",
    "gemv_q4_k_dp4a_f16_gidx",
    "gemv_norm_q8_0_dp4a_f16",
    "gemv_norm_q4_k_dp4a_f16",
    "gemv_norm_q6_k_dp4a_f16",
    "gemv_norm_silu_q8_0_dp4a_f16",
    "gemv_norm_silu_q4_k_dp4a_f16",
    "gemv_norm_silu_q6_k_dp4a_f16",
    "gemv_residual_q8_0_dp4a_f16",
    "gemv_residual_q4_k_dp4a_f16",
    "gemv_residual_q6_k_dp4a_f16",
    "gemv_q6_k_dp4a_out_f32",
    "kv_pack_rot_hd64_b4",
    "kv_pack_rot_hd64_b3",
    "kv_pack_rot_hd128_b4",
    "kv_pack_rot_hd128_b3",
    "kv_pack_rot_from_cache_hd64_b4",
    "kv_pack_rot_from_cache_hd64_b3",
    "kv_pack_rot_from_cache_hd128_b4",
    "kv_pack_rot_from_cache_hd128_b3",
    "attn_decode_rot_hd64_b4",
    "attn_decode_rot_hd64_b3",
    "attn_decode_rot_hd128_b4",
    "attn_decode_rot_hd128_b3",
    "attn_decode_combine_rot_hd64",
    "attn_decode_combine_rot_hd128",
    "attn_prefill_rot_hd64_b4",
    "attn_prefill_rot_hd64_b3",
    "attn_prefill_rot_hd128_b4",
    "attn_prefill_rot_hd128_b3",
    "penalize_f32",
    "penalize_batched_f32",
    "argmax_batched_f32",
    "topk_batched_f32",
    "argmax_partial_f32",
    "argmax_final_f32",
    "topk_partial_f32",
    "topk_final_f32",
    "gemv_q5_k_f16_v2",
    "gemv_q5_k_out_f32_v2",
    "gemv_q3_k_f16_v2",
    "gemv_q3_k_out_f32_v2",
    "gemv_q2_k_f16_v2",
    "gemv_q2_k_out_f32_v2",
    "gemv_q4_0_f16_v2",
    "gemv_q4_0_out_f32_v2",
    "gemv_q4_1_f16_v2",
    "gemv_q4_1_out_f32_v2",
    "gemv_q5_0_f16_v2",
    "gemv_q5_0_out_f32_v2",
    "gemv_q5_1_f16_v2",
    "gemv_q5_1_out_f32_v2",
    "gemm_q5_k_f16",
    "gemm_q5_k_f16_bm64",
    "gemm_q3_k_f16",
    "gemm_q3_k_f16_bm64",
    "gemm_q2_k_f16",
    "gemm_q2_k_f16_bm64",
    "gemm_q4_0_f16",
    "gemm_q4_0_f16_bm64",
    "gemm_q4_1_f16",
    "gemm_q4_1_f16_bm64",
    "gemm_q5_0_f16",
    "gemm_q5_0_f16_bm64",
    "gemm_q5_1_f16",
    "gemm_q5_1_f16_bm64",
    "gemv_norm_q5_k_f16",
    "gemv_norm_q3_k_f16",
    "gemv_norm_q2_k_f16",
    "gemv_norm_q4_0_f16",
    "gemv_norm_q4_1_f16",
    "gemv_norm_q5_0_f16",
    "gemv_norm_q5_1_f16",
    "gemv_norm_silu_q5_k_f16",
    "gemv_norm_silu_q3_k_f16",
    "gemv_norm_silu_q2_k_f16",
    "gemv_norm_silu_q4_0_f16",
    "gemv_norm_silu_q4_1_f16",
    "gemv_norm_silu_q5_0_f16",
    "gemv_norm_silu_q5_1_f16",
    "gemv_residual_q5_k_f16",
    "gemv_residual_q3_k_f16",
    "gemv_residual_q2_k_f16",
    "gemv_residual_q4_0_f16",
    "gemv_residual_q4_1_f16",
    "gemv_residual_q5_0_f16",
    "gemv_residual_q5_1_f16",
    "gemv_iq4_nl_f16_v2",
    "gemv_iq4_nl_out_f32_v2",
    "gemv_iq4_xs_f16_v2",
    "gemv_iq4_xs_out_f32_v2",
    "gemv_mxfp4_f16_v2",
    "gemv_mxfp4_out_f32_v2",
    "gemm_iq4_nl_f16",
    "gemm_iq4_nl_f16_bm64",
    "gemm_iq4_xs_f16",
    "gemm_iq4_xs_f16_bm64",
    "gemm_mxfp4_gguf_f16",
    "gemm_mxfp4_gguf_f16_bm64",
    "gemv_norm_iq4_nl_f16",
    "gemv_norm_iq4_xs_f16",
    "gemv_norm_mxfp4_f16",
    "gemv_norm_silu_iq4_nl_f16",
    "gemv_norm_silu_iq4_xs_f16",
    "gemv_norm_silu_mxfp4_f16",
    "gemv_residual_iq4_nl_f16",
    "gemv_residual_iq4_xs_f16",
    "gemv_residual_mxfp4_f16",
    "gemv_iq2_xs_f16_v2",
    "gemv_iq2_xs_out_f32_v2",
    "gemm_iq2_xs_f16",
    "gemm_iq2_xs_f16_bm64",
    "gemv_norm_iq2_xs_f16",
    "gemv_norm_silu_iq2_xs_f16",
    "gemv_residual_iq2_xs_f16",
    "gemv_iq2_s_f16_v2",
    "gemv_iq2_s_out_f32_v2",
    "gemm_iq2_s_f16",
    "gemm_iq2_s_f16_bm64",
    "gemv_norm_iq2_s_f16",
    "gemv_norm_silu_iq2_s_f16",
    "gemv_residual_iq2_s_f16",
    "gemv_iq3_s_f16_v2",
    "gemv_iq3_s_out_f32_v2",
    "gemm_iq3_s_f16",
    "gemm_iq3_s_f16_bm64",
    "gemv_norm_iq3_s_f16",
    "gemv_norm_silu_iq3_s_f16",
    "gemv_residual_iq3_s_f16",
    "gemv_iq2_xxs_f16_v2",
    "gemv_iq2_xxs_out_f32_v2",
    "gemm_iq2_xxs_f16",
    "gemm_iq2_xxs_f16_bm64",
    "gemv_norm_iq2_xxs_f16",
    "gemv_norm_silu_iq2_xxs_f16",
    "gemv_residual_iq2_xxs_f16",
    "gemv_iq3_xxs_f16_v2",
    "gemv_iq3_xxs_out_f32_v2",
    "gemm_iq3_xxs_f16",
    "gemm_iq3_xxs_f16_bm64",
    "gemv_norm_iq3_xxs_f16",
    "gemv_norm_silu_iq3_xxs_f16",
    "gemv_residual_iq3_xxs_f16",
    "gemv_iq1_s_f16_v2",
    "gemv_iq1_s_out_f32_v2",
    "gemm_iq1_s_f16",
    "gemm_iq1_s_f16_bm64",
    "gemv_norm_iq1_s_f16",
    "gemv_norm_silu_iq1_s_f16",
    "gemv_residual_iq1_s_f16",
    "gemv_iq1_m_f16_v2",
    "gemv_iq1_m_out_f32_v2",
    "gemm_iq1_m_f16",
    "gemm_iq1_m_f16_bm64",
    "gemv_norm_iq1_m_f16",
    "gemv_norm_silu_iq1_m_f16",
    "gemv_residual_iq1_m_f16",
    "moe_router_f16",
    "moe_scale_add_f16",
    "moe_scale_add_gidx_f16",
    "moe_sigmoid_f16_to_f32",
    "conv1d_f32",
    "relu_f32",
    "sigmoid_f32",
    "add_f32",
    "pow_f32",
    "sqrt_f32",
    "reduce_mean_f32",
    "lstm_f32",
];

/// Loaded modules + resolved kernel handles for one device.
pub struct KernelArtifacts {
    handles: HashMap<String, KernelHandle>,
    arch: String,
}

impl KernelArtifacts {
    /// Load kernel artifacts for `device`. `FORGE_KERNEL_DIR` (pointing at
    /// kernels/mojo/build) overrides the embedded set for development
    /// iteration without a Rust rebuild.
    pub fn load(device: &dyn Device) -> Result<Self> {
        let arch = device.caps().arch.clone();
        if let Ok(dir) = std::env::var("FORGE_KERNEL_DIR") {
            return Self::load_dir(device, Path::new(&dir), &arch);
        }
        Self::load_embedded(device, &arch)
    }

    fn load_embedded(device: &dyn Device, arch: &str) -> Result<Self> {
        // Embedded artifacts are compiled for sm_89; PTX is forward-compatible
        // within a major architecture via driver JIT, so newer sm_8x/9x parts
        // still load them. Older parts must supply FORGE_KERNEL_DIR.
        let manifest: Manifest = serde_json::from_str(EMBEDDED_MANIFEST)
            .map_err(|e| ForgeError::Kernel(format!("embedded manifest parse: {e}")))?;
        let mut handles = HashMap::new();
        for art in EMBEDDED_SM89 {
            let entry = manifest.kernels.get(art.name).ok_or_else(|| {
                ForgeError::Kernel(format!("kernel {} missing from embedded manifest", art.name))
            })?;
            let module = device.load_module(art.ptx)?;
            handles.insert(art.name.to_string(), module.kernel(&entry.entry)?);
        }
        // The reverse direction: manifest entries with no embedded bytes mean
        // build_kernels.mojo and this crate went out of sync.
        for name in manifest.kernels.keys() {
            if !EMBEDDED_SM89.iter().any(|a| a.name == name) {
                return Err(ForgeError::Kernel(format!(
                    "manifest kernel {name} not embedded — update forge-kernels EMBEDDED_SM89"
                )));
            }
        }
        Self::load_cuda_cubin(device, EMBEDDED_CUDA_CUBIN_SM89, CUDA_CUBIN_ENTRIES, &mut handles)?;
        Self::load_cuda_cubin(
            device,
            EMBEDDED_CUDA_CUBIN_W4A8_SM89,
            CUDA_W4A8_ENTRIES,
            &mut handles,
        )?;
        Self::load_cuda_cubin(
            device,
            EMBEDDED_CUDA_CUBIN_FATTN_SM89,
            CUDA_FATTN_ENTRIES,
            &mut handles,
        )?;
        Self::load_cuda_cubin(
            device,
            EMBEDDED_CUDA_CUBIN_MMQ_SM89,
            CUDA_MMQ_ENTRIES,
            &mut handles,
        )?;
        Ok(Self { handles, arch: arch.to_string() })
    }

    /// Resolve a raw-CUDA cubin's entry points into `handles`. The cubin
    /// loads exactly like Mojo PTX (`load_module` → cuModuleLoadData).
    fn load_cuda_cubin(
        device: &dyn Device,
        cubin: &[u8],
        entries: &[(&str, &str)],
        handles: &mut HashMap<String, KernelHandle>,
    ) -> Result<()> {
        let module = device.load_module(cubin)?;
        for (key, entry) in entries {
            handles.insert((*key).to_string(), module.kernel(entry)?);
        }
        Ok(())
    }

    fn load_dir(device: &dyn Device, dir: &Path, arch: &str) -> Result<Self> {
        let arch_dir = dir.join(arch);
        let manifest_path = arch_dir.join("manifest.json");
        let manifest_src = std::fs::read_to_string(&manifest_path).map_err(|e| {
            ForgeError::Kernel(format!("read {}: {e}", manifest_path.display()))
        })?;
        let manifest: Manifest = serde_json::from_str(&manifest_src)
            .map_err(|e| ForgeError::Kernel(format!("manifest parse: {e}")))?;
        let mut handles = HashMap::new();
        for (name, entry) in &manifest.kernels {
            let ptx = std::fs::read(arch_dir.join(&entry.file)).map_err(|e| {
                ForgeError::Kernel(format!("read {}: {e}", entry.file))
            })?;
            let module: Module = device.load_module(&ptx)?;
            handles.insert(name.clone(), module.kernel(&entry.entry)?);
        }
        let cubin = std::fs::read(arch_dir.join("gemm_i8mma_cuda.cubin")).map_err(|e| {
            ForgeError::Kernel(format!("read gemm_i8mma_cuda.cubin: {e}"))
        })?;
        Self::load_cuda_cubin(device, &cubin, CUDA_CUBIN_ENTRIES, &mut handles)?;
        let w4a8 = std::fs::read(arch_dir.join("w4a8_gemm_cuda.cubin")).map_err(|e| {
            ForgeError::Kernel(format!("read w4a8_gemm_cuda.cubin: {e}"))
        })?;
        Self::load_cuda_cubin(device, &w4a8, CUDA_W4A8_ENTRIES, &mut handles)?;
        let fattn = std::fs::read(arch_dir.join("fattn_prefill_cuda.cubin")).map_err(|e| {
            ForgeError::Kernel(format!("read fattn_prefill_cuda.cubin: {e}"))
        })?;
        Self::load_cuda_cubin(device, &fattn, CUDA_FATTN_ENTRIES, &mut handles)?;
        let mmq = std::fs::read(arch_dir.join("mmq_q4k_cuda.cubin")).map_err(|e| {
            ForgeError::Kernel(format!("read mmq_q4k_cuda.cubin: {e}"))
        })?;
        Self::load_cuda_cubin(device, &mmq, CUDA_MMQ_ENTRIES, &mut handles)?;
        Ok(Self { handles, arch: arch.to_string() })
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    pub fn get(&self, name: &str) -> Result<&KernelHandle> {
        self.handles
            .get(name)
            .ok_or_else(|| ForgeError::Kernel(format!("kernel not loaded: {name}")))
    }
}
