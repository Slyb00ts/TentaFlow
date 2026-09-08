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
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("workspace_export", ROOT / "scripts/export-workspace.py")
EXPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORT)


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-workspace-export-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()

    def test_offline_export_prunes_lock_without_downloading_other_target_sources(self):
        root = self.directory / "source"
        root.mkdir()
        (root / "Cargo.toml").write_text('[workspace]\nresolver="2"\nmembers=["example", "unused"]\n[workspace.dependencies]\n', encoding="utf-8")
        for name in ("example", "unused"):
            (root / name / "src").mkdir(parents=True)
            (root / name / "Cargo.toml").write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n', encoding="utf-8")
            (root / name / "src/lib.rs").write_text("", encoding="utf-8")
        with (root / "example/Cargo.toml").open("a", encoding="utf-8") as manifest:
            manifest.write('[target.\'cfg(windows)\'.dependencies]\nwindows-link="0.2"\n')
        checksum = "45e46c0661abb7180e7b9c281db115305d49ca1709ab8242adf09666d2173c65"
        (root / "Cargo.lock").write_text(f'''version = 4
[[package]]
name="example"
version="0.1.0"
dependencies=["windows-link"]
[[package]]
name="unused"
version="0.1.0"
[[package]]
name="windows-link"
version="0.2.0"
source="registry+https://github.com/rust-lang/crates.io-index"
checksum="{checksum}"
''', encoding="utf-8")
        root_lock = (root / "Cargo.lock").read_bytes()
        (root / "vendor").mkdir()
        (root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/export-workspace.py", root / "scripts/export-workspace.py")
        registry = self.directory / "registry"
        (registry / "index/wi/nd").mkdir(parents=True)
        # Prawdziwe wpisy bez archiwów źródeł; nowsza wersja nie może zmienić locka.
        entries = [{"name": "windows-link", "vers": version, "deps": [],
                    "cksum": digest, "features": {}, "yanked": False}
                   for version, digest in [("0.2.0", checksum),
                       ("0.2.1", "f0805222e57f7521d6a62e36fa9163bc891acd422f971defe97d64e70d0a4fe5")]]
        (registry / "index/wi/nd/windows-link").write_text(
            "".join(json.dumps(entry) + "\n" for entry in entries), encoding="utf-8")
        cargo_home = self.directory / "cargo-home"
        cargo_home.mkdir()
        (cargo_home / "config.toml").write_text(
            '[source.crates-io]\nreplace-with="fixture"\n[source.fixture]\n'
            f'local-registry={json.dumps(str(registry))}\n', encoding="utf-8")
        output = self.directory / "export"
        with patch.dict(os.environ, {"CARGO_HOME": str(cargo_home)}):
            fetched = subprocess.run(["cargo", "fetch", "--locked", "--offline", "--target", "x86_64-unknown-linux-gnu"], cwd=root, capture_output=True, text=True, encoding="utf-8", timeout=30)
            self.assertEqual(0, fetched.returncode, fetched.stderr)
            metadata = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version=1"], cwd=root, capture_output=True, text=True, encoding="utf-8", timeout=30)
            self.assertNotEqual(0, metadata.returncode)
            self.assertIn("windows-link", metadata.stderr)
            EXPORT.export_workspace(root, output, ["example"], self.directory / "bundle.tar.gz")
        self.assertEqual(root_lock, (root / "Cargo.lock").read_bytes())
        packages = EXPORT.load_manifest(output / "Cargo.lock")["package"]
        self.assertEqual({"example", "windows-link"}, {p["name"] for p in packages})
        dependency = next(p for p in packages if p["name"] == "windows-link")
        self.assertEqual(("0.2.0", checksum), (dependency["version"], dependency["checksum"]))
        self.assertFalse((cargo_home / "registry/src").exists())

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
        for directory in ("tentaflow-containers", "vendor", "tentaflow-containers/runtime/target", "tentaflow-containers/runtime/.build-linux", "tentaflow-containers/output", "private-assets"):
            (root / directory).mkdir(parents=True, exist_ok=True)
        (root / "tentaflow-containers/runtime/src").mkdir()
        (root / "tentaflow-containers/runtime/Cargo.toml").write_text('[package]\nname="runtime"\nversion="0.1.0"\n')
        (root / "tentaflow-containers/runtime/src/lib.rs").write_text("pub fn runtime() {}")
        for name in EXPORT.EXTRA_FILES:
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text("dane")
        for name in ("tentaflow-containers/runtime/target/old.rlib", "tentaflow-containers/runtime/.build-linux/native.a", "tentaflow-containers/output/native.tar.gz", "tentaflow-containers/.env", "private-assets/secret"):
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
