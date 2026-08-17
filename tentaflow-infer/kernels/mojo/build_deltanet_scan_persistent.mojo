# =============================================================================
# Plik: build_deltanet_scan_persistent.mojo
# Opis: Kompiluje izolowany rejestrowy kernel pełnego skanu DeltaNet d128.
# Przykład: pixi run mojo build_deltanet_scan_persistent.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.deltanet_scan_persistent import deltanet_gated_scan_persistent_d128_f16


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("brak wpisu .visible .entry w " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("niepoprawny wpis kernela w " + String(ptx_path))
    return String(text[byte=i + marker.byte_length():j])


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    name = "deltanet_gated_scan_persistent_d128_f16"
    temporary = Path(name + ".ptx")
    final = out_dir / (name + ".ptx")
    _ = ctx.compile_function[
        deltanet_gated_scan_persistent_d128_f16,
        dump_asm=Path("deltanet_gated_scan_persistent_d128_f16.ptx"),
    ]()
    text = temporary.read_text().replace(".target sm_89", ".target sm_80")
    final.write_text(text)
    os.remove(String(temporary))
    print(name, _entry_from_ptx(final))
