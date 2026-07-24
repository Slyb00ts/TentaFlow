# =============================================================================
# Plik: test_nvfp4_ct_layout_exhaustive.mojo
# Opis: Sprawdza wszystkie 256 kodów wejściowych E4M3 kontraktu S0.
# Przykład: mojo test_nvfp4_ct_layout_exhaustive.mojo
# =============================================================================

from std.memory import bitcast
from src.nvfp4_ct_layout import (
    nvfp4_ct_decode_s0,
    nvfp4_ct_s0_from_e4m3,
)


def main() raises:
    var finite_checked = 0
    var rejected_checked = 0
    for value in range(256):
        raw = UInt8(value)
        encoded = nvfp4_ct_s0_from_e4m3(raw)
        actual = Float32(nvfp4_ct_decode_s0(encoded))
        if value <= 0x7E:
            source = bitcast[DType.float8_e4m3fn, 1](
                SIMD[DType.uint8, 1](raw)
            )[0]
            expected = Float32(source) * 128.0
            if actual != expected:
                raise Error(
                    "niezgodne mapowanie E4M3 dla kodu " + String(value)
                )
            finite_checked += 1
        else:
            if actual == actual:
                raise Error(
                    "niedozwolony kod E4M3 nie dał S0 NaN: " + String(value)
                )
            rejected_checked += 1
    if nvfp4_ct_s0_from_e4m3(UInt8(0x7F)) != UInt8(0xF9):
        raise Error("NaN E4M3 nie używa kanonicznego kodu S0")
    print(
        "finite", finite_checked,
        "rejected", rejected_checked,
        "canonical_nan", "0xF9",
    )

