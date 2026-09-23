#!/usr/bin/env python3
# =============================================================================
# File: scripts/release/test-stage-windows.py
# Purpose: Tests the Windows archive staging on synthetic PE32+ files, so the
#          dependency closure is checked on any OS, not only on the runner.
# Usage:   python3 scripts/release/test-stage-windows.py
# =============================================================================

import argparse
import hashlib
import importlib.util
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("stage_windows", HERE / "stage-windows.py")
stage_windows = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stage_windows)


def pe_with_imports(*names):
    """A minimal PE32+ image whose only content is an import directory."""
    section_rva, section_raw = 0x1000, 0x200
    descriptors = (len(names) + 1) * 20
    strings = b""
    name_rvas = []
    for name in names:
        name_rvas.append(section_rva + descriptors + len(strings))
        strings += name.encode("ascii") + b"\0"
    body = b"".join(struct.pack("<IIIII", 0, 0, 0, rva, 0) for rva in name_rvas)
    body += b"\0" * 20 + strings

    optional_size = 112 + 16 * 8
    image = bytearray(section_raw + len(body))
    image[0:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, 0x40)
    image[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<HH12xHH", image, 0x44, 0x8664, 1, optional_size, 0x22)
    optional = 0x40 + 24
    struct.pack_into("<H", image, optional, 0x20B)
    struct.pack_into("<II", image, optional + 112 + 8, section_rva if names else 0, descriptors if names else 0)
    section = optional + optional_size
    image[section:section + 8] = b".idata\0\0"
    struct.pack_into("<IIII", image, section + 8, len(body), section_rva, len(body), section_raw)
    image[section_raw:] = body
    return bytes(image)


class PeImportTests(unittest.TestCase):
    def test_reads_every_import_in_order(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "a.dll"
            path.write_bytes(pe_with_imports("KERNEL32.dll", "zvec_c_api.dll"))
            self.assertEqual(["KERNEL32.dll", "zvec_c_api.dll"], stage_windows.pe_imports(path))

    def test_image_without_imports_has_none(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "a.dll"
            path.write_bytes(pe_with_imports())
            self.assertEqual([], stage_windows.pe_imports(path))

    def test_non_pe_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "a.dll"
            path.write_bytes(b"#!/bin/sh\n")
            with self.assertRaises(stage_windows.StageError):
                stage_windows.pe_imports(path)


class StageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-stage-windows-")
        self.addCleanup(self.temporary.cleanup)
        base = Path(self.temporary.name)
        self.out = base / "target/release"
        self.native = base / "native-libs/windows-x86_64"
        self.cuda = base / "cuda/bin/x64"
        self.crt = base / "redist/Microsoft.VC143.CRT"
        self.gstreamer = base / "gstreamer/bin"
        self.repo = base / "repo"
        self.dest = base / "dist"
        for directory in (self.out, self.native / "lib-dynamic", self.cuda, self.crt, self.gstreamer, self.repo):
            directory.mkdir(parents=True)
        for name in ("LICENSE", "README.md"):
            (self.repo / name).write_text("fixture", encoding="utf-8")

        self.write(self.out / "tentaflow-meeting.exe", "KERNEL32.dll", "VCRUNTIME140.dll")
        self.write(self.out / "tentaflow-coding-agent-bridge.exe", "KERNEL32.dll", "api-ms-win-crt-heap-l1-1-0.dll")
        self.write(self.native / "lib-dynamic/zvec_c_api.dll", "KERNEL32.dll")
        self.write(self.native / "lib-dynamic/pdfium.dll", "KERNEL32.dll")
        self.write(self.native / "lib-dynamic/onnxruntime.dll", "MSVCP140.dll")
        self.write(self.crt / "VCRUNTIME140.dll", "KERNEL32.dll")
        self.write(self.crt / "MSVCP140.dll", "VCRUNTIME140.dll")
        self.write(self.cuda / "cublas64_13.dll", "cublasLt64_13.dll")
        self.write(self.cuda / "cublasLt64_13.dll", "KERNEL32.dll")
        self.write(self.gstreamer / "gstreamer-1.0-0.dll", "KERNEL32.dll")

    def write(self, path, *imports):
        path.write_bytes(pe_with_imports(*imports))

    def args(self, edition="full", asset="full-cuda13"):
        return argparse.Namespace(
            tag="v0.3.0", asset=asset, edition=edition, out=str(self.out), native=str(self.native),
            search=[str(self.cuda), str(self.crt)], gstreamer_bin=str(self.gstreamer),
            gstreamer_version="1.28.6", repo=str(self.repo), dest=str(self.dest),
        )

    def full_binaries(self):
        self.write(self.out / "tentaflow.exe", "KERNEL32.dll", "zvec_c_api.dll", "whisper_tf.dll",
                   "gstreamer-1.0-0.dll", "vulkan-1.dll", "VCRUNTIME140.dll")
        self.write(self.out / "whisper_tf.dll", "cublas64_13.dll", "VCRUNTIME140.dll")

    def test_closure_pulls_transitive_dlls_and_leaves_gstreamer_outside(self):
        self.full_binaries()
        root, archive, external = stage_windows.stage(self.args())
        names = {path.name for path in root.iterdir()}
        # cublasLt is only reachable through cublas, MSVCP140 only through ORT.
        for name in ("tentaflow.exe", "whisper_tf.dll", "cublas64_13.dll", "cublasLt64_13.dll",
                     "VCRUNTIME140.dll", "MSVCP140.dll", "onnxruntime.dll", "pdfium.dll"):
            self.assertIn(name, names)
        self.assertNotIn("gstreamer-1.0-0.dll", names)
        self.assertNotIn("vulkan-1.dll", names)
        self.assertEqual(["gstreamer-1.0-0.dll"], external)
        self.assertIn("GStreamer 1.28.6", (root / "REQUIREMENTS.txt").read_text(encoding="utf-8"))

    def test_archive_has_one_root_folder_and_a_matching_checksum(self):
        self.full_binaries()
        root, archive, _ = stage_windows.stage(self.args())
        self.assertEqual("tentaflow-v0.3.0-x86_64-pc-windows-msvc-full-cuda13.zip", archive.name)
        with zipfile.ZipFile(archive) as zipped:
            tops = {name.split("/", 1)[0] for name in zipped.namelist()}
            self.assertEqual({root.name}, tops)
            self.assertIn(f"{root.name}/tentaflow.exe", zipped.namelist())
        digest, name = (self.dest / (archive.name + ".sha256")).read_text(encoding="utf-8").split()
        self.assertEqual(archive.name, name)
        self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)

    def test_unresolved_import_fails(self):
        self.full_binaries()
        self.write(self.out / "whisper_tf.dll", "mystery.dll")
        with self.assertRaisesRegex(stage_windows.StageError, "whisper_tf.dll imports mystery.dll"):
            stage_windows.stage(self.args())

    def test_missing_runtime_loaded_library_fails(self):
        self.full_binaries()
        (self.native / "lib-dynamic/pdfium.dll").unlink()
        with self.assertRaisesRegex(stage_windows.StageError, "missing required artifact.*pdfium.dll"):
            stage_windows.stage(self.args())

    def test_slim_carries_no_inference_libraries_and_no_gstreamer(self):
        self.write(self.out / "tentaflow.exe", "KERNEL32.dll", "zvec_c_api.dll")
        root, _, external = stage_windows.stage(self.args(edition="slim", asset="slim"))
        names = {path.name for path in root.iterdir()}
        self.assertNotIn("onnxruntime.dll", names)
        self.assertNotIn("whisper_tf.dll", names)
        self.assertNotIn("REQUIREMENTS.txt", names)
        self.assertEqual([], external)

    def test_slim_that_links_gstreamer_is_rejected(self):
        self.write(self.out / "tentaflow.exe", "KERNEL32.dll", "zvec_c_api.dll", "gstreamer-1.0-0.dll")
        with self.assertRaisesRegex(stage_windows.StageError, "slim edition must not need GStreamer"):
            stage_windows.stage(self.args(edition="slim", asset="slim"))


if __name__ == "__main__":
    unittest.main()
