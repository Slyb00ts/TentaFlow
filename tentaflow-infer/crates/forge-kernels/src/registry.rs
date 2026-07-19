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
        Self::load_cuda_cubin(device, EMBEDDED_CUDA_CUBIN_SM89, &mut handles)?;
        Ok(Self { handles, arch: arch.to_string() })
    }

    /// Resolve the raw-CUDA MMQ cubin's entry points into `handles`. The cubin
    /// loads exactly like Mojo PTX (`load_module` → cuModuleLoadData).
    fn load_cuda_cubin(
        device: &dyn Device,
        cubin: &[u8],
        handles: &mut HashMap<String, KernelHandle>,
    ) -> Result<()> {
        let module = device.load_module(cubin)?;
        for (key, entry) in CUDA_CUBIN_ENTRIES {
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
        Self::load_cuda_cubin(device, &cubin, &mut handles)?;
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
