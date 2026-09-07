# =============================================================================
# Plik: tests/infra/tentanas-vm/test_vm.py
# Opis: Testy odmowy niebezpiecznych ścieżek, obrazów i tożsamości VM.
# Przykład: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/infra/tentanas-vm -v
# =============================================================================

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import uuid
import contextlib
import guest_packages

import vm


class VmGuards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-vm-guards-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.manifest = {"uuid": str(uuid.uuid4()), "ssh_port": 32123}
        self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"])

    def file(self, name, content=""):
        path = self.path / name
        path.write_text(content)
        path.chmod(0o600)
        return path

    def inventory(self):
        return {"uuid": self.manifest["uuid"], "disks": [
            {"serial": disk["serial"], "size": disk["bytes"], "type": "disk",
             "name": "nvme0n1" if role == "cache" else f"vd{chr(97 + index)}",
             "contains_root": role == "os", "used": role == "os"}
            for index, (role, disk) in enumerate(self.manifest["disks"].items())]}

    def images(self):
        self.manifest["image_inodes"] = {}
        for role, size in vm.DISKS.items():
            path = self.path / f"{role}.qcow2"
            subprocess.run([vm.QEMU_IMG, "create", "-f", "qcow2", str(path), f"{size}G"],
                           check=True, capture_output=True)
            path.chmod(0o600)
            info = path.stat()
            self.manifest["image_inodes"][role] = [info.st_dev, info.st_ino]
        seed = self.file("seed.iso", "prywatny seed")
        self.manifest["seed_sha256"] = vm.hashlib.sha256(seed.read_bytes()).hexdigest()
        self.file("state.json", json.dumps({"status": "prepared"}))

    def test_rejects_host_devices_traversal_and_broad_runtime_before_subprocess(self):
        with patch.object(vm, "run") as external:
            for value in ("/dev/sda", "/mnt/d/repos", "/tmp/tentanas-vm.ABC123",
                          "/mnt/d/repos/tentanas-vm.ABC123/../tentanas-vm.ABC123"):
                with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "ścieżka"):
                    vm.runtime_path(value)
            external.assert_not_called()

    def test_rejects_symlink_hardlink_and_public_file(self):
        target = self.file("target")
        link = self.path / "symlink"
        link.symlink_to(target)
        with self.assertRaises(RuntimeError):
            vm.private_file(link)
        os.link(target, self.path / "hardlink")
        with self.assertRaises(RuntimeError):
            vm.private_file(target)
        public = self.file("public")
        public.chmod(0o644)
        with self.assertRaises(RuntimeError):
            vm.private_file(public)

    def test_exclusive_write_does_not_overwrite_existing_runtime_file(self):
        target = self.file("manifest.json", "zachowaj")
        with self.assertRaises(FileExistsError):
            vm.write_new(target, "nadpisanie")
        self.assertEqual(target.read_text(), "zachowaj")

    def test_wrong_sha512_refuses_before_image_parser(self):
        target = self.file("download.qcow2", "niezatwierdzony obraz")
        with patch.object(vm, "run") as external:
            with self.assertRaisesRegex(RuntimeError, "SHA512"):
                vm.verify_download(target)
            external.assert_not_called()

    def test_real_qcow2_accepts_standalone_and_rejects_backing_chain(self):
        base = self.path / "base.qcow2"
        overlay = self.path / "overlay.qcow2"
        subprocess.run([vm.QEMU_IMG, "create", "-f", "qcow2", str(base), "1M"],
                       check=True, capture_output=True)
        base.chmod(0o600)
        vm.inspect_image(base, 1024 ** 2)
        subprocess.run([vm.QEMU_IMG, "create", "-f", "qcow2", "-F", "qcow2", "-b", str(base),
                        str(overlay)], check=True, capture_output=True)
        overlay.chmod(0o600)
        with self.assertRaisesRegex(RuntimeError, "backing"):
            vm.inspect_image(overlay)

    def test_real_raw_file_is_not_a_qcow2(self):
        with self.assertRaisesRegex(RuntimeError, "QCOW2"):
            vm.inspect_image(self.file("raw.qcow2", "raw"))

    def test_real_external_data_file_is_rejected(self):
        image = self.path / "external.qcow2"
        external = self.path / "external.data"
        subprocess.run([vm.QEMU_IMG, "create", "-f", "qcow2", "-o", f"data_file={external}",
                        str(image), "1M"], check=True, capture_output=True)
        image.chmod(0o600)
        with self.assertRaisesRegex(RuntimeError, "external data"):
            vm.inspect_image(image)

    def test_ssh_pins_identity_and_rejects_changed_known_hosts_and_client_key(self):
        for name in ("client", "other"):
            subprocess.run(["/usr/bin/ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f",
                            str(self.path / name)], check=True, capture_output=True)
        self.manifest["client_public"] = (self.path / "client.pub").read_text().strip()
        self.manifest["host_public"] = (self.path / "other.pub").read_text().strip()
        self.file("host.pub", self.manifest["host_public"])
        known = self.file("known_hosts", f"[127.0.0.1]:32123 {self.manifest['host_public']}\n")
        args = vm.ssh_command(self.path, self.manifest)
        self.assertIn("StrictHostKeyChecking=yes", args)
        self.assertIn("IdentityAgent=none", args)
        known.write_text("niezaufany")
        with self.assertRaisesRegex(RuntimeError, "known_hosts"):
            vm.ssh_command(self.path, self.manifest)
        known.write_text(f"[127.0.0.1]:32123 {self.manifest['host_public']}\n")
        (self.path / "client").write_bytes((self.path / "other").read_bytes())
        with self.assertRaisesRegex(RuntimeError, "klucz klienta"):
            vm.ssh_command(self.path, self.manifest)

    def test_inventory_accepts_exact_roles_and_rejects_each_identity_boundary(self):
        inventory = self.inventory()
        vm.validate_inventory(self.manifest, inventory)
        variants = []
        wrong_uuid = copy.deepcopy(inventory)
        wrong_uuid["uuid"] = str(uuid.uuid4())
        variants.append(wrong_uuid)
        for field, value in (("serial", "obcy"), ("size", 1), ("type", "part"),
                             ("contains_root", True), ("used", True), ("name", "sda")):
            changed = copy.deepcopy(inventory)
            changed["disks"][1][field] = value
            variants.append(changed)
        extra = copy.deepcopy(inventory)
        extra["disks"].append(extra["disks"][1])
        variants.append(extra)
        wrong_cache = copy.deepcopy(inventory)
        wrong_cache["disks"][4]["name"] = "vde"
        variants.append(wrong_cache)
        with patch.object(vm, "run") as external:
            for changed in variants:
                with self.subTest(inventory=changed), self.assertRaises(RuntimeError):
                    vm.validate_inventory(self.manifest, changed)
            external.assert_not_called()

    def test_running_start_refuses_before_any_subprocess(self):
        self.file("state.json", json.dumps({"status": "running"}))
        with patch.object(vm, "run") as external:
            with self.assertRaisesRegex(RuntimeError, "już działa"):
                vm.start(self.path, self.manifest)
            external.assert_not_called()

    def test_changed_seed_refuses_start_before_qemu(self):
        self.images()
        (self.path / "seed.iso").write_text("podmieniony seed")
        with patch.object(vm, "run", wraps=vm.run) as external:
            with self.assertRaisesRegex(RuntimeError, "Podmieniony seed"):
                vm.start(self.path, self.manifest)
            self.assertFalse(any(call.args[0][0] == vm.QEMU for call in external.call_args_list))

    def test_replaced_disk_inode_refuses_start_before_qemu(self):
        self.images()
        self.file("replacement", "obcy dysk").replace(self.path / "data1.qcow2")
        with patch.object(vm, "run", wraps=vm.run) as external:
            with self.assertRaisesRegex(RuntimeError, "Podmieniony plik obrazu data1"):
                vm.start(self.path, self.manifest)
            self.assertFalse(any(call.args[0][0] == vm.QEMU for call in external.call_args_list))

    def test_wrong_process_identity_refuses_before_qmp(self):
        state = {"status": "running", "process": {"pid": os.getpid(), "start_ticks": "wrong"}}
        with patch.object(vm, "qmp") as control:
            with self.assertRaisesRegex(RuntimeError, "PID/starttime"):
                vm.running_identity(self.path, self.manifest, state)
            control.assert_not_called()

    def test_reused_pid_is_not_treated_as_exited(self):
        with self.assertRaisesRegex(RuntimeError, "ponownie użyty"):
            vm.process_finished({"pid": os.getpid(), "start_ticks": "wrong"})

    def test_qemu_has_only_fixed_private_drives_and_restricted_localhost_network(self):
        args = vm.qemu_command(self.path, self.manifest)
        self.assertEqual(args[0], "/usr/bin/qemu-system-x86_64")
        self.assertIn("q35,accel=kvm,smm=off", args)
        self.assertNotIn("-virtfs", args)
        self.assertNotIn("-fsdev", args)
        self.assertIn("user,id=net0,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:32123-:22", args)
        drives = [args[index + 1] for index, value in enumerate(args) if value == "-drive"]
        self.assertEqual(len(drives), 7)
        self.assertTrue(all(f"file={self.path}/" in drive for drive in drives))

    def test_remote_argv_quotes_spaces_and_shell_metacharacters(self):
        args = ["python3", "-c", "print('a; b')", "$(touch /tmp/not-executed)"]
        with patch.object(vm.sys, "argv", ["vm.py", "ssh", "runtime"] + args), \
                patch.object(vm, "locked_runtime") as lock, \
                patch.object(vm, "running_identity"), patch.object(vm, "read_state"), \
                patch.object(vm, "ssh_command", return_value=["ssh", "guest"]), \
                patch.object(vm, "run") as external:
            lock.return_value.__enter__.return_value = (self.path, self.manifest)
            vm.main()
            external.assert_called_once_with(["ssh", "guest", vm.shlex.join(args)])


class PackageFlow(unittest.TestCase):
    def flow(self, failure=None, download=True):
        events = []
        path = Path("/mnt/d/repos/tentanas-vm.ABC123")
        manifest = {"uuid": str(uuid.uuid4())}

        def step(name):
            def invoke(*args, **kwargs):
                events.append(name)
                if name == failure:
                    raise RuntimeError(f"awaria {name}")
            return invoke

        def phase(path, manifest, name):
            events.append(name)
            if name == failure:
                raise RuntimeError(f"awaria {name}")
            if failure == "interrupt" and name == "download":
                raise KeyboardInterrupt("przerwanie")
            if failure == "sigterm" and name == "download":
                vm.signal.getsignal(vm.signal.SIGTERM)(vm.signal.SIGTERM, None)

        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(vm, "read_state", return_value={"status": "running"}))
            stack.enter_context(patch.object(vm, "inventory", side_effect=step("inventory")))
            stack.enter_context(patch.object(vm, "package_phase", side_effect=phase))
            for name in ("start", "stop", "wait_ssh"):
                stack.enter_context(patch.object(vm, name, side_effect=step(name)))
            restore = stack.enter_context(patch.object(vm, "restore_isolation", side_effect=step("restore")))
            if failure:
                exception = KeyboardInterrupt if failure in ("interrupt", "sigterm") else RuntimeError
                with self.assertRaises(exception):
                    vm.packages(path, manifest, download)
            else:
                vm.packages(path, manifest, download)
            restore.assert_called_once_with(path, manifest, restore_apt=not download or bool(failure))
        return events

    def test_prepare_start_ssh_download_failures_all_restore_isolation(self):
        for failure in ("prepare", "start", "wait_ssh", "download"):
            with self.subTest(failure=failure):
                events = self.flow(failure)
                self.assertEqual(events[-1], "restore")
                self.assertNotIn("seal", events)

    def test_install_and_probe_failures_restore_without_ready(self):
        for failure in ("install", "probe"):
            with self.subTest(failure=failure):
                events = self.flow(failure, download=False)
                self.assertEqual(events[-1], "restore")
                self.assertNotIn("seal", events)

    def test_sigint_and_sigterm_handlers_enter_cleanup(self):
        original = vm.signal.getsignal(vm.signal.SIGTERM)
        for failure in ("interrupt", "sigterm"):
            with self.subTest(failure=failure):
                self.assertEqual(self.flow(failure)[-1], "restore")
                self.assertIs(vm.signal.getsignal(vm.signal.SIGTERM), original)

    def test_successful_download_never_installs_or_seals(self):
        events = self.flow()
        self.assertIn("download", events)
        self.assertNotIn("install", events)
        self.assertNotIn("seal", events)

    def test_successful_offline_install_seals_only_after_restore(self):
        events = self.flow(download=False)
        self.assertEqual(events[-3:], ["restore", "verify-network", "seal"])
        self.assertNotIn("download", events)
        self.assertNotIn("start", events)

    def test_emergency_stop_cannot_return_successful_cleanup(self):
        path, manifest = Path("unused"), {}
        with patch.object(vm, "recover_start"), \
                patch.object(vm, "read_state", side_effect=[{"status": "bootstrap"}, {"status": "stopped"}]), \
                patch.object(vm, "stop", side_effect=RuntimeError("powerdown timeout")), \
                patch.object(vm, "emergency_stop") as emergency, patch.object(vm, "start") as start, \
                patch.object(vm, "wait_ssh"), patch.object(vm, "package_phase"), patch.object(vm, "inventory"):
            with self.assertRaisesRegex(RuntimeError, "awaryjnie"):
                vm.restore_isolation(path, manifest, restore_apt=True)
            emergency.assert_called_once_with(path, manifest)
            start.assert_called_once_with(path, manifest)

    def test_start_failure_without_launch_records_stopped_without_qmp(self):
        with tempfile.TemporaryDirectory(prefix="tentanas-start-not-launched-") as value:
            path = Path(value)
            state = path / "state.json"
            state.write_text(json.dumps({"status": "starting-bootstrap"}))
            state.chmod(0o600)
            with patch.object(vm, "qmp") as control, patch.object(vm, "run") as external:
                vm.recover_start(path, {})
                self.assertEqual(json.loads(state.read_text())["status"], "stopped")
                control.assert_not_called()
                external.assert_not_called()

    def test_transitional_systemd_states_are_not_idle(self):
        for state in ("active", "activating", "deactivating", "reloading"):
            with self.subTest(state=state), patch.object(guest_packages, "command",
                return_value=subprocess.CompletedProcess([], 3, stdout=state + "\n")):
                self.assertTrue(guest_packages.active("apt-daily.service"))

    def test_template_checks_concrete_instances_not_invalid_is_active_template(self):
        result = subprocess.CompletedProcess([], 0, stdout="")
        with patch.object(guest_packages, "command", return_value=result) as command:
            self.assertFalse(guest_packages.active("e2scrub@.service"))
            command.assert_called_once_with(["systemctl", "list-units", "--all", "--plain",
                "--no-legend", "--state=active,activating,reloading,deactivating,failed",
                "e2scrub@*.service"], timeout=15)
        result.stdout = "e2scrub@vdb.service loaded active running\n"
        with patch.object(guest_packages, "command", return_value=result):
            self.assertTrue(guest_packages.active("e2scrub@.service"))

    def test_apt_boolean_spellings_fail_closed(self):
        for value in ("true", "1", "yes", "on", "YES", "On", "unknown"):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "autentyczność"):
                guest_packages.validate_apt_config(f'APT::Get::AllowUnauthenticated "{value}";')
        for value in ("false", "0", "no", "off", "NO", "Off", "unknown"):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "apt/TLS"):
                guest_packages.validate_apt_config(f'Acquire::https::Verify-Peer "{value}";')
        guest_packages.validate_apt_config('APT::Get::AllowUnauthenticated "no";\nAcquire::https::Verify-Peer "yes";')

    def test_sources_list_accepts_comment_only_but_rejects_active_lines_and_symlinks(self):
        with tempfile.TemporaryDirectory(prefix="tentanas-apt-comments-") as value:
            path = Path(value) / "sources.list"
            path.write_text("# See /etc/apt/sources.list.d/debian.sources\n  # komentarz\n\n")
            guest_packages.check_sources_list(path)
            path.write_text("# komentarz\ndeb https://example.invalid sid main\n")
            with self.assertRaisesRegex(RuntimeError, "aktywne sources.list"):
                guest_packages.check_sources_list(path)
            link = Path(value) / "link.list"
            link.symlink_to(path)
            with self.assertRaisesRegex(RuntimeError, "Symlink"):
                guest_packages.check_sources_list(link)

    def test_prepare_failure_in_restricted_vm_does_not_stop_legal_apt(self):
        path, manifest = Path("unused"), {}
        events = []

        def phase(path, manifest, name):
            events.append(name)
            if name == "prepare":
                raise RuntimeError("apt już działa")

        with patch.object(vm, "read_state", return_value={"status": "running"}), \
                patch.object(vm, "inventory"), patch.object(vm, "running_identity") as identity, \
                patch.object(vm, "package_phase", side_effect=phase), \
                patch.object(vm, "stop") as stop, patch.object(vm, "start") as start:
            with self.assertRaisesRegex(RuntimeError, "apt już działa"):
                vm.packages(path, manifest, download=True)
            identity.assert_called_once_with(path, manifest, {"status": "running"})
            stop.assert_not_called()
            start.assert_not_called()
            self.assertEqual(events, ["prepare", "finalize"])

    def test_download_failure_uses_real_cleanup_to_return_to_restricted_state(self):
        path, manifest = Path("unused"), {}
        state = {"status": "running"}
        events = []

        def stop(path, manifest):
            events.append("stop " + state["status"])
            state["status"] = "stopped"

        def start(path, manifest, bootstrap=False):
            state["status"] = "bootstrap" if bootstrap else "running"
            events.append("start " + state["status"])

        def phase(path, manifest, name):
            events.append(name)
            if name == "download":
                raise RuntimeError("download failed")

        with patch.object(vm, "read_state", side_effect=lambda path: dict(state)), \
                patch.object(vm, "inventory"), patch.object(vm, "running_identity"), \
                patch.object(vm, "package_phase", side_effect=phase), patch.object(vm, "wait_ssh"), \
                patch.object(vm, "stop", side_effect=stop), patch.object(vm, "start", side_effect=start):
            with self.assertRaisesRegex(RuntimeError, "download failed"):
                vm.packages(path, manifest, download=True)
            self.assertEqual(state["status"], "running")
            self.assertEqual(events, ["prepare", "stop running", "start bootstrap", "download",
                                      "stop bootstrap", "start running", "finalize"])

    def test_finalize_preserves_preexisting_masks(self):
        with tempfile.TemporaryDirectory(prefix="tentanas-package-finalize-") as value:
            path = Path(value)
            (path / "apt-active.json").write_text(json.dumps({
                "apt-daily.timer": {"active": False, "masked": True},
                "apt-daily-upgrade.timer": {"active": True, "masked": False},
            }))
            with patch.object(guest_packages, "command") as command:
                guest_packages.finalize(path)
                self.assertEqual(command.call_args_list, [
                    unittest.mock.call(["systemctl", "unmask", "apt-daily-upgrade.timer"]),
                    unittest.mock.call(["systemctl", "start", "apt-daily-upgrade.timer"]),
                ])


if __name__ == "__main__":
    unittest.main()
