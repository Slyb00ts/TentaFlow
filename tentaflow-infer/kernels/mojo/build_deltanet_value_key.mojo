# =============================================================================
# Plik: build_deltanet_value_key.mojo
# Opis: Kompiluje izolowany zestaw kerneli stanu DeltaNet ValueKey.
# Przykład: pixi run mojo run -I . build_deltanet_value_key.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.deltanet_value_key import (
    deltanet_value_key_scan_inplace_f16,
    deltanet_value_key_scan_persistent_f16,
    deltanet_value_key_scan_checkpoints_f16,
    deltanet_value_key_commit_recompute_f32,
)


def main() raises:
    var ctx = DeviceContext()
    _ = ctx.compile_function[
        deltanet_value_key_scan_inplace_f16,
        dump_asm=Path("deltanet_value_key_scan_inplace_f16.ptx"),
    ]()
    _ = ctx.compile_function[
        deltanet_value_key_scan_checkpoints_f16,
        dump_asm=Path("deltanet_value_key_scan_checkpoints_f16.ptx"),
    ]()
    _ = ctx.compile_function[
        deltanet_value_key_scan_persistent_f16,
        dump_asm=Path("deltanet_value_key_scan_persistent_f16.ptx"),
    ]()
    _ = ctx.compile_function[
        deltanet_value_key_commit_recompute_f32,
        dump_asm=Path("deltanet_value_key_commit_recompute_f32.ptx"),
    ]()
