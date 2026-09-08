#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/ci-local/test-install-choice.py
# Opis: Sprawdza wybór edycji w pełnym instalatorze z potokiem stdin i terminalem.
# Przykład: python3 scripts/ci-local/test-install-choice.py
# =============================================================================

import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import tempfile
import termios
import time
import unittest


INSTALLER = Path(os.environ.get(
    "TENTAFLOW_TEST_INSTALLER",
    Path(__file__).resolve().parents[2] / "scripts/install/install.sh",
))


class InstallerChoiceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tentaflow-install-choice-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.trace = self.root / "mutations"
        self.env = {
            "PATH": str(self.bin), "HOME": str(self.root), "NO_COLOR": "1",
            "LC_ALL": "C", "TENTAFLOW_USER_INSTALL": "1",
            "TENTAFLOW_NO_AUTOSTART": "1", "TENTAFLOW_VERSION": "v-test",
            "TENTAFLOW_VARIANT": "vulkan", "TEST_MUTATIONS": str(self.trace),
            "TEST_OS": "Linux", "TEST_ARCH": "x86_64",
        }
        # Nie dziedziczymy PATH: żaden prawdziwy menedżer pakietów ani klient HTTP
        # nie może zostać wywołany także wtedy, gdy instalator ma regresję.
        for command in ("head", "tail", "awk", "sort", "grep", "sed", "tr", "cut", "env"):
            executable = shutil.which(command)
            self.assertIsNotNone(executable, command)
            (self.bin / command).symlink_to(executable)
        self.mock("uname", 'case "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac')
        self.mock("id", 'case "$1" in -un) echo tester;; *) echo 1000;; esac')
        self.mock("ldd", 'echo "ldd 2.40"')
        self.mock("ldconfig", "exit 0")
        self.mock("sw_vers", 'echo "15.0"')
        self.mock("sysctl", 'echo "Apple M2"')
        self.mock("nvidia-smi", "exit 1")
        for command in ("apt-get", "brew", "curl", "tar", "mkdir", "cp", "mv", "ln",
                        "rm", "chmod", "chown", "sudo", "systemctl", "launchctl"):
            self.mock(command, f'printf "%s\\n" "{command}" >> "$TEST_MUTATIONS"\nexit 93')
        # To pierwsza operacja etapu pobierania. Zatrzymuje cały instalator po
        # wyborze edycji, zanim powstanie archiwum, konfiguracja lub usługa.
        self.mock("mktemp", 'printf "%s\\n" "mktemp" >> "$TEST_MUTATIONS"\nexit 93')

    def mock(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
        path.chmod(0o755)

    def run_installer(self, *, terminal=False, answers=(), edition=None, macos=False):
        self.trace.unlink(missing_ok=True)
        env = dict(self.env)
        if edition is not None:
            env["TENTAFLOW_EDITION"] = edition
        if macos:
            env.update(TEST_OS="Darwin", TEST_ARCH="arm64", TENTAFLOW_VARIANT="metal")
        prompt = "Aby potwierdzić instalację, wpisz full" if macos else "Wpisz full lub slim"
        master = slave = None
        if terminal:
            master, slave = pty.openpty()

            def attach_terminal():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            process = subprocess.Popen(
                ["/bin/sh"], stdin=subprocess.PIPE, stdout=slave, stderr=slave,
                env=env, preexec_fn=attach_terminal,
            )
            os.close(slave)
        else:
            process = subprocess.Popen(
                ["/bin/sh"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, env=env, start_new_session=True,
            )
        try:
            if not terminal:
                output, _ = process.communicate(INSTALLER.read_bytes(), timeout=10)
            else:
                process.stdin.write(INSTALLER.read_bytes())
                process.stdin.close()
                output = b""
                sent = 0
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if select.select([master], [], [], 0.1)[0]:
                        try:
                            chunk = os.read(master, 65536)
                        except OSError as error:
                            if error.errno == errno.EIO:
                                break
                            raise
                        if not chunk:
                            break
                        output += chunk
                        prompts = output.count(prompt.encode()) + output.count(b"Which edition")
                        if prompts > sent and sent < len(answers):
                            os.write(master, answers[sent])
                            sent += 1
                    elif process.poll() is not None:
                        break
                else:
                    self.fail("Instalator nie zakończył się w 10 s:\n" + output.decode(errors="replace"))
                process.wait(timeout=2)
            mutations = self.trace.read_text().splitlines() if self.trace.exists() else []
            return process.returncode, output.decode(errors="replace"), mutations
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            if master is not None:
                os.close(master)

    def assert_selected(self, result, edition):
        code, output, mutations = result
        self.assertEqual(code, 93, output)
        self.assertIn("mktemp", mutations, output)
        self.assertRegex(output, rf"(?:Edition(?: from TENTAFLOW_EDITION)?|Edycja wskazana przez TENTAFLOW_EDITION|Wybrana edycja): {edition}\b")
        package_commands = {"apt-get", "brew"}.intersection(mutations)
        if edition == "slim":
            self.assertEqual(package_commands, set(), output)
        else:
            self.assertTrue(package_commands, output)

    def assert_rejected(self, result):
        code, output, mutations = result
        self.assertNotEqual(code, 0, output)
        self.assertEqual(mutations, [], output)

    def test_pipe_with_controlling_terminal_accepts_slim(self):
        self.mock("nvidia-smi", 'case "$1" in -L) echo "GPU 0: NVIDIA test";; --query-gpu=compute_cap) echo "8.9";; --query-gpu=driver_version) echo "580.0";; esac')
        result = self.run_installer(terminal=True, answers=[b"slim\n"])
        self.assertIn("Wpisz full lub slim", result[1])
        self.assertIn("Propozycja sprzętowa: full", result[1])
        self.assert_selected(result, "slim")

    def test_pipe_with_controlling_terminal_accepts_full(self):
        result = self.run_installer(terminal=True, answers=[b"full\n"])
        self.assertIn("Wpisz full lub slim", result[1])
        self.assert_selected(result, "full")

    def test_empty_answer_requires_another_explicit_choice(self):
        result = self.run_installer(terminal=True, answers=[b"\n", b"slim\n"])
        self.assertGreaterEqual(result[1].count("Wpisz full lub slim"), 2)
        self.assert_selected(result, "slim")

    def test_terminal_eof_does_not_select_edition(self):
        self.assert_rejected(self.run_installer(terminal=True, answers=[b"\x04"]))

    def test_headless_requires_explicit_edition(self):
        result = self.run_installer()
        self.assert_rejected(result)
        self.assertIn("TENTAFLOW_EDITION", result[1])

    def test_headless_explicit_editions_continue(self):
        for edition in ("slim", "full"):
            with self.subTest(edition=edition):
                self.assert_selected(self.run_installer(edition=edition), edition)

    def test_invalid_environment_or_terminal_edition_is_rejected(self):
        for macos in (False, True):
            with self.subTest(macos=macos):
                self.assert_rejected(self.run_installer(edition="typo", macos=macos))
        self.assert_rejected(self.run_installer(terminal=True, answers=[b"typo\n"]))

    def test_macos_headless_requires_explicit_edition(self):
        self.assert_rejected(self.run_installer(macos=True))

    def test_macos_slim_is_rejected(self):
        self.assert_rejected(self.run_installer(macos=True, edition="slim"))

    def test_macos_full_requires_explicit_environment_or_confirmation(self):
        self.assert_selected(self.run_installer(macos=True, edition="full"), "full")
        result = self.run_installer(macos=True, terminal=True, answers=[b"full\n"])
        self.assertIn("Aby potwierdzić instalację, wpisz full", result[1])
        self.assert_selected(result, "full")

    def test_config_generation_keeps_mesh_defaults_and_existing_config(self):
        binary = self.root / "current/tentaflow"
        binary.parent.mkdir()
        binary.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$TEST_MUTATIONS"\n')
        binary.chmod(0o755)
        config = self.root / "config.toml"
        env = dict(self.env, PREFIX=str(self.root), CONFIG=str(config),
                   BIND="127.0.0.1:8090", SUDO="")
        # Wywołujemy bieżącą funkcję powłoki bez etapu pobierania archiwum.
        # Zastąpiona binarka zapisuje rzeczywiste argumenty init-config.
        source = INSTALLER.read_text()
        function = "write_config() {" + source.split("write_config() {", 1)[1].split("\nwrite_receipt()", 1)[0]
        script = "set -eu\nlog() { :; }\nok() { :; }\n" + function + "\nwrite_config\n"
        subprocess.run(["/bin/sh"], input=script, text=True, env=env, check=True, timeout=5)
        self.assertEqual(self.trace.read_text().splitlines(),
                         ["init-config", "--output", str(config), "--bind", "127.0.0.1:8090"])

        self.trace.unlink()
        config.write_text("istniejąca konfiguracja użytkownika\n")
        subprocess.run(["/bin/sh"], input=script, text=True, env=env, check=True, timeout=5)
        self.assertFalse(self.trace.exists())
        self.assertEqual(config.read_text(), "istniejąca konfiguracja użytkownika\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
