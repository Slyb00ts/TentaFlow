# =============================================================================
# Plik: test_nvfp4_ct_bm16.mojo
# Opis: Sprawdza kompilację modułu BM16 dla naturalnego układu NVFP4.
# Przykład: mojo build test_nvfp4_ct_bm16.mojo
# =============================================================================

from src.nvfp4_ct_direct import (
    gemm_nvfp4_ct_bm16_down_m16,
    gemm_nvfp4_ct_bm16_down_m4,
    gemm_nvfp4_ct_bm16_down_m8,
    gemm_nvfp4_ct_bm16_gateup_m16,
    gemm_nvfp4_ct_bm16_gateup_m4,
    gemm_nvfp4_ct_bm16_gateup_m8,
    gemm_nvfp4_ct_bm16_o_m16,
    gemm_nvfp4_ct_bm16_o_m4,
    gemm_nvfp4_ct_bm16_o_m8,
    gemm_nvfp4_ct_bm16_qkv_m16,
    gemm_nvfp4_ct_bm16_qkv_m4,
    gemm_nvfp4_ct_bm16_qkv_m8,
    nvfp4_ct_split_pipeline_supported,
    reduce_nvfp4_direct_down,
)


def main():
    comptime assert not nvfp4_ct_split_pipeline_supported[13, 4, 3]()
    comptime assert nvfp4_ct_split_pipeline_supported[32, 3, 4]()
    comptime assert nvfp4_ct_split_pipeline_supported[32, 4, 4]()
    comptime assert nvfp4_ct_split_pipeline_supported[32, 1, 3]()
    comptime assert nvfp4_ct_split_pipeline_supported[88, 4, 4]()
    _ = gemm_nvfp4_ct_bm16_down_m16
    _ = gemm_nvfp4_ct_bm16_down_m4
    _ = gemm_nvfp4_ct_bm16_down_m8
    _ = gemm_nvfp4_ct_bm16_gateup_m16
    _ = gemm_nvfp4_ct_bm16_gateup_m4
    _ = gemm_nvfp4_ct_bm16_gateup_m8
    _ = gemm_nvfp4_ct_bm16_o_m16
    _ = gemm_nvfp4_ct_bm16_o_m4
    _ = gemm_nvfp4_ct_bm16_o_m8
    _ = gemm_nvfp4_ct_bm16_qkv_m16
    _ = gemm_nvfp4_ct_bm16_qkv_m4
    _ = gemm_nvfp4_ct_bm16_qkv_m8
    _ = reduce_nvfp4_direct_down
