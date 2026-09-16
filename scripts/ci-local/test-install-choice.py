#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/ci-local/test-install-choice.py
# Opis: Sprawdza wybór edycji, nasłuch LAN i konfigurację zapory przez instalator.
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
        self.mock("id", 'case "$1" in -un) echo tester;; *) echo "${TEST_UID:-1000}";; esac')
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
        script = "set -eu\nlog() { :; }\nok() { printf '%s\\n' \"$*\"; }\nwarn() { printf '%s\\n' \"$*\" >&2; }\n" + function + "\nwrite_config\n"
        subprocess.run(["/bin/sh"], input=script, text=True, env=env, check=True, capture_output=True, timeout=5)
        self.assertEqual(self.trace.read_text().splitlines(),
                         ["init-config", "--output", str(config), "--bind", "127.0.0.1:8090"])

        self.trace.unlink()
        config.write_text("istniejąca konfiguracja użytkownika\n")
        result = subprocess.run(["/bin/sh"], input=script, text=True, env=env, check=True, capture_output=True, timeout=5)
        self.assertFalse(self.trace.exists())
        self.assertEqual(config.read_text(), "istniejąca konfiguracja użytkownika\n")
        self.assertIn(str(config), result.stdout)
        self.assertIn("TENTAFLOW_BIND", result.stderr)

    def run_network_setup(self, *, config=None, bind=None, ufw="inactive",
                          firewalld="inactive", failure="", user_install=False,
                          empty_zones=False):
        self.trace.unlink(missing_ok=True)
        config_path = self.root / "network-config.toml"
        config_path.unlink(missing_ok=True)
        if config is not None:
            config_path.write_text(config)
        binary = self.root / "current/tentaflow"
        binary.parent.mkdir(exist_ok=True)
        binary.write_text('''#!/bin/sh
printf 'init-config' >> "$TEST_MUTATIONS"
printf '|%s' "$@" >> "$TEST_MUTATIONS"
printf '\\n' >> "$TEST_MUTATIONS"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift;;
    --bind) bind="$2"; shift;;
  esac
  shift
done
printf '[protocols.openai_api]\\nenabled = true\\nbind = "%s"\\n[mesh]\\nenabled = true\\nport = 8090\\n' "$bind" > "$output"
''')
        binary.chmod(0o755)
        self.mock("ufw", '''printf 'ufw' >> "$TEST_MUTATIONS"
printf '|%s' "$@" >> "$TEST_MUTATIONS"
printf '\\n' >> "$TEST_MUTATIONS"
case "$1" in
  status) printf 'Status: %s\\n' "$TEST_UFW";;
  allow) [ "$TEST_FAILURE" != ufw ] || exit 17;;
  *) exit 19;;
esac''')
        self.mock("firewall-cmd", '''printf 'firewall-cmd' >> "$TEST_MUTATIONS"
printf '|%s' "$@" >> "$TEST_MUTATIONS"
printf '\\n' >> "$TEST_MUTATIONS"
case "$*" in
  --state) [ "$TEST_FIREWALLD" = active ] || exit 252; echo running;;
  --get-active-zones) [ "$TEST_EMPTY_ZONES" != 1 ] || exit 0; printf 'public\\n  interfaces: eth0\\nwork\\n  interfaces: eth1\\n';;
  --get-default-zone) echo external;;
  *--add-port=*) [ "$TEST_FAILURE" != firewalld ] || exit 17;;
  *) exit 19;;
esac''')
        env = dict(self.env, TENTAFLOW_USER_INSTALL=str(int(user_install)),
                   TENTAFLOW_PREFIX=str(self.root), TEST_CONFIG=str(config_path),
                   TEST_UID="1000" if user_install else "0", TEST_UFW=ufw,
                   TEST_FIREWALLD=firewalld, TEST_FAILURE=failure,
                   TEST_EMPTY_ZONES=str(int(empty_zones)))
        if bind is not None:
            env["TENTAFLOW_BIND"] = bind
        # Definicje i inicjalizacja zmiennych pochodzą z instalatora. Pomijamy
        # pobieranie, ale wykonujemy jego prawdziwe generowanie configu i zaporę.
        source = INSTALLER.read_text().split("\n# Run\n", 1)[0]
        script = source + '\nCONFIG="$TEST_CONFIG"\nwrite_config\nconfigure_firewall\n'
        result = subprocess.run(["/bin/sh"], input=script, text=True, env=env,
                                capture_output=True, timeout=5)
        commands = [line.split("|") for line in self.trace.read_text().splitlines()] if self.trace.exists() else []
        return result, commands, config_path.read_text() if config_path.exists() else ""

    @staticmethod
    def network_config(bind, mesh_port=8090, mesh_enabled=True, https_enabled=True):
        return (f'[protocols.openai_api]\nenabled = {str(https_enabled).lower()}\nbind = "{bind}"\n'
                f'[mesh]\nenabled = {str(mesh_enabled).lower()}\nport = {mesh_port}\n')

    def test_default_bind_generates_lan_config_and_ufw_tcp_udp_rules(self):
        result, commands, _ = self.run_network_setup(ufw="active")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(any("--bind" in c and "0.0.0.0:8090" in c for c in commands), commands)
        self.assertIn(["ufw", "allow", "8090/tcp"], commands)
        self.assertIn(["ufw", "allow", "8090/udp"], commands)

    def test_explicit_loopback_does_not_open_dashboard_port(self):
        result, commands, _ = self.run_network_setup(bind="127.0.0.1:8443", ufw="active")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(any("127.0.0.1:8443" in c for c in commands), commands)
        self.assertFalse(any("8443/tcp" in c for c in commands), commands)
        self.assertIn(["ufw", "allow", "8090/udp"], commands)

    def test_firewalld_uses_configured_ports_in_all_active_zones(self):
        config = self.network_config("0.0.0.0:8443", 9443)
        result, commands, retained = self.run_network_setup(config=config, firewalld="active", ufw="active")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(retained, config)
        for zone in ("public", "work"):
            for port in ("8443/tcp", "9443/udp"):
                for permanent in (False, True):
                    self.assertTrue(any(c[0] == "firewall-cmd" and f"--zone={zone}" in c
                                        and f"--add-port={port}" in c
                                        and ("--permanent" in c) == permanent for c in commands), commands)
        self.assertFalse(any("--reload" in c for c in commands), commands)
        self.assertFalse(any(c[0] == "init-config" for c in commands), commands)
        self.assertIn(["ufw", "allow", "8443/tcp"], commands)
        self.assertIn(["ufw", "allow", "9443/udp"], commands)

    def test_inactive_firewalls_are_not_enabled_or_modified(self):
        result, commands, _ = self.run_network_setup()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        mutations = [c for c in commands if c[0] != "init-config"
                     and c not in (["ufw", "status"], ["ufw", "status", "verbose"],
                                  ["firewall-cmd", "--state"])]
        self.assertEqual(mutations, [])

    def test_firewall_rule_errors_fail_installation(self):
        for firewall in ("ufw", "firewalld"):
            with self.subTest(firewall=firewall):
                result, commands, _ = self.run_network_setup(**{firewall: "active"}, failure=firewall)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(any(c[0] == firewall or c[0] == "firewall-cmd" for c in commands))
                self.assertTrue(result.stderr.strip(), result.stdout)

    def test_user_install_does_not_modify_system_firewall(self):
        result, commands, _ = self.run_network_setup(ufw="active", firewalld="active", user_install=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(any("allow" in c or any(arg.startswith("--add-port=") for arg in c)
                             for c in commands), commands)
        self.assertFalse(any(c[0] == "sudo" for c in commands), commands)

    def test_retained_loopback_config_is_not_overridden_by_lan_default(self):
        config = self.network_config("127.0.0.1:9443", mesh_enabled=False)
        result, commands, retained = self.run_network_setup(config=config, ufw="active")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(retained, config)
        self.assertFalse(any(c[0] == "init-config" or "allow" in c for c in commands), commands)
        self.assertIn("127.0.0.1", result.stdout + result.stderr)

    def test_ambiguous_retained_config_requires_manual_firewall_setup(self):
        valid = self.network_config("0.0.0.0:9443")
        for config in (self.network_config("server.example:9443"),
                       valid.replace("port = 8090\n", ""),
                       valid.replace("port = 8090", "port = 65536"),
                       valid.replace("enabled = true", "enabled = true\nenabled = false", 1)):
            with self.subTest(config=config):
                result, commands, retained = self.run_network_setup(config=config, ufw="active", firewalld="active")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(retained, config)
                self.assertEqual(commands, [])
                self.assertIn("ręcznie", result.stderr)

    def test_disabled_services_do_not_get_firewall_rules(self):
        for https_enabled, mesh_enabled, allowed in ((False, True, "9443/udp"), (True, False, "8443/tcp")):
            with self.subTest(https=https_enabled, mesh=mesh_enabled):
                config = self.network_config("0.0.0.0:8443", 9443, mesh_enabled, https_enabled)
                result, commands, _ = self.run_network_setup(config=config, ufw="active")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual([c for c in commands if "allow" in c], [["ufw", "allow", allowed]])

    def test_firewalld_without_active_zones_uses_default_zone(self):
        result, commands, _ = self.run_network_setup(firewalld="active", empty_zones=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(["firewall-cmd", "--get-default-zone"], commands)
        rules = [c for c in commands if any(arg.startswith("--add-port=") for arg in c)]
        self.assertEqual(len(rules), 4, commands)
        self.assertTrue(all("--zone=external" in c for c in rules), commands)


if __name__ == "__main__":
    unittest.main(verbosity=2)
