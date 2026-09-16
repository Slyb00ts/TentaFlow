#!/usr/bin/env python3
# =============================================================================
# Plik: test-android-native-cache.py
# Opis: Sprawdza decyzję skryptu Androida o użyciu lub przebudowie bibliotek.
# =============================================================================

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "tentaflow-mobile/android/scripts/build-rust.sh").read_text()
FUNCTIONS = SOURCE[SOURCE.index("platform_for_abi() {"):SOURCE.index("rust_target_for_abi() {")] + SOURCE[SOURCE.index("native_libs_ready() {"):SOURCE.index('BUILD_MODE="')]


class AndroidNativeCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.platform = self.root / "native-libs/android-arm64"
        for name in (
            "lib-static/libzvec_c_api.a",
            "lib-static/llama-cpp/multi/libllama.a",
            "lib-dynamic/whisper-cpp/multi/libwhisper_tf.so",
            "lib-dynamic/libc++_shared.so",
        ):
            path = self.platform / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.touch()
        builder = self.root / "scripts/native-libs/build-all-android.sh"
        builder.parent.mkdir(parents=True)
        builder.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$BUILD_CALLS"\n')
        builder.chmod(0o755)
        self.calls = self.root / "build-calls"
        self.pin, self.zvec_pin = subprocess.check_output(
            ["bash", "-c", 'source "$1/scripts/native-libs/common.sh"; printf "%s\\n%s" "$LLAMA_CPP_REF" "$ZVEC_REF"', "bash", str(ROOT)],
            text=True,
        ).splitlines()

    def run_ensure(self, manifest, ref=None):
        path = self.platform / "manifest.toml"
        if manifest is not None:
            path.write_text(manifest)
        script = '''set -euo pipefail
source "$1/scripts/native-libs/common.sh"
REPO_ROOT="$2"
NATIVE_LIBS_DIR="$2/native-libs"
''' + FUNCTIONS + '\nensure_native_libs arm64-v8a\n'
        env = dict(os.environ, BUILD_CALLS=str(self.calls))
        if ref is not None:
            env["LLAMA_CPP_REF"] = ref
        subprocess.run(["bash", "-c", script, "bash", str(ROOT), str(self.root)], env=env, check=True, capture_output=True, text=True)
        return self.calls.read_text() if self.calls.exists() else ""

    def entry(self, ref):
        return f'[[library]]\nname = "llama-cpp-multi"\nref = "{ref}"\n\n[[library]]\nname = "zvec"\nref = "{self.zvec_pin}"\n\n'

    def test_matching_pin_reuses_libraries(self):
        self.assertEqual(self.run_ensure(self.entry(self.pin)), "")

    def test_old_or_missing_provenance_rebuilds(self):
        for manifest in (None, self.entry("6b80c74f285390368b3c99c5e750f19e9b096e98"), '[[library]]\nname = "zvec"\nref = "other"\n', self.entry(self.pin) + '[[library]]\nname = "llama-cpp-multi"\n'):
            with self.subTest(manifest=manifest):
                (self.platform / "manifest.toml").unlink(missing_ok=True)
                self.calls.unlink(missing_ok=True)
                self.assertEqual(self.run_ensure(manifest), "--platform android-arm64\n")

    def test_latest_completed_library_entry_wins(self):
        self.assertEqual(self.run_ensure(self.entry("old") + self.entry(self.pin)), "")
        self.assertEqual(self.run_ensure(self.entry(self.pin) + self.entry("old")), "--platform android-arm64\n")

    def test_missing_archive_rebuilds_even_with_matching_pin(self):
        (self.platform / "lib-static/llama-cpp/multi/libllama.a").unlink()
        self.assertEqual(self.run_ensure(self.entry(self.pin)), "--platform android-arm64\n")

    def test_explicit_ref_controls_cache_validation(self):
        self.assertEqual(self.run_ensure(self.entry("custom-commit"), ref="custom-commit"), "")
        self.assertEqual(self.run_ensure(self.entry(self.pin), ref="custom-commit"), "--platform android-arm64\n")

    def test_old_zvec_rebuilds_despite_current_llama(self):
        manifest = self.entry(self.pin).replace(self.zvec_pin, "old-zvec")
        self.assertEqual(self.run_ensure(manifest), "--platform android-arm64\n")

    def test_ndk_receives_selected_api_and_locked_release(self):
        invocation = SOURCE[SOURCE.index("cargo ndk \\\n"):SOURCE.index('for abi in "${ABIS[@]}"; do\n    copy_android_dynamic_libs')]
        cargo = self.root / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$BUILD_CALLS"\n')
        cargo.chmod(0o755)
        env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ["PATH"], BUILD_CALLS=str(self.calls))
        for api in (26, 29):
            with self.subTest(api=api):
                setup = 'set -euo pipefail\nANDROID_API_LEVEL="$1"\nCARGO_NDK_TARGETS=(-t arm64-v8a)\nJNILIBS_DIR="$2/jni libs"\nCARGO_FLAGS=--release\n'
                subprocess.run(["bash", "-c", setup + invocation, "bash", str(api), str(self.root)], env=env, check=True)
                args = self.calls.read_text().splitlines()
                self.assertIn("--platform", args)
                self.assertEqual(args[args.index("--platform") + 1], str(api))
                self.assertEqual(args[args.index("-o") + 1], str(self.root / "jni libs"))
                self.assertEqual(args[args.index("build"):], ["build", "--locked", "--release"])


if __name__ == "__main__":
    unittest.main()
