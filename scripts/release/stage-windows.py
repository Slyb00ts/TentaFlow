#!/usr/bin/env python3
# =============================================================================
# File: scripts/release/stage-windows.py
# Purpose: Stages the Windows release archive. Copies the executables and the
#          libraries loaded at runtime, then closes the DLL dependency set over
#          the PE import tables: every import must be a Windows DLL, a GPU
#          driver DLL, a file already in the archive, or a file found in one of
#          the search directories (it is copied in). GStreamer is the one
#          external runtime and is reported, not bundled. The archive is a ZIP
#          with a sha256sum-style checksum next to it.
#
# Usage:
#   python scripts/release/stage-windows.py --tag v0.3.0 --asset full-vulkan \
#       --edition full --out target_shared/release \
#       --native native-libs/windows-x86_64 \
#       --search "<CUDA>/bin/x64" --search "<VC redist CRT>" \
#       --gstreamer-bin "<GStreamer>/bin" --gstreamer-version 1.28.6 --dest dist
# =============================================================================

import argparse
import hashlib
import os
import shutil
import struct
import sys
import zipfile
from collections import deque
from pathlib import Path

TARGET = "x86_64-pc-windows-msvc"

# DLLs every supported Windows (10 1809+ / Server 2019+) ships in System32.
# A name missing here fails the staging loudly, which is the point: a DLL the
# archive neither carries nor can count on is a binary that does not start.
SYSTEM_DLLS = frozenset(
    name.lower()
    for name in (
        "advapi32.dll", "bcrypt.dll", "bcryptprimitives.dll", "cfgmgr32.dll",
        "combase.dll", "comctl32.dll", "comdlg32.dll", "crypt32.dll",
        "d3d11.dll", "d3d12.dll", "d3dcompiler_47.dll", "dbghelp.dll",
        "dnsapi.dll", "dwmapi.dll", "dxcore.dll", "dxgi.dll", "gdi32.dll",
        "imm32.dll", "iphlpapi.dll", "kernel32.dll", "kernelbase.dll",
        "mswsock.dll", "ncrypt.dll", "netapi32.dll", "normaliz.dll",
        "ntdll.dll", "ole32.dll", "oleaut32.dll", "opengl32.dll", "pdh.dll",
        "powrprof.dll", "propsys.dll", "psapi.dll", "rpcrt4.dll",
        "secur32.dll", "setupapi.dll", "shell32.dll", "shlwapi.dll",
        "ucrtbase.dll", "user32.dll", "userenv.dll", "uxtheme.dll",
        "version.dll", "winhttp.dll", "wininet.dll", "winmm.dll",
        "wintrust.dll", "ws2_32.dll", "wtsapi32.dll",
    )
)

# Installed by the GPU driver, never redistributable: the Vulkan loader comes
# with every Vulkan-capable driver, nvcuda.dll with the NVIDIA one.
DRIVER_DLLS = frozenset({"vulkan-1.dll", "nvcuda.dll"})

# Loaded with LoadLibrary at runtime, so no import table names them:
# pdfium through pdfium-render, ONNX Runtime through ort's load-dynamic.
RUNTIME_LOADED = {
    "slim": ("zvec_c_api.dll", "pdfium.dll"),
    "full": ("zvec_c_api.dll", "pdfium.dll", "onnxruntime.dll"),
}
OPTIONAL_RUNTIME_LOADED = ("onnxruntime_providers_shared.dll",)

EXECUTABLES = ("tentaflow.exe", "tentaflow-meeting.exe", "tentaflow-coding-agent-bridge.exe")


class StageError(Exception):
    pass


def is_system_dll(name):
    lowered = name.lower()
    return (
        lowered in SYSTEM_DLLS
        or lowered.startswith("api-ms-win-")
        or lowered.startswith("ext-ms-")
    )


def pe_imports(path):
    """Names of the DLLs in the import directory of a PE32/PE32+ file."""
    data = Path(path).read_bytes()
    if data[:2] != b"MZ":
        raise StageError(f"{path}: not a PE file")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise StageError(f"{path}: missing PE signature")
    sections, optional_size = struct.unpack_from("<H12xH", data, pe + 6)
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    directories = {0x10B: optional + 96, 0x20B: optional + 112}.get(magic)
    if directories is None:
        raise StageError(f"{path}: unknown optional header magic {magic:#x}")
    import_rva, import_size = struct.unpack_from("<II", data, directories + 8)
    if import_rva == 0 or import_size == 0:
        return []

    table = optional + optional_size
    spans = []
    for index in range(sections):
        virtual_size, virtual_address, raw_size, raw_pointer = struct.unpack_from(
            "<IIII", data, table + index * 40 + 8
        )
        spans.append((virtual_address, max(virtual_size, raw_size), raw_pointer))

    def offset(rva):
        for start, size, raw in spans:
            if start <= rva < start + size:
                return raw + rva - start
        raise StageError(f"{path}: RVA {rva:#x} is outside every section")

    names = []
    descriptor = offset(import_rva)
    while True:
        fields = struct.unpack_from("<IIIII", data, descriptor)
        if not any(fields):
            break
        name_at = offset(fields[3])
        names.append(data[name_at:data.index(b"\0", name_at)].decode("ascii"))
        descriptor += 20
    return names


def stage(args, read_imports=pe_imports):
    stage_name = f"tentaflow-{args.tag}-{TARGET}-{args.asset}"
    dest = Path(args.dest)
    root = dest / stage_name
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    native = Path(args.native) / "lib-dynamic"
    out = Path(args.out)

    def require(source):
        source = Path(source)
        if not source.is_file():
            raise StageError(f"missing required artifact: {source}")
        shutil.copy2(source, root / source.name)

    for name in EXECUTABLES:
        require(out / name)
    for name in RUNTIME_LOADED[args.edition]:
        require(native / name)
    for name in OPTIONAL_RUNTIME_LOADED:
        if args.edition == "full" and (native / name).is_file():
            require(native / name)
    if args.edition == "full":
        require(out / "whisper_tf.dll")
    for name in ("LICENSE", "README.md"):
        require(Path(args.repo) / name)

    search = [out, native] + [Path(directory) for directory in args.search]
    gstreamer = Path(args.gstreamer_bin) if args.gstreamer_bin else None
    staged = {path.name.lower() for path in root.iterdir()}
    external = set()
    queue = deque(path for path in root.iterdir() if path.suffix.lower() in (".exe", ".dll"))
    while queue:
        binary = queue.popleft()
        for name in read_imports(binary):
            lowered = name.lower()
            if lowered in staged or is_system_dll(name) or lowered in DRIVER_DLLS:
                continue
            if gstreamer and (gstreamer / name).is_file():
                external.add(name)
                continue
            found = next((directory / name for directory in search if (directory / name).is_file()), None)
            if found is None:
                raise StageError(f"{binary.name} imports {name}, which is neither a Windows DLL nor found in any search directory")
            copied = root / found.name
            shutil.copy2(found, copied)
            staged.add(lowered)
            queue.append(copied)

    if external:
        if args.edition != "full":
            raise StageError(f"the slim edition must not need GStreamer, but it imports: {', '.join(sorted(external))}")
        (root / "REQUIREMENTS.txt").write_text(
            "This edition needs the GStreamer runtime (MSVC x86_64) on PATH:\n"
            f"GStreamer {args.gstreamer_version}, https://gstreamer.freedesktop.org/download/\n"
            "The camera and video pipeline links it; the server does not start without it.\n",
            encoding="utf-8",
            newline="\r\n",
        )

    archive = dest / f"{stage_name}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zipped:
        for path in sorted(root.rglob("*")):
            zipped.write(path, path.relative_to(dest).as_posix())
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (dest / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n")
    return root, archive, sorted(external)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Stage the Windows release archive.")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--asset", required=True)
    parser.add_argument("--edition", required=True, choices=sorted(RUNTIME_LOADED))
    parser.add_argument("--out", required=True, help="cargo release output directory")
    parser.add_argument("--native", required=True, help="native-libs/windows-x86_64")
    parser.add_argument("--search", action="append", default=[], help="extra directory to take DLLs from")
    parser.add_argument("--gstreamer-bin", default="")
    parser.add_argument("--gstreamer-version", default="")
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parents[2]))
    parser.add_argument("--dest", required=True)
    args = parser.parse_args(argv)
    try:
        root, archive, external = stage(args)
    except StageError as error:
        print(f"::error::{error}")
        return 1
    for path in sorted(root.iterdir()):
        print(f"  {path.name:48} {path.stat().st_size:>12,}")
    if external:
        print(f"external runtime (not bundled): {', '.join(external)}")
    print(f"{archive} ({archive.stat().st_size:,} bytes)")
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.write(f"stage={root.name}\narchive={archive.name}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
