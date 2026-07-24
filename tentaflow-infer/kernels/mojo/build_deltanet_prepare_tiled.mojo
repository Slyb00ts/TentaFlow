# =============================================================================
# Plik: build_deltanet_prepare_tiled.mojo
# Opis: Kompiluje izolowany kafelkowany kernel przygotowania DeltaNet d128/c4.
# Przykład: pixi run mojo build_deltanet_prepare_tiled.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.deltanet_prepare_tiled import deltanet_prepare_tiled_d128_c4_f16


def _entry_from_ptx(path: Path) raises -> String:
    text = path.read_text()
    marker = ".visible .entry "
    start = text.find(marker)
    if start < 0:
        raise Error("brak wpisu kernela w " + String(path))
    end = text.find("(", start)
    if end < 0:
        raise Error("niepoprawny wpis kernela w " + String(path))
    return String(text[byte=start + marker.byte_length():end])


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    name = "deltanet_prepare_tiled_d128_c4_f16"
    temporary = Path(name + ".ptx")
    final = out_dir / (name + ".ptx")
    _ = ctx.compile_function[
        deltanet_prepare_tiled_d128_c4_f16,
        dump_asm=Path("deltanet_prepare_tiled_d128_c4_f16.ptx"),
    ]()
    final.write_text(temporary.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary))
    print(name, _entry_from_ptx(final))
