#!/usr/bin/env python3
# =============================================================================
# Plik: test-cargo-workspace.py
# Opis: Testy reguł centralizacji Cargo na izolowanych repozytoriach Git.
# Przykład: python3 scripts/test-cargo-workspace.py
# =============================================================================

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
import os
import shutil
import sys

spec = importlib.util.spec_from_file_location("workspace_check", Path(__file__).with_name("check-cargo-workspace.py"))
workspace_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workspace_check)


class WorkspaceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-workspace-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        subprocess.run(["git", "init", "-q", self.root], check=True)
        self.write("Cargo.toml", '[workspace]\nresolver="2"\nmembers=["app", "addons*/[ps]*"]\n[workspace.dependencies]\nserde="1"\n[profile.release]\nlto="thin"\n')
        self.write("Cargo.lock", "version=4\n")
        self.write("app/Cargo.toml", '[package]\nname="app"\nversion="2.0.0"\n[dependencies]\nserde={workspace=true}\n')
        self.write("addons/public/Cargo.toml", '[package]\nname="public"\nversion="0.1.0"\n')
        self.write("app/src/lib.rs", "")
        self.write("addons/public/src/lib.rs", "")

    def write(self, name, contents):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
        subprocess.run(["git", "add", "--", name], cwd=self.root, check=True)

    def errors(self):
        return workspace_check.validate(self.root)[0]

    def test_package_versions_remain_independent(self):
        self.assertEqual(self.errors(), [])

    def test_unicode_workspace_path_without_python_utf8_mode(self):
        with tempfile.TemporaryDirectory(prefix="tentaflow-zażółć-") as directory:
            root = Path(directory).resolve() / "źródła"
            shutil.copytree(self.root, root)
            result = subprocess.run(
                [sys.executable, str(Path(workspace_check.__file__)), "--root", str(root)],
                env=dict(os.environ, PYTHONUTF8="0", PYTHONIOENCODING="utf-8"),
                capture_output=True,
                text=True,
                encoding="utf-8",
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Workspace poprawny", result.stdout)

    def test_child_dependency_version_is_rejected(self):
        self.write("app/Cargo.toml", '[package]\nname="app"\n[dependencies]\nserde={workspace=true,version="1"}\n')
        self.assertTrue(any("powiela pola version" in error for error in self.errors()))

    def test_target_dependency_without_inheritance_is_rejected(self):
        self.write("app/Cargo.toml", '[package]\nname="app"\n[target."cfg(windows)".dependencies]\nserde="1"\n')
        self.assertTrue(any("nie dziedziczy" in error for error in self.errors()))

    def test_profile_in_cargo_configuration_is_rejected(self):
        self.write(".cargo/config.toml", '[profile.dev]\ndebug=1\n')
        self.assertTrue(any("sekcja [profile]" in error for error in self.errors()))

    def test_owned_package_outside_members_is_rejected(self):
        self.write("missed/Cargo.toml", '[package]\nname="missed"\n')
        self.assertTrue(any("poza workspace" in error for error in self.errors()))

    def test_untracked_owned_package_is_rejected(self):
        self.write("untracked/Cargo.toml", '[package]\nname="untracked"\nversion="0.1.0"\n')
        subprocess.run(["git", "rm", "--cached", "-q", "untracked/Cargo.toml"], cwd=self.root, check=True)
        self.assertTrue(any("poza workspace" in error for error in self.errors()))

    def test_optional_pro_package_is_validated_by_glob(self):
        self.write("addons-pro/private/Cargo.toml", '[package]\nname="private"\n[dependencies]\nserde="1"\n')
        self.assertTrue(any("addons-pro/private" in error for error in self.errors()))

    def test_non_cargo_addon_directory_is_ignored(self):
        (self.root / "addons/dotnet").mkdir()
        self.assertEqual(self.errors(), [])

    def test_manifest_file_glob_is_rejected(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["app", "addons*/*/Cargo.toml"]\n[workspace.dependencies]\nserde="1"\n')
        self.assertTrue(any("musi wskazywać katalog" in error for error in self.errors()))

    def test_new_pro_package_outside_patterns_is_rejected(self):
        self.write("tentaflow-core/addons-pro/new-addon/Cargo.toml", '[package]\nname="new-addon"\nversion="0.1.0"\n')
        subprocess.run(["git", "rm", "--cached", "-q", "tentaflow-core/addons-pro/new-addon/Cargo.toml"], cwd=self.root, check=True)
        self.assertTrue(any("poza workspace" in error for error in self.errors()))

    def test_upstream_manifests_are_not_rewritten(self):
        self.write("vendor/external/Cargo.toml", '[package]\nname="external"\n[dependencies]\nserde="1"\n[profile.release]\nlto=true\n')
        self.assertEqual(self.errors(), [])

    def test_deleted_old_lockfiles_do_not_fail(self):
        self.write("app/Cargo.lock", "version=4\n")
        (self.root / "app/Cargo.lock").unlink()
        self.assertEqual(self.errors(), [])

    def test_existing_child_lockfile_is_rejected(self):
        self.write("app/Cargo.lock", "version=4\n")
        self.assertTrue(any("lockfile poza korzeniem" in error for error in self.errors()))


if __name__ == "__main__":
    unittest.main()
