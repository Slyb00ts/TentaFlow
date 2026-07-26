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
# Diagnostyka kompilatora, ktora NIE przerywa builda, a oznacza cicho
# pominieta instrukcje inline asm (patrz kontrola w compile_catalog).
SILENT_ASM_MARKER = "unknown asm constraint"
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
            marker = "dump_asm=Path("
            tail = options.partition(marker)[2].lstrip()
            if not tail.startswith('"'):
                raise RuntimeError(f"brak dump_asm dla {symbol.strip()}")
            artifact = tail[1:].partition('.ptx"')[0]
            if not artifact:
                raise RuntimeError(f"pusty dump_asm dla {symbol.strip()}")
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


# --- AMD (AMDGCN) -----------------------------------------------------------
#
# Mojo zrzuca dla celu AMD assembler AMDGCN, nie ładowalny obraz. HIP wymaga
# code objectu, więc doklejamy jeden krok: łatka identyfikatora celu (Mojo pisze
# `...-unknown-gfxNNNN`, którego asembler nie przyjmuje) i złożenie do HSACO.
AMD_TARGET_MARKER = ".amdgcn_target "


def amdgcn_arch(text, artifact):
    for line in text.splitlines():
        if line.strip().startswith(AMD_TARGET_MARKER):
            target = line.split('"')[1]
            return target.rsplit("-", 1)[1]
    raise RuntimeError(f"brak .amdgcn_target w {artifact}.s")


def normalized_amdgcn(text):
    return text.replace("amdgcn-amd-amdhsa-unknown-", "amdgcn-amd-amdhsa--")


def entry_from_amdgcn(text, artifact):
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(".globl"):
            return stripped.split()[1].rstrip(",")
    raise RuntimeError(f"brak symbolu .globl w {artifact}.s")


def assemble_amdgcn(text, arch, artifact, out_path):
    rocm = Path(os.environ.get("ROCM_PATH", "/opt/rocm"))
    clang = rocm / "llvm" / "bin" / "clang"
    if not clang.is_file():
        raise RuntimeError(f"brak {clang}; ustaw ROCM_PATH")
    with tempfile.TemporaryDirectory(prefix="forge-amdgcn-") as work:
        source = Path(work) / f"{artifact}.s"
        source.write_text(text)
        result = subprocess.run(
            [
                str(clang),
                "-x",
                "assembler",
                "-target",
                "amdgcn-amd-amdhsa",
                f"-mcpu={arch}",
                str(source),
                "-o",
                str(out_path),
            ],
            capture_output=True,
            text=True,
        )
    if result.returncode != 0:
        raise RuntimeError(f"asemblacja {artifact} dla {arch} nie powiodla sie: {result.stderr}")

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
    partial = os.environ.get("FORGE_KERNEL_BUILD_PARTIAL") == "1"
    unsupported = []
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
                    result = subprocess.run(
                        [mojo, "run", "-I", str(ROOT), str(builder)],
                        cwd=temporary_path,
                        env=environment,
                        check=True,
                        capture_output=partial,
                        text=partial,
                    )
                except subprocess.CalledProcessError as failure:
                    for stale in list(temporary_path.glob("*.ptx")) + list(
                        temporary_path.glob("*.s")
                    ):
                        stale.unlink()
                    if len(items) == 1:
                        # Tryb czastkowy (port na nowa architekture): kernel,
                        # ktory sie nie kompiluje, jest zapisywany na liste i
                        # pomijany. Publikowany katalog jest wtedy niepelny, a
                        # brakujacy kernel zglasza sie dopiero przy uruchomieniu
                        # ('kernel not loaded') — to jedyny tryb, w ktorym to
                        # dopuszczamy, bo architektura jest w trakcie portu.
                        if partial:
                            symbol, artifact = items[0]
                            unsupported.append(
                                (artifact, (failure.stderr or "").strip().splitlines()[-1:] or [""])
                            )
                            print(f"NIEOBSLUGIWANY: {artifact}")
                            continue
                        raise
                    middle = len(items) // 2
                    pending.insert(0, (module, items[middle:]))
                    pending.insert(0, (module, items[:middle]))
                    print(f"dziele jednostke {module} ({len(items)} kerneli)")
                    continue
                # PUŁAPKA: `inlined_assembly` z treścią PTX na architekturze
                # innej niż NVIDIA NIE przerywa builda — kompilator zgłasza
                # tylko `unknown asm constraint`, a kernel powstaje z CICHO
                # pominiętą instrukcją i liczy śmieci. Traktujemy ten komunikat
                # jak błąd, bo inaczej wychodzi dopiero na wyniku modelu.
                if partial and SILENT_ASM_MARKER in (result.stderr or ""):
                    raise RuntimeError(
                        f"jednostka {module} zawiera inline asm niezgodny z "
                        f"architekturą (\"{SILENT_ASM_MARKER}\") — kernel "
                        "powstalby z pominieta instrukcja"
                    )
                dumped = sorted(temporary_path.glob("*.ptx")) + sorted(
                    temporary_path.glob("*.s")
                )
                if len(dumped) != len(items):
                    raise RuntimeError(
                        f"jednostka {attempt} wygenerowala {len(dumped)} z {len(items)} artefaktow"
                    )
                # Mangling Mojo nie koduje parametrow comptime, wiec dwie
                # specjalizacje tej samej funkcji potrafia dostac ten sam symbol.
                # Kompilator deduplikuje je wtedy w obrebie jednostki i DWA
                # artefakty dostaja TO SAMO cialo — kernel liczy wtedy cicho co
                # innego, niz nazwa obiecuje. Tego nie wolno przepuscic.
                seen_bodies = {}
                for dump_path in dumped:
                    artifact = dump_path.stem
                    body = dump_path.read_text()
                    twin = seen_bodies.get(body)
                    if twin is not None:
                        raise RuntimeError(
                            f"artefakty {twin} i {artifact} maja identyczne cialo — "
                            "kolizja symboli manglingu; nadaj specjalizacjom osobne "
                            "definicje zamiast aliasow comptime"
                        )
                    seen_bodies[body] = artifact
                    raw = dump_path.read_text()
                    if AMD_TARGET_MARKER in raw:
                        arch = amdgcn_arch(raw, artifact)
                        text = normalized_amdgcn(raw)
                        file_name = f"{artifact}.hsaco"
                        entry = entry_from_amdgcn(text, artifact)
                        writer = lambda target, text=text, arch=arch, artifact=artifact: (
                            assemble_amdgcn(text, arch, artifact, target)
                        )
                    else:
                        text = normalized_ptx(raw, artifact, portable_nvfp4)
                        target_line = next(
                            (line for line in text.splitlines() if line.startswith(".target sm_")),
                            None,
                        )
                        if target_line is None:
                            raise RuntimeError(f"brak architektury PTX w {dump_path}")
                        generated_arch = target_line.split()[1]
                        if generated_arch == "sm_80":
                            validate_sm80_text(text)
                        arch = "sm_89" if generated_arch == "sm_80" else generated_arch
                        file_name = dump_path.name
                        entry = entry_from_ptx(text, artifact)
                        writer = lambda target, text=text: target.write_text(text)
                    if manifest is None:
                        manifest = {"arch": arch, "kernels": {}}
                    elif manifest["arch"] != arch:
                        raise RuntimeError("jednostki Mojo zwrocily rozne architektury")
                    staged_arch = publication_root / arch
                    staged_arch.mkdir(parents=True, exist_ok=True)
                    writer(staged_arch / file_name)
                    manifest["kernels"][artifact] = {
                        "file": file_name,
                        "entry": entry,
                    }
                    dump_path.unlink()
                compiled_units += 1
                print(f"skompilowano jednostke {compiled_units}: {module} ({len(items)} kerneli)")
                if fail_after and compiled_units >= fail_after:
                    raise RuntimeError(f"wymuszony błąd po jednostce {compiled_units}")

        if manifest is None:
            raise RuntimeError("zaden kernel sie nie skompilowal")
        if not partial and len(manifest["kernels"]) != len(kernels):
            raise RuntimeError("niepelny manifest po kompilacji")
        staged_arch = publication_root / manifest["arch"]
        if requires_sm89_cubins(manifest["arch"]):
            for cubin in ("w4a8_gemm_cuda.cubin", "fattn_prefill_cuda.cubin"):
                source = ROOT / "build" / manifest["arch"] / cubin
                if not source.is_file():
                    raise RuntimeError(f"brak wymaganego cubina {source}")
                shutil.copy2(source, staged_arch / cubin)
        expected = set(manifest["kernels"][artifact]["file"] for artifact in manifest["kernels"])
        actual = {
            path.name
            for path in staged_arch.iterdir()
            if path.suffix in (".ptx", ".hsaco")
        }
        if actual != expected:
            raise RuntimeError("staging zawiera niepelny lub nadmiarowy zestaw artefaktow")
        manifest_path = staged_arch / "manifest.json"
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        destination = OUTPUT_ROOT / manifest["arch"]
        publish_arch(staged_arch, destination)
        if partial:
            report = destination / "unsupported.txt"
            report.write_text(
                "".join(f"{artifact}\t{reason[0]}\n" for artifact, reason in unsupported)
            )
            print(
                f"zapisano {len(manifest['kernels'])} z {len(kernels)} kerneli dla "
                f"{manifest['arch']}; {len(unsupported)} sie nie kompiluje. Lista: {report}"
            )
        else:
            print(f"zapisano {len(kernels)} kerneli: {destination / 'manifest.json'}")


def main():
    kernels, portable_nvfp4 = parse_catalog()
    compile_catalog(kernels, portable_nvfp4)


if __name__ == "__main__":
    main()
