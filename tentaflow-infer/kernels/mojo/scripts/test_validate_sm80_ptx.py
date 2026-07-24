#!/usr/bin/env python3
# =============================================================================
# Plik: test_validate_sm80_ptx.py
# Opis: Sprawdza transakcyjną publikację pary PTX i manifestu.
# Przykład: python -m unittest test_validate_sm80_ptx
# =============================================================================

import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from validate_sm80_ptx import main


class ValidateSm80PtxTest(unittest.TestCase):
    def assert_no_staging_files(self, root):
        leftovers = [
            path.name
            for path in root.iterdir()
            if path.name.endswith(".tmp") or path.name.endswith(".previous")
        ]
        self.assertEqual(leftovers, [])

    def test_ptxas_failure_preserves_previous_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.ptx"
            target = root / "kernel.ptx"
            manifest = root / "manifest.json"
            ptxas = root / "ptxas"
            source.write_text(
                ".version 8.1\n.target sm_89\n"
                ".visible .entry test_kernel() {}\n"
            )
            target.write_text(
                ".version 8.1\n.target sm_80\n"
                ".visible .entry old_kernel() {}\n"
            )
            manifest.write_text(
                '{"kernels":{"test_kernel":{"entry":"old_kernel","file":"kernel.ptx"}}}\n'
            )
            ptxas.write_text("#!/bin/sh\nexit 9\n")
            ptxas.chmod(0o755)

            with patch.dict("os.environ", {"FORGE_REAL_PTXAS": str(ptxas)}):
                with patch(
                    "sys.argv",
                    [
                        "validate_sm80_ptx.py",
                        str(source),
                        str(target),
                        "test_kernel",
                        str(manifest),
                    ],
                ):
                    with self.assertRaises(Exception):
                        main()

            self.assertFalse(source.exists())
            self.assertIn(".visible .entry old_kernel()", target.read_text())
            self.assertIn('"entry":"old_kernel"', manifest.read_text())
            self.assert_no_staging_files(root)

    def test_manifest_failure_preserves_previous_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.ptx"
            target = root / "kernel.ptx"
            manifest = root / "manifest.json"
            ptxas = root / "ptxas"
            source.write_text(
                ".version 8.1\n.target sm_89\n"
                ".visible .entry test_kernel() {}\n"
            )
            target.write_text(
                ".version 8.1\n.target sm_80\n"
                ".visible .entry old_kernel() {}\n"
            )
            manifest.write_text("{niepoprawny json")
            ptxas.write_text("#!/bin/sh\nexit 0\n")
            ptxas.chmod(0o755)

            with patch.dict("os.environ", {"FORGE_REAL_PTXAS": str(ptxas)}):
                with patch(
                    "sys.argv",
                    [
                        "validate_sm80_ptx.py",
                        str(source),
                        str(target),
                        "test_kernel",
                        str(manifest),
                    ],
                ):
                    with self.assertRaises(Exception):
                        main()

            self.assertFalse(source.exists())
            self.assertIn(".visible .entry old_kernel()", target.read_text())
            self.assertEqual(manifest.read_text(), "{niepoprawny json")
            self.assert_no_staging_files(root)

    def test_replace_failure_rolls_back_previous_pair(self):
        for failed_replace in range(1, 5):
            with self.subTest(failed_replace=failed_replace):
                with tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    source = root / "source.ptx"
                    target = root / "kernel.ptx"
                    manifest = root / "manifest.json"
                    ptxas = root / "ptxas"
                    source.write_text(
                        ".version 8.1\n.target sm_89\n"
                        ".visible .entry test_kernel() {}\n"
                    )
                    old_target = (
                        ".version 8.1\n.target sm_80\n"
                        ".visible .entry old_kernel() {}\n"
                    )
                    old_manifest = (
                        '{"kernels":{"test_kernel":'
                        '{"entry":"old_kernel","file":"kernel.ptx"}}}\n'
                    )
                    target.write_text(old_target)
                    manifest.write_text(old_manifest)
                    ptxas.write_text("#!/bin/sh\nexit 0\n")
                    ptxas.chmod(0o755)
                    real_replace = __import__("os").replace
                    replace_count = 0

                    def failing_replace(source_path, target_path):
                        nonlocal replace_count
                        replace_count += 1
                        if replace_count == failed_replace:
                            raise OSError(f"błąd replace {failed_replace}")
                        return real_replace(source_path, target_path)

                    with patch.dict("os.environ", {"FORGE_REAL_PTXAS": str(ptxas)}):
                        with patch("validate_sm80_ptx.os.replace", failing_replace):
                            with patch(
                                "sys.argv",
                                [
                                    "validate_sm80_ptx.py",
                                    str(source),
                                    str(target),
                                    "test_kernel",
                                    str(manifest),
                                ],
                            ):
                                with self.assertRaises(OSError):
                                    main()

                    self.assertFalse(source.exists())
                    self.assertEqual(target.read_text(), old_target)
                    self.assertEqual(manifest.read_text(), old_manifest)
                    self.assert_no_staging_files(root)


if __name__ == "__main__":
    unittest.main()
