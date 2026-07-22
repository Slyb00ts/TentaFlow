# =============================================================================
# Plik: build_mtp_stage.mojo
# Opis: Izolowany kompilator kernela metadanych kroku MTP.
# Przykład: pixi run mojo build_mtp_stage.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.mtp import mtp_stage_step, mtp_norm_join_shifted_f16, mtp_project_joined_q8_f16, mtp_verify_decide_segmented, mtp_select_row_segmented_f16
from src.prefill import kv_append_batch_segmented_f16
from src.attention import attn_verify_segmented_f16_hd256, attn_verify_segmented_f16_hd256_warp32
from src.deltanet_verify import deltanet_prepare_segmented_f16, deltanet_gated_scan_segmented_d128_f16, deltanet_commit_checkpoint_segmented_f32, deltanet_gated_scan_segmented_shared_d128_f16, deltanet_commit_recompute_segmented_shared_d128_f32
from src.nvfp4_gguf_batch import gemm_nvfp4_gguf_f16_b8_nvidia
from src.q8_0_batch import gemm_q8_0_i8mma_b8, gemm_q8_0_f16_exact_out_f32_b8


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[mtp_stage_step, dump_asm=Path("mtp_stage_step.ptx")]()
    source = Path("mtp_stage_step.ptx")
    target = out_dir / "mtp_stage_step.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd256_warp32,
        dump_asm=Path("attn_verify_segmented_f16_hd256_warp32.ptx"),
    ]()
    source = Path("attn_verify_segmented_f16_hd256_warp32.ptx")
    target = out_dir / "attn_verify_segmented_f16_hd256_warp32.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b8,
        dump_asm=Path("gemm_q8_0_i8mma_b8.ptx"),
    ]()
    source = Path("gemm_q8_0_i8mma_b8.ptx")
    target = out_dir / "gemm_q8_0_i8mma_b8.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b8,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b8.ptx"),
    ]()
    source = Path("gemm_q8_0_f16_exact_out_f32_b8.ptx")
    target = out_dir / "gemm_q8_0_f16_exact_out_f32_b8.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b8_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b8_nvidia.ptx"),
    ]()
    source = Path("gemm_nvfp4_gguf_f16_b8_nvidia.ptx")
    target = out_dir / "gemm_nvfp4_gguf_f16_b8_nvidia.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_select_row_segmented_f16,
        dump_asm=Path("mtp_select_row_segmented_f16.ptx"),
    ]()
    source = Path("mtp_select_row_segmented_f16.ptx")
    target = out_dir / "mtp_select_row_segmented_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        deltanet_prepare_segmented_f16,
        dump_asm=Path("deltanet_prepare_segmented_f16.ptx"),
    ]()
    source = Path("deltanet_prepare_segmented_f16.ptx")
    target = out_dir / "deltanet_prepare_segmented_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        deltanet_gated_scan_segmented_d128_f16,
        dump_asm=Path("deltanet_gated_scan_segmented_d128_f16.ptx"),
    ]()
    source = Path("deltanet_gated_scan_segmented_d128_f16.ptx")
    target = out_dir / "deltanet_gated_scan_segmented_d128_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        deltanet_gated_scan_segmented_shared_d128_f16,
        dump_asm=Path("deltanet_gated_scan_segmented_shared_d128_f16.ptx"),
    ]()
    source = Path("deltanet_gated_scan_segmented_shared_d128_f16.ptx")
    target = out_dir / "deltanet_gated_scan_segmented_shared_d128_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        deltanet_commit_recompute_segmented_shared_d128_f32,
        dump_asm=Path("deltanet_commit_recompute_segmented_shared_d128_f32.ptx"),
    ]()
    source = Path("deltanet_commit_recompute_segmented_shared_d128_f32.ptx")
    target = out_dir / "deltanet_commit_recompute_segmented_shared_d128_f32.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        deltanet_commit_checkpoint_segmented_f32,
        dump_asm=Path("deltanet_commit_checkpoint_segmented_f32.ptx"),
    ]()
    source = Path("deltanet_commit_checkpoint_segmented_f32.ptx")
    target = out_dir / "deltanet_commit_checkpoint_segmented_f32.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_project_joined_q8_f16,
        dump_asm=Path("mtp_project_joined_q8_f16.ptx"),
    ]()
    source = Path("mtp_project_joined_q8_f16.ptx")
    target = out_dir / "mtp_project_joined_q8_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_norm_join_shifted_f16,
        dump_asm=Path("mtp_norm_join_shifted_f16.ptx"),
    ]()
    source = Path("mtp_norm_join_shifted_f16.ptx")
    target = out_dir / "mtp_norm_join_shifted_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_verify_decide_segmented,
        dump_asm=Path("mtp_verify_decide_segmented.ptx"),
    ]()
    source = Path("mtp_verify_decide_segmented.ptx")
    target = out_dir / "mtp_verify_decide_segmented.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        kv_append_batch_segmented_f16,
        dump_asm=Path("kv_append_batch_segmented_f16.ptx"),
    ]()
    source = Path("kv_append_batch_segmented_f16.ptx")
    target = out_dir / "kv_append_batch_segmented_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd256,
        dump_asm=Path("attn_verify_segmented_f16_hd256.ptx"),
    ]()
    source = Path("attn_verify_segmented_f16_hd256.ptx")
    target = out_dir / "attn_verify_segmented_f16_hd256.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
