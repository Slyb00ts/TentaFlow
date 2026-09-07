#!/usr/bin/env python3
# =============================================================================
# Plik: test-export-workspace.py
# Opis: Sprawdza samodzielność eksportowanego workspace i zakres bundla.
# Przykład: python3 scripts/test-export-workspace.py
# =============================================================================

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import os

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("workspace_export", ROOT / "scripts/export-workspace.py")
EXPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORT)


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-workspace-export-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()

    @unittest.skipUnless(os.name == "nt", "Junction wymaga Windows")
    def test_junction_source_and_destination_are_rejected(self):
        source = self.directory / "source"
        outside = self.directory / "outside"
        source.mkdir()
        outside.mkdir()
        (outside / "keep").write_text("prywatne", encoding="utf-8")
        link = source / "external"
        subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(outside)], check=True, capture_output=True)
        self.addCleanup(lambda: os.rmdir(link) if link.exists() else None)
        with self.assertRaises(ValueError):
            EXPORT.copy_sources(source, source, self.directory / "copy")
        with self.assertRaises(ValueError):
            EXPORT.export_workspace(source, link, [], self.directory / "bundle.tar.gz")
        self.assertEqual("prywatne", (outside / "keep").read_text(encoding="utf-8"))

    @unittest.skipUnless(shutil.which("cargo"), "Test wymaga Cargo")
    def test_real_container_subset_resolves_offline_with_root_lock(self):
        target = self.directory / "context"
        archive = self.directory / "bundle.tar.gz"
        members = EXPORT.export_workspace(ROOT, target, EXPORT.CONTAINER_MEMBERS, archive)
        with tarfile.open(archive) as bundle:
            bundle.extractall(target, filter="data")
        lock_before = (target / "Cargo.lock").read_bytes()
        result = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version=1"], cwd=target, capture_output=True, text=True, encoding="utf-8", timeout=120)
        self.assertEqual(0, result.returncode, result.stderr)
        metadata = json.loads(result.stdout)
        expected = {str((target / member / "Cargo.toml").resolve()) for member in members}
        actual = {package["manifest_path"] for package in metadata["packages"] if package["id"] in metadata["workspace_members"]}
        self.assertEqual(expected, actual)
        self.assertEqual(lock_before, (target / "Cargo.lock").read_bytes())

    @unittest.skipUnless(shutil.which("cargo"), "Test wymaga Cargo")
    def test_sdk_template_export_resolves_independently(self):
        target = self.directory / "sdk-context"
        archive = self.directory / "sdk.tar.gz"
        members = EXPORT.export_workspace(ROOT, target, ["tentaflow-core/addon-sdk/template"], archive)
        with tarfile.open(archive) as bundle:
            bundle.extractall(target, filter="data")
        result = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version=1"], cwd=target, capture_output=True, text=True, encoding="utf-8", timeout=120)
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("tentaflow-core/addon-sdk/sdk", members)
        self.assertFalse((target / "tentaflow-containers").exists())

    def test_manifest_preserves_central_versions_and_profiles(self):
        members = EXPORT.member_closure(ROOT, EXPORT.CONTAINER_MEMBERS)
        exported = EXPORT.tomllib.loads(EXPORT.exported_manifest(ROOT, members))
        original = EXPORT.load_manifest(ROOT / "Cargo.toml")
        self.assertEqual(original["workspace"]["dependencies"], exported["workspace"]["dependencies"])
        self.assertEqual(original["profile"], exported["profile"])
        self.assertNotIn("tentaflow-core", exported["workspace"]["members"])

    def test_archive_excludes_build_outputs_and_private_environment(self):
        root = self.directory / "source"
        root.mkdir()
        (root / "Cargo.toml").write_text('[workspace]\nresolver="2"\nmembers=["example"]\n[workspace.dependencies]\n')
        (root / "Cargo.lock").write_text('version = 4\n[[package]]\nname="example"\nversion="0.1.0"\n[[package]]\nname="runtime"\nversion="0.1.0"\n')
        (root / "example/src").mkdir(parents=True)
        (root / "example/Cargo.toml").write_text('[package]\nname="example"\nversion="0.1.0"\n')
        (root / "example/src/lib.rs").write_text('pub fn answer() -> u8 { 42 }')
        (root / "example/Cargo.lock").write_text("stary lock ignorowany przez workspace", encoding="utf-8")
        (root / "example/src/obsolete.rs").write_text('pub const VALUE: u8 = 1;')
        for directory in ("tentaflow-containers", "vendor", "tentaflow-containers/runtime/target", "tentaflow-containers/runtime/.build-linux", "tentaflow-containers/output", "tentaflow-core/addons-pro/private"):
            (root / directory).mkdir(parents=True, exist_ok=True)
        (root / "tentaflow-containers/runtime/src").mkdir()
        (root / "tentaflow-containers/runtime/Cargo.toml").write_text('[package]\nname="runtime"\nversion="0.1.0"\n')
        (root / "tentaflow-containers/runtime/src/lib.rs").write_text("pub fn runtime() {}")
        for name in EXPORT.EXTRA_FILES:
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text("dane")
        for name in ("tentaflow-containers/runtime/target/old.rlib", "tentaflow-containers/runtime/.build-linux/native.a", "tentaflow-containers/output/native.tar.gz", "tentaflow-containers/.env", "tentaflow-core/addons-pro/private/secret"):
            (root / name).write_text("prywatne")
        (root / "tentaflow-containers/.env.example").write_text("PORT=8090")
        archive = self.directory / "bundle.tar.gz"
        EXPORT.export_workspace(root, self.directory / "export", ["example", "tentaflow-containers/runtime"], archive)
        with tarfile.open(archive) as bundle:
            names = set(bundle.getnames())
        self.assertIn("Cargo.toml", names)
        self.assertEqual(["Cargo.lock"], sorted(name for name in names if name.endswith("Cargo.lock")))
        self.assertIn("example/src/lib.rs", names)
        self.assertIn("tentaflow-containers/.env.example", names)
        self.assertFalse(any("target/" in name or ".build-" in name or "containers/output" in name or "private" in name or name.endswith("/.env") for name in names))
        (root / "example/src/obsolete.rs").unlink()
        (root / "example/src/lib.rs").write_text('pub fn answer() -> u8 { 7 }')
        EXPORT.export_workspace(root, self.directory / "export", ["example", "tentaflow-containers/runtime"], archive)
        with tarfile.open(archive) as bundle:
            self.assertNotIn("example/src/obsolete.rs", bundle.getnames())
            self.assertEqual(b'pub fn answer() -> u8 { 7 }', bundle.extractfile("example/src/lib.rs").read())


if __name__ == "__main__":
    unittest.main()
