#!/usr/bin/env python3
# =============================================================================
# Plik: build_kernel_catalog.py
# Opis: Dzieli katalog kerneli na male jednostki Mojo i sklada artefakty PTX.
# Przykład: python scripts/build_kernel_catalog.py
# =============================================================================

import json
import os
import shutil
import subprocess
import tempfile
import ctypes
from collections import defaultdict
from pathlib import Path

from validate_sm80_ptx import validate_sm80_text

ROOT = Path(__file__).resolve().parent.parent
CATALOG = ROOT / "build_kernels_catalog.mojo"
OUTPUT_ROOT = Path(os.environ.get("FORGE_KERNEL_BUILD_DIR", ROOT / "build")).resolve()
CHUNK_SIZE = 12
AT_FDCWD = -100
RENAME_EXCHANGE = 2


def parse_catalog():
    lines = CATALOG.read_text().splitlines()
    imports = {}
    kernels = []
    portable_nvfp4 = set()
    index = 0
    while index < len(lines):
        stripped = lines[index].strip()
        if stripped.startswith("from src."):
            module, separator, imported = stripped.partition(" import ")
            if not separator:
                raise RuntimeError(f"niepoprawny import w linii {index + 1}")
            module = module.removeprefix("from ")
            if imported == "(":
                imported_lines = []
                index += 1
                while index < len(lines) and lines[index].strip() != ")":
                    imported_lines.append(lines[index].strip())
                    index += 1
                if index == len(lines):
                    raise RuntimeError(f"niezamkniety import modulu {module}")
                imported = "".join(imported_lines)
            for name in imported.split(","):
                name = name.strip()
                if name:
                    imports[name] = module
        elif "ctx.compile_function[" in stripped:
            expression = stripped
            while "]()" not in expression:
                index += 1
                if index == len(lines):
                    raise RuntimeError("niezamkniete wywolanie compile_function")
                expression += " " + lines[index].strip()
            arguments = expression.partition("ctx.compile_function[")[2].partition("]()")[0]
            symbol, separator, options = arguments.partition(",")
            if not separator:
                raise RuntimeError(f"brak opcji compile_function dla {symbol.strip()}")
            marker = 'dump_asm=Path("'
            artifact = options.partition(marker)[2].partition('.ptx")')[0]
            if not artifact:
                raise RuntimeError(f"brak dump_asm dla {symbol.strip()}")
            kernels.append((symbol.strip(), artifact))
        elif stripped.startswith('or name == "') or stripped.startswith('name == "'):
            portable_nvfp4.add(stripped.partition('name == "')[2].partition('"')[0])
        index += 1

    resolved_kernels = []
    for symbol, artifact in kernels:
        module = imports.get(symbol)
        if module is None:
            raise RuntimeError(f"brak importu dla {symbol}")
        resolved_kernels.append((module, symbol, artifact))
    if not resolved_kernels:
        raise RuntimeError("katalog nie zawiera kerneli")
    if len({artifact for _, _, artifact in resolved_kernels}) != len(resolved_kernels):
        raise RuntimeError("katalog zawiera powtorzone nazwy artefaktow")
    return resolved_kernels, portable_nvfp4


def chunk_kernels(kernels):
    grouped = defaultdict(list)
    for module, symbol, artifact in kernels:
        grouped[module].append((symbol, artifact))
    for module in sorted(grouped):
        items = grouped[module]
        for offset in range(0, len(items), CHUNK_SIZE):
            yield module, items[offset : offset + CHUNK_SIZE]


def builder_source(module, items):
    symbols = ",\n    ".join(symbol for symbol, _ in items)
    calls = "\n".join(
        "    _ = ctx.compile_function[\n"
        f"        {symbol}, dump_asm=Path(\"{artifact}.ptx\"),\n"
        "    ]()"
        for symbol, artifact in items
    )
    return f'''# =============================================================================
# Plik: wygenerowana_jednostka_kerneli.mojo
# Opis: Izolowana jednostka kompilacji kerneli GPU generowana z katalogu.
# Przykład: uruchamiana przez build_kernels.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from {module} import (
    {symbols},
)


def main() raises:
    var ctx = DeviceContext()
{calls}
'''


def entry_from_ptx(text, artifact):
    marker = ".visible .entry "
    start = text.find(marker)
    end = text.find("(", start + len(marker))
    if start < 0 or end < 0:
        raise RuntimeError(f"brak wpisu .visible .entry w {artifact}.ptx")
    return text[start + len(marker) : end]


def normalized_ptx(text, artifact, portable_nvfp4):
    if artifact in portable_nvfp4 or ("fp8" not in artifact and "nvfp4" not in artifact):
        text = text.replace(".target sm_89", ".target sm_80")
    if artifact.startswith("gemm_fp8"):
        for version in ("8.0", "8.1", "8.2", "8.3"):
            text = text.replace(f".version {version}", ".version 8.4")
    return text


def requires_sm89_cubins(arch):
    return arch == "sm_89"


def publish_arch(staged_arch, destination):
    if not destination.exists():
        os.replace(staged_arch, destination)
        return
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = libc.renameat2
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    renameat2.restype = ctypes.c_int
    result = renameat2(
        AT_FDCWD,
        os.fsencode(staged_arch),
        AT_FDCWD,
        os.fsencode(destination),
        RENAME_EXCHANGE,
    )
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), destination)


def compile_catalog(kernels, portable_nvfp4):
    mojo = shutil.which("mojo")
    if mojo is None:
        raise RuntimeError("brak kompilatora mojo w PATH")
    environment = os.environ.copy()
    environment.setdefault(
        "MODULAR_NVPTX_COMPILER_PATH", str(ROOT / "scripts" / "ptxas_fp8_shim.sh")
    )
    fail_after = int(os.environ.get("FORGE_KERNEL_BUILD_FAIL_AFTER", "0"))
    OUTPUT_ROOT.mkdir(parents=True, exist_ok=True)
    manifest = None
    with tempfile.TemporaryDirectory(
        prefix=".kernel-build-", dir=OUTPUT_ROOT, ignore_cleanup_errors=True
    ) as publication:
        publication_root = Path(publication)
        with tempfile.TemporaryDirectory(prefix="tentaflow-kernels-") as temporary:
            temporary_path = Path(temporary)
            pending = list(chunk_kernels(kernels))
            compiled_units = 0
            attempt = 0
            while pending:
                module, items = pending.pop(0)
                builder = temporary_path / f"build_{attempt:03}.mojo"
                attempt += 1
                builder.write_text(builder_source(module, items))
                try:
                    subprocess.run(
                        [mojo, "run", "-I", str(ROOT), str(builder)],
                        cwd=temporary_path,
                        env=environment,
                        check=True,
                    )
                except subprocess.CalledProcessError:
                    for ptx_path in temporary_path.glob("*.ptx"):
                        ptx_path.unlink()
                    if len(items) == 1:
                        raise
                    middle = len(items) // 2
                    pending.insert(0, (module, items[middle:]))
                    pending.insert(0, (module, items[:middle]))
                    print(f"dziele jednostke {module} ({len(items)} kerneli)")
                    continue
                dumped = sorted(temporary_path.glob("*.ptx"))
                if len(dumped) != len(items):
                    raise RuntimeError(
                        f"jednostka {attempt} wygenerowala {len(dumped)} z {len(items)} PTX"
                    )
                for ptx_path in dumped:
                    artifact = ptx_path.stem
                    text = normalized_ptx(ptx_path.read_text(), artifact, portable_nvfp4)
                    target_line = next(
                        (line for line in text.splitlines() if line.startswith(".target sm_")),
                        None,
                    )
                    if target_line is None:
                        raise RuntimeError(f"brak architektury PTX w {ptx_path}")
                    generated_arch = target_line.split()[1]
                    if generated_arch == "sm_80":
                        validate_sm80_text(text)
                    arch = "sm_89" if generated_arch == "sm_80" else generated_arch
                    if manifest is None:
                        manifest = {"arch": arch, "kernels": {}}
                    elif manifest["arch"] != arch:
                        raise RuntimeError("jednostki Mojo zwrocily rozne architektury")
                    staged_arch = publication_root / arch
                    staged_arch.mkdir(parents=True, exist_ok=True)
                    target = staged_arch / ptx_path.name
                    target.write_text(text)
                    manifest["kernels"][artifact] = {
                        "file": ptx_path.name,
                        "entry": entry_from_ptx(text, artifact),
                    }
                    ptx_path.unlink()
                compiled_units += 1
                print(f"skompilowano jednostke {compiled_units}: {module} ({len(items)} kerneli)")
                if fail_after and compiled_units >= fail_after:
                    raise RuntimeError(f"wymuszony błąd po jednostce {compiled_units}")

        if manifest is None or len(manifest["kernels"]) != len(kernels):
            raise RuntimeError("niepelny manifest po kompilacji")
        staged_arch = publication_root / manifest["arch"]
        if requires_sm89_cubins(manifest["arch"]):
            for cubin in ("w4a8_gemm_cuda.cubin", "fattn_prefill_cuda.cubin"):
                source = ROOT / "build" / manifest["arch"] / cubin
                if not source.is_file():
                    raise RuntimeError(f"brak wymaganego cubina {source}")
                shutil.copy2(source, staged_arch / cubin)
        expected = {f"{artifact}.ptx" for _, _, artifact in kernels}
        actual = {path.name for path in staged_arch.glob("*.ptx")}
        if actual != expected:
            raise RuntimeError("staging zawiera niepelny lub nadmiarowy zestaw PTX")
        manifest_path = staged_arch / "manifest.json"
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        destination = OUTPUT_ROOT / manifest["arch"]
        publish_arch(staged_arch, destination)
        print(f"zapisano {len(kernels)} kerneli: {destination / 'manifest.json'}")


def main():
    kernels, portable_nvfp4 = parse_catalog()
    compile_catalog(kernels, portable_nvfp4)


if __name__ == "__main__":
    main()
