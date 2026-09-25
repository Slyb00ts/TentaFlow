#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/ci-local/test-release-packaging.py
# Opis: Sprawdza rzeczywisty krok pakowania Metal z workflow na małych plikach.
# Przykład: python3 scripts/ci-local/test-release-packaging.py
# =============================================================================

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/release.yml"


def stage_script(job_name):
    job = WORKFLOW.read_text(encoding="utf-8").split(f"\n  {job_name}:\n", 1)[1]
    step = job.split("      - name: Stage archive\n", 1)[1]
    body = step.split("        run: |\n", 1)[1].split("\n      - ", 1)[0]
    return textwrap.dedent(body)


def metal_stage_script():
    return stage_script("build-macos")


def linux_stage_script():
    # GitHub substitutes the matrix values before the shell ever sees the step,
    # so the test has to do it too.
    body = stage_script("build")
    for placeholder, value in {
        "matrix.asset": "full",
        "matrix.platform": "linux-x86_64",
        "matrix.edition": "full",
    }.items():
        body = body.replace("${{ " + placeholder + " }}", value)
    return body


@unittest.skipUnless(shutil.which("bash") and shutil.which("shasum"), "Test wymaga Bash i shasum")
class MetalPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-metal-packaging-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.target = self.directory / "target_shared"
        self.output = self.target / "aarch64-apple-darwin/release"
        self.output.mkdir(parents=True)
        (self.directory / "tentaflow").mkdir()
        (self.directory / "scripts/install").mkdir(parents=True)
        self.commands = self.directory / "commands"
        self.commands.mkdir()
        cargo = self.commands / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$TEST_CARGO_METADATA"\n', encoding="utf-8")
        cargo.chmod(0o755)
        for name in ("tentaflow", "libzvec_c_api.dylib", "libpdfium.dylib",
                     "libMLXBridge.dylib", "libKokoroBridge.dylib"):
            (self.output / name).write_bytes(name.encode())
        (self.output / "mlx-swift_Cmlx.bundle").mkdir()
        (self.output / "mlx-swift_Cmlx.bundle/shader.metallib").write_bytes(b"fixture")
        for name in ("LICENSE", "README.md", "scripts/install/ai.tentaflow.plist.in"):
            (self.directory / name).write_bytes(b"fixture")
        self.sidecar = self.output / "tentaflow-meeting"
        self.payload = b'#!/bin/sh\nprintf "fixture pakowania\\n"\n'
        self.sidecar.write_bytes(self.payload)
        self.sidecar.chmod(0o755)
        self.bridge = self.output / "tentaflow-coding-agent-bridge"
        self.bridge_payload = b'#!/bin/sh\nprintf "fixture mostka agentow\\n"\n'
        self.bridge.write_bytes(self.bridge_payload)
        self.bridge.chmod(0o755)
        self.stage = "tentaflow-v0.2.0-beta-aarch64-apple-darwin-full-metal"

    def stage_archive(self):
        return subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", metal_stage_script()],
            cwd=self.directory,
            env={**os.environ, "PATH": str(self.commands) + os.pathsep + os.environ["PATH"],
                 "TEST_CARGO_METADATA": json.dumps({"target_directory": str(self.target)}),
                 "TENTAFLOW_TARGET": "aarch64-apple-darwin", "GITHUB_REF_NAME": "v0.2.0-beta",
                 "GITHUB_OUTPUT": str(self.directory / "github-output")},
            capture_output=True, text=True, encoding="utf-8", timeout=20,
        )

    def test_archive_contains_exact_executable_sidecar_next_to_main_binary(self):
        result = self.stage_archive()
        self.assertEqual(0, result.returncode, result.stderr)
        with tarfile.open(self.directory / (self.stage + ".tar.gz")) as archive:
            sidecar = archive.getmember(self.stage + "/tentaflow-meeting")
            self.assertEqual(self.payload, archive.extractfile(sidecar).read())
            self.assertEqual(0o755, sidecar.mode & 0o777)
            self.assertTrue(archive.getmember(self.stage + "/tentaflow").isfile())

    def test_missing_sidecar_fails_before_archive_is_created(self):
        self.sidecar.unlink()
        result = self.stage_archive()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("missing required artifact", result.stdout + result.stderr)
        self.assertFalse((self.directory / (self.stage + ".tar.gz")).exists())

    def test_archive_carries_the_coding_agent_bridge_next_to_the_main_binary(self):
        result = self.stage_archive()
        self.assertEqual(0, result.returncode, result.stderr)
        with tarfile.open(self.directory / (self.stage + ".tar.gz")) as archive:
            bridge = archive.getmember(self.stage + "/tentaflow-coding-agent-bridge")
            self.assertEqual(self.bridge_payload, archive.extractfile(bridge).read())
            self.assertEqual(0o755, bridge.mode & 0o777)

    def test_missing_bridge_fails_before_archive_is_created(self):
        self.bridge.unlink()
        result = self.stage_archive()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("missing required artifact", result.stdout + result.stderr)
        self.assertFalse((self.directory / (self.stage + ".tar.gz")).exists())


@unittest.skipUnless(shutil.which("bash") and shutil.which("tar"), "Test wymaga Bash i tar")
class LinuxPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-linux-packaging-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.target = self.directory / "target_shared"
        self.output = self.target / "x86_64-unknown-linux-gnu/release"
        self.output.mkdir(parents=True)
        (self.directory / "tentaflow").mkdir()
        (self.directory / "scripts/install").mkdir(parents=True)
        (self.directory / "native-libs/linux-x86_64/lib-dynamic").mkdir(parents=True)
        self.commands = self.directory / "commands"
        self.commands.mkdir()
        cargo = self.commands / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\n" "$TEST_CARGO_METADATA"\n', encoding="utf-8")
        cargo.chmod(0o755)
        sha256sum = self.commands / "sha256sum"
        sha256sum.write_text('#!/bin/sh\nprintf "%s  %s\n" fixture "$1"\n', encoding="utf-8")
        sha256sum.chmod(0o755)
        # The stage step pipes cargo metadata into python3. On Windows that name
        # resolves to the Microsoft Store alias, not an interpreter.
        python3 = self.commands / "python3"
        python3.write_text(f'#!/bin/sh\nexec "{Path(sys.executable).as_posix()}" "$@"\n', encoding="utf-8")
        python3.chmod(0o755)
        for name in ("tentaflow", "tentaflow-meeting", "libwhisper_tf.so"):
            (self.output / name).write_bytes(name.encode())
        for name in ("libzvec_c_api.so", "libpdfium.so", "libonnxruntime.so.1.30.0"):
            (self.directory / "native-libs/linux-x86_64/lib-dynamic" / name).write_bytes(name.encode())
        for name in ("LICENSE", "README.md", "scripts/install/tentaflow.service.in"):
            (self.directory / name).write_bytes(b"fixture")
        self.bridge = self.output / "tentaflow-coding-agent-bridge"
        self.bridge_payload = b'#!/bin/sh\nprintf "fixture mostka agentow\n"\n'
        self.bridge.write_bytes(self.bridge_payload)
        self.bridge.chmod(0o755)
        self.stage = "tentaflow-v0.2.0-beta-x86_64-unknown-linux-gnu-full"

    def stage_archive(self):
        return subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", linux_stage_script()],
            cwd=self.directory,
            env={**os.environ, "PATH": str(self.commands) + os.pathsep + os.environ["PATH"],
                 "TEST_CARGO_METADATA": json.dumps({"target_directory": str(self.target)}),
                 "TENTAFLOW_TARGET": "x86_64-unknown-linux-gnu", "GITHUB_REF_NAME": "v0.2.0-beta",
                 "GITHUB_OUTPUT": str(self.directory / "github-output")},
            capture_output=True, text=True, encoding="utf-8", timeout=20,
        )

    def test_archive_carries_the_coding_agent_bridge_next_to_the_main_binary(self):
        result = self.stage_archive()
        self.assertEqual(0, result.returncode, result.stderr)
        with tarfile.open(self.directory / (self.stage + ".tar.gz")) as archive:
            bridge = archive.getmember(self.stage + "/tentaflow-coding-agent-bridge")
            self.assertEqual(self.bridge_payload, archive.extractfile(bridge).read())
            self.assertEqual(0o755, bridge.mode & 0o777)
            self.assertTrue(archive.getmember(self.stage + "/tentaflow").isfile())

    def test_missing_bridge_fails_before_archive_is_created(self):
        self.bridge.unlink()
        result = self.stage_archive()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("missing required artifact", result.stdout + result.stderr)
        self.assertFalse((self.directory / (self.stage + ".tar.gz")).exists())

    def test_onnxruntime_is_taken_by_pattern_not_by_a_pinned_version(self):
        result = self.stage_archive()
        self.assertEqual(0, result.returncode, result.stderr)
        with tarfile.open(self.directory / (self.stage + ".tar.gz")) as archive:
            self.assertTrue(archive.getmember(self.stage + "/libonnxruntime.so.1.30.0").isfile())

    def test_onnxruntime_soname_the_binary_needs_resolves_inside_the_archive(self):
        result = self.stage_archive()
        self.assertEqual(0, result.returncode, result.stderr)
        with tarfile.open(self.directory / (self.stage + ".tar.gz")) as archive:
            soname = archive.getmember(self.stage + "/libonnxruntime.so.1")
            self.assertTrue(soname.issym())
            self.assertEqual("libonnxruntime.so.1.30.0", soname.linkname)

    def test_two_onnxruntime_versions_fail_instead_of_shipping_either(self):
        stale = self.directory / "native-libs/linux-x86_64/lib-dynamic/libonnxruntime.so.1.26.0"
        stale.write_bytes(b"stale")
        result = self.stage_archive()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("exactly one libonnxruntime", result.stdout + result.stderr)
        self.assertFalse((self.directory / (self.stage + ".tar.gz")).exists())


if __name__ == "__main__":
    unittest.main()
