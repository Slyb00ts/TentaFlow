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
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/release.yml"


def metal_stage_script():
    job = WORKFLOW.read_text(encoding="utf-8").split("\n  build-macos:\n", 1)[1]
    step = job.split("      - name: Stage archive\n", 1)[1]
    body = step.split("        run: |\n", 1)[1].split("\n      - ", 1)[0]
    return textwrap.dedent(body)


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


if __name__ == "__main__":
    unittest.main()
