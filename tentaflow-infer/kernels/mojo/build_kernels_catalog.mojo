# ===== File: build_kernels_catalog.mojo — katalog kerneli AOT Mojo =====
# Compiles every registered kernel for the local GPU arch and dumps PTX into
# kernels/build/<arch>/<name>.ptx plus manifest.json describing each artifact.
# Rust (forge-kernels) loads these artifacts; no Mojo runtime ships in the
# server binary (ADR-0001).
#
# Registration is intentionally explicit: `dump_asm` is a compile-time
# parameter, so each kernel gets a literal dump path here and the file is
# relocated into the per-arch directory at runtime.

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.norm import (
    rmsnorm_delta_residual_f16,
    rmsnorm_qkv_f16,
    rmsnorm_f16,
    rmsnorm_residual_f16,
    rmsnorm_fp8,
    rmsnorm_residual_fp8,
)
from src.activation import silu_mul_f16, gelu_mul_f16, scale_f16, softcap_f32, sigmoid_mul_f16, deinterleave_gate_f16
from src.rope import rope_neox_f16, rope_neox_ff_f16
from src.gemv import gemv_q8_0_f16, gemv_f16
from src.attention import (
    attn_decode_f16_hd128,
    attn_decode_f16_hd256,
    attn_decode_f16_hd512,
    attn_decode_f16_hd64,
    hd128_attn_split8,
    hd128_attn_split8_combine,
    hd256_attn_split8,
    hd256_attn_split8_combine,
    hd512_attn_split8,
    hd512_attn_split8_combine,
    hd64_attn_split8,
    hd64_attn_split8_combine,
)
from src.attention import (
    attn_decode_batch_exact_f16_hd256,
    attn_verify_segmented_f16_hd128,
    attn_verify_segmented_f16_hd128_warp32,
    attn_verify_segmented_f16_hd256,
    attn_verify_segmented_f16_hd256_warp32,
)
from src.attention_verify_split8 import (
    attn_verify_split8_f16_hd256_t3,
    attn_verify_split8_f16_hd256_t4,
    attn_verify_split8_combine_f16_hd256,
)
from src.rope import rope_neox_partial_f16
from src.deltanet import (
    deltanet_conv_silu_f16,
    l2norm_heads_f16,
    deltanet_gated_step_f16,
    deltanet_gated_rmsnorm_f16,
    deltanet_log_decay_f32,
    deltanet_beta_sigmoid_f32,
)
from src.deltanet_value_key import (
    deltanet_value_key_scan_inplace_f16,
    deltanet_value_key_scan_persistent_f16,
    deltanet_value_key_scan_checkpoints_f16,
    deltanet_value_key_commit_recompute_f32,
)
from src.deltanet_verify import (
    deltanet_prepare_t2_f16,
    deltanet_prepare_t3_f16,
    deltanet_prepare_t4_f16,
    deltanet_prepare_dynamic_f16,
    deltanet_prepare_segmented_f16,
    deltanet_prepare_segmented_final_f16,
    deltanet_gated_scan_t2_f16,
    deltanet_gated_scan_t3_f16,
    deltanet_gated_scan_t4_f16,
    deltanet_gated_scan_t3_d128_f16,
    deltanet_gated_scan_t4_d128_f16,
    deltanet_gated_scan_dynamic_f16,
    deltanet_gated_scan_dynamic_d128_f16,
    deltanet_gated_scan_segmented_d128_f16,
    deltanet_gated_scan_segmented_shared_d128_f16,
    deltanet_commit_recompute_segmented_shared_d128_f32,
    deltanet_gated_scan_inplace_dynamic_d128_f16,
    deltanet_gated_scan_inplace_shared_d128_f16,
    deltanet_commit_checkpoint_f32,
    deltanet_commit_checkpoint_segmented_f32,
)
from src.deltanet_scan_persistent import deltanet_gated_scan_persistent_d128_f16
from src.deltanet_prepare_tiled import deltanet_prepare_tiled_d128_c4_f16
from src.nvfp4 import (
    gemv_nvfp4_gguf_f16_wave,
    gemv_nvfp4_f16,
    pack_f16_fp8,
    pack_nvfp4_fp8,
    gemv_nvfp4_gguf_f16,
    gemv_nvfp4_gguf_out_f32,
    pack_q8_0_nvfp4_gguf,
)
from src.nvfp4_gguf_dp4a import gemv_nvfp4_gguf_q8_1_f16
from src.mtp import (
    mtp_prepare_f16,
    mtp_stage_step,
    mtp_norm_join_shifted_f16,
    mtp_norm_join_shifted_segmented_f16,
    mtp_commit_catchup_metadata_segmented,
    mtp_project_joined_q8_f16,
    gather_f16_row_f16,
    gather_q8_0_row_f16,
    gather_nvfp4_gguf_row_f16,
    mtp_pack_verify_inputs,
    gather_q8_0_rows_f16,
    gather_nvfp4_gguf_rows_f16,
    gather_nvfp4_gguf_rows_f16_nvidia,
    mtp_verify_decide,
    mtp_verify_decide_segmented,
    mtp_select_row_f16,
    mtp_select_row_f32,
    mtp_select_row_segmented_f16,
)
from src.nvfp4_batch import (
    gemv_batch_nvfp4_f16_b4,
    gemv_batch_nvfp4_f16_b8,
    gemv_batch_nvfp4_f16_b16,
    gemv_batch_f16_out_f32_b4,
    gemv_batch_f16_out_f32_b8,
)
from src.misc import gather_rows_f16, gemv_f16_out_f32, gemv_q8_0_out_f32
from src.layernorm import layernorm_f16, layernorm_residual_f16
from src.conv import gelu_f16, conv1d_k3_f16
from src.attn_full import attn_full_f16_hd64, attn_full_f16_hd128
from src.gemv import gemv_f16_bias
from src.kv_append import kv_append_f16
from src.deepseek import (
    rmsnorm_head_f16,
    rope_interleaved_f16,
    hadamard_bf16_f16,
    act_quant_fp8_f16,
    act_quant_fp4_f16,
    compressor_pool_f16,
    sparse_attn_f16,
    hc_sinkhorn_f32,
    hc_reduce_f16,
    hc_expand_f16,
    index_score_f16,
    compressor_add_ape_f32,
    moe_gate_sqrtsoftplus_f16,
    swiglu_limit_f16,
    rmsnorm_mix_f32,
    hc_head_reduce_f16,
)
from src.gemv2 import (
    gemv_q8_0_f16_v2,
    gemv_q8_0_out_f32_v2,
    gemv_nvfp4_f16_v2,
    gemv_f16_out_f32_v2,
    gemv_fp8_out_f32_v2,
)
from src.gemv2 import gemv_q4_k_f16_v2, gemv_q4_k_out_f32_v2
from src.gemv2 import gemv_q6_k_f16_v2, gemv_q6_k_out_f32_v2, gemv_q6_k_f16_gidx, gemv_fp8_row_f16_v2
from src.gemm import gemm_q8_0_f16, gemm_nvfp4_f16, gemm_f16
from src.nvfp4_gguf_wmma import (
    gemm_nvfp4_gguf_wmma_f16_bm32,
    gemm_nvfp4_gguf_wmma_f16_bm128,
    gemm_nvfp4_gguf_wmma_f16_bm128_bn32,
)
from src.gemm_wmma import (
    gemm_q8_0_wmma_triplet_bm64,
    gemm_q8_0_wmma_64x128,
    gemm_q8_0_wmma_out_f32_64x128,
    gemm_q8_0_wmma_16x64,
    gemm_q8_0_wmma_out_f32_16x64,
)
from src.gemm_dot import (
    gemm_f16_dot2_64x64,
    gemm_f16_dot2_128x64,
    gemm_f16_dot2_128x128,
    gemm_f16_dot2_256x64,
    gemm_q8_0_dot4_64x64,
    gemm_q8_0_dot4_128x64,
    gemm_q8_0_dot4_128x128,
    gemm_q4_k_dot4_64x64,
    gemm_q4_k_dot4_128x64,
    gemm_q4_k_dot4_128x128,
    gemm_q6_k_dot4_64x64,
    gemm_q6_k_dot4_128x64,
    gemm_nvfp4_dot4_64x64,
    gemm_nvfp4_dot4_128x64,
    gemm_f16_dot2_out_f32_64x64,
    gemm_q8_0_dot4_out_f32_64x64,
    gemm_q4_k_dot4_out_f32_64x64,
    gemm_q6_k_dot4_out_f32_64x64,
    gemm_q4_0_dot4_64x64,
    gemm_q4_0_dot4_128x64,
    gemm_q4_0_dot4_128x128,
    gemm_q4_0_dot4_out_f32_64x64,
)
from src.q8_0_batch import (
    gemm_q8_0_i8mma_b2,
    gemm_q8_0_i8mma_b3,
    gemm_q8_0_i8mma_b4,
    gemm_q8_0_i8mma_b16,
)
from src.q8_0_batch import (
    gemm_q8_0_i8mma_out_f32_b3,
    gemm_q8_0_i8mma_out_f32_b4,
)
from src.q8_0_batch import gemm_q8_0_i8mma_b8, gemm_q8_0_f16_exact_out_f32_b8
from src.q8_0_batch import gemm_q8_0_dp4a_b3_nvidia, gemm_q8_0_dp4a_b4_nvidia
from src.q8_0_batch import (
    gemm_q8_0_dp4a_out_f32_b3_nvidia,
    gemm_q8_0_dp4a_out_f32_b4_nvidia,
)
from src.q8_0_batch import (
    gemm_q8_0_f16_exact_out_f32_b2,
    gemm_q8_0_f16_exact_out_f32_b3,
    gemm_q8_0_f16_exact_out_f32_b4,
)
from src.gemm import gemm_q8_0_f16_bm64, gemm_nvfp4_f16_bm64, gemm_f16_bm64
from src.gemm import gemm_f16_out_f32, gemm_f16_out_f32_bm64
from src.gemm import gemm_nvfp4_f16_bm32, gemm_f16_out_f32_bm32
from src.gemm import gemm_q8_0_out_f32, gemm_q8_0_out_f32_bm64
from src.gemm import gemm_q4_k_f16, gemm_q4_k_f16_bm64
from src.gemm import (
    gemm_q8_0_i8mma,
    gemm_q8_0_i8mma_bm64,
    gemm_q8_0_i8mma_big,
    gemm_q8_0_i8mma_triplet_bm64,
)
from src.gemm_q8_triplet_variants import (
    gemm_q8_0_i8mma_triplet_single_bm64,
    gemm_q8_0_i8mma_triplet_single_big,
)
from src.q8_single_big_poststage import (
    gemm_q8_0_i8mma_triplet_single_big_poststage,
)
from src.gemm import quantize_act_q8_1
from src.gemm import gemm_q4_k_i8mma, gemm_q4_k_i8mma_bm64, gemm_q4_k_i8mma_big
from src.gemm_fp8 import (
    gemm_fp8_f16,
    gemm_fp8_f16_bm64,
    gemm_fp8_f16_big,
    quantize_act_fp8,
)
from src.gemm_fp8_modular import (
    gemm_fp8_mod_4096_4096,
    gemm_fp8_mod_1024_4096,
    gemm_fp8_mod_14336_4096,
    gemm_fp8_mod_4096_14336,
    gemm_fp8_mod_11264_4096,
    gemm_fp8_mod_4096_11264,
    gemm_fp8_mod_4096_4096_bn256,
    gemm_fp8_mod_11264_4096_bn256,
)
from src.gemm import gemm_q6_k_f16, gemm_q6_k_f16_bm64
from src.gemm_q4k_i8_multistage import (
    gemm_q4k_i8_native_4096_4096_m128,
    gemm_q4k_i8_native_4096_4096_m256,
    gemm_q4k_i8_native_4096_4096_m512,
    gemm_q4k_i8_native_4096_4096_m1024,
    gemm_q4k_i8_native_4096_4096_m2048,
    gemm_q4k_i8_native_4096_4096_m4096,
    gemm_q4k_i8_native_1024_4096_m128,
    gemm_q4k_i8_native_1024_4096_m256,
    gemm_q4k_i8_native_1024_4096_m512,
    gemm_q4k_i8_native_1024_4096_m1024,
    gemm_q4k_i8_native_1024_4096_m2048,
    gemm_q4k_i8_native_1024_4096_m4096,
    gemm_q4k_i8_native_14336_4096_m128,
    gemm_q4k_i8_native_14336_4096_m256,
    gemm_q4k_i8_native_14336_4096_m512,
    gemm_q4k_i8_native_14336_4096_m1024,
    gemm_q4k_i8_native_14336_4096_m2048,
    gemm_q4k_i8_native_14336_4096_m4096,
    gemm_q4k_i8_native_4096_14336_m128,
    gemm_q4k_i8_native_4096_14336_m256,
    gemm_q4k_i8_native_4096_14336_m512,
    gemm_q4k_i8_native_4096_14336_m1024,
    gemm_q4k_i8_native_4096_14336_m2048,
    gemm_q4k_i8_native_4096_14336_m4096,
)
from src.gemm_q6k_i8_multistage import (
)
from src.prefill import (
    kv_append_batch_f16,
    attn_prefill_f16_hd64,
    attn_prefill_f16_hd128,
    attn_prefill_f16_hd256,
    attn_prefill_f16_hd512,
)
from src.prefill import (
    kv_append_batch_fp8,
    attn_prefill_fp8_hd64,
    attn_prefill_fp8_hd128,
)
from src.prefill import attn_prefill_fa_f16_hd64, attn_prefill_fa_f16_hd128
from src.prefill import (
    kv_append_batch_device_pos_f16,
    kv_append_batch_segmented_f16,
    kv_append_batch_segmented_masked_f16,
    attn_prefill_device_pos_f16_hd256,
    attn_prefill_segmented_f16_hd128,
    attn_prefill_segmented_f16_hd256,
    attn_prefill_fa_segmented_f16_hd128,
)
from src.prefill_fa_hd256 import (
    attn_prefill_fa_mojo_f16_hd256,
    attn_prefill_fa_mojo_device_pos_f16_hd256,
    attn_prefill_fa_mojo_device_pos_f16_hd256_bk32,
    attn_prefill_fa_mojo_f16_hd256_bk32,
    attn_prefill_fa_mojo_f16_hd256_vtrans,
    attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
)
from src.qkv_post import qkv_post_f16
from src.attention import (
    attn_decode_split_f16_hd64,
    attn_decode_split_f16_hd128,
    attn_decode_split_f16_hd512,
)
from src.attention import (
    attn_decode_split_fp8_hd64,
    attn_decode_split_fp8_hd128,
)
from src.attention import (
    attn_decode_combine_f16_hd64,
    attn_decode_combine_f16_hd128,
    attn_decode_combine_f16_hd512,
)
from src.attention_gqa import attn_decode_split_gqa4_f16_hd128
from src.attention_gqa_combine import attn_decode_combine_gqa2_f16_hd128
from src.decode_fused import (
    gemv_norm_q8_0_f16,
    gemv_norm_nvfp4_f16,
    gemv_norm_nvfp4_ct_s0_f16,
    gemv_norm_silu_nvfp4_ct_s0_f16,
    gemv_residual_nvfp4_ct_s0_f16,
    gemv_norm_f16,
)
from src.decode_fused import (
    gemv_norm_silu_q8_0_f16,
    gemv_norm_silu_nvfp4_f16,
    gemv_norm_silu_f16,
)
from src.decode_fused import (
    gemv_residual_q8_0_f16,
    gemv_residual_nvfp4_f16,
    gemv_residual_f16,
)
from src.decode_fused import rmsnorm_h32_f16
from src.decode_fused import gemv_norm_q4_k_f16, gemv_norm_q6_k_f16
from src.decode_fused import gemv_norm_silu_q4_k_f16, gemv_norm_silu_q6_k_f16
from src.decode_fused import gemv_residual_q4_k_f16, gemv_residual_q6_k_f16
from src.decode_dp4a import (
    gemv_q8_0_dp4a_f16,
    gemv_q4_k_dp4a_f16,
    gemv_q4_k_dp4a_out_f32,
    gemv_q4_k_dp4a_f16_gidx,
)
from src.decode_dp4a import (
    gemv_norm_q8_0_dp4a_f16,
    gemv_norm_q4_k_dp4a_f16,
    gemv_norm_q6_k_dp4a_f16,
)
from src.decode_dp4a import (
    gemv_norm_silu_q8_0_dp4a_f16,
    gemv_norm_silu_q4_k_dp4a_f16,
    gemv_norm_silu_q6_k_dp4a_f16,
)
from src.decode_dp4a import (
    gemv_residual_q8_0_dp4a_f16,
    gemv_residual_q4_k_dp4a_f16,
)
from src.decode_dp4a import gemv_residual_q6_k_dp4a_f16, gemv_q6_k_dp4a_out_f32
from src.gemv2 import gemv_q5_k_f16_v2, gemv_q5_k_out_f32_v2
from src.gemm import gemm_q5_k_f16, gemm_q5_k_f16_bm64
from src.decode_fused import (
    gemv_norm_q5_k_f16,
    gemv_norm_silu_q5_k_f16,
    gemv_residual_q5_k_f16,
)
from src.gemv2 import gemv_q3_k_f16_v2, gemv_q3_k_out_f32_v2
from src.gemm import gemm_q3_k_f16, gemm_q3_k_f16_bm64
from src.decode_fused import (
    gemv_norm_q3_k_f16,
    gemv_norm_silu_q3_k_f16,
    gemv_residual_q3_k_f16,
)
from src.gemv2 import gemv_q2_k_f16_v2, gemv_q2_k_out_f32_v2
from src.gemm import gemm_q2_k_f16, gemm_q2_k_f16_bm64
from src.decode_fused import (
    gemv_norm_q2_k_f16,
    gemv_norm_silu_q2_k_f16,
    gemv_residual_q2_k_f16,
)
from src.gemv2 import gemv_q4_0_f16_v2, gemv_q4_0_out_f32_v2
from src.gemm import gemm_q4_0_f16, gemm_q4_0_f16_bm64
from src.decode_fused import (
    gemv_norm_q4_0_f16,
    gemv_norm_silu_q4_0_f16,
    gemv_residual_q4_0_f16,
)
from src.gemv2 import gemv_q4_1_f16_v2, gemv_q4_1_out_f32_v2
from src.gemm import gemm_q4_1_f16, gemm_q4_1_f16_bm64
from src.decode_fused import (
    gemv_norm_q4_1_f16,
    gemv_norm_silu_q4_1_f16,
    gemv_residual_q4_1_f16,
)
from src.gemv2 import gemv_q5_0_f16_v2, gemv_q5_0_out_f32_v2
from src.gemm import gemm_q5_0_f16, gemm_q5_0_f16_bm64
from src.decode_fused import (
    gemv_norm_q5_0_f16,
    gemv_norm_silu_q5_0_f16,
    gemv_residual_q5_0_f16,
)
from src.gemv2 import gemv_q5_1_f16_v2, gemv_q5_1_out_f32_v2
from src.gemm import gemm_q5_1_f16, gemm_q5_1_f16_bm64
from src.decode_fused import (
    gemv_norm_q5_1_f16,
    gemv_norm_silu_q5_1_f16,
    gemv_residual_q5_1_f16,
)
from src.gemv2 import gemv_iq4_nl_f16_v2, gemv_iq4_nl_out_f32_v2
from src.gemv2 import gemv_iq4_xs_f16_v2, gemv_iq4_xs_out_f32_v2
from src.gemv2 import gemv_mxfp4_f16_v2, gemv_mxfp4_out_f32_v2
from src.gemm import gemm_iq4_nl_f16, gemm_iq4_nl_f16_bm64
from src.gemm import gemm_iq4_xs_f16, gemm_iq4_xs_f16_bm64
from src.gemm import gemm_mxfp4_gguf_f16, gemm_mxfp4_gguf_f16_bm64
from src.decode_fused import (
    gemv_norm_iq4_nl_f16,
    gemv_norm_silu_iq4_nl_f16,
    gemv_residual_iq4_nl_f16,
)
from src.decode_fused import (
    gemv_norm_iq4_xs_f16,
    gemv_norm_silu_iq4_xs_f16,
    gemv_residual_iq4_xs_f16,
)
from src.decode_fused import (
    gemv_norm_mxfp4_f16,
    gemv_norm_silu_mxfp4_f16,
    gemv_residual_mxfp4_f16,
)
from src.gemv2 import gemv_iq2_xs_f16_v2, gemv_iq2_xs_out_f32_v2
from src.gemv2 import gemv_iq2_s_f16_v2, gemv_iq2_s_out_f32_v2
from src.gemv2 import gemv_iq3_s_f16_v2, gemv_iq3_s_out_f32_v2
from src.gemm import gemm_iq2_xs_f16, gemm_iq2_xs_f16_bm64
from src.gemm import gemm_iq2_s_f16, gemm_iq2_s_f16_bm64
from src.gemm import gemm_iq3_s_f16, gemm_iq3_s_f16_bm64
from src.decode_fused import (
    gemv_norm_iq2_xs_f16,
    gemv_norm_silu_iq2_xs_f16,
    gemv_residual_iq2_xs_f16,
)
from src.decode_fused import (
    gemv_norm_iq2_s_f16,
    gemv_norm_silu_iq2_s_f16,
    gemv_residual_iq2_s_f16,
)
from src.decode_fused import (
    gemv_norm_iq3_s_f16,
    gemv_norm_silu_iq3_s_f16,
    gemv_residual_iq3_s_f16,
)
from src.gemv2 import gemv_iq2_xxs_f16_v2, gemv_iq2_xxs_out_f32_v2
from src.gemv2 import gemv_iq3_xxs_f16_v2, gemv_iq3_xxs_out_f32_v2
from src.gemv2 import gemv_iq1_s_f16_v2, gemv_iq1_s_out_f32_v2
from src.gemv2 import gemv_iq1_m_f16_v2, gemv_iq1_m_out_f32_v2
from src.gemm import gemm_iq2_xxs_f16, gemm_iq2_xxs_f16_bm64
from src.gemm import gemm_iq3_xxs_f16, gemm_iq3_xxs_f16_bm64
from src.gemm import gemm_iq1_s_f16, gemm_iq1_s_f16_bm64
from src.gemm import gemm_iq1_m_f16, gemm_iq1_m_f16_bm64
from src.decode_fused import (
    gemv_norm_iq2_xxs_f16,
    gemv_norm_silu_iq2_xxs_f16,
    gemv_residual_iq2_xxs_f16,
)
from src.decode_fused import (
    gemv_norm_iq3_xxs_f16,
    gemv_norm_silu_iq3_xxs_f16,
    gemv_residual_iq3_xxs_f16,
)
from src.decode_fused import (
    gemv_norm_iq1_s_f16,
    gemv_norm_silu_iq1_s_f16,
    gemv_residual_iq1_s_f16,
)
from src.decode_fused import (
    gemv_norm_iq1_m_f16,
    gemv_norm_silu_iq1_m_f16,
    gemv_residual_iq1_m_f16,
)
from src.rotkv import kv_pack_rot_hd64_b4, kv_pack_rot_hd64_b3
from src.rotkv import kv_pack_rot_hd128_b4, kv_pack_rot_hd128_b3
from src.rotkv import (
    kv_pack_rot_from_cache_hd64_b4,
    kv_pack_rot_from_cache_hd64_b3,
)
from src.rotkv import (
    kv_pack_rot_from_cache_hd128_b4,
    kv_pack_rot_from_cache_hd128_b3,
)
from src.rotkv import attn_decode_rot_hd64_b4, attn_decode_rot_hd64_b3
from src.rotkv import attn_decode_rot_hd128_b4, attn_decode_rot_hd128_b3
from src.rotkv import (
    attn_decode_combine_rot_hd64,
    attn_decode_combine_rot_hd128,
)
from src.rotkv import attn_prefill_rot_hd64_b4, attn_prefill_rot_hd64_b3
from src.rotkv import attn_prefill_rot_hd128_b4, attn_prefill_rot_hd128_b3
from src.sampling import penalize_f32, argmax_partial_f32, argmax_final_f32
from src.sampling import topk_partial_f32, topk_final_f32
from src.sampling import (
    penalize_batched_f32,
    argmax_batched_f32,
    topk_batched_partial_f32,
    topk_batched_final_f32,
)
from src.sampling import penalize_histogram_f32, penalized_argmax_f32
from src.moe import (
    moe_router_f16,
    moe_scale_add_f16,
    moe_scale_add_gidx_f16,
    moe_sigmoid_f16_to_f32,
)
from src.onnx_ops import (
    conv1d_f32,
    relu_f32,
    sigmoid_f32,
    add_f32,
    pow_f32,
    sqrt_f32,
    reduce_mean_f32,
    lstm_f32,
)
from src.nvfp4_gguf_batch import (
    gemm_nvfp4_gguf_f16_b2,
    gemm_nvfp4_gguf_out_f32_b2,
    gemm_nvfp4_gguf_out_f32_b4,
    gemm_nvfp4_gguf_out_f32_b8,
    gemm_nvfp4_gguf_out_f32_b16,
    gemm_nvfp4_gguf_f16_b3,
    gemm_nvfp4_gguf_f16_b4,
    gemm_nvfp4_gguf_f16_b1_nvidia,
    gemm_nvfp4_gguf_out_f32_b1_nvidia,
    gemm_nvfp4_gguf_f16_b3_nvidia,
    gemm_nvfp4_gguf_f16_b4_nvidia,
    gemm_nvfp4_gguf_f16_b8_nvidia,
    gemm_nvfp4_gguf_f16_b8,
    gemm_nvfp4_gguf_f16_b16,
    gemm_nvfp4_gguf_f16_b16_nvidia,
)
from src.nvfp4_gguf_mma import (
    gemm_nvfp4_gguf_mma_f16_bm32,
    gemm_nvfp4_gguf_mma_f16_bm128,
    gemm_nvfp4_gguf_mma_f16_bm128_bn32,
    gemm_nvfp4_gguf_mma_f16_bm128_prefetch,
)
from src.nvfp4_gguf_mma_bn128 import (
    gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
    gemm_nvfp4_gguf_mma_f16_bm128_bn128,
)
from src.nvfp4_tile128_repack import nvfp4_repack_tile128
from src.nvfp4_tile128_decode import gemv_nvfp4_tile128_coop_q8_1_f16
from src.nvfp4_tile128_mma import (
    gemm_nvfp4_tile128_mma_f16_bm128_bn64,
    gemm_nvfp4_tile128_mma_f16_bm128_bn128,
)
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_decode import (
    gemv_nvfp4_ct_s0_n64k128_f16,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b4,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b8,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b16,
)
from src.nvfp4_ct_fp8 import pack_nvfp4_ct_s0_fp8
from src.nvfp4_ct_prefill import (
    gemm_nvfp4_ct_s0_f16_bm64,
    gemm_nvfp4_ct_s0_f16_bm128,
)
from src.decode_dp4a_batch import (
    gemv_q4_k_dp4a_batch_b2,
    gemv_q4_k_dp4a_batch_b4,
    gemv_q4_k_dp4a_batch_b8,
    gemv_q4_k_dp4a_batch_b16,
    gemv_q6_k_dp4a_batch_b2,
    gemv_q6_k_dp4a_batch_b4,
    gemv_q6_k_dp4a_batch_b8,
    gemv_q6_k_dp4a_batch_b16,
)
from src.pack_gguf_fp8 import (
    pack_q4_k_fp8,
    pack_q6_k_fp8,
    pack_q8_0_fp8,
)
from src.nvfp4_ct_direct import (
    gemm_nvfp4_ct_bm16_qkv_m4,
    gemm_nvfp4_ct_bm16_qkv_m8,
    gemm_nvfp4_ct_bm16_qkv_m16,
    gemm_nvfp4_ct_bm16_o_m4,
    gemm_nvfp4_ct_bm16_o_m8,
    gemm_nvfp4_ct_bm16_o_m16,
    gemm_nvfp4_ct_bm16_gateup_m4,
    gemm_nvfp4_ct_bm16_gateup_m8,
    gemm_nvfp4_ct_bm16_gateup_m16,
    gemm_nvfp4_ct_bm16_down_m4,
    gemm_nvfp4_ct_bm16_down_m8,
    gemm_nvfp4_ct_bm16_down_m16,
    gemm_nvfp4_ct_bm32_qkv_m24,
    gemm_nvfp4_ct_bm32_qkv_m32,
    gemm_nvfp4_ct_bm32_o_m24,
    gemm_nvfp4_ct_bm32_o_m32,
    gemm_nvfp4_ct_bm32_gateup_m24,
    gemm_nvfp4_ct_bm32_gateup_m32,
    gemm_nvfp4_ct_bm32_down_m24,
    gemm_nvfp4_ct_bm32_down_m32,
    reduce_nvfp4_direct_down,
)


def _entry_from_ptx(ptx_path: Path) raises -> String:
    # The mangled kernel symbol is only known post-compilation, so recover it
    # from the emitted `.visible .entry <name>(` line.
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("no .visible .entry in " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("malformed entry line in " + String(ptx_path))
    return String(text[byte = i + marker.byte_length() : j])


def _is_portable_raw_nvfp4(name: StringSlice) -> Bool:
    # Te kernele dekoduja UE4M3 programowo i korzystaja najwyzej z F16 MMA,
    # dlatego nie wymagaja instrukcji FP8 dostepnych dopiero od Ada.
    return (
        name == "gemv_nvfp4_gguf_f16"
        or name == "gemv_nvfp4_gguf_out_f32"
        or name == "pack_q8_0_nvfp4_gguf"
        or name == "gemv_nvfp4_gguf_q8_1_f16"
        or name == "mtp_prepare_f16"
        or name == "mtp_stage_step"
        or name == "mtp_norm_join_shifted_f16"
        or name == "mtp_project_joined_q8_f16"
        or name == "gather_f16_row_f16"
        or name == "gather_q8_0_row_f16"
        or name == "gather_nvfp4_gguf_row_f16"
        or name == "mtp_pack_verify_inputs"
        or name == "gather_q8_0_rows_f16"
        or name == "gather_nvfp4_gguf_rows_f16"
        or name == "gather_nvfp4_gguf_rows_f16_nvidia"
        or name == "gemm_nvfp4_gguf_f16_b2"
        or name == "gemm_nvfp4_gguf_out_f32_b2"
        or name == "gemm_nvfp4_gguf_f16_b3"
        or name == "gemm_nvfp4_gguf_f16_b4"
        or name == "gemm_nvfp4_gguf_f16_b1_nvidia"
        or name == "gemm_nvfp4_gguf_out_f32_b1_nvidia"
        or name == "gemm_nvfp4_gguf_f16_b3_nvidia"
        or name == "gemm_nvfp4_gguf_f16_b4_nvidia"
        or name == "gemm_nvfp4_gguf_f16_b8_nvidia"
        or name == "gemm_nvfp4_gguf_f16_b8"
        or name == "gemm_nvfp4_gguf_f16_b16"
        or name == "gemm_nvfp4_gguf_out_f32_b4"
        or name == "gemm_nvfp4_gguf_out_f32_b8"
        or name == "gemm_nvfp4_gguf_out_f32_b16"
        or name == "gemm_nvfp4_gguf_f16_b16_nvidia"
        or name == "gemm_nvfp4_gguf_mma_f16_bm32"
        or name == "gemm_nvfp4_gguf_mma_f16_bm128"
        or name == "gemm_nvfp4_gguf_mma_f16_bm128_bn32"
        or name == "gemm_nvfp4_gguf_mma_f16_bm128_prefetch"
        or name == "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1"
        or name == "gemm_nvfp4_gguf_mma_f16_bm128_bn128"
        or name == "nvfp4_repack_tile128"
        or name == "gemv_nvfp4_tile128_coop_q8_1_f16"
        or name == "gemm_nvfp4_tile128_mma_f16_bm128_bn64"
        or name == "gemm_nvfp4_tile128_mma_f16_bm128_bn128"
        or name == "repack_nvfp4_ct_s0_n64k128_into"
        or name == "gemv_nvfp4_ct_s0_n64k128_f16"
        or name == "gemv_batch_nvfp4_ct_s0_n64k128_f16_b4"
        or name == "gemv_batch_nvfp4_ct_s0_n64k128_f16_b8"
        or name == "gemv_batch_nvfp4_ct_s0_n64k128_f16_b16"
        or name == "gemm_nvfp4_ct_s0_f16_bm64"
        or name == "gemm_nvfp4_ct_s0_f16_bm128"
        or name == "gemv_norm_nvfp4_ct_s0_f16"
        or name == "gemv_norm_silu_nvfp4_ct_s0_f16"
        or name == "gemv_residual_nvfp4_ct_s0_f16"
        or name == "pack_nvfp4_ct_s0_fp8"
        or name == "gemm_nvfp4_ct_bm16_qkv_m4"
        or name == "gemm_nvfp4_ct_bm16_qkv_m8"
        or name == "gemm_nvfp4_ct_bm16_qkv_m16"
        or name == "gemm_nvfp4_ct_bm16_o_m4"
        or name == "gemm_nvfp4_ct_bm16_o_m8"
        or name == "gemm_nvfp4_ct_bm16_o_m16"
        or name == "gemm_nvfp4_ct_bm16_gateup_m4"
        or name == "gemm_nvfp4_ct_bm16_gateup_m8"
        or name == "gemm_nvfp4_ct_bm16_gateup_m16"
        or name == "gemm_nvfp4_ct_bm16_down_m4"
        or name == "gemm_nvfp4_ct_bm16_down_m8"
        or name == "gemm_nvfp4_ct_bm16_down_m16"
        or name == "gemm_nvfp4_ct_bm32_qkv_m24"
        or name == "gemm_nvfp4_ct_bm32_qkv_m32"
        or name == "gemm_nvfp4_ct_bm32_o_m24"
        or name == "gemm_nvfp4_ct_bm32_o_m32"
        or name == "gemm_nvfp4_ct_bm32_gateup_m24"
        or name == "gemm_nvfp4_ct_bm32_gateup_m32"
        or name == "gemm_nvfp4_ct_bm32_down_m24"
        or name == "gemm_nvfp4_ct_bm32_down_m32"
        or name == "reduce_nvfp4_ct_bm16"
    )


def _finalize(out_dir: Path, name: StringSlice) raises -> String:
    # Relocate the statically-named dump into the per-arch directory and
    # return its manifest fragment.
    #
    # Portability: Mojo emits `.target sm_89` for the local Ada GPU, but PTX JIT
    # is forward-only — an sm_89 module will NOT load on sm_86 (RTX 3090). The
    # portable kernels (f16/bf16/int8 mma, attention, gemv, norm, rope, sampling)
    # use only sm_80-level features, so lowering their target floor to sm_80 lets
    # the driver JIT them onto ANY sm_80+ device (3090 sm_86 AND 4090 sm_89) while
    # still producing arch-optimal SASS at load. Only the genuinely Ada-only
    # kernels (fp8 mma/cvt, NVFP4 fp8-scale cvt) must stay sm_89; they are keyed
    # by name and skipped at load on pre-Ada devices (forge-kernels registry).
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text()
    if _is_portable_raw_nvfp4(name) or (
        "fp8" not in name and "nvfp4" not in name
    ):
        text = text.replace(".target sm_89", ".target sm_80")
    final.write_text(text)
    os.remove(String(tmp))
    entry = _entry_from_ptx(final)
    print("  compiled", name, "->", entry)
    return (
        String('    "')
        + String(name)
        + String('": {"file": "')
        + String(name)
        + String('.ptx", "entry": "')
        + entry
        + String('"}')
    )


def _finalize_fp8(out_dir: Path, name: StringSlice) raises -> String:
    # Same as `_finalize`, but bump the PTX ISA `.version` to 8.4. Mojo's NVPTX
    # emitter caps sm_89 at 8.1, which ptxas (and the driver's JIT) reject for
    # the fp8 e4m3 m16n8k32 mma (needs >= 8.4). Ada's 4th-gen fp8 tensor cores
    # are hardware-valid at 8.4, so the committed .ptx is self-contained — the
    # driver JIT accepts it with no runtime shim (the shim is only for `mojo
    # run` JIT of scratch/tests). This only lifts an emitter version cap; it
    # does not change kernel semantics.
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text()
    text = text.replace(".version 8.0", ".version 8.4")
    text = text.replace(".version 8.1", ".version 8.4")
    text = text.replace(".version 8.2", ".version 8.4")
    text = text.replace(".version 8.3", ".version 8.4")
    final.write_text(text)
    os.remove(String(tmp))
    entry = _entry_from_ptx(final)
    print("  compiled", name, "->", entry)
    return (
        String('    "')
        + String(name)
        + String('": {"file": "')
        + String(name)
        + String('.ptx", "entry": "')
        + entry
        + String('"}')
    )


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    print("target arch:", arch)

    var entries = List[String]()

    _ = ctx.compile_function[rmsnorm_f16, dump_asm=Path("rmsnorm_f16.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_f16"))

    _ = ctx.compile_function[
        rmsnorm_residual_f16, dump_asm=Path("rmsnorm_residual_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_residual_f16"))
    _ = ctx.compile_function[
        rmsnorm_delta_residual_f16,
        dump_asm=Path("rmsnorm_delta_residual_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_delta_residual_f16"))
    _ = ctx.compile_function[
        rmsnorm_qkv_f16, dump_asm=Path("rmsnorm_qkv_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_qkv_f16"))

    _ = ctx.compile_function[rmsnorm_fp8, dump_asm=Path("rmsnorm_fp8.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_fp8"))

    _ = ctx.compile_function[
        rmsnorm_residual_fp8, dump_asm=Path("rmsnorm_residual_fp8.ptx")
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_residual_fp8"))

    _ = ctx.compile_function[silu_mul_f16, dump_asm=Path("silu_mul_f16.ptx")]()
    entries.append(_finalize(out_dir, "silu_mul_f16"))
    _ = ctx.compile_function[gelu_mul_f16, dump_asm=Path("gelu_mul_f16.ptx")]()
    entries.append(_finalize(out_dir, "gelu_mul_f16"))
    _ = ctx.compile_function[scale_f16, dump_asm=Path("scale_f16.ptx")]()
    entries.append(_finalize(out_dir, "scale_f16"))
    _ = ctx.compile_function[softcap_f32, dump_asm=Path("softcap_f32.ptx")]()
    entries.append(_finalize(out_dir, "softcap_f32"))

    _ = ctx.compile_function[
        sigmoid_mul_f16, dump_asm=Path("sigmoid_mul_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "sigmoid_mul_f16"))

    _ = ctx.compile_function[
        deinterleave_gate_f16, dump_asm=Path("deinterleave_gate_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deinterleave_gate_f16"))

    _ = ctx.compile_function[
        rope_neox_f16, dump_asm=Path("rope_neox_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rope_neox_f16"))
    _ = ctx.compile_function[
        rope_neox_ff_f16, dump_asm=Path("rope_neox_ff_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rope_neox_ff_f16"))

    _ = ctx.compile_function[
        gemv_q8_0_f16, dump_asm=Path("gemv_q8_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q8_0_f16"))

    _ = ctx.compile_function[gemv_f16, dump_asm=Path("gemv_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16"))

    _ = ctx.compile_function[
        attn_decode_f16_hd64, dump_asm=Path("attn_decode_f16_hd64.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd64"))

    _ = ctx.compile_function[
        attn_decode_f16_hd128, dump_asm=Path("attn_decode_f16_hd128.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd128"))

    _ = ctx.compile_function[
        attn_decode_f16_hd256, dump_asm=Path("attn_decode_f16_hd256.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd256"))
    _ = ctx.compile_function[
        attn_decode_f16_hd512, dump_asm=Path("attn_decode_f16_hd512.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd512"))

    _ = ctx.compile_function[
        hd256_attn_split8,
        dump_asm=Path("attn_decode_split8_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_f16_hd256"))
    _ = ctx.compile_function[
        hd256_attn_split8_combine,
        dump_asm=Path("attn_decode_split8_combine_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_combine_f16_hd256"))
    _ = ctx.compile_function[
        hd512_attn_split8,
        dump_asm=Path("attn_decode_split8_f16_hd512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_f16_hd512"))
    _ = ctx.compile_function[
        hd512_attn_split8_combine,
        dump_asm=Path("attn_decode_split8_combine_f16_hd512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_combine_f16_hd512"))
    _ = ctx.compile_function[
        hd64_attn_split8,
        dump_asm=Path("attn_decode_split8_f16_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_f16_hd64"))
    _ = ctx.compile_function[
        hd64_attn_split8_combine,
        dump_asm=Path("attn_decode_split8_combine_f16_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_combine_f16_hd64"))
    _ = ctx.compile_function[
        hd128_attn_split8,
        dump_asm=Path("attn_decode_split8_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_f16_hd128"))
    _ = ctx.compile_function[
        hd128_attn_split8_combine,
        dump_asm=Path("attn_decode_split8_combine_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split8_combine_f16_hd128"))


    _ = ctx.compile_function[
        attn_decode_batch_exact_f16_hd256,
        dump_asm=Path("attn_decode_batch_exact_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_batch_exact_f16_hd256"))
    _ = ctx.compile_function[
        attn_verify_split8_f16_hd256_t3,
        dump_asm=Path("attn_verify_split8_f16_hd256_t3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_split8_f16_hd256_t3"))
    _ = ctx.compile_function[
        attn_verify_split8_f16_hd256_t4,
        dump_asm=Path("attn_verify_split8_f16_hd256_t4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_split8_f16_hd256_t4"))
    _ = ctx.compile_function[
        attn_verify_split8_combine_f16_hd256,
        dump_asm=Path("attn_verify_split8_combine_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_split8_combine_f16_hd256"))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd128,
        dump_asm=Path("attn_verify_segmented_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_segmented_f16_hd128"))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd128_warp32,
        dump_asm=Path("attn_verify_segmented_f16_hd128_warp32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_segmented_f16_hd128_warp32"))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd256,
        dump_asm=Path("attn_verify_segmented_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_segmented_f16_hd256"))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd256_warp32,
        dump_asm=Path("attn_verify_segmented_f16_hd256_warp32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_verify_segmented_f16_hd256_warp32"))

    _ = ctx.compile_function[
        rope_neox_partial_f16, dump_asm=Path("rope_neox_partial_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rope_neox_partial_f16"))

    _ = ctx.compile_function[
        deltanet_conv_silu_f16, dump_asm=Path("deltanet_conv_silu_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_conv_silu_f16"))

    _ = ctx.compile_function[
        l2norm_heads_f16, dump_asm=Path("l2norm_heads_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "l2norm_heads_f16"))

    _ = ctx.compile_function[
        deltanet_gated_step_f16, dump_asm=Path("deltanet_gated_step_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_step_f16"))
    _ = ctx.compile_function[
        deltanet_value_key_scan_inplace_f16,
        dump_asm=Path("deltanet_value_key_scan_inplace_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_value_key_scan_inplace_f16"))
    _ = ctx.compile_function[
        deltanet_value_key_scan_persistent_f16,
        dump_asm=Path("deltanet_value_key_scan_persistent_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_value_key_scan_persistent_f16"))
    _ = ctx.compile_function[
        deltanet_value_key_scan_checkpoints_f16,
        dump_asm=Path("deltanet_value_key_scan_checkpoints_f16.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_value_key_scan_checkpoints_f16")
    )
    _ = ctx.compile_function[
        deltanet_value_key_commit_recompute_f32,
        dump_asm=Path("deltanet_value_key_commit_recompute_f32.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_value_key_commit_recompute_f32")
    )

    _ = ctx.compile_function[
        deltanet_prepare_t2_f16, dump_asm=Path("deltanet_prepare_t2_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_t2_f16"))

    _ = ctx.compile_function[
        deltanet_prepare_t3_f16, dump_asm=Path("deltanet_prepare_t3_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_t3_f16"))

    _ = ctx.compile_function[
        deltanet_prepare_t4_f16, dump_asm=Path("deltanet_prepare_t4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_t4_f16"))
    _ = ctx.compile_function[
        deltanet_prepare_dynamic_f16,
        dump_asm=Path("deltanet_prepare_dynamic_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_dynamic_f16"))
    _ = ctx.compile_function[
        deltanet_prepare_segmented_f16,
        dump_asm=Path("deltanet_prepare_segmented_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_segmented_f16"))
    _ = ctx.compile_function[
        deltanet_prepare_segmented_final_f16,
        dump_asm=Path("deltanet_prepare_segmented_final_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_segmented_final_f16"))

    _ = ctx.compile_function[
        deltanet_gated_scan_t2_f16,
        dump_asm=Path("deltanet_gated_scan_t2_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_t2_f16"))

    _ = ctx.compile_function[
        deltanet_gated_scan_t3_f16,
        dump_asm=Path("deltanet_gated_scan_t3_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_t3_f16"))

    _ = ctx.compile_function[
        deltanet_gated_scan_t4_f16,
        dump_asm=Path("deltanet_gated_scan_t4_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_t4_f16"))

    _ = ctx.compile_function[
        deltanet_gated_scan_t3_d128_f16,
        dump_asm=Path("deltanet_gated_scan_t3_d128_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_t3_d128_f16"))

    _ = ctx.compile_function[
        deltanet_gated_scan_t4_d128_f16,
        dump_asm=Path("deltanet_gated_scan_t4_d128_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_t4_d128_f16"))
    _ = ctx.compile_function[
        deltanet_gated_scan_dynamic_f16,
        dump_asm=Path("deltanet_gated_scan_dynamic_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_dynamic_f16"))
    _ = ctx.compile_function[
        deltanet_gated_scan_dynamic_d128_f16,
        dump_asm=Path("deltanet_gated_scan_dynamic_d128_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_dynamic_d128_f16"))
    _ = ctx.compile_function[
        deltanet_gated_scan_segmented_d128_f16,
        dump_asm=Path("deltanet_gated_scan_segmented_d128_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_scan_segmented_d128_f16"))
    _ = ctx.compile_function[
        deltanet_gated_scan_segmented_shared_d128_f16,
        dump_asm=Path("deltanet_gated_scan_segmented_shared_d128_f16.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_gated_scan_segmented_shared_d128_f16")
    )
    _ = ctx.compile_function[
        deltanet_commit_recompute_segmented_shared_d128_f32,
        dump_asm=Path(
            "deltanet_commit_recompute_segmented_shared_d128_f32.ptx"
        ),
    ]()
    entries.append(
        _finalize(
            out_dir, "deltanet_commit_recompute_segmented_shared_d128_f32"
        )
    )
    _ = ctx.compile_function[
        deltanet_gated_scan_inplace_dynamic_d128_f16,
        dump_asm=Path("deltanet_gated_scan_inplace_dynamic_d128_f16.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_gated_scan_inplace_dynamic_d128_f16")
    )
    _ = ctx.compile_function[
        deltanet_gated_scan_inplace_shared_d128_f16,
        dump_asm=Path("deltanet_gated_scan_inplace_shared_d128_f16.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_gated_scan_inplace_shared_d128_f16")
    )
    _ = ctx.compile_function[
        deltanet_gated_scan_persistent_d128_f16,
        dump_asm=Path("deltanet_gated_scan_persistent_d128_f16.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_gated_scan_persistent_d128_f16")
    )

    _ = ctx.compile_function[
        deltanet_prepare_tiled_d128_c4_f16,
        dump_asm=Path("deltanet_prepare_tiled_d128_c4_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_prepare_tiled_d128_c4_f16"))

    _ = ctx.compile_function[
        deltanet_commit_checkpoint_f32,
        dump_asm=Path("deltanet_commit_checkpoint_f32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_commit_checkpoint_f32"))
    _ = ctx.compile_function[
        deltanet_commit_checkpoint_segmented_f32,
        dump_asm=Path("deltanet_commit_checkpoint_segmented_f32.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "deltanet_commit_checkpoint_segmented_f32")
    )

    _ = ctx.compile_function[
        deltanet_gated_rmsnorm_f16,
        dump_asm=Path("deltanet_gated_rmsnorm_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_gated_rmsnorm_f16"))

    _ = ctx.compile_function[
        deltanet_log_decay_f32, dump_asm=Path("deltanet_log_decay_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "deltanet_log_decay_f32"))

    _ = ctx.compile_function[
        deltanet_beta_sigmoid_f32,
        dump_asm=Path("deltanet_beta_sigmoid_f32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "deltanet_beta_sigmoid_f32"))

    _ = ctx.compile_function[
        gemv_nvfp4_f16, dump_asm=Path("gemv_nvfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_f16"))
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_f16, dump_asm=Path("gemv_nvfp4_gguf_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_gguf_f16"))
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_f16_wave, dump_asm=Path("gemv_nvfp4_gguf_f16_wave.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_gguf_f16_wave"))
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_out_f32, dump_asm=Path("gemv_nvfp4_gguf_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_gguf_out_f32"))
    _ = ctx.compile_function[
        pack_q8_0_nvfp4_gguf, dump_asm=Path("pack_q8_0_nvfp4_gguf.ptx")
    ]()
    entries.append(_finalize(out_dir, "pack_q8_0_nvfp4_gguf"))
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_q8_1_f16, dump_asm=Path("gemv_nvfp4_gguf_q8_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_gguf_q8_1_f16"))
    _ = ctx.compile_function[
        mtp_prepare_f16, dump_asm=Path("mtp_prepare_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_prepare_f16"))
    _ = ctx.compile_function[
        mtp_stage_step, dump_asm=Path("mtp_stage_step.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_stage_step"))
    _ = ctx.compile_function[
        mtp_norm_join_shifted_f16,
        dump_asm=Path("mtp_norm_join_shifted_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_norm_join_shifted_f16"))
    _ = ctx.compile_function[
        mtp_norm_join_shifted_segmented_f16,
        dump_asm=Path("mtp_norm_join_shifted_segmented_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_norm_join_shifted_segmented_f16"))
    _ = ctx.compile_function[
        mtp_commit_catchup_metadata_segmented,
        dump_asm=Path("mtp_commit_catchup_metadata_segmented.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_commit_catchup_metadata_segmented"))
    _ = ctx.compile_function[
        mtp_project_joined_q8_f16,
        dump_asm=Path("mtp_project_joined_q8_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_project_joined_q8_f16"))
    _ = ctx.compile_function[
        gather_f16_row_f16, dump_asm=Path("gather_f16_row_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gather_f16_row_f16"))
    _ = ctx.compile_function[
        gather_q8_0_row_f16, dump_asm=Path("gather_q8_0_row_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gather_q8_0_row_f16"))
    _ = ctx.compile_function[
        gather_nvfp4_gguf_row_f16,
        dump_asm=Path("gather_nvfp4_gguf_row_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gather_nvfp4_gguf_row_f16"))
    _ = ctx.compile_function[
        mtp_pack_verify_inputs, dump_asm=Path("mtp_pack_verify_inputs.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_pack_verify_inputs"))
    _ = ctx.compile_function[
        gather_q8_0_rows_f16, dump_asm=Path("gather_q8_0_rows_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gather_q8_0_rows_f16"))
    _ = ctx.compile_function[
        gather_nvfp4_gguf_rows_f16,
        dump_asm=Path("gather_nvfp4_gguf_rows_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gather_nvfp4_gguf_rows_f16"))
    _ = ctx.compile_function[
        gather_nvfp4_gguf_rows_f16_nvidia,
        dump_asm=Path("gather_nvfp4_gguf_rows_f16_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gather_nvfp4_gguf_rows_f16_nvidia"))

    _ = ctx.compile_function[
        mtp_verify_decide, dump_asm=Path("mtp_verify_decide.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_verify_decide"))

    _ = ctx.compile_function[
        mtp_verify_decide_segmented,
        dump_asm=Path("mtp_verify_decide_segmented.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_verify_decide_segmented"))

    _ = ctx.compile_function[
        mtp_select_row_f16, dump_asm=Path("mtp_select_row_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_select_row_f16"))

    _ = ctx.compile_function[
        mtp_select_row_f32, dump_asm=Path("mtp_select_row_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "mtp_select_row_f32"))

    _ = ctx.compile_function[
        mtp_select_row_segmented_f16,
        dump_asm=Path("mtp_select_row_segmented_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "mtp_select_row_segmented_f16"))

    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b2, dump_asm=Path("gemm_nvfp4_gguf_f16_b2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b2"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b2,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b2.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b2"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b4,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b4"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b8,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b8"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b16,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b16"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b3, dump_asm=Path("gemm_nvfp4_gguf_f16_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b3"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b4, dump_asm=Path("gemm_nvfp4_gguf_f16_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b4"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b1_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b1_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b1_nvidia"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b1_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b1_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b1_nvidia"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b3_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b3_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b3_nvidia"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b4_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b4_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b4_nvidia"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b8_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b8_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b8_nvidia"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b8, dump_asm=Path("gemm_nvfp4_gguf_f16_b8.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b8"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b16, dump_asm=Path("gemm_nvfp4_gguf_f16_b16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm32,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm32"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn32,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128_bn32"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_prefetch,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_prefetch.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128_prefetch"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1")
    )
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn128,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128_bn128"))
    _ = ctx.compile_function[
        nvfp4_repack_tile128, dump_asm=Path("nvfp4_repack_tile128.ptx")
    ]()
    entries.append(_finalize(out_dir, "nvfp4_repack_tile128"))
    _ = ctx.compile_function[
        gemv_nvfp4_tile128_coop_q8_1_f16,
        dump_asm=Path("gemv_nvfp4_tile128_coop_q8_1_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_tile128_coop_q8_1_f16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_tile128_mma_f16_bm128_bn64,
        dump_asm=Path("gemm_nvfp4_tile128_mma_f16_bm128_bn64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_tile128_mma_f16_bm128_bn64"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_tile128_mma_f16_bm128_bn128,
        dump_asm=Path("gemm_nvfp4_tile128_mma_f16_bm128_bn128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_tile128_mma_f16_bm128_bn128"))
    _ = ctx.compile_function[
        repack_nvfp4_ct_s0_n64k128_into,
        dump_asm=Path("repack_nvfp4_ct_s0_n64k128_into.ptx"),
    ]()
    entries.append(_finalize(out_dir, "repack_nvfp4_ct_s0_n64k128_into"))
    _ = ctx.compile_function[
        gemv_nvfp4_ct_s0_n64k128_f16,
        dump_asm=Path("gemv_nvfp4_ct_s0_n64k128_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_ct_s0_n64k128_f16"))
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b4,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b4"))
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b8,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b8"))
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b16,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_s0_f16_bm64,
        dump_asm=Path("gemm_nvfp4_ct_s0_f16_bm64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_s0_f16_bm64"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_s0_f16_bm128,
        dump_asm=Path("gemm_nvfp4_ct_s0_f16_bm128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_s0_f16_bm128"))
    _ = ctx.compile_function[
        gemv_norm_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_norm_nvfp4_ct_s0_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_nvfp4_ct_s0_f16"))
    _ = ctx.compile_function[
        gemv_norm_silu_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_norm_silu_nvfp4_ct_s0_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_nvfp4_ct_s0_f16"))
    _ = ctx.compile_function[
        gemv_residual_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_residual_nvfp4_ct_s0_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_nvfp4_ct_s0_f16"))
    _ = ctx.compile_function[
        pack_nvfp4_ct_s0_fp8, dump_asm=Path("pack_nvfp4_ct_s0_fp8.ptx")
    ]()
    entries.append(_finalize(out_dir, "pack_nvfp4_ct_s0_fp8"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_qkv_m4"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_qkv_m8"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_qkv_m16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m4, dump_asm=Path("gemm_nvfp4_ct_bm16_o_m4.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_o_m4"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m8, dump_asm=Path("gemm_nvfp4_ct_bm16_o_m8.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_o_m8"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m16, dump_asm=Path("gemm_nvfp4_ct_bm16_o_m16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_o_m16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_gateup_m4"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_gateup_m8"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_gateup_m16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_down_m4"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_down_m8"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm16_down_m16"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_qkv_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_qkv_m24.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_qkv_m24"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_qkv_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_qkv_m32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_qkv_m32"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_o_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_o_m24.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_o_m24"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_o_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_o_m32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_o_m32"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_gateup_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_gateup_m24.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_gateup_m24"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_gateup_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_gateup_m32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_gateup_m32"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_down_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_down_m24.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_down_m24"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_down_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_down_m32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_ct_bm32_down_m32"))
    _ = ctx.compile_function[
        topk_batched_partial_f32,
        dump_asm=Path("topk_batched_partial_f32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "topk_batched_partial_f32"))
    _ = ctx.compile_function[
        topk_batched_final_f32,
        dump_asm=Path("topk_batched_final_f32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "topk_batched_final_f32"))
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b2,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b2.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_batch_b2"))
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b4,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_batch_b4"))
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b8,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_batch_b8"))
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b16,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_batch_b16"))
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b2,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b2.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_batch_b2"))
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b4,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_batch_b4"))
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b8,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_batch_b8"))
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b16,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_batch_b16"))
    _ = ctx.compile_function[
        pack_q4_k_fp8,
        dump_asm=Path("pack_q4_k_fp8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "pack_q4_k_fp8"))
    _ = ctx.compile_function[
        pack_q6_k_fp8,
        dump_asm=Path("pack_q6_k_fp8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "pack_q6_k_fp8"))
    _ = ctx.compile_function[
        pack_q8_0_fp8,
        dump_asm=Path("pack_q8_0_fp8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "pack_q8_0_fp8"))
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b16,
        dump_asm=Path("gemm_q8_0_i8mma_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_b16"))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b16_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b16_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_f16_b16_nvidia"))
    _ = ctx.compile_function[
        reduce_nvfp4_direct_down, dump_asm=Path("reduce_nvfp4_ct_bm16.ptx")
    ]()
    entries.append(_finalize(out_dir, "reduce_nvfp4_ct_bm16"))
    _ = ctx.compile_function[
        pack_nvfp4_fp8, dump_asm=Path("pack_nvfp4_fp8.ptx")
    ]()
    entries.append(_finalize(out_dir, "pack_nvfp4_fp8"))
    _ = ctx.compile_function[pack_f16_fp8, dump_asm=Path("pack_f16_fp8.ptx")]()
    entries.append(_finalize(out_dir, "pack_f16_fp8"))

    _ = ctx.compile_function[
        gather_rows_f16, dump_asm=Path("gather_rows_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gather_rows_f16"))

    _ = ctx.compile_function[
        gemv_f16_out_f32, dump_asm=Path("gemv_f16_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_f16_out_f32"))

    _ = ctx.compile_function[
        gemv_q8_0_out_f32, dump_asm=Path("gemv_q8_0_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q8_0_out_f32"))

    _ = ctx.compile_function[
        layernorm_f16, dump_asm=Path("layernorm_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "layernorm_f16"))

    _ = ctx.compile_function[
        layernorm_residual_f16, dump_asm=Path("layernorm_residual_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "layernorm_residual_f16"))

    _ = ctx.compile_function[gelu_f16, dump_asm=Path("gelu_f16.ptx")]()
    entries.append(_finalize(out_dir, "gelu_f16"))

    _ = ctx.compile_function[
        conv1d_k3_f16, dump_asm=Path("conv1d_k3_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "conv1d_k3_f16"))

    _ = ctx.compile_function[
        attn_full_f16_hd64, dump_asm=Path("attn_full_f16_hd64.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_full_f16_hd64"))

    _ = ctx.compile_function[
        attn_full_f16_hd128, dump_asm=Path("attn_full_f16_hd128.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_full_f16_hd128"))

    _ = ctx.compile_function[
        gemv_f16_bias, dump_asm=Path("gemv_f16_bias.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_f16_bias"))

    _ = ctx.compile_function[
        kv_append_f16, dump_asm=Path("kv_append_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_append_f16"))

    _ = ctx.compile_function[
        gemv_q8_0_f16_v2, dump_asm=Path("gemv_q8_0_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q8_0_f16_v2"))

    _ = ctx.compile_function[
        gemv_q8_0_out_f32_v2, dump_asm=Path("gemv_q8_0_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q8_0_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_nvfp4_f16_v2, dump_asm=Path("gemv_nvfp4_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_f16_v2"))

    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b4, dump_asm=Path("gemv_batch_nvfp4_f16_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b4"))

    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b8, dump_asm=Path("gemv_batch_nvfp4_f16_b8.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b8"))

    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b16, dump_asm=Path("gemv_batch_nvfp4_f16_b16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b16"))

    _ = ctx.compile_function[
        gemv_batch_f16_out_f32_b4,
        dump_asm=Path("gemv_batch_f16_out_f32_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_f16_out_f32_b4"))

    _ = ctx.compile_function[
        gemv_batch_f16_out_f32_b8,
        dump_asm=Path("gemv_batch_f16_out_f32_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_f16_out_f32_b8"))

    _ = ctx.compile_function[
        gemv_f16_out_f32_v2, dump_asm=Path("gemv_f16_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_f16_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_fp8_out_f32_v2, dump_asm=Path("gemv_fp8_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_fp8_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_f16, dump_asm=Path("gemm_q8_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b2, dump_asm=Path("gemm_q8_0_i8mma_b2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_b2"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b3, dump_asm=Path("gemm_q8_0_i8mma_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_b3"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b4, dump_asm=Path("gemm_q8_0_i8mma_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_b4"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b8, dump_asm=Path("gemm_q8_0_i8mma_b8.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_b8"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_out_f32_b3,
        dump_asm=Path("gemm_q8_0_i8mma_out_f32_b3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_out_f32_b3"))

    _ = ctx.compile_function[
        gemm_q8_0_i8mma_out_f32_b4,
        dump_asm=Path("gemm_q8_0_i8mma_out_f32_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_out_f32_b4"))

    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b8,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b8"))

    _ = ctx.compile_function[
        gemm_q8_0_dp4a_b3_nvidia, dump_asm=Path("gemm_q8_0_dp4a_b3_nvidia.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dp4a_b3_nvidia"))

    _ = ctx.compile_function[
        gemm_q8_0_dp4a_b4_nvidia, dump_asm=Path("gemm_q8_0_dp4a_b4_nvidia.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dp4a_b4_nvidia"))

    _ = ctx.compile_function[
        gemm_q8_0_dp4a_out_f32_b3_nvidia,
        dump_asm=Path("gemm_q8_0_dp4a_out_f32_b3_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dp4a_out_f32_b3_nvidia"))

    _ = ctx.compile_function[
        gemm_q8_0_dp4a_out_f32_b4_nvidia,
        dump_asm=Path("gemm_q8_0_dp4a_out_f32_b4_nvidia.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dp4a_out_f32_b4_nvidia"))

    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b2,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b2.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b2"))

    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b3,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b3"))

    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b4,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b4"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_f16, dump_asm=Path("gemm_nvfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16"))

    # arch: nvidia
    _ = ctx.compile_function[gemm_f16, dump_asm=Path("gemm_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_f16"))
    _ = ctx.compile_function[
        gemm_f16_dot2_64x64, dump_asm=Path("gemm_f16_dot2_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_dot2_64x64"))
    _ = ctx.compile_function[
        gemm_f16_dot2_128x64, dump_asm=Path("gemm_f16_dot2_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_dot2_128x64"))
    _ = ctx.compile_function[
        gemm_f16_dot2_128x128, dump_asm=Path("gemm_f16_dot2_128x128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_dot2_128x128"))
    _ = ctx.compile_function[
        gemm_f16_dot2_256x64, dump_asm=Path("gemm_f16_dot2_256x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_dot2_256x64"))
    _ = ctx.compile_function[
        gemm_q8_0_dot4_64x64, dump_asm=Path("gemm_q8_0_dot4_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dot4_64x64"))
    _ = ctx.compile_function[
        gemm_q8_0_dot4_128x64, dump_asm=Path("gemm_q8_0_dot4_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dot4_128x64"))
    _ = ctx.compile_function[
        gemm_q8_0_dot4_128x128, dump_asm=Path("gemm_q8_0_dot4_128x128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dot4_128x128"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_q8_0_wmma_64x128, dump_asm=Path("gemm_q8_0_wmma_64x128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_wmma_64x128"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_q8_0_wmma_out_f32_64x128,
        dump_asm=Path("gemm_q8_0_wmma_out_f32_64x128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_wmma_out_f32_64x128"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_q8_0_wmma_16x64, dump_asm=Path("gemm_q8_0_wmma_16x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_wmma_16x64"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_q8_0_wmma_out_f32_16x64,
        dump_asm=Path("gemm_q8_0_wmma_out_f32_16x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_wmma_out_f32_16x64"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_q8_0_wmma_triplet_bm64, dump_asm=Path("gemm_q8_0_wmma_triplet_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_wmma_triplet_bm64"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_wmma_f16_bm32, dump_asm=Path("gemm_nvfp4_gguf_wmma_f16_bm32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_wmma_f16_bm32"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_wmma_f16_bm128, dump_asm=Path("gemm_nvfp4_gguf_wmma_f16_bm128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_wmma_f16_bm128"))
    # arch: amd:gfx11+
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_wmma_f16_bm128_bn32, dump_asm=Path("gemm_nvfp4_gguf_wmma_f16_bm128_bn32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_gguf_wmma_f16_bm128_bn32"))
    _ = ctx.compile_function[
        gemm_q4_k_dot4_64x64, dump_asm=Path("gemm_q4_k_dot4_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_dot4_64x64"))
    _ = ctx.compile_function[
        gemm_q4_k_dot4_128x64, dump_asm=Path("gemm_q4_k_dot4_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_dot4_128x64"))
    _ = ctx.compile_function[
        gemm_q4_k_dot4_128x128, dump_asm=Path("gemm_q4_k_dot4_128x128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_dot4_128x128"))
    _ = ctx.compile_function[
        gemm_q6_k_dot4_64x64, dump_asm=Path("gemm_q6_k_dot4_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q6_k_dot4_64x64"))
    _ = ctx.compile_function[
        gemm_q6_k_dot4_128x64, dump_asm=Path("gemm_q6_k_dot4_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q6_k_dot4_128x64"))
    _ = ctx.compile_function[
        gemm_nvfp4_dot4_64x64, dump_asm=Path("gemm_nvfp4_dot4_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_dot4_64x64"))
    _ = ctx.compile_function[
        gemm_nvfp4_dot4_128x64, dump_asm=Path("gemm_nvfp4_dot4_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_dot4_128x64"))
    _ = ctx.compile_function[
        gemm_f16_dot2_out_f32_64x64,
        dump_asm=Path("gemm_f16_dot2_out_f32_64x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_dot2_out_f32_64x64"))
    _ = ctx.compile_function[
        gemm_q8_0_dot4_out_f32_64x64,
        dump_asm=Path("gemm_q8_0_dot4_out_f32_64x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_dot4_out_f32_64x64"))
    _ = ctx.compile_function[
        gemm_q4_k_dot4_out_f32_64x64,
        dump_asm=Path("gemm_q4_k_dot4_out_f32_64x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_dot4_out_f32_64x64"))
    _ = ctx.compile_function[
        gemm_q6_k_dot4_out_f32_64x64,
        dump_asm=Path("gemm_q6_k_dot4_out_f32_64x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q6_k_dot4_out_f32_64x64"))
    _ = ctx.compile_function[
        gemm_q4_0_dot4_64x64, dump_asm=Path("gemm_q4_0_dot4_64x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_dot4_64x64"))
    _ = ctx.compile_function[
        gemm_q4_0_dot4_128x64, dump_asm=Path("gemm_q4_0_dot4_128x64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_dot4_128x64"))
    _ = ctx.compile_function[
        gemm_q4_0_dot4_128x128, dump_asm=Path("gemm_q4_0_dot4_128x128.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_dot4_128x128"))
    _ = ctx.compile_function[
        gemm_q4_0_dot4_out_f32_64x64,
        dump_asm=Path("gemm_q4_0_dot4_out_f32_64x64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_dot4_out_f32_64x64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_f16_bm64, dump_asm=Path("gemm_q8_0_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_f16_bm64, dump_asm=Path("gemm_nvfp4_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_nvfp4_f16_bm32, dump_asm=Path("gemm_nvfp4_f16_bm32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16_bm32"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_f16_bm64, dump_asm=Path("gemm_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_f16_out_f32, dump_asm=Path("gemm_f16_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_out_f32"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_f16_out_f32_bm64, dump_asm=Path("gemm_f16_out_f32_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_out_f32_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_f16_out_f32_bm32, dump_asm=Path("gemm_f16_out_f32_bm32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_out_f32_bm32"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_out_f32, dump_asm=Path("gemm_q8_0_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_out_f32"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_out_f32_bm64, dump_asm=Path("gemm_q8_0_out_f32_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_out_f32_bm64"))

    _ = ctx.compile_function[
        kv_append_batch_f16, dump_asm=Path("kv_append_batch_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_append_batch_f16"))

    _ = ctx.compile_function[
        kv_append_batch_device_pos_f16,
        dump_asm=Path("kv_append_batch_device_pos_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_append_batch_device_pos_f16"))
    _ = ctx.compile_function[
        kv_append_batch_segmented_f16,
        dump_asm=Path("kv_append_batch_segmented_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_append_batch_segmented_f16"))
    _ = ctx.compile_function[
        kv_append_batch_segmented_masked_f16,
        dump_asm=Path("kv_append_batch_segmented_masked_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_append_batch_segmented_masked_f16"))

    _ = ctx.compile_function[
        attn_prefill_f16_hd64, dump_asm=Path("attn_prefill_f16_hd64.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd64"))

    _ = ctx.compile_function[
        attn_prefill_f16_hd128, dump_asm=Path("attn_prefill_f16_hd128.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd128"))

    _ = ctx.compile_function[
        attn_prefill_f16_hd256, dump_asm=Path("attn_prefill_f16_hd256.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd256"))
    _ = ctx.compile_function[
        attn_prefill_f16_hd512, dump_asm=Path("attn_prefill_f16_hd512.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd512"))
    _ = ctx.compile_function[
        attn_prefill_segmented_f16_hd128,
        dump_asm=Path("attn_prefill_segmented_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_segmented_f16_hd128"))
    _ = ctx.compile_function[
        attn_prefill_segmented_f16_hd256,
        dump_asm=Path("attn_prefill_segmented_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_segmented_f16_hd256"))
    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_segmented_f16_hd128,
        dump_asm=Path("attn_prefill_fa_segmented_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_segmented_f16_hd128"))

    _ = ctx.compile_function[
        attn_prefill_device_pos_f16_hd256,
        dump_asm=Path("attn_prefill_device_pos_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_device_pos_f16_hd256"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_f16_hd256,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_mojo_f16_hd256"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256")
    )

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256_bk32,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256_bk32.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256_bk32")
    )

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_f16_hd256_bk32,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd256_bk32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_mojo_f16_hd256_bk32"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_f16_hd256_vtrans,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd256_vtrans.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_mojo_f16_hd256_vtrans"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans")
    )

    _ = ctx.compile_function[
        kv_append_batch_fp8, dump_asm=Path("kv_append_batch_fp8.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_append_batch_fp8"))

    _ = ctx.compile_function[
        attn_prefill_fp8_hd64, dump_asm=Path("attn_prefill_fp8_hd64.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fp8_hd64"))

    _ = ctx.compile_function[
        attn_prefill_fp8_hd128, dump_asm=Path("attn_prefill_fp8_hd128.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fp8_hd128"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_f16_hd64,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_mojo_f16_hd64"))

    # arch: nvidia
    _ = ctx.compile_function[
        attn_prefill_fa_f16_hd128,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_fa_mojo_f16_hd128"))

    _ = ctx.compile_function[qkv_post_f16, dump_asm=Path("qkv_post_f16.ptx")]()
    entries.append(_finalize(out_dir, "qkv_post_f16"))

    _ = ctx.compile_function[
        gemv_q4_k_f16_v2, dump_asm=Path("gemv_q4_k_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_f16_v2"))

    _ = ctx.compile_function[
        gemv_q4_k_out_f32_v2, dump_asm=Path("gemv_q4_k_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_k_f16, dump_asm=Path("gemm_q4_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_k_f16_bm64, dump_asm=Path("gemm_q4_k_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_f16_bm64"))

    _ = ctx.compile_function[
        quantize_act_q8_1, dump_asm=Path("quantize_act_q8_1.ptx")
    ]()
    entries.append(_finalize(out_dir, "quantize_act_q8_1"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma, dump_asm=Path("gemm_q8_0_i8mma.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_bm64, dump_asm=Path("gemm_q8_0_i8mma_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_big, dump_asm=Path("gemm_q8_0_i8mma_big.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_big"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_bm64,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_bm64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_triplet_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_bm64,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_bm64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_triplet_single_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_big,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_big.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q8_0_i8mma_triplet_single_big"))
    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_big_poststage,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_big_poststage.ptx"),
    ]()
    entries.append(
        _finalize(out_dir, "gemm_q8_0_i8mma_triplet_single_big_poststage")
    )

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_k_i8mma, dump_asm=Path("gemm_q4_k_i8mma.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_i8mma"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_k_i8mma_bm64, dump_asm=Path("gemm_q4_k_i8mma_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_i8mma_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_k_i8mma_big, dump_asm=Path("gemm_q4_k_i8mma_big.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_k_i8mma_big"))

    _ = ctx.compile_function[
        attn_decode_split_f16_hd64,
        dump_asm=Path("attn_decode_split_f16_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_f16_hd64"))

    _ = ctx.compile_function[
        attn_decode_split_f16_hd128,
        dump_asm=Path("attn_decode_split_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_f16_hd128"))
    _ = ctx.compile_function[
        attn_decode_split_f16_hd512,
        dump_asm=Path("attn_decode_split_f16_hd512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_f16_hd512"))

    _ = ctx.compile_function[
        attn_decode_split_fp8_hd64,
        dump_asm=Path("attn_decode_split_fp8_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_fp8_hd64"))

    _ = ctx.compile_function[
        attn_decode_split_fp8_hd128,
        dump_asm=Path("attn_decode_split_fp8_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_fp8_hd128"))

    _ = ctx.compile_function[
        attn_decode_combine_f16_hd64,
        dump_asm=Path("attn_decode_combine_f16_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_f16_hd64"))

    _ = ctx.compile_function[
        attn_decode_combine_f16_hd128,
        dump_asm=Path("attn_decode_combine_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_f16_hd128"))
    _ = ctx.compile_function[
        attn_decode_combine_f16_hd512,
        dump_asm=Path("attn_decode_combine_f16_hd512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_f16_hd512"))

    _ = ctx.compile_function[
        attn_decode_split_gqa4_f16_hd128,
        dump_asm=Path("attn_decode_split_gqa4_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_split_gqa4_f16_hd128"))

    _ = ctx.compile_function[
        attn_decode_combine_gqa2_f16_hd128,
        dump_asm=Path("attn_decode_combine_gqa2_f16_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_gqa2_f16_hd128"))

    _ = ctx.compile_function[
        gemv_norm_q8_0_f16, dump_asm=Path("gemv_norm_q8_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q8_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_nvfp4_f16, dump_asm=Path("gemv_norm_nvfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_nvfp4_f16"))

    _ = ctx.compile_function[
        gemv_norm_f16, dump_asm=Path("gemv_norm_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q8_0_f16, dump_asm=Path("gemv_norm_silu_q8_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q8_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_nvfp4_f16, dump_asm=Path("gemv_norm_silu_nvfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_nvfp4_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_f16, dump_asm=Path("gemv_norm_silu_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_f16"))

    _ = ctx.compile_function[
        gemv_residual_q8_0_f16, dump_asm=Path("gemv_residual_q8_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q8_0_f16"))

    _ = ctx.compile_function[
        gemv_residual_nvfp4_f16, dump_asm=Path("gemv_residual_nvfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_nvfp4_f16"))

    _ = ctx.compile_function[
        gemv_residual_f16, dump_asm=Path("gemv_residual_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_f16"))

    _ = ctx.compile_function[
        rmsnorm_h32_f16, dump_asm=Path("rmsnorm_h32_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_h32_f16"))

    _ = ctx.compile_function[
        gemv_q6_k_f16_v2, dump_asm=Path("gemv_q6_k_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_f16_v2"))

    _ = ctx.compile_function[
        gemv_q6_k_out_f32_v2, dump_asm=Path("gemv_q6_k_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q6_k_f16, dump_asm=Path("gemm_q6_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q6_k_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q6_k_f16_bm64, dump_asm=Path("gemm_q6_k_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q6_k_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_q4_k_f16, dump_asm=Path("gemv_norm_q4_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_q6_k_f16, dump_asm=Path("gemv_norm_q6_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q6_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q4_k_f16, dump_asm=Path("gemv_norm_silu_q4_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q6_k_f16, dump_asm=Path("gemv_norm_silu_q6_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q6_k_f16"))

    _ = ctx.compile_function[
        gemv_residual_q4_k_f16, dump_asm=Path("gemv_residual_q4_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_k_f16"))

    _ = ctx.compile_function[
        gemv_residual_q6_k_f16, dump_asm=Path("gemv_residual_q6_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q6_k_f16"))

    _ = ctx.compile_function[
        gemv_q8_0_dp4a_f16, dump_asm=Path("gemv_q8_0_dp4a_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q8_0_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_q4_k_dp4a_f16, dump_asm=Path("gemv_q4_k_dp4a_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_q4_k_dp4a_out_f32, dump_asm=Path("gemv_q4_k_dp4a_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_out_f32"))

    _ = ctx.compile_function[
        gemv_q4_k_dp4a_f16_gidx, dump_asm=Path("gemv_q4_k_dp4a_f16_gidx.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_f16_gidx"))

    _ = ctx.compile_function[
        gemv_q6_k_f16_gidx, dump_asm=Path("gemv_q6_k_f16_gidx.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_f16_gidx"))

    _ = ctx.compile_function[
        gemv_fp8_row_f16_v2, dump_asm=Path("gemv_fp8_row_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_fp8_row_f16_v2"))

    _ = ctx.compile_function[
        rmsnorm_head_f16, dump_asm=Path("rmsnorm_head_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rmsnorm_head_f16"))

    _ = ctx.compile_function[
        rope_interleaved_f16, dump_asm=Path("rope_interleaved_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "rope_interleaved_f16"))

    _ = ctx.compile_function[
        hadamard_bf16_f16, dump_asm=Path("hadamard_bf16_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "hadamard_bf16_f16"))

    _ = ctx.compile_function[
        act_quant_fp8_f16, dump_asm=Path("act_quant_fp8_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "act_quant_fp8_f16"))

    _ = ctx.compile_function[
        act_quant_fp4_f16, dump_asm=Path("act_quant_fp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "act_quant_fp4_f16"))

    _ = ctx.compile_function[
        compressor_pool_f16, dump_asm=Path("compressor_pool_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "compressor_pool_f16"))

    _ = ctx.compile_function[
        sparse_attn_f16, dump_asm=Path("sparse_attn_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "sparse_attn_f16"))

    _ = ctx.compile_function[
        hc_sinkhorn_f32, dump_asm=Path("hc_sinkhorn_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "hc_sinkhorn_f32"))

    _ = ctx.compile_function[hc_reduce_f16, dump_asm=Path("hc_reduce_f16.ptx")]()
    entries.append(_finalize(out_dir, "hc_reduce_f16"))

    _ = ctx.compile_function[hc_expand_f16, dump_asm=Path("hc_expand_f16.ptx")]()
    entries.append(_finalize(out_dir, "hc_expand_f16"))

    _ = ctx.compile_function[index_score_f16, dump_asm=Path("index_score_f16.ptx")]()
    entries.append(_finalize(out_dir, "index_score_f16"))

    _ = ctx.compile_function[
        compressor_add_ape_f32, dump_asm=Path("compressor_add_ape_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "compressor_add_ape_f32"))

    _ = ctx.compile_function[
        moe_gate_sqrtsoftplus_f16, dump_asm=Path("moe_gate_sqrtsoftplus_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "moe_gate_sqrtsoftplus_f16"))

    _ = ctx.compile_function[swiglu_limit_f16, dump_asm=Path("swiglu_limit_f16.ptx")]()
    entries.append(_finalize(out_dir, "swiglu_limit_f16"))

    _ = ctx.compile_function[rmsnorm_mix_f32, dump_asm=Path("rmsnorm_mix_f32.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_mix_f32"))

    _ = ctx.compile_function[hc_head_reduce_f16, dump_asm=Path("hc_head_reduce_f16.ptx")]()
    entries.append(_finalize(out_dir, "hc_head_reduce_f16"))

    _ = ctx.compile_function[
        gemv_norm_q8_0_dp4a_f16, dump_asm=Path("gemv_norm_q8_0_dp4a_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q8_0_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_norm_q4_k_dp4a_f16, dump_asm=Path("gemv_norm_q4_k_dp4a_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_norm_q6_k_dp4a_f16, dump_asm=Path("gemv_norm_q6_k_dp4a_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q6_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q8_0_dp4a_f16,
        dump_asm=Path("gemv_norm_silu_q8_0_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q8_0_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q4_k_dp4a_f16,
        dump_asm=Path("gemv_norm_silu_q4_k_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q6_k_dp4a_f16,
        dump_asm=Path("gemv_norm_silu_q6_k_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q6_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_residual_q8_0_dp4a_f16,
        dump_asm=Path("gemv_residual_q8_0_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q8_0_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_residual_q4_k_dp4a_f16,
        dump_asm=Path("gemv_residual_q4_k_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_residual_q6_k_dp4a_f16,
        dump_asm=Path("gemv_residual_q6_k_dp4a_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q6_k_dp4a_f16"))

    _ = ctx.compile_function[
        gemv_q6_k_dp4a_out_f32, dump_asm=Path("gemv_q6_k_dp4a_out_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_out_f32"))

    _ = ctx.compile_function[
        kv_pack_rot_hd64_b4, dump_asm=Path("kv_pack_rot_hd64_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_hd64_b4"))

    _ = ctx.compile_function[
        kv_pack_rot_hd64_b3, dump_asm=Path("kv_pack_rot_hd64_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_hd64_b3"))

    _ = ctx.compile_function[
        kv_pack_rot_hd128_b4, dump_asm=Path("kv_pack_rot_hd128_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_hd128_b4"))

    _ = ctx.compile_function[
        kv_pack_rot_hd128_b3, dump_asm=Path("kv_pack_rot_hd128_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_hd128_b3"))

    _ = ctx.compile_function[
        kv_pack_rot_from_cache_hd64_b4,
        dump_asm=Path("kv_pack_rot_from_cache_hd64_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_from_cache_hd64_b4"))

    _ = ctx.compile_function[
        kv_pack_rot_from_cache_hd64_b3,
        dump_asm=Path("kv_pack_rot_from_cache_hd64_b3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_from_cache_hd64_b3"))

    _ = ctx.compile_function[
        kv_pack_rot_from_cache_hd128_b4,
        dump_asm=Path("kv_pack_rot_from_cache_hd128_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_from_cache_hd128_b4"))

    _ = ctx.compile_function[
        kv_pack_rot_from_cache_hd128_b3,
        dump_asm=Path("kv_pack_rot_from_cache_hd128_b3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "kv_pack_rot_from_cache_hd128_b3"))

    _ = ctx.compile_function[
        attn_decode_rot_hd64_b4, dump_asm=Path("attn_decode_rot_hd64_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_rot_hd64_b4"))

    _ = ctx.compile_function[
        attn_decode_rot_hd64_b3, dump_asm=Path("attn_decode_rot_hd64_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_rot_hd64_b3"))

    _ = ctx.compile_function[
        attn_decode_rot_hd128_b4, dump_asm=Path("attn_decode_rot_hd128_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_rot_hd128_b4"))

    _ = ctx.compile_function[
        attn_decode_rot_hd128_b3, dump_asm=Path("attn_decode_rot_hd128_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_decode_rot_hd128_b3"))

    _ = ctx.compile_function[
        attn_decode_combine_rot_hd64,
        dump_asm=Path("attn_decode_combine_rot_hd64.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_rot_hd64"))

    _ = ctx.compile_function[
        attn_decode_combine_rot_hd128,
        dump_asm=Path("attn_decode_combine_rot_hd128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_decode_combine_rot_hd128"))

    _ = ctx.compile_function[
        attn_prefill_rot_hd64_b4, dump_asm=Path("attn_prefill_rot_hd64_b4.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_rot_hd64_b4"))

    _ = ctx.compile_function[
        attn_prefill_rot_hd64_b3, dump_asm=Path("attn_prefill_rot_hd64_b3.ptx")
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_rot_hd64_b3"))

    _ = ctx.compile_function[
        attn_prefill_rot_hd128_b4,
        dump_asm=Path("attn_prefill_rot_hd128_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_rot_hd128_b4"))

    _ = ctx.compile_function[
        attn_prefill_rot_hd128_b3,
        dump_asm=Path("attn_prefill_rot_hd128_b3.ptx"),
    ]()
    entries.append(_finalize(out_dir, "attn_prefill_rot_hd128_b3"))

    _ = ctx.compile_function[penalize_f32, dump_asm=Path("penalize_f32.ptx")]()
    entries.append(_finalize(out_dir, "penalize_f32"))

    _ = ctx.compile_function[
        penalized_argmax_f32, dump_asm=Path("penalized_argmax_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "penalized_argmax_f32"))

    _ = ctx.compile_function[
        penalize_histogram_f32, dump_asm=Path("penalize_histogram_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "penalize_histogram_f32"))

    _ = ctx.compile_function[
        argmax_partial_f32, dump_asm=Path("argmax_partial_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "argmax_partial_f32"))

    _ = ctx.compile_function[
        argmax_final_f32, dump_asm=Path("argmax_final_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "argmax_final_f32"))

    _ = ctx.compile_function[
        topk_partial_f32, dump_asm=Path("topk_partial_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "topk_partial_f32"))

    _ = ctx.compile_function[
        topk_final_f32, dump_asm=Path("topk_final_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "topk_final_f32"))

    _ = ctx.compile_function[
        penalize_batched_f32, dump_asm=Path("penalize_batched_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "penalize_batched_f32"))

    _ = ctx.compile_function[
        argmax_batched_f32, dump_asm=Path("argmax_batched_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "argmax_batched_f32"))


    _ = ctx.compile_function[
        moe_router_f16, dump_asm=Path("moe_router_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "moe_router_f16"))

    _ = ctx.compile_function[
        moe_scale_add_f16, dump_asm=Path("moe_scale_add_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "moe_scale_add_f16"))

    _ = ctx.compile_function[
        moe_scale_add_gidx_f16, dump_asm=Path("moe_scale_add_gidx_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "moe_scale_add_gidx_f16"))

    _ = ctx.compile_function[
        moe_sigmoid_f16_to_f32, dump_asm=Path("moe_sigmoid_f16_to_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "moe_sigmoid_f16_to_f32"))

    _ = ctx.compile_function[
        gemv_q5_k_f16_v2, dump_asm=Path("gemv_q5_k_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_k_f16_v2"))

    _ = ctx.compile_function[
        gemv_q5_k_out_f32_v2, dump_asm=Path("gemv_q5_k_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_k_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q3_k_f16_v2, dump_asm=Path("gemv_q3_k_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q3_k_f16_v2"))

    _ = ctx.compile_function[
        gemv_q3_k_out_f32_v2, dump_asm=Path("gemv_q3_k_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q3_k_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q2_k_f16_v2, dump_asm=Path("gemv_q2_k_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q2_k_f16_v2"))

    _ = ctx.compile_function[
        gemv_q2_k_out_f32_v2, dump_asm=Path("gemv_q2_k_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q2_k_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q4_0_f16_v2, dump_asm=Path("gemv_q4_0_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_0_f16_v2"))

    _ = ctx.compile_function[
        gemv_q4_0_out_f32_v2, dump_asm=Path("gemv_q4_0_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_0_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q4_1_f16_v2, dump_asm=Path("gemv_q4_1_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_1_f16_v2"))

    _ = ctx.compile_function[
        gemv_q4_1_out_f32_v2, dump_asm=Path("gemv_q4_1_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q4_1_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q5_0_f16_v2, dump_asm=Path("gemv_q5_0_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_0_f16_v2"))

    _ = ctx.compile_function[
        gemv_q5_0_out_f32_v2, dump_asm=Path("gemv_q5_0_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_0_out_f32_v2"))

    _ = ctx.compile_function[
        gemv_q5_1_f16_v2, dump_asm=Path("gemv_q5_1_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_1_f16_v2"))

    _ = ctx.compile_function[
        gemv_q5_1_out_f32_v2, dump_asm=Path("gemv_q5_1_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_q5_1_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_k_f16, dump_asm=Path("gemm_q5_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_k_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_k_f16_bm64, dump_asm=Path("gemm_q5_k_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_k_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q3_k_f16, dump_asm=Path("gemm_q3_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q3_k_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q3_k_f16_bm64, dump_asm=Path("gemm_q3_k_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q3_k_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q2_k_f16, dump_asm=Path("gemm_q2_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q2_k_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q2_k_f16_bm64, dump_asm=Path("gemm_q2_k_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q2_k_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_0_f16, dump_asm=Path("gemm_q4_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_0_f16_bm64, dump_asm=Path("gemm_q4_0_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_0_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_1_f16, dump_asm=Path("gemm_q4_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_1_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4_1_f16_bm64, dump_asm=Path("gemm_q4_1_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q4_1_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_0_f16, dump_asm=Path("gemm_q5_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_0_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_0_f16_bm64, dump_asm=Path("gemm_q5_0_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_0_f16_bm64"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_1_f16, dump_asm=Path("gemm_q5_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_1_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q5_1_f16_bm64, dump_asm=Path("gemm_q5_1_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_q5_1_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_q5_k_f16, dump_asm=Path("gemv_norm_q5_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_q3_k_f16, dump_asm=Path("gemv_norm_q3_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q3_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_q2_k_f16, dump_asm=Path("gemv_norm_q2_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q2_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_q4_0_f16, dump_asm=Path("gemv_norm_q4_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_q4_1_f16, dump_asm=Path("gemv_norm_q4_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_1_f16"))

    _ = ctx.compile_function[
        gemv_norm_q5_0_f16, dump_asm=Path("gemv_norm_q5_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_q5_1_f16, dump_asm=Path("gemv_norm_q5_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_1_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q5_k_f16, dump_asm=Path("gemv_norm_silu_q5_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q3_k_f16, dump_asm=Path("gemv_norm_silu_q3_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q3_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q2_k_f16, dump_asm=Path("gemv_norm_silu_q2_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q2_k_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q4_0_f16, dump_asm=Path("gemv_norm_silu_q4_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q4_1_f16, dump_asm=Path("gemv_norm_silu_q4_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_1_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q5_0_f16, dump_asm=Path("gemv_norm_silu_q5_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_0_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_q5_1_f16, dump_asm=Path("gemv_norm_silu_q5_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_1_f16"))

    _ = ctx.compile_function[
        gemv_residual_q5_k_f16, dump_asm=Path("gemv_residual_q5_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_k_f16"))

    _ = ctx.compile_function[
        gemv_residual_q3_k_f16, dump_asm=Path("gemv_residual_q3_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q3_k_f16"))

    _ = ctx.compile_function[
        gemv_residual_q2_k_f16, dump_asm=Path("gemv_residual_q2_k_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q2_k_f16"))

    _ = ctx.compile_function[
        gemv_residual_q4_0_f16, dump_asm=Path("gemv_residual_q4_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_0_f16"))

    _ = ctx.compile_function[
        gemv_residual_q4_1_f16, dump_asm=Path("gemv_residual_q4_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_1_f16"))

    _ = ctx.compile_function[
        gemv_residual_q5_0_f16, dump_asm=Path("gemv_residual_q5_0_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_0_f16"))

    _ = ctx.compile_function[
        gemv_residual_q5_1_f16, dump_asm=Path("gemv_residual_q5_1_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_1_f16"))

    _ = ctx.compile_function[
        gemv_iq4_nl_f16_v2, dump_asm=Path("gemv_iq4_nl_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq4_nl_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq4_nl_out_f32_v2, dump_asm=Path("gemv_iq4_nl_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq4_nl_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq4_nl_f16, dump_asm=Path("gemm_iq4_nl_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq4_nl_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq4_nl_f16_bm64, dump_asm=Path("gemm_iq4_nl_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq4_nl_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq4_nl_f16, dump_asm=Path("gemv_norm_iq4_nl_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq4_nl_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq4_nl_f16,
        dump_asm=Path("gemv_norm_silu_iq4_nl_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq4_nl_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq4_nl_f16, dump_asm=Path("gemv_residual_iq4_nl_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq4_nl_f16"))

    _ = ctx.compile_function[
        gemv_iq4_xs_f16_v2, dump_asm=Path("gemv_iq4_xs_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq4_xs_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq4_xs_out_f32_v2, dump_asm=Path("gemv_iq4_xs_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq4_xs_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq4_xs_f16, dump_asm=Path("gemm_iq4_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq4_xs_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq4_xs_f16_bm64, dump_asm=Path("gemm_iq4_xs_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq4_xs_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq4_xs_f16, dump_asm=Path("gemv_norm_iq4_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq4_xs_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq4_xs_f16,
        dump_asm=Path("gemv_norm_silu_iq4_xs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq4_xs_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq4_xs_f16, dump_asm=Path("gemv_residual_iq4_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq4_xs_f16"))

    _ = ctx.compile_function[
        gemv_mxfp4_f16_v2, dump_asm=Path("gemv_mxfp4_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_mxfp4_f16_v2"))

    _ = ctx.compile_function[
        gemv_mxfp4_out_f32_v2, dump_asm=Path("gemv_mxfp4_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_mxfp4_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_mxfp4_gguf_f16, dump_asm=Path("gemm_mxfp4_gguf_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_mxfp4_gguf_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_mxfp4_gguf_f16_bm64, dump_asm=Path("gemm_mxfp4_gguf_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_mxfp4_gguf_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_mxfp4_f16, dump_asm=Path("gemv_norm_mxfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_mxfp4_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_mxfp4_f16, dump_asm=Path("gemv_norm_silu_mxfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_mxfp4_f16"))

    _ = ctx.compile_function[
        gemv_residual_mxfp4_f16, dump_asm=Path("gemv_residual_mxfp4_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_mxfp4_f16"))

    _ = ctx.compile_function[
        gemv_iq2_xs_f16_v2, dump_asm=Path("gemv_iq2_xs_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_xs_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq2_xs_out_f32_v2, dump_asm=Path("gemv_iq2_xs_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_xs_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_xs_f16, dump_asm=Path("gemm_iq2_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_xs_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_xs_f16_bm64, dump_asm=Path("gemm_iq2_xs_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_xs_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq2_xs_f16, dump_asm=Path("gemv_norm_iq2_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_xs_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq2_xs_f16,
        dump_asm=Path("gemv_norm_silu_iq2_xs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_xs_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq2_xs_f16, dump_asm=Path("gemv_residual_iq2_xs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_xs_f16"))

    _ = ctx.compile_function[
        gemv_iq2_s_f16_v2, dump_asm=Path("gemv_iq2_s_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_s_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq2_s_out_f32_v2, dump_asm=Path("gemv_iq2_s_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_s_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_s_f16, dump_asm=Path("gemm_iq2_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_s_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_s_f16_bm64, dump_asm=Path("gemm_iq2_s_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_s_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq2_s_f16, dump_asm=Path("gemv_norm_iq2_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_s_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq2_s_f16, dump_asm=Path("gemv_norm_silu_iq2_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_s_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq2_s_f16, dump_asm=Path("gemv_residual_iq2_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_s_f16"))

    _ = ctx.compile_function[
        gemv_iq3_s_f16_v2, dump_asm=Path("gemv_iq3_s_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq3_s_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq3_s_out_f32_v2, dump_asm=Path("gemv_iq3_s_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq3_s_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq3_s_f16, dump_asm=Path("gemm_iq3_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq3_s_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq3_s_f16_bm64, dump_asm=Path("gemm_iq3_s_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq3_s_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq3_s_f16, dump_asm=Path("gemv_norm_iq3_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq3_s_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq3_s_f16, dump_asm=Path("gemv_norm_silu_iq3_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq3_s_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq3_s_f16, dump_asm=Path("gemv_residual_iq3_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq3_s_f16"))

    _ = ctx.compile_function[
        gemv_iq2_xxs_f16_v2, dump_asm=Path("gemv_iq2_xxs_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_xxs_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq2_xxs_out_f32_v2, dump_asm=Path("gemv_iq2_xxs_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq2_xxs_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_xxs_f16, dump_asm=Path("gemm_iq2_xxs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_xxs_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq2_xxs_f16_bm64, dump_asm=Path("gemm_iq2_xxs_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq2_xxs_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq2_xxs_f16, dump_asm=Path("gemv_norm_iq2_xxs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_xxs_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq2_xxs_f16,
        dump_asm=Path("gemv_norm_silu_iq2_xxs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_xxs_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq2_xxs_f16,
        dump_asm=Path("gemv_residual_iq2_xxs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_xxs_f16"))

    _ = ctx.compile_function[
        gemv_iq3_xxs_f16_v2, dump_asm=Path("gemv_iq3_xxs_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq3_xxs_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq3_xxs_out_f32_v2, dump_asm=Path("gemv_iq3_xxs_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq3_xxs_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq3_xxs_f16, dump_asm=Path("gemm_iq3_xxs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq3_xxs_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq3_xxs_f16_bm64, dump_asm=Path("gemm_iq3_xxs_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq3_xxs_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq3_xxs_f16, dump_asm=Path("gemv_norm_iq3_xxs_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq3_xxs_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq3_xxs_f16,
        dump_asm=Path("gemv_norm_silu_iq3_xxs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq3_xxs_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq3_xxs_f16,
        dump_asm=Path("gemv_residual_iq3_xxs_f16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq3_xxs_f16"))

    _ = ctx.compile_function[
        gemv_iq1_s_f16_v2, dump_asm=Path("gemv_iq1_s_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq1_s_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq1_s_out_f32_v2, dump_asm=Path("gemv_iq1_s_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq1_s_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq1_s_f16, dump_asm=Path("gemm_iq1_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq1_s_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq1_s_f16_bm64, dump_asm=Path("gemm_iq1_s_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq1_s_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq1_s_f16, dump_asm=Path("gemv_norm_iq1_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq1_s_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq1_s_f16, dump_asm=Path("gemv_norm_silu_iq1_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq1_s_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq1_s_f16, dump_asm=Path("gemv_residual_iq1_s_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq1_s_f16"))

    _ = ctx.compile_function[
        gemv_iq1_m_f16_v2, dump_asm=Path("gemv_iq1_m_f16_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq1_m_f16_v2"))

    _ = ctx.compile_function[
        gemv_iq1_m_out_f32_v2, dump_asm=Path("gemv_iq1_m_out_f32_v2.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_iq1_m_out_f32_v2"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq1_m_f16, dump_asm=Path("gemm_iq1_m_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq1_m_f16"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_iq1_m_f16_bm64, dump_asm=Path("gemm_iq1_m_f16_bm64.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemm_iq1_m_f16_bm64"))

    _ = ctx.compile_function[
        gemv_norm_iq1_m_f16, dump_asm=Path("gemv_norm_iq1_m_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_iq1_m_f16"))

    _ = ctx.compile_function[
        gemv_norm_silu_iq1_m_f16, dump_asm=Path("gemv_norm_silu_iq1_m_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq1_m_f16"))

    _ = ctx.compile_function[
        gemv_residual_iq1_m_f16, dump_asm=Path("gemv_residual_iq1_m_f16.ptx")
    ]()
    entries.append(_finalize(out_dir, "gemv_residual_iq1_m_f16"))

    _ = ctx.compile_function[conv1d_f32, dump_asm=Path("conv1d_f32.ptx")]()
    entries.append(_finalize(out_dir, "conv1d_f32"))

    _ = ctx.compile_function[relu_f32, dump_asm=Path("relu_f32.ptx")]()
    entries.append(_finalize(out_dir, "relu_f32"))

    _ = ctx.compile_function[sigmoid_f32, dump_asm=Path("sigmoid_f32.ptx")]()
    entries.append(_finalize(out_dir, "sigmoid_f32"))

    _ = ctx.compile_function[add_f32, dump_asm=Path("add_f32.ptx")]()
    entries.append(_finalize(out_dir, "add_f32"))

    _ = ctx.compile_function[pow_f32, dump_asm=Path("pow_f32.ptx")]()
    entries.append(_finalize(out_dir, "pow_f32"))

    _ = ctx.compile_function[sqrt_f32, dump_asm=Path("sqrt_f32.ptx")]()
    entries.append(_finalize(out_dir, "sqrt_f32"))

    _ = ctx.compile_function[
        reduce_mean_f32, dump_asm=Path("reduce_mean_f32.ptx")
    ]()
    entries.append(_finalize(out_dir, "reduce_mean_f32"))

    _ = ctx.compile_function[lstm_f32, dump_asm=Path("lstm_f32.ptx")]()
    entries.append(_finalize(out_dir, "lstm_f32"))

    _ = ctx.compile_function[
        quantize_act_fp8, dump_asm=Path("quantize_act_fp8.ptx")
    ]()
    entries.append(_finalize(out_dir, "quantize_act_fp8"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[gemm_fp8_f16, dump_asm=Path("gemm_fp8_f16.ptx")]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_f16"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_f16_bm64, dump_asm=Path("gemm_fp8_f16_bm64.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_f16_bm64"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_f16_big, dump_asm=Path("gemm_fp8_f16_big.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_f16_big"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_4096_4096, dump_asm=Path("gemm_fp8_mod_4096_4096.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_4096_4096"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_1024_4096, dump_asm=Path("gemm_fp8_mod_1024_4096.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_1024_4096"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_14336_4096, dump_asm=Path("gemm_fp8_mod_14336_4096.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_14336_4096"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_4096_14336, dump_asm=Path("gemm_fp8_mod_4096_14336.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_4096_14336"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_11264_4096, dump_asm=Path("gemm_fp8_mod_11264_4096.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_11264_4096"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_4096_11264, dump_asm=Path("gemm_fp8_mod_4096_11264.ptx")
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_4096_11264"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_4096_4096_bn256,
        dump_asm=Path("gemm_fp8_mod_4096_4096_bn256.ptx"),
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_4096_4096_bn256"))

    # arch: nvidia:sm_89+
    _ = ctx.compile_function[
        gemm_fp8_mod_11264_4096_bn256,
        dump_asm=Path("gemm_fp8_mod_11264_4096_bn256.ptx"),
    ]()
    entries.append(_finalize_fp8(out_dir, "gemm_fp8_mod_11264_4096_bn256"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m128,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m128"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m256,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m256"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m512,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m512"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m1024,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m1024.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m1024"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m2048,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m2048.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m2048"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_4096_m4096,
        dump_asm=Path("gemm_q4k_i8_native_4096_4096_m4096.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m4096"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m128,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m128"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m256,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m256"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m512,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m512"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m1024,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m1024.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m1024"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m2048,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m2048.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m2048"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_1024_4096_m4096,
        dump_asm=Path("gemm_q4k_i8_native_1024_4096_m4096.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m4096"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m128,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m128"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m256,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m256"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m512,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m512"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m1024,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m1024.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m1024"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m2048,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m2048.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m2048"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_14336_4096_m4096,
        dump_asm=Path("gemm_q4k_i8_native_14336_4096_m4096.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m4096"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m128,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m128.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m128"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m256,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m256.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m256"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m512,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m512.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m512"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m1024,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m1024.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m1024"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m2048,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m2048.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m2048"))

    # arch: nvidia
    _ = ctx.compile_function[
        gemm_q4k_i8_native_4096_14336_m4096,
        dump_asm=Path("gemm_q4k_i8_native_4096_14336_m4096.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m4096"))


    var manifest = (
        String('{\n  "arch": "') + arch + String('",\n  "kernels": {\n')
    )
    for i in range(len(entries)):
        manifest += entries[i]
        if i + 1 < len(entries):
            manifest += ","
        manifest += "\n"
    manifest += String("  }\n}\n")
    (out_dir / "manifest.json").write_text(manifest)
    print("manifest written:", String(out_dir / "manifest.json"))
