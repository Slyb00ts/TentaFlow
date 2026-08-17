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


# Katalogi, ktore sa W TYLE za katalogiem zrodel, bo nie ma pod reka karty, na
# ktorej mozna je przebudowac. Mojo kompiluje WYLACZNIE dla lokalnego GPU
# (`MOJO_TARGET_ACCELERATOR` nie dziala), wiec kazdy zestaw wymaga swojej karty.
#
#   sm_89 — brakuje kerneli dodanych po ostatnim buildzie na Adzie (DeepSeek,
#           rodzina GEMM na instrukcjach dot). Na sm_89..sm_120 zglosza sie one
#           jako „kernel not loaded". Do przebudowania na RTX 4090.
#
# Wpis usuwa sie razem z przebudowaniem — test zaciska sie wtedy sam.
KNOWN_STALE = {"sm_89"}


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
        """KAZDY zbudowany katalog musi rownac sie zasiegowi swojej architektury.

        Wczesniej sprawdzany byl tylko sm_89 i tylko przeciw CALEMU katalogowi;
        odkad kernel moze byc zawezony do rodziny kart, kontraktem jest zbior
        W ZASIEGU danej architektury, a sprawdzamy wszystkie zbudowane.
        """
        kernels, _ = build_kernel_catalog.parse_catalog()
        build_root = build_kernel_catalog.ROOT / "build"
        checked = 0
        for manifest_path in sorted(build_root.glob("*/manifest.json")):
            manifest = json.loads(manifest_path.read_text())
            arch = manifest["arch"]
            self.assertEqual(arch, manifest_path.parent.name)
            expected = {
                artifact
                for _, _, artifact, scope in kernels
                if build_kernel_catalog.scope_allows(scope, arch)
            }
            built = set(manifest["kernels"])
            if arch in KNOWN_STALE:
                # Katalog moze byc W TYLE, ale nigdy rozjechany: nadmiarowy
                # artefakt znaczylby, ze manifest opisuje kernel, ktorego katalog
                # juz nie zna, i tego nie wolno przepuscic nawet tutaj.
                self.assertTrue(built <= expected, f"{arch}: nadmiarowe {built - expected}")
                self.assertTrue(built, arch)
            else:
                self.assertEqual(expected, built, arch)
            checked += 1
        self.assertGreater(checked, 0, "brak zbudowanego katalogu do sprawdzenia")

    def test_optimized_artifacts_are_registered(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        catalog_names = {artifact for _, _, artifact, _ in kernels}
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


class ArchScopeTest(unittest.TestCase):
    """Zasieg architektury: co ma sie zbudowac na ktorej karcie.

    To jest jedyne miejsce, w ktorym „kernel nie dla tej karty" jest odrozniane
    od „kernel sie nie kompiluje", wiec regula musi byc przypieta testem.
    """

    def test_brak_deklaracji_znaczy_przenosny(self):
        for arch in ("sm_89", "sm_121", "gfx1030", "gfx1100", "gfx1201"):
            self.assertTrue(build_kernel_catalog.scope_allows(None, arch))

    def test_producent_bez_wersji_obejmuje_cala_rodzine(self):
        scope = ("nvidia",)
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_89"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_121"))
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1100"))

    def test_amd_porownuje_pokolenia_a_nie_pelne_numery(self):
        # gfx1030 i gfx1036 to jedno pokolenie (RDNA2), gfx1100 to nastepne.
        scope = ("amd:gfx11+",)
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1030"))
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1036"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1100"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1201"))

    def test_zasieg_bez_plusa_jest_dokladny(self):
        scope = ("amd:gfx11",)
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1100"))
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1201"))

    def test_nvidia_z_dolnym_progiem(self):
        scope = ("nvidia:sm_89+",)
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "sm_80"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_89"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_121"))

    def test_zasieg_moze_wymieniac_obie_rodziny(self):
        scope = ("nvidia:sm_89+", "amd:gfx12+")
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_121"))
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1201"))
        self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1100"))

    def test_warianty_blackwella_z_litera_to_to_samo_pokolenie(self):
        # `sm_121a` to sm_121 z instrukcjami zawezonymi do architektury —
        # nadzbior, wiec w porzadkowaniu pokolen litera nic nie zmienia.
        self.assertEqual(build_kernel_catalog.parse_arch("sm_121a"), ("nvidia", 121))
        self.assertEqual(build_kernel_catalog.parse_arch("sm_120a"), ("nvidia", 120))
        scope = ("nvidia:sm_89+",)
        self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_121a"))

    def test_nieznane_nazwy_architektur_sa_odrzucane_glosno(self):
        # CDNA ma inny schemat numeracji; zgadnieta regula ustawilaby karty w
        # zlej kolejnosci i zrobilaby to cicho.
        for arch in ("gfx90a", "gfx942", "rdna3", "sm_", "gfx"):
            with self.assertRaises(RuntimeError):
                build_kernel_catalog.parse_arch(arch)

    def test_zla_deklaracja_w_katalogu_konczy_parsowanie_bledem(self):
        for text in (" ", " intel", " nvidia:gfx1100"):
            with self.assertRaises(RuntimeError):
                scope = build_kernel_catalog.parse_scope(text)
                build_kernel_catalog.scope_allows(scope, "sm_89")

    def test_katalog_daje_kazdej_karcie_wlasny_podzbior(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        counts = {
            arch: sum(
                1 for item in kernels if build_kernel_catalog.scope_allows(item[3], arch)
            )
            for arch in ("sm_89", "gfx1030", "gfx1100")
        }
        for arch, count in counts.items():
            self.assertGreater(count, 0, arch)
            # Gdyby ktoras karta dostawala CALY katalog, deklaracje przestalyby
            # cokolwiek odsiewac i regresja bylaby niewidoczna.
            self.assertLess(count, len(kernels), arch)
        # WMMA jest wylacznie dla gfx11+, wiec RDNA2 ma go NIE dostac.
        self.assertLess(counts["gfx1030"], counts["gfx1100"])
        # Rodzina mma/FP8 jest wylacznie dla NVIDII.
        self.assertGreater(counts["sm_89"], counts["gfx1100"])

    def test_grouped_q6k_ma_osobne_warianty_nvidia_i_amd(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        scopes = {
            artifact: scope
            for _, _, artifact, scope in kernels
            if artifact
            in {
                "gemm_q6_k_f16_grouped",
                "gemm_q6_k_f16_grouped_bm128_bn64",
                "gemm_q6_k_wmma_f16_grouped",
                "gemm_q6_k_wmma_f16_grouped_bm128_bn64",
            }
        }
        self.assertEqual(set(scopes), {
            "gemm_q6_k_f16_grouped",
            "gemm_q6_k_f16_grouped_bm128_bn64",
            "gemm_q6_k_wmma_f16_grouped",
            "gemm_q6_k_wmma_f16_grouped_bm128_bn64",
        })
        for artifact in (
            "gemm_q6_k_f16_grouped",
            "gemm_q6_k_f16_grouped_bm128_bn64",
        ):
            scope = scopes[artifact]
            self.assertEqual(scope, ("nvidia",), artifact)
            self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1201"), artifact)
            self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_89"), artifact)
        for artifact in (
            "gemm_q6_k_wmma_f16_grouped",
            "gemm_q6_k_wmma_f16_grouped_bm128_bn64",
        ):
            scope = scopes[artifact]
            self.assertEqual(scope, ("amd:gfx11+",), artifact)
            self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1030"), artifact)
            self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1100"), artifact)
            self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1201"), artifact)
            self.assertFalse(build_kernel_catalog.scope_allows(scope, "sm_89"), artifact)

    def test_grouped_q4k_i_q8_0_maja_osobne_warianty_nvidia_i_amd(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        scopes = {
            artifact: scope
            for _, _, artifact, scope in kernels
            if artifact
            in {
                "gemm_q4_k_i8mma_grouped",
                "gemm_q4_k_i8mma_grouped_bm128_bn64",
                "gemm_q8_0_i8mma_grouped",
                "gemm_q8_0_i8mma_grouped_bm128_bn64",
                "gemm_q4_k_i8wmma_f16_grouped",
                "gemm_q4_k_i8wmma_f16_grouped_bm128_bn64",
                "gemm_q8_0_wmma_f16_grouped",
                "gemm_q8_0_wmma_f16_grouped_bm128_bn64",
            }
        }
        self.assertEqual(len(scopes), 8)
        for artifact, scope in scopes.items():
            if "wmma" in artifact:
                self.assertEqual(scope, ("amd:gfx11+",), artifact)
                self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1201"), artifact)
                self.assertFalse(build_kernel_catalog.scope_allows(scope, "sm_89"), artifact)
            else:
                self.assertEqual(scope, ("nvidia",), artifact)
                self.assertTrue(build_kernel_catalog.scope_allows(scope, "sm_89"), artifact)
                self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1201"), artifact)

    def test_rdna4_u4_decode_jest_ograniczony_do_gfx12(self):
        kernels, _ = build_kernel_catalog.parse_catalog()
        scopes = {
            artifact: scope
            for _, _, artifact, scope in kernels
            if artifact
            in {
                "gemv_q4_k_dp4a_amd_u4_f16",
                "gemv_q4_k_dp4a_amd_u4_persist_f16",
                "gemv_q4_k_dp4a_amd_u4_persist_x4k_f16",
                "gemv_q4_k_dp4a_amd_u4_group4_f16",
            }
        }
        self.assertEqual(len(scopes), 4)
        for artifact, scope in scopes.items():
            self.assertEqual(scope, ("amd:gfx12",), artifact)
            self.assertTrue(build_kernel_catalog.scope_allows(scope, "gfx1201"), artifact)
            self.assertFalse(build_kernel_catalog.scope_allows(scope, "gfx1100"), artifact)
            self.assertFalse(build_kernel_catalog.scope_allows(scope, "sm_89"), artifact)


if __name__ == "__main__":
    unittest.main()
