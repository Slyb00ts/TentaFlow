#!/usr/bin/env python3
# =============================================================================
# Plik: test_build_kernel_catalog.py
# Opis: Sprawdza kompletnosc strukturalnego katalogu kerneli Mojo.
# Przykład: python scripts/test_build_kernel_catalog.py
# =============================================================================

import json
import unittest
import tempfile
from pathlib import Path

import build_kernel_catalog


class KernelCatalogTest(unittest.TestCase):
    def test_cubins_are_required_only_for_exact_sm89(self):
        self.assertFalse(build_kernel_catalog.requires_sm89_cubins("sm_86"))
        self.assertTrue(build_kernel_catalog.requires_sm89_cubins("sm_89"))
        self.assertFalse(build_kernel_catalog.requires_sm89_cubins("sm_90"))

    def test_atomic_exchange_replaces_whole_arch_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / "sm_89"
            destination.mkdir()
            (destination / "old.ptx").write_text("old")
            staged = root / ".staging" / "sm_89"
            staged.mkdir(parents=True)
            (staged / "new.ptx").write_text("new")
            build_kernel_catalog.publish_arch(staged, destination)
            self.assertEqual((destination / "new.ptx").read_text(), "new")
            self.assertFalse((destination / "old.ptx").exists())
            self.assertEqual((staged / "old.ptx").read_text(), "old")

    def test_catalog_matches_committed_manifest(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        catalog_names = {artifact for _, _, artifact in kernels}
        manifest_path = build_kernel_catalog.ROOT / "build" / "sm_89" / "manifest.json"
        manifest_names = set(json.loads(manifest_path.read_text())["kernels"])
        self.assertEqual(len(kernels), len(manifest_names))
        self.assertEqual(catalog_names, manifest_names)

    def test_optimized_artifacts_are_registered(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        catalog_names = {artifact for _, _, artifact in kernels}
        expected = {
            "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1",
            "gemm_nvfp4_gguf_mma_f16_bm128_bn128",
            "gemm_nvfp4_gguf_mma_f16_bm128_prefetch",
            "repack_nvfp4_ct_s0_n64k128_into",
            "gemv_nvfp4_ct_s0_n64k128_f16",
            "pack_nvfp4_ct_s0_fp8",
            "gemm_nvfp4_ct_bm16_qkv_m4",
            "gemm_nvfp4_ct_bm16_qkv_m8",
            "gemm_nvfp4_ct_bm16_qkv_m16",
            "gemm_nvfp4_ct_bm16_o_m4",
            "gemm_nvfp4_ct_bm16_o_m8",
            "gemm_nvfp4_ct_bm16_o_m16",
            "gemm_nvfp4_ct_bm16_gateup_m4",
            "gemm_nvfp4_ct_bm16_gateup_m8",
            "gemm_nvfp4_ct_bm16_gateup_m16",
            "gemm_nvfp4_ct_bm16_down_m4",
            "gemm_nvfp4_ct_bm16_down_m8",
            "gemm_nvfp4_ct_bm16_down_m16",
            "gemm_nvfp4_ct_bm32_qkv_m24",
            "gemm_nvfp4_ct_bm32_qkv_m32",
            "gemm_nvfp4_ct_bm32_o_m24",
            "gemm_nvfp4_ct_bm32_o_m32",
            "gemm_nvfp4_ct_bm32_gateup_m24",
            "gemm_nvfp4_ct_bm32_gateup_m32",
            "gemm_nvfp4_ct_bm32_down_m24",
            "gemm_nvfp4_ct_bm32_down_m32",
            "reduce_nvfp4_ct_bm16",
            "gemm_q8_0_i8mma_triplet_bm64",
            "gemm_q8_0_i8mma_triplet_single_bm64",
            "gemm_q8_0_i8mma_triplet_single_big",
            "gemm_q8_0_i8mma_triplet_single_big_poststage",
            "attn_prefill_fa_mojo_f16_hd256",
            "attn_prefill_fa_mojo_device_pos_f16_hd256",
            "attn_prefill_fa_mojo_device_pos_f16_hd256_bk32",
            "attn_prefill_fa_mojo_f16_hd256_bk32",
            "attn_prefill_fa_mojo_f16_hd256_vtrans",
            "attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans",
            "deltanet_prepare_tiled_d128_c4_f16",
            "deltanet_gated_scan_persistent_d128_f16",
        }
        self.assertTrue(expected <= catalog_names)


if __name__ == "__main__":
    unittest.main()
