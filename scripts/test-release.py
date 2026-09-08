#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/test-release.py
# Opis: Sprawdza centralną wersję i wydanie w lokalnym repozytorium testowym.
# Przykład: python3 scripts/test-release.py
# =============================================================================

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest


REPO = Path(__file__).resolve().parent.parent


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-release-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "source"
        self.root.mkdir()
        (self.root / "scripts").mkdir()
        for name in ("release.sh", "workspace-version.py"):
            shutil.copy2(REPO / "scripts" / name, self.root / "scripts" / name)
        (self.root / "Cargo.toml").write_text('''[workspace]
resolver = "2"
members = ["tentaflow", "other"]
[workspace.package]
version = "0.1.0-beta"
[workspace.dependencies]
wasm-bindgen = { version = "=0.2.125" }
''')
        for name, version in (("tentaflow", 'version.workspace = true'),
                              ("other", 'version = "7.4.0"')):
            directory = self.root / name
            (directory / "src").mkdir(parents=True)
            (directory / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n{version}\nedition = "2021"\n')
            (directory / "src/lib.rs").write_text("")
        (self.root / "CHANGELOG.md").write_text("# Historia\n\n## [0.2.0-beta] — przygotowywane\n\n- Opis wydania.\n")
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.run_command("git", "init", "-q", "-b", "main")
        for key, value in (("user.name", "Test wydania"), ("user.email", "release@example.invalid"),
                           ("commit.gpgsign", "false"), ("tag.gpgSign", "false")):
            self.run_command("git", "config", key, value)
        self.run_command("cargo", "generate-lockfile", "--offline")
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-qm", "test: przygotuj wydanie")
        self.remote = Path(self.temporary.name) / "origin.git"
        self.run_command("git", "init", "-q", "--bare", str(self.remote))
        self.run_command("git", "remote", "add", "origin", str(self.remote))

    def run_command(self, *arguments, check=True):
        return subprocess.run(arguments, cwd=self.root, env=self.env, check=check,
                              text=True, capture_output=True, timeout=30)

    def release(self, *arguments, check=True):
        return self.run_command("bash", "scripts/release.sh", *arguments, check=check)

    def test_central_package_and_dependency_versions_are_read_independently(self):
        self.assertEqual(self.run_command("python3", "scripts/workspace-version.py", "--package").stdout.strip(), "0.1.0-beta")
        self.assertEqual(self.run_command("python3", "scripts/workspace-version.py", "wasm-bindgen").stdout.strip(), "0.2.125")

    def test_dry_run_changes_neither_files_commits_nor_tags(self):
        before = self.run_command("git", "rev-parse", "HEAD").stdout
        result = self.release("--minor", "--dry-run")
        self.assertIn("Next version:    0.2.0-beta", result.stdout)
        self.assertEqual(self.run_command("git", "status", "--porcelain").stdout, "")
        self.assertEqual(self.run_command("git", "rev-parse", "HEAD").stdout, before)
        self.assertEqual(self.run_command("git", "tag", "--list").stdout, "")

    def test_release_commits_matching_root_and_lock_to_local_remote(self):
        changelog = (self.root / "CHANGELOG.md").read_bytes()
        child = (self.root / "tentaflow/Cargo.toml").read_bytes()
        self.release("--set", "0.2.0-beta")
        self.run_command("cargo", "metadata", "--offline", "--locked", "--format-version", "1")
        package = tomllib.loads((self.root / "Cargo.toml").read_text())["workspace"]["package"]
        lock = tomllib.loads((self.root / "Cargo.lock").read_text())
        self.assertEqual(package["version"], "0.2.0-beta")
        self.assertEqual({p["name"]: p["version"] for p in lock["package"]},
                         {"tentaflow": "0.2.0-beta", "other": "7.4.0"})
        self.assertEqual((self.root / "CHANGELOG.md").read_bytes(), changelog)
        self.assertEqual((self.root / "tentaflow/Cargo.toml").read_bytes(), child)
        self.assertEqual(set(self.run_command("git", "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD").stdout.splitlines()),
                         {"Cargo.toml", "Cargo.lock"})
        remote_head = self.run_command("git", f"--git-dir={self.remote}", "rev-parse", "refs/tags/v0.2.0-beta^{}").stdout
        self.assertEqual(remote_head, self.run_command("git", "rev-parse", "HEAD").stdout)
        self.assertEqual(self.run_command("git", "status", "--porcelain").stdout, "")

    def test_already_prepared_version_tags_existing_commit(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "0.1.0-beta"', 'version = "0.2.0-beta"'))
        self.run_command("cargo", "update", "--workspace", "--offline")
        self.run_command("git", "add", "Cargo.toml", "Cargo.lock")
        self.run_command("git", "commit", "-qm", "test: ustaw wersję")
        before = self.run_command("git", "rev-parse", "HEAD").stdout
        self.release("--set", "0.2.0-beta")
        self.assertEqual(self.run_command("git", "rev-parse", "HEAD").stdout, before)
        self.assertEqual(self.run_command("git", f"--git-dir={self.remote}", "rev-parse", "refs/tags/v0.2.0-beta^{}").stdout, before)

    def test_missing_release_notes_fail_before_changing_version(self):
        before = (self.root / "Cargo.toml").read_bytes()
        result = self.release("--set", "0.3.0-beta", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CHANGELOG.md", result.stderr)
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), before)
        self.assertEqual(self.run_command("git", "status", "--porcelain").stdout, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
