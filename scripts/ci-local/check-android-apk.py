#!/usr/bin/env python3
# =============================================================================
# Plik: check-android-apk.py
# Opis: Sprawdza kompletność bibliotek JNI i ich zależności w rzeczywistym APK.
# Przykład: python3 scripts/ci-local/check-android-apk.py app-debug.apk --abi arm64-v8a
# =============================================================================

import argparse
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import subprocess
import tempfile
import zipfile


MACHINES = {"arm64-v8a": 183, "armeabi-v7a": 40, "x86_64": 62}
SYSTEM_LIBRARIES = {
    "libandroid.so", "libc.so", "libdl.so", "liblog.so", "libm.so", "libz.so",
    "libEGL.so", "libGLESv1_CM.so", "libGLESv2.so", "libGLESv3.so",
    "libOpenSLES.so", "libvulkan.so", "libjnigraphics.so", "libmediandk.so",
    "libaaudio.so", "libcamera2ndk.so", "libstdc++.so",
}


def check_apk(apk, expected_abis):
    libraries = {}
    dependencies = {}
    with zipfile.ZipFile(apk) as archive, tempfile.TemporaryDirectory(dir=apk.parent) as scratch:
        for item in archive.infolist():
            parts = PurePosixPath(item.filename).parts
            if len(parts) != 3 or parts[0] != "lib" or not parts[2].endswith(".so"):
                continue
            _, abi, name = parts
            if abi not in MACHINES:
                raise ValueError(f"Nieobsługiwane ABI: {abi}")
            if name in libraries.setdefault(abi, set()):
                raise ValueError(f"Powtórzona biblioteka w APK: {item.filename}")
            if name.startswith(("libiroh-", "libiroh_relay-")):
                raise ValueError(f"Zbędny cdylib iroh w APK: {item.filename}")
            libraries[abi].add(name)
            # Jedna lokalna nazwa omija ścieżki wpisane w archiwum i ogranicza scratch.
            path = Path(scratch) / "library.so"
            with archive.open(item) as source, path.open("wb") as output:
                header = source.read(20)
                if len(header) != 20 or header[:4] != b"\x7fELF" or header[5] != 1:
                    raise ValueError(f"Nieprawidłowy ELF: {item.filename}")
                elf_class = 1 if abi == "armeabi-v7a" else 2
                if header[4] != elf_class or struct.unpack("<HH", header[16:20]) != (3, MACHINES[abi]):
                    raise ValueError(f"Architektura ELF nie pasuje do ABI: {item.filename}")
                output.write(header)
                shutil.copyfileobj(source, output)
            dynamic = subprocess.check_output(["readelf", "-d", str(path)], text=True, env=dict(os.environ, LC_ALL="C"))
            dependencies[(abi, name)] = re.findall(r"Shared library: \[(.*?)\]", dynamic)
        if not libraries:
            raise ValueError("APK nie zawiera bibliotek JNI")
        if expected_abis and set(libraries) != set(expected_abis):
            raise ValueError(f"ABI w APK: {sorted(libraries)}; oczekiwane: {sorted(expected_abis)}")
        for abi, names in libraries.items():
            if "libtentaflow_mobile.so" not in names:
                raise ValueError(f"Brak Core TentaFlow dla ABI {abi}")
            for name in names:
                missing = set(dependencies[(abi, name)]) - SYSTEM_LIBRARIES - names
                if missing:
                    raise ValueError(f"{abi}/{name}: brak zależności {sorted(missing)}")
    print(f"APK OK: ABI {', '.join(sorted(libraries))}; kompletne JNI i zależności")


def main():
    parser = argparse.ArgumentParser(description="Kontrola JNI w APK TentaFlow")
    parser.add_argument("apk", type=Path)
    parser.add_argument("--abi", action="append", choices=sorted(MACHINES), default=[])
    args = parser.parse_args()
    try:
        check_apk(args.apk, args.abi)
    except (ValueError, OSError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Błąd APK: {error}\n")


if __name__ == "__main__":
    main()
