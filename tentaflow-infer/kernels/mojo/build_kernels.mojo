# ===== File: build_kernels.mojo — AOT kernel compiler: Mojo → PTX + manifest =====
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
from src.norm import rmsnorm_f16, rmsnorm_residual_f16
from src.activation import silu_mul_f16
from src.rope import rope_neox_f16
from src.gemv import gemv_q8_0_f16, gemv_f16
from src.attention import attn_decode_f16_hd64, attn_decode_f16_hd128
from src.nvfp4 import gemv_nvfp4_f16
from src.misc import gather_rows_f16, gemv_f16_out_f32, gemv_q8_0_out_f32
from src.layernorm import layernorm_f16, layernorm_residual_f16
from src.conv import gelu_f16, conv1d_k3_f16
from src.attn_full import attn_full_f16_hd64, attn_full_f16_hd128
from src.gemv import gemv_f16_bias
from src.kv_append import kv_append_f16
from src.gemv2 import gemv_q8_0_f16_v2, gemv_q8_0_out_f32_v2, gemv_nvfp4_f16_v2, gemv_f16_out_f32_v2
from src.gemv2 import gemv_q4_k_f16_v2, gemv_q4_k_out_f32_v2
from src.gemv2 import gemv_q6_k_f16_v2, gemv_q6_k_out_f32_v2
from src.gemm import gemm_q8_0_f16, gemm_nvfp4_f16, gemm_f16
from src.gemm import gemm_q8_0_f16_bm64, gemm_nvfp4_f16_bm64, gemm_f16_bm64
from src.gemm import gemm_q4_k_f16, gemm_q4_k_f16_bm64
from src.gemm import gemm_q6_k_f16, gemm_q6_k_f16_bm64
from src.prefill import kv_append_batch_f16, attn_prefill_f16_hd64, attn_prefill_f16_hd128
from src.prefill import kv_append_batch_fp8, attn_prefill_fp8_hd64, attn_prefill_fp8_hd128
from src.qkv_post import qkv_post_f16
from src.attention import attn_decode_split_f16_hd64, attn_decode_split_f16_hd128
from src.attention import attn_decode_split_fp8_hd64, attn_decode_split_fp8_hd128
from src.attention import attn_decode_combine_f16_hd64, attn_decode_combine_f16_hd128
from src.decode_fused import gemv_norm_q8_0_f16, gemv_norm_nvfp4_f16, gemv_norm_f16
from src.decode_fused import gemv_norm_silu_q8_0_f16, gemv_norm_silu_nvfp4_f16, gemv_norm_silu_f16
from src.decode_fused import gemv_residual_q8_0_f16, gemv_residual_nvfp4_f16, gemv_residual_f16
from src.decode_fused import rmsnorm_h32_f16
from src.decode_fused import gemv_norm_q4_k_f16, gemv_norm_q6_k_f16
from src.decode_fused import gemv_norm_silu_q4_k_f16, gemv_norm_silu_q6_k_f16
from src.decode_fused import gemv_residual_q4_k_f16, gemv_residual_q6_k_f16
from src.decode_dp4a import gemv_q8_0_dp4a_f16, gemv_q4_k_dp4a_f16, gemv_q4_k_dp4a_out_f32
from src.decode_dp4a import gemv_norm_q8_0_dp4a_f16, gemv_norm_q4_k_dp4a_f16, gemv_norm_q6_k_dp4a_f16
from src.decode_dp4a import gemv_norm_silu_q8_0_dp4a_f16, gemv_norm_silu_q4_k_dp4a_f16, gemv_norm_silu_q6_k_dp4a_f16
from src.decode_dp4a import gemv_residual_q8_0_dp4a_f16, gemv_residual_q4_k_dp4a_f16
from src.decode_dp4a import gemv_residual_q6_k_dp4a_f16, gemv_q6_k_dp4a_out_f32
from src.gemv2 import gemv_q5_k_f16_v2, gemv_q5_k_out_f32_v2
from src.gemm import gemm_q5_k_f16, gemm_q5_k_f16_bm64
from src.decode_fused import gemv_norm_q5_k_f16, gemv_norm_silu_q5_k_f16, gemv_residual_q5_k_f16
from src.gemv2 import gemv_q3_k_f16_v2, gemv_q3_k_out_f32_v2
from src.gemm import gemm_q3_k_f16, gemm_q3_k_f16_bm64
from src.decode_fused import gemv_norm_q3_k_f16, gemv_norm_silu_q3_k_f16, gemv_residual_q3_k_f16
from src.gemv2 import gemv_q2_k_f16_v2, gemv_q2_k_out_f32_v2
from src.gemm import gemm_q2_k_f16, gemm_q2_k_f16_bm64
from src.decode_fused import gemv_norm_q2_k_f16, gemv_norm_silu_q2_k_f16, gemv_residual_q2_k_f16
from src.gemv2 import gemv_q4_0_f16_v2, gemv_q4_0_out_f32_v2
from src.gemm import gemm_q4_0_f16, gemm_q4_0_f16_bm64
from src.decode_fused import gemv_norm_q4_0_f16, gemv_norm_silu_q4_0_f16, gemv_residual_q4_0_f16
from src.gemv2 import gemv_q4_1_f16_v2, gemv_q4_1_out_f32_v2
from src.gemm import gemm_q4_1_f16, gemm_q4_1_f16_bm64
from src.decode_fused import gemv_norm_q4_1_f16, gemv_norm_silu_q4_1_f16, gemv_residual_q4_1_f16
from src.gemv2 import gemv_q5_0_f16_v2, gemv_q5_0_out_f32_v2
from src.gemm import gemm_q5_0_f16, gemm_q5_0_f16_bm64
from src.decode_fused import gemv_norm_q5_0_f16, gemv_norm_silu_q5_0_f16, gemv_residual_q5_0_f16
from src.gemv2 import gemv_q5_1_f16_v2, gemv_q5_1_out_f32_v2
from src.gemm import gemm_q5_1_f16, gemm_q5_1_f16_bm64
from src.decode_fused import gemv_norm_q5_1_f16, gemv_norm_silu_q5_1_f16, gemv_residual_q5_1_f16
from src.gemv2 import gemv_iq4_nl_f16_v2, gemv_iq4_nl_out_f32_v2
from src.gemv2 import gemv_iq4_xs_f16_v2, gemv_iq4_xs_out_f32_v2
from src.gemv2 import gemv_mxfp4_f16_v2, gemv_mxfp4_out_f32_v2
from src.gemm import gemm_iq4_nl_f16, gemm_iq4_nl_f16_bm64
from src.gemm import gemm_iq4_xs_f16, gemm_iq4_xs_f16_bm64
from src.gemm import gemm_mxfp4_gguf_f16, gemm_mxfp4_gguf_f16_bm64
from src.decode_fused import gemv_norm_iq4_nl_f16, gemv_norm_silu_iq4_nl_f16, gemv_residual_iq4_nl_f16
from src.decode_fused import gemv_norm_iq4_xs_f16, gemv_norm_silu_iq4_xs_f16, gemv_residual_iq4_xs_f16
from src.decode_fused import gemv_norm_mxfp4_f16, gemv_norm_silu_mxfp4_f16, gemv_residual_mxfp4_f16
from src.gemv2 import gemv_iq2_xs_f16_v2, gemv_iq2_xs_out_f32_v2
from src.gemv2 import gemv_iq2_s_f16_v2, gemv_iq2_s_out_f32_v2
from src.gemv2 import gemv_iq3_s_f16_v2, gemv_iq3_s_out_f32_v2
from src.gemm import gemm_iq2_xs_f16, gemm_iq2_xs_f16_bm64
from src.gemm import gemm_iq2_s_f16, gemm_iq2_s_f16_bm64
from src.gemm import gemm_iq3_s_f16, gemm_iq3_s_f16_bm64
from src.decode_fused import gemv_norm_iq2_xs_f16, gemv_norm_silu_iq2_xs_f16, gemv_residual_iq2_xs_f16
from src.decode_fused import gemv_norm_iq2_s_f16, gemv_norm_silu_iq2_s_f16, gemv_residual_iq2_s_f16
from src.decode_fused import gemv_norm_iq3_s_f16, gemv_norm_silu_iq3_s_f16, gemv_residual_iq3_s_f16
from src.gemv2 import gemv_iq2_xxs_f16_v2, gemv_iq2_xxs_out_f32_v2
from src.gemv2 import gemv_iq3_xxs_f16_v2, gemv_iq3_xxs_out_f32_v2
from src.gemv2 import gemv_iq1_s_f16_v2, gemv_iq1_s_out_f32_v2
from src.gemv2 import gemv_iq1_m_f16_v2, gemv_iq1_m_out_f32_v2
from src.gemm import gemm_iq2_xxs_f16, gemm_iq2_xxs_f16_bm64
from src.gemm import gemm_iq3_xxs_f16, gemm_iq3_xxs_f16_bm64
from src.gemm import gemm_iq1_s_f16, gemm_iq1_s_f16_bm64
from src.gemm import gemm_iq1_m_f16, gemm_iq1_m_f16_bm64
from src.decode_fused import gemv_norm_iq2_xxs_f16, gemv_norm_silu_iq2_xxs_f16, gemv_residual_iq2_xxs_f16
from src.decode_fused import gemv_norm_iq3_xxs_f16, gemv_norm_silu_iq3_xxs_f16, gemv_residual_iq3_xxs_f16
from src.decode_fused import gemv_norm_iq1_s_f16, gemv_norm_silu_iq1_s_f16, gemv_residual_iq1_s_f16
from src.decode_fused import gemv_norm_iq1_m_f16, gemv_norm_silu_iq1_m_f16, gemv_residual_iq1_m_f16
from src.sampling import penalize_f32, argmax_partial_f32, argmax_final_f32
from src.sampling import topk_partial_f32, topk_final_f32


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
    return String(text[byte = i + marker.byte_length():j])


def _finalize(out_dir: Path, name: StringSlice) raises -> String:
    # Relocate the statically-named dump into the per-arch directory and
    # return its manifest fragment.
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    final.write_text(tmp.read_text())
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

    _ = ctx.compile_function[rmsnorm_residual_f16, dump_asm=Path("rmsnorm_residual_f16.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_residual_f16"))

    _ = ctx.compile_function[silu_mul_f16, dump_asm=Path("silu_mul_f16.ptx")]()
    entries.append(_finalize(out_dir, "silu_mul_f16"))

    _ = ctx.compile_function[rope_neox_f16, dump_asm=Path("rope_neox_f16.ptx")]()
    entries.append(_finalize(out_dir, "rope_neox_f16"))

    _ = ctx.compile_function[gemv_q8_0_f16, dump_asm=Path("gemv_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_f16"))

    _ = ctx.compile_function[gemv_f16, dump_asm=Path("gemv_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16"))

    _ = ctx.compile_function[attn_decode_f16_hd64, dump_asm=Path("attn_decode_f16_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd64"))

    _ = ctx.compile_function[attn_decode_f16_hd128, dump_asm=Path("attn_decode_f16_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_f16_hd128"))

    _ = ctx.compile_function[gemv_nvfp4_f16, dump_asm=Path("gemv_nvfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_f16"))

    _ = ctx.compile_function[gather_rows_f16, dump_asm=Path("gather_rows_f16.ptx")]()
    entries.append(_finalize(out_dir, "gather_rows_f16"))

    _ = ctx.compile_function[gemv_f16_out_f32, dump_asm=Path("gemv_f16_out_f32.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16_out_f32"))

    _ = ctx.compile_function[gemv_q8_0_out_f32, dump_asm=Path("gemv_q8_0_out_f32.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_out_f32"))

    _ = ctx.compile_function[layernorm_f16, dump_asm=Path("layernorm_f16.ptx")]()
    entries.append(_finalize(out_dir, "layernorm_f16"))

    _ = ctx.compile_function[layernorm_residual_f16, dump_asm=Path("layernorm_residual_f16.ptx")]()
    entries.append(_finalize(out_dir, "layernorm_residual_f16"))

    _ = ctx.compile_function[gelu_f16, dump_asm=Path("gelu_f16.ptx")]()
    entries.append(_finalize(out_dir, "gelu_f16"))

    _ = ctx.compile_function[conv1d_k3_f16, dump_asm=Path("conv1d_k3_f16.ptx")]()
    entries.append(_finalize(out_dir, "conv1d_k3_f16"))

    _ = ctx.compile_function[attn_full_f16_hd64, dump_asm=Path("attn_full_f16_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_full_f16_hd64"))

    _ = ctx.compile_function[attn_full_f16_hd128, dump_asm=Path("attn_full_f16_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_full_f16_hd128"))

    _ = ctx.compile_function[gemv_f16_bias, dump_asm=Path("gemv_f16_bias.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16_bias"))

    _ = ctx.compile_function[kv_append_f16, dump_asm=Path("kv_append_f16.ptx")]()
    entries.append(_finalize(out_dir, "kv_append_f16"))

    _ = ctx.compile_function[gemv_q8_0_f16_v2, dump_asm=Path("gemv_q8_0_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_f16_v2"))

    _ = ctx.compile_function[gemv_q8_0_out_f32_v2, dump_asm=Path("gemv_q8_0_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_out_f32_v2"))

    _ = ctx.compile_function[gemv_nvfp4_f16_v2, dump_asm=Path("gemv_nvfp4_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_nvfp4_f16_v2"))

    _ = ctx.compile_function[gemv_f16_out_f32_v2, dump_asm=Path("gemv_f16_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16_out_f32_v2"))

    _ = ctx.compile_function[gemm_q8_0_f16, dump_asm=Path("gemm_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16"))

    _ = ctx.compile_function[gemm_nvfp4_f16, dump_asm=Path("gemm_nvfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16"))

    _ = ctx.compile_function[gemm_f16, dump_asm=Path("gemm_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_f16"))

    _ = ctx.compile_function[gemm_q8_0_f16_bm64, dump_asm=Path("gemm_q8_0_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q8_0_f16_bm64"))

    _ = ctx.compile_function[gemm_nvfp4_f16_bm64, dump_asm=Path("gemm_nvfp4_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16_bm64"))

    _ = ctx.compile_function[gemm_f16_bm64, dump_asm=Path("gemm_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_f16_bm64"))

    _ = ctx.compile_function[kv_append_batch_f16, dump_asm=Path("kv_append_batch_f16.ptx")]()
    entries.append(_finalize(out_dir, "kv_append_batch_f16"))

    _ = ctx.compile_function[attn_prefill_f16_hd64, dump_asm=Path("attn_prefill_f16_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd64"))

    _ = ctx.compile_function[attn_prefill_f16_hd128, dump_asm=Path("attn_prefill_f16_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_prefill_f16_hd128"))

    _ = ctx.compile_function[kv_append_batch_fp8, dump_asm=Path("kv_append_batch_fp8.ptx")]()
    entries.append(_finalize(out_dir, "kv_append_batch_fp8"))

    _ = ctx.compile_function[attn_prefill_fp8_hd64, dump_asm=Path("attn_prefill_fp8_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_prefill_fp8_hd64"))

    _ = ctx.compile_function[attn_prefill_fp8_hd128, dump_asm=Path("attn_prefill_fp8_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_prefill_fp8_hd128"))

    _ = ctx.compile_function[qkv_post_f16, dump_asm=Path("qkv_post_f16.ptx")]()
    entries.append(_finalize(out_dir, "qkv_post_f16"))

    _ = ctx.compile_function[gemv_q4_k_f16_v2, dump_asm=Path("gemv_q4_k_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_k_f16_v2"))

    _ = ctx.compile_function[gemv_q4_k_out_f32_v2, dump_asm=Path("gemv_q4_k_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_k_out_f32_v2"))

    _ = ctx.compile_function[gemm_q4_k_f16, dump_asm=Path("gemm_q4_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_k_f16"))

    _ = ctx.compile_function[gemm_q4_k_f16_bm64, dump_asm=Path("gemm_q4_k_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_k_f16_bm64"))

    _ = ctx.compile_function[attn_decode_split_f16_hd64, dump_asm=Path("attn_decode_split_f16_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_split_f16_hd64"))

    _ = ctx.compile_function[attn_decode_split_f16_hd128, dump_asm=Path("attn_decode_split_f16_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_split_f16_hd128"))

    _ = ctx.compile_function[attn_decode_split_fp8_hd64, dump_asm=Path("attn_decode_split_fp8_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_split_fp8_hd64"))

    _ = ctx.compile_function[attn_decode_split_fp8_hd128, dump_asm=Path("attn_decode_split_fp8_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_split_fp8_hd128"))

    _ = ctx.compile_function[attn_decode_combine_f16_hd64, dump_asm=Path("attn_decode_combine_f16_hd64.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_combine_f16_hd64"))

    _ = ctx.compile_function[attn_decode_combine_f16_hd128, dump_asm=Path("attn_decode_combine_f16_hd128.ptx")]()
    entries.append(_finalize(out_dir, "attn_decode_combine_f16_hd128"))

    _ = ctx.compile_function[gemv_norm_q8_0_f16, dump_asm=Path("gemv_norm_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q8_0_f16"))

    _ = ctx.compile_function[gemv_norm_nvfp4_f16, dump_asm=Path("gemv_norm_nvfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_nvfp4_f16"))

    _ = ctx.compile_function[gemv_norm_f16, dump_asm=Path("gemv_norm_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q8_0_f16, dump_asm=Path("gemv_norm_silu_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q8_0_f16"))

    _ = ctx.compile_function[gemv_norm_silu_nvfp4_f16, dump_asm=Path("gemv_norm_silu_nvfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_nvfp4_f16"))

    _ = ctx.compile_function[gemv_norm_silu_f16, dump_asm=Path("gemv_norm_silu_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_f16"))

    _ = ctx.compile_function[gemv_residual_q8_0_f16, dump_asm=Path("gemv_residual_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q8_0_f16"))

    _ = ctx.compile_function[gemv_residual_nvfp4_f16, dump_asm=Path("gemv_residual_nvfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_nvfp4_f16"))

    _ = ctx.compile_function[gemv_residual_f16, dump_asm=Path("gemv_residual_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_f16"))

    _ = ctx.compile_function[rmsnorm_h32_f16, dump_asm=Path("rmsnorm_h32_f16.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_h32_f16"))

    _ = ctx.compile_function[gemv_q6_k_f16_v2, dump_asm=Path("gemv_q6_k_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q6_k_f16_v2"))

    _ = ctx.compile_function[gemv_q6_k_out_f32_v2, dump_asm=Path("gemv_q6_k_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q6_k_out_f32_v2"))

    _ = ctx.compile_function[gemm_q6_k_f16, dump_asm=Path("gemm_q6_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q6_k_f16"))

    _ = ctx.compile_function[gemm_q6_k_f16_bm64, dump_asm=Path("gemm_q6_k_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q6_k_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_q4_k_f16, dump_asm=Path("gemv_norm_q4_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_k_f16"))

    _ = ctx.compile_function[gemv_norm_q6_k_f16, dump_asm=Path("gemv_norm_q6_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q6_k_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q4_k_f16, dump_asm=Path("gemv_norm_silu_q4_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_k_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q6_k_f16, dump_asm=Path("gemv_norm_silu_q6_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q6_k_f16"))

    _ = ctx.compile_function[gemv_residual_q4_k_f16, dump_asm=Path("gemv_residual_q4_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_k_f16"))

    _ = ctx.compile_function[gemv_residual_q6_k_f16, dump_asm=Path("gemv_residual_q6_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q6_k_f16"))

    _ = ctx.compile_function[gemv_q8_0_dp4a_f16, dump_asm=Path("gemv_q8_0_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_dp4a_f16"))

    _ = ctx.compile_function[gemv_q4_k_dp4a_f16, dump_asm=Path("gemv_q4_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_q4_k_dp4a_out_f32, dump_asm=Path("gemv_q4_k_dp4a_out_f32.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_k_dp4a_out_f32"))

    _ = ctx.compile_function[gemv_norm_q8_0_dp4a_f16, dump_asm=Path("gemv_norm_q8_0_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q8_0_dp4a_f16"))

    _ = ctx.compile_function[gemv_norm_q4_k_dp4a_f16, dump_asm=Path("gemv_norm_q4_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_norm_q6_k_dp4a_f16, dump_asm=Path("gemv_norm_q6_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q6_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q8_0_dp4a_f16, dump_asm=Path("gemv_norm_silu_q8_0_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q8_0_dp4a_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q4_k_dp4a_f16, dump_asm=Path("gemv_norm_silu_q4_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q6_k_dp4a_f16, dump_asm=Path("gemv_norm_silu_q6_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q6_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_residual_q8_0_dp4a_f16, dump_asm=Path("gemv_residual_q8_0_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q8_0_dp4a_f16"))

    _ = ctx.compile_function[gemv_residual_q4_k_dp4a_f16, dump_asm=Path("gemv_residual_q4_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_residual_q6_k_dp4a_f16, dump_asm=Path("gemv_residual_q6_k_dp4a_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q6_k_dp4a_f16"))

    _ = ctx.compile_function[gemv_q6_k_dp4a_out_f32, dump_asm=Path("gemv_q6_k_dp4a_out_f32.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q6_k_dp4a_out_f32"))

    _ = ctx.compile_function[penalize_f32, dump_asm=Path("penalize_f32.ptx")]()
    entries.append(_finalize(out_dir, "penalize_f32"))

    _ = ctx.compile_function[argmax_partial_f32, dump_asm=Path("argmax_partial_f32.ptx")]()
    entries.append(_finalize(out_dir, "argmax_partial_f32"))

    _ = ctx.compile_function[argmax_final_f32, dump_asm=Path("argmax_final_f32.ptx")]()
    entries.append(_finalize(out_dir, "argmax_final_f32"))

    _ = ctx.compile_function[topk_partial_f32, dump_asm=Path("topk_partial_f32.ptx")]()
    entries.append(_finalize(out_dir, "topk_partial_f32"))

    _ = ctx.compile_function[topk_final_f32, dump_asm=Path("topk_final_f32.ptx")]()
    entries.append(_finalize(out_dir, "topk_final_f32"))

    _ = ctx.compile_function[gemv_q5_k_f16_v2, dump_asm=Path("gemv_q5_k_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_k_f16_v2"))

    _ = ctx.compile_function[gemv_q5_k_out_f32_v2, dump_asm=Path("gemv_q5_k_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_k_out_f32_v2"))

    _ = ctx.compile_function[gemv_q3_k_f16_v2, dump_asm=Path("gemv_q3_k_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q3_k_f16_v2"))

    _ = ctx.compile_function[gemv_q3_k_out_f32_v2, dump_asm=Path("gemv_q3_k_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q3_k_out_f32_v2"))

    _ = ctx.compile_function[gemv_q2_k_f16_v2, dump_asm=Path("gemv_q2_k_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q2_k_f16_v2"))

    _ = ctx.compile_function[gemv_q2_k_out_f32_v2, dump_asm=Path("gemv_q2_k_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q2_k_out_f32_v2"))

    _ = ctx.compile_function[gemv_q4_0_f16_v2, dump_asm=Path("gemv_q4_0_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_0_f16_v2"))

    _ = ctx.compile_function[gemv_q4_0_out_f32_v2, dump_asm=Path("gemv_q4_0_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_0_out_f32_v2"))

    _ = ctx.compile_function[gemv_q4_1_f16_v2, dump_asm=Path("gemv_q4_1_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_1_f16_v2"))

    _ = ctx.compile_function[gemv_q4_1_out_f32_v2, dump_asm=Path("gemv_q4_1_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q4_1_out_f32_v2"))

    _ = ctx.compile_function[gemv_q5_0_f16_v2, dump_asm=Path("gemv_q5_0_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_0_f16_v2"))

    _ = ctx.compile_function[gemv_q5_0_out_f32_v2, dump_asm=Path("gemv_q5_0_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_0_out_f32_v2"))

    _ = ctx.compile_function[gemv_q5_1_f16_v2, dump_asm=Path("gemv_q5_1_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_1_f16_v2"))

    _ = ctx.compile_function[gemv_q5_1_out_f32_v2, dump_asm=Path("gemv_q5_1_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q5_1_out_f32_v2"))

    _ = ctx.compile_function[gemm_q5_k_f16, dump_asm=Path("gemm_q5_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_k_f16"))

    _ = ctx.compile_function[gemm_q5_k_f16_bm64, dump_asm=Path("gemm_q5_k_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_k_f16_bm64"))

    _ = ctx.compile_function[gemm_q3_k_f16, dump_asm=Path("gemm_q3_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q3_k_f16"))

    _ = ctx.compile_function[gemm_q3_k_f16_bm64, dump_asm=Path("gemm_q3_k_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q3_k_f16_bm64"))

    _ = ctx.compile_function[gemm_q2_k_f16, dump_asm=Path("gemm_q2_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q2_k_f16"))

    _ = ctx.compile_function[gemm_q2_k_f16_bm64, dump_asm=Path("gemm_q2_k_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q2_k_f16_bm64"))

    _ = ctx.compile_function[gemm_q4_0_f16, dump_asm=Path("gemm_q4_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_0_f16"))

    _ = ctx.compile_function[gemm_q4_0_f16_bm64, dump_asm=Path("gemm_q4_0_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_0_f16_bm64"))

    _ = ctx.compile_function[gemm_q4_1_f16, dump_asm=Path("gemm_q4_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_1_f16"))

    _ = ctx.compile_function[gemm_q4_1_f16_bm64, dump_asm=Path("gemm_q4_1_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4_1_f16_bm64"))

    _ = ctx.compile_function[gemm_q5_0_f16, dump_asm=Path("gemm_q5_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_0_f16"))

    _ = ctx.compile_function[gemm_q5_0_f16_bm64, dump_asm=Path("gemm_q5_0_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_0_f16_bm64"))

    _ = ctx.compile_function[gemm_q5_1_f16, dump_asm=Path("gemm_q5_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_1_f16"))

    _ = ctx.compile_function[gemm_q5_1_f16_bm64, dump_asm=Path("gemm_q5_1_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q5_1_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_q5_k_f16, dump_asm=Path("gemv_norm_q5_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_k_f16"))

    _ = ctx.compile_function[gemv_norm_q3_k_f16, dump_asm=Path("gemv_norm_q3_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q3_k_f16"))

    _ = ctx.compile_function[gemv_norm_q2_k_f16, dump_asm=Path("gemv_norm_q2_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q2_k_f16"))

    _ = ctx.compile_function[gemv_norm_q4_0_f16, dump_asm=Path("gemv_norm_q4_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_0_f16"))

    _ = ctx.compile_function[gemv_norm_q4_1_f16, dump_asm=Path("gemv_norm_q4_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q4_1_f16"))

    _ = ctx.compile_function[gemv_norm_q5_0_f16, dump_asm=Path("gemv_norm_q5_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_0_f16"))

    _ = ctx.compile_function[gemv_norm_q5_1_f16, dump_asm=Path("gemv_norm_q5_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_q5_1_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q5_k_f16, dump_asm=Path("gemv_norm_silu_q5_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_k_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q3_k_f16, dump_asm=Path("gemv_norm_silu_q3_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q3_k_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q2_k_f16, dump_asm=Path("gemv_norm_silu_q2_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q2_k_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q4_0_f16, dump_asm=Path("gemv_norm_silu_q4_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_0_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q4_1_f16, dump_asm=Path("gemv_norm_silu_q4_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q4_1_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q5_0_f16, dump_asm=Path("gemv_norm_silu_q5_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_0_f16"))

    _ = ctx.compile_function[gemv_norm_silu_q5_1_f16, dump_asm=Path("gemv_norm_silu_q5_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_q5_1_f16"))

    _ = ctx.compile_function[gemv_residual_q5_k_f16, dump_asm=Path("gemv_residual_q5_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_k_f16"))

    _ = ctx.compile_function[gemv_residual_q3_k_f16, dump_asm=Path("gemv_residual_q3_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q3_k_f16"))

    _ = ctx.compile_function[gemv_residual_q2_k_f16, dump_asm=Path("gemv_residual_q2_k_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q2_k_f16"))

    _ = ctx.compile_function[gemv_residual_q4_0_f16, dump_asm=Path("gemv_residual_q4_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_0_f16"))

    _ = ctx.compile_function[gemv_residual_q4_1_f16, dump_asm=Path("gemv_residual_q4_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q4_1_f16"))

    _ = ctx.compile_function[gemv_residual_q5_0_f16, dump_asm=Path("gemv_residual_q5_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_0_f16"))

    _ = ctx.compile_function[gemv_residual_q5_1_f16, dump_asm=Path("gemv_residual_q5_1_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_q5_1_f16"))

    _ = ctx.compile_function[gemv_iq4_nl_f16_v2, dump_asm=Path("gemv_iq4_nl_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq4_nl_f16_v2"))

    _ = ctx.compile_function[gemv_iq4_nl_out_f32_v2, dump_asm=Path("gemv_iq4_nl_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq4_nl_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq4_nl_f16, dump_asm=Path("gemm_iq4_nl_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq4_nl_f16"))

    _ = ctx.compile_function[gemm_iq4_nl_f16_bm64, dump_asm=Path("gemm_iq4_nl_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq4_nl_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq4_nl_f16, dump_asm=Path("gemv_norm_iq4_nl_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq4_nl_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq4_nl_f16, dump_asm=Path("gemv_norm_silu_iq4_nl_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq4_nl_f16"))

    _ = ctx.compile_function[gemv_residual_iq4_nl_f16, dump_asm=Path("gemv_residual_iq4_nl_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq4_nl_f16"))

    _ = ctx.compile_function[gemv_iq4_xs_f16_v2, dump_asm=Path("gemv_iq4_xs_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq4_xs_f16_v2"))

    _ = ctx.compile_function[gemv_iq4_xs_out_f32_v2, dump_asm=Path("gemv_iq4_xs_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq4_xs_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq4_xs_f16, dump_asm=Path("gemm_iq4_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq4_xs_f16"))

    _ = ctx.compile_function[gemm_iq4_xs_f16_bm64, dump_asm=Path("gemm_iq4_xs_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq4_xs_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq4_xs_f16, dump_asm=Path("gemv_norm_iq4_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq4_xs_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq4_xs_f16, dump_asm=Path("gemv_norm_silu_iq4_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq4_xs_f16"))

    _ = ctx.compile_function[gemv_residual_iq4_xs_f16, dump_asm=Path("gemv_residual_iq4_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq4_xs_f16"))

    _ = ctx.compile_function[gemv_mxfp4_f16_v2, dump_asm=Path("gemv_mxfp4_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_mxfp4_f16_v2"))

    _ = ctx.compile_function[gemv_mxfp4_out_f32_v2, dump_asm=Path("gemv_mxfp4_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_mxfp4_out_f32_v2"))

    _ = ctx.compile_function[gemm_mxfp4_gguf_f16, dump_asm=Path("gemm_mxfp4_gguf_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_mxfp4_gguf_f16"))

    _ = ctx.compile_function[gemm_mxfp4_gguf_f16_bm64, dump_asm=Path("gemm_mxfp4_gguf_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_mxfp4_gguf_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_mxfp4_f16, dump_asm=Path("gemv_norm_mxfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_mxfp4_f16"))

    _ = ctx.compile_function[gemv_norm_silu_mxfp4_f16, dump_asm=Path("gemv_norm_silu_mxfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_mxfp4_f16"))

    _ = ctx.compile_function[gemv_residual_mxfp4_f16, dump_asm=Path("gemv_residual_mxfp4_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_mxfp4_f16"))

    _ = ctx.compile_function[gemv_iq2_xs_f16_v2, dump_asm=Path("gemv_iq2_xs_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_xs_f16_v2"))

    _ = ctx.compile_function[gemv_iq2_xs_out_f32_v2, dump_asm=Path("gemv_iq2_xs_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_xs_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq2_xs_f16, dump_asm=Path("gemm_iq2_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_xs_f16"))

    _ = ctx.compile_function[gemm_iq2_xs_f16_bm64, dump_asm=Path("gemm_iq2_xs_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_xs_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq2_xs_f16, dump_asm=Path("gemv_norm_iq2_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_xs_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq2_xs_f16, dump_asm=Path("gemv_norm_silu_iq2_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_xs_f16"))

    _ = ctx.compile_function[gemv_residual_iq2_xs_f16, dump_asm=Path("gemv_residual_iq2_xs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_xs_f16"))

    _ = ctx.compile_function[gemv_iq2_s_f16_v2, dump_asm=Path("gemv_iq2_s_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_s_f16_v2"))

    _ = ctx.compile_function[gemv_iq2_s_out_f32_v2, dump_asm=Path("gemv_iq2_s_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_s_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq2_s_f16, dump_asm=Path("gemm_iq2_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_s_f16"))

    _ = ctx.compile_function[gemm_iq2_s_f16_bm64, dump_asm=Path("gemm_iq2_s_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_s_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq2_s_f16, dump_asm=Path("gemv_norm_iq2_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_s_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq2_s_f16, dump_asm=Path("gemv_norm_silu_iq2_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_s_f16"))

    _ = ctx.compile_function[gemv_residual_iq2_s_f16, dump_asm=Path("gemv_residual_iq2_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_s_f16"))

    _ = ctx.compile_function[gemv_iq3_s_f16_v2, dump_asm=Path("gemv_iq3_s_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq3_s_f16_v2"))

    _ = ctx.compile_function[gemv_iq3_s_out_f32_v2, dump_asm=Path("gemv_iq3_s_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq3_s_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq3_s_f16, dump_asm=Path("gemm_iq3_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq3_s_f16"))

    _ = ctx.compile_function[gemm_iq3_s_f16_bm64, dump_asm=Path("gemm_iq3_s_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq3_s_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq3_s_f16, dump_asm=Path("gemv_norm_iq3_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq3_s_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq3_s_f16, dump_asm=Path("gemv_norm_silu_iq3_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq3_s_f16"))

    _ = ctx.compile_function[gemv_residual_iq3_s_f16, dump_asm=Path("gemv_residual_iq3_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq3_s_f16"))

    _ = ctx.compile_function[gemv_iq2_xxs_f16_v2, dump_asm=Path("gemv_iq2_xxs_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_xxs_f16_v2"))

    _ = ctx.compile_function[gemv_iq2_xxs_out_f32_v2, dump_asm=Path("gemv_iq2_xxs_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq2_xxs_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq2_xxs_f16, dump_asm=Path("gemm_iq2_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_xxs_f16"))

    _ = ctx.compile_function[gemm_iq2_xxs_f16_bm64, dump_asm=Path("gemm_iq2_xxs_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq2_xxs_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq2_xxs_f16, dump_asm=Path("gemv_norm_iq2_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq2_xxs_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq2_xxs_f16, dump_asm=Path("gemv_norm_silu_iq2_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq2_xxs_f16"))

    _ = ctx.compile_function[gemv_residual_iq2_xxs_f16, dump_asm=Path("gemv_residual_iq2_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq2_xxs_f16"))

    _ = ctx.compile_function[gemv_iq3_xxs_f16_v2, dump_asm=Path("gemv_iq3_xxs_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq3_xxs_f16_v2"))

    _ = ctx.compile_function[gemv_iq3_xxs_out_f32_v2, dump_asm=Path("gemv_iq3_xxs_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq3_xxs_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq3_xxs_f16, dump_asm=Path("gemm_iq3_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq3_xxs_f16"))

    _ = ctx.compile_function[gemm_iq3_xxs_f16_bm64, dump_asm=Path("gemm_iq3_xxs_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq3_xxs_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq3_xxs_f16, dump_asm=Path("gemv_norm_iq3_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq3_xxs_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq3_xxs_f16, dump_asm=Path("gemv_norm_silu_iq3_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq3_xxs_f16"))

    _ = ctx.compile_function[gemv_residual_iq3_xxs_f16, dump_asm=Path("gemv_residual_iq3_xxs_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq3_xxs_f16"))

    _ = ctx.compile_function[gemv_iq1_s_f16_v2, dump_asm=Path("gemv_iq1_s_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq1_s_f16_v2"))

    _ = ctx.compile_function[gemv_iq1_s_out_f32_v2, dump_asm=Path("gemv_iq1_s_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq1_s_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq1_s_f16, dump_asm=Path("gemm_iq1_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq1_s_f16"))

    _ = ctx.compile_function[gemm_iq1_s_f16_bm64, dump_asm=Path("gemm_iq1_s_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq1_s_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq1_s_f16, dump_asm=Path("gemv_norm_iq1_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq1_s_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq1_s_f16, dump_asm=Path("gemv_norm_silu_iq1_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq1_s_f16"))

    _ = ctx.compile_function[gemv_residual_iq1_s_f16, dump_asm=Path("gemv_residual_iq1_s_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq1_s_f16"))

    _ = ctx.compile_function[gemv_iq1_m_f16_v2, dump_asm=Path("gemv_iq1_m_f16_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq1_m_f16_v2"))

    _ = ctx.compile_function[gemv_iq1_m_out_f32_v2, dump_asm=Path("gemv_iq1_m_out_f32_v2.ptx")]()
    entries.append(_finalize(out_dir, "gemv_iq1_m_out_f32_v2"))

    _ = ctx.compile_function[gemm_iq1_m_f16, dump_asm=Path("gemm_iq1_m_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq1_m_f16"))

    _ = ctx.compile_function[gemm_iq1_m_f16_bm64, dump_asm=Path("gemm_iq1_m_f16_bm64.ptx")]()
    entries.append(_finalize(out_dir, "gemm_iq1_m_f16_bm64"))

    _ = ctx.compile_function[gemv_norm_iq1_m_f16, dump_asm=Path("gemv_norm_iq1_m_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_iq1_m_f16"))

    _ = ctx.compile_function[gemv_norm_silu_iq1_m_f16, dump_asm=Path("gemv_norm_silu_iq1_m_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_norm_silu_iq1_m_f16"))

    _ = ctx.compile_function[gemv_residual_iq1_m_f16, dump_asm=Path("gemv_residual_iq1_m_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_residual_iq1_m_f16"))

    var manifest = String('{\n  "arch": "') + arch + String('",\n  "kernels": {\n')
    for i in range(len(entries)):
        manifest += entries[i]
        if i + 1 < len(entries):
            manifest += ","
        manifest += "\n"
    manifest += String("  }\n}\n")
    (out_dir / "manifest.json").write_text(manifest)
    print("manifest written:", String(out_dir / "manifest.json"))
