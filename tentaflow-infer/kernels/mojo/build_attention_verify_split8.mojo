# =============================================================================
# Plik: build_attention_verify_split8.mojo
# Opis: Eksportuje PTX izolowanego split8 verifiera T3/T4 do analizy zasobów.
# Przykład: pixi run mojo build_attention_verify_split8_probe.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.attention_verify_split8 import (
    attn_verify_split8_combine_f16_hd256,
    attn_verify_split8_f16_hd256_t3,
    attn_verify_split8_f16_hd256_t4,
)


def main() raises:
    var ctx = DeviceContext()
    _ = ctx.compile_function[
        attn_verify_split8_f16_hd256_t3,
        dump_asm=Path("attn_verify_split8_f16_hd256_t3.ptx"),
    ]()
    _ = ctx.compile_function[
        attn_verify_split8_f16_hd256_t4,
        dump_asm=Path("attn_verify_split8_f16_hd256_t4.ptx"),
    ]()
    _ = ctx.compile_function[
        attn_verify_split8_combine_f16_hd256,
        dump_asm=Path("attn_verify_split8_combine_f16_hd256.ptx"),
    ]()
