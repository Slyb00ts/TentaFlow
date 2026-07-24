# =============================================================================
# Plik: test_nvfp4_ct_stage1.mojo
# Opis: Sprawdza kompilację kontraktu i kernela B1 S0 N64/K128.
# Przykład: pixi run mojo build test_nvfp4_ct_stage1.mojo
# =============================================================================

from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_decode import gemv_nvfp4_ct_s0_n64k128_f16
from src.nvfp4_ct_fp8 import pack_nvfp4_ct_s0_fp8


def main():
    _ = repack_nvfp4_ct_s0_n64k128_into
    _ = gemv_nvfp4_ct_s0_n64k128_f16
    _ = pack_nvfp4_ct_s0_fp8
