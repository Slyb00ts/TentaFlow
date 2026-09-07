#!/usr/bin/env python3
# =============================================================================
# Plik: test-cargo-build.py
# Opis: Sprawdza retencję cache oraz współpracę wrappera z prawdziwym Cargo.
# Przykład: python3 scripts/test-cargo-build.py
# =============================================================================

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("cargo-build.py")
SPEC = importlib.util.spec_from_file_location("cargo_build", SCRIPT)
CACHE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CACHE)


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-cache-test-")
        self.addCleanup(self.temporary.cleanup)
        self.target = Path(self.temporary.name).resolve() / "target"
        self.target.mkdir()
        (self.target / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55", encoding="utf-8")
        (self.target / "debug").mkdir()
        (self.target / "debug/.cargo-lock").touch()
        self.config = dict(successful_builds=5, max_gib=0.000001, older_than_days=14, bootstrap_variants=1, incremental_max_gib=0.0000002, incremental_variants=2)

    def artifact(self, name, tag, modified, size=4096):
        root = self.target / "debug"
        fingerprint = root / ".fingerprint" / f"{name}-{tag}"
        fingerprint.mkdir(parents=True)
        (fingerprint / f"lib-{name}.json").write_text("{}", encoding="utf-8")
        dependency = root / "deps" / f"lib{name}-{tag}.rlib"
        dependency.parent.mkdir(exist_ok=True)
        dependency.write_bytes(b"x" * size)
        for path in (fingerprint, *fingerprint.iterdir(), dependency):
            os.utime(path, (modified, modified))
        return dependency

    def plan(self, history=None):
        return CACHE.make_plan(self.target, self.config, history or [])

    def candidates(self, plan):
        return {name.replace("\\", "/") for group in plan["candidates"] for name in group["paths"]}

    def test_preserves_recorded_fresh_artifact_and_latest_variant(self):
        old = self.artifact("core", "1111111111111111", 1)
        stale = self.artifact("core", "2222222222222222", 2)
        current = self.artifact("core", "3333333333333333", 3)
        plan = self.plan([{"artifacts": [str(old.relative_to(self.target))]}])
        self.assertEqual({f"debug/deps/{stale.name}", "debug/.fingerprint/core-2222222222222222"}, self.candidates(plan))
        self.assertGreater(plan["after_bytes"], plan["max_bytes"])
        self.assertTrue(current.exists())

    def test_apply_removes_whole_obsolete_group_but_not_sources_or_lock(self):
        old = self.artifact("core", "1111111111111111", 1)
        current = self.artifact("core", "2222222222222222", 2)
        source = self.target.parent / "source.rs"
        source.write_text("fn main() {}", encoding="utf-8")
        with contextlib.redirect_stderr(io.StringIO()):
            CACHE.prune(self.target, self.config, apply=True)
        self.assertFalse(old.exists())
        self.assertFalse((self.target / "debug/.fingerprint/core-1111111111111111").exists())
        self.assertTrue(current.exists())
        self.assertTrue((self.target / "debug/.cargo-lock").exists())
        self.assertEqual("fn main() {}", source.read_text(encoding="utf-8"))

    def test_dry_run_saves_plan_without_deleting_artifacts(self):
        old = self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        with contextlib.redirect_stderr(io.StringIO()):
            CACHE.prune(self.target, self.config)
        self.assertTrue(old.exists())
        self.assertTrue((self.target / ".build-cache-plan.json").exists())

    def test_history_keeps_last_five_successful_records(self):
        for index in range(7):
            CACHE.save_build(self.target, {f"debug/deps/item{index}"}, self.config)
        self.assertEqual([f"debug/deps/item{i}" for i in range(2, 7)], [r["artifacts"][0] for r in CACHE.read_history(self.target)])

    def test_rejects_history_path_escape(self):
        (self.target / ".build-cache-history.json").write_text(json.dumps([{"artifacts": ["../source.rs"]}]), encoding="utf-8")
        with self.assertRaises(CACHE.CacheError):
            CACHE.prune(self.target, self.config, apply=True)

    def test_symlink_preserves_group_and_never_follows_external_tree(self):
        old = self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        outside = self.target.parent / "outside"
        outside.mkdir()
        (outside / "keep").write_text("dane", encoding="utf-8")
        link = self.target / "debug/.fingerprint/core-1111111111111111/external"
        try:
            link.symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("System nie pozwala utworzyć testowego dowiązania")
        self.assertNotIn(f"debug/deps/{old.name}", self.candidates(self.plan()))
        self.assertEqual("dane", (outside / "keep").read_text(encoding="utf-8"))

    @unittest.skipUnless(os.name == "nt", "Junction wymaga Windows")
    def test_junction_preserves_external_files_during_prune(self):
        old = self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        outside = self.target.parent / "outside"
        outside.mkdir()
        (outside / "keep").write_text("dane", encoding="utf-8")
        link = self.target / "debug/.fingerprint/core-1111111111111111/external"
        subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(outside)], check=True, capture_output=True)
        self.addCleanup(lambda: os.rmdir(link) if link.exists() else None)
        with contextlib.redirect_stderr(io.StringIO()):
            CACHE.prune(self.target, self.config, apply=True)
        self.assertTrue(old.exists())
        self.assertEqual("dane", (outside / "keep").read_text(encoding="utf-8"))
        with self.assertRaises(CACHE.CacheError):
            CACHE.validate_target(link)

    def test_refuses_source_directory_as_target(self):
        (self.target / "Cargo.toml").write_text("[package]", encoding="utf-8")
        with self.assertRaises(CACHE.CacheError):
            CACHE.prune(self.target, self.config, apply=True)

    def test_unlocked_new_profile_never_enters_deletion_plan(self):
        self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        plan = CACHE.make_plan(self.target, self.config, [], locked_profiles=set())
        self.assertEqual([], plan["candidates"])

    def test_metadata_receives_config_and_offline_options(self):
        self.assertEqual(["--offline", "--config", "build.target-dir='custom'", "--locked"], CACHE.metadata_arguments(["--offline", "--config", "build.target-dir='custom'", "--locked", "--", "--nocapture"]))

    def test_incremental_budget_preserves_latest_per_crate(self):
        for index in range(4):
            folder = self.target / "debug/incremental" / f"core-{index}"
            folder.mkdir(parents=True)
            artifact = folder / "work-products.bin"
            artifact.write_bytes(b"x" * 500)
            for path in (folder, artifact):
                os.utime(path, (index + 1, index + 1))
        self.assertEqual({f"debug/incremental/core-{index}" for index in range(3)}, self.candidates(self.plan()))

    def test_hardlinked_top_level_binary_is_preserved_and_not_reclaimed(self):
        old = self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        top = self.target / "debug/application"
        os.link(old, top)
        plan = self.plan()
        self.assertLess(sum(group["bytes"] for group in plan["candidates"]), old.stat().st_size)
        self.assertTrue(top.exists())

    def test_history_top_level_binary_protects_hardlinked_group(self):
        old = self.artifact("core", "1111111111111111", 1)
        self.artifact("core", "2222222222222222", 2)
        os.link(old, self.target / "debug/application")
        self.assertEqual([], self.plan([{"artifacts": ["debug/application"]}])["candidates"])


@unittest.skipUnless(shutil.which("cargo"), "Test wymaga rzeczywistego Cargo")
class CargoIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-cargo-integration-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve() / "projekt-żółć"
        (self.root / "src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text('[package]\nname="cache-probe"\nversion="0.1.0"\nedition="2021"\n', encoding="utf-8")
        (self.root / "src/main.rs").write_text('fn main() { println!("działa"); }\n', encoding="utf-8")
        self.target = self.root / "target"
        self.env = dict(os.environ, RUSTC_WRAPPER="", CARGO_TARGET_DIR=str(self.target), PYTHONUTF8="1")

    def test_successful_build_then_fresh_build_collects_artifacts(self):
        command = [sys.executable, str(SCRIPT), "build", "--offline", "--manifest-path", str(self.root / "Cargo.toml"), "--target-dir", str(self.target)]
        first = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True, encoding="utf-8", timeout=60)
        second = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True, encoding="utf-8", timeout=60)
        self.assertEqual(0, first.returncode, first.stderr)
        self.assertEqual(0, second.returncode, second.stderr)
        history = CACHE.read_history(self.target)
        self.assertEqual(2, len(history), first.stderr + second.stderr)
        self.assertTrue(history[1]["artifacts"])
        self.assertEqual(history[0]["artifacts"], history[1]["artifacts"])

    def test_failed_build_does_not_record_success_or_prune(self):
        (self.root / "src/main.rs").write_text("fn main() { undefined_function(); }", encoding="utf-8")
        command = [sys.executable, str(SCRIPT), "build", "--offline", "--manifest-path", str(self.root / "Cargo.toml")]
        result = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True, encoding="utf-8", timeout=60)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("undefined_function", result.stderr)
        self.assertEqual([], CACHE.read_history(self.target))
        self.assertFalse((self.target / ".build-cache-plan.json").exists())

    def test_separate_build_dir_environment_is_checked_with_older_metadata(self):
        metadata = {"target_directory": str(self.target), "workspace_root": str(self.root)}
        result = subprocess.CompletedProcess([], 0, stdout=json.dumps(metadata))
        with mock.patch.dict(os.environ, {"CARGO_BUILD_BUILD_DIR": str(self.root / "separate")}):
            with mock.patch.object(CACHE.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(CACHE.CacheError, "Osobny build.build-dir"):
                    CACHE.run_cargo("build", [], CACHE.read_config())

    def test_nested_cache_skips_active_lock_then_prunes_after_fresh_build(self):
        nested = self.root / "target-browser-wasm"
        (nested / "debug").mkdir(parents=True)
        (nested / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55", encoding="utf-8")
        lock = nested / "debug/.cargo-lock"
        lock.touch()
        artifacts = []
        for index in (1, 2):
            tag = str(index) * 16
            fingerprint = nested / "debug/.fingerprint" / f"codec-{tag}"
            fingerprint.mkdir(parents=True)
            marker = fingerprint / "lib-codec.json"
            marker.write_text("{}", encoding="utf-8")
            artifact = nested / "debug/deps" / f"libcodec-{tag}.rlib"
            artifact.parent.mkdir(exist_ok=True)
            artifact.write_bytes(b"regenerowalny cache")
            for path in (fingerprint, marker, artifact):
                os.utime(path, (index, index))
            artifacts.append(artifact)
        command = [sys.executable, str(SCRIPT), "build", "--offline", "--manifest-path", str(self.root / "Cargo.toml")]
        with CACHE.file_lock(lock):
            first = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True, encoding="utf-8", timeout=60)
            self.assertEqual(0, first.returncode, first.stderr)
            self.assertIn("Retencja target-browser-wasm wstrzymana", first.stderr)
            self.assertTrue(artifacts[0].exists())
        second = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True, encoding="utf-8", timeout=60)
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertFalse(artifacts[0].exists())
        self.assertTrue(artifacts[1].exists())
        self.assertTrue(lock.exists())

    def test_prune_refuses_lock_held_by_actual_cargo(self):
        (self.root / "build.rs").write_text('fn main() { std::fs::write("build-started", "1").unwrap(); std::thread::sleep(std::time::Duration::from_secs(15)); }\n', encoding="utf-8")
        cargo = subprocess.Popen(["cargo", "build", "--offline"], cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 30
            while not (self.root / "build-started").exists() and time.monotonic() < deadline:
                if cargo.poll() is not None:
                    self.fail(cargo.communicate()[1].decode())
                time.sleep(0.05)
            self.assertTrue((self.root / "build-started").exists())
            with self.assertRaisesRegex(CACHE.CacheError, "Cache zajęty"):
                CACHE.prune(self.target, CACHE.read_config(), apply=True)
            self.assertTrue((self.target / "debug/.cargo-lock").exists())
        finally:
            cargo.wait(timeout=30)
            cargo.communicate()


if __name__ == "__main__":
    unittest.main()
