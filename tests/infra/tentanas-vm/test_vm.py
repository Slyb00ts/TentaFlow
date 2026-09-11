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
import shlex
import tarfile

import vm
import io


class VmGuards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-vm-guards-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.manifest = {"uuid": str(uuid.uuid4()), "ssh_port": 32123}
        self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"], "storage")

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
        for role, disk in self.manifest["disks"].items():
            path = self.path / f"{role}.qcow2"
            subprocess.run([vm.QEMU_IMG, "create", "-f", "qcow2", str(path), str(disk["bytes"])],
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

    def test_profiles_load_exact_maps_without_changing_old_manifest(self):
        manifest = {**self.manifest, "schema": 1, "uid": os.getuid(), "runtime": str(self.path),
                    "image_url": vm.IMAGE_URL, "image_sha512": vm.IMAGE_SHA512, "machine": vm.MACHINE}
        for profile in ("storage", "e2", "e2-cache"):
            with self.subTest(profile=profile):
                manifest["disks"] = vm.disk_manifest(manifest["uuid"], profile)
                if profile in ("e2", "e2-cache"):
                    manifest["api_port"] = 32124
                target = self.file("manifest.json", json.dumps(manifest))
                before = target.read_bytes()
                actual = vm.load_manifest(self.path)
                self.assertEqual(actual, manifest)
                self.assertEqual(vm.disk_profile(actual), profile)
                self.assertEqual(target.read_bytes(), before)
                self.manifest = actual
                vm.validate_inventory(actual, self.inventory())
                self.images()
                vm.check_images(self.path, actual)
                self.assertEqual(sum(arg == "-drive" for arg in vm.qemu_command(self.path, actual)), 7)
                for role in actual["disks"]:
                    (self.path / f"{role}.qcow2").unlink()

    def test_profiles_refuse_mixed_or_modified_maps(self):
        original = {**self.manifest, "schema": 1, "uid": os.getuid(), "runtime": str(self.path),
                    "image_url": vm.IMAGE_URL, "image_sha512": vm.IMAGE_SHA512, "machine": vm.MACHINE}
        for change in ("mixed", "size", "serial", "missing", "extra", "float"):
            with self.subTest(change=change):
                manifest = copy.deepcopy(original)
                disks = manifest["disks"]
                if change == "mixed":
                    disks["data1"]["bytes"] = 32 * vm.GIB
                elif change == "size":
                    disks["data1"]["bytes"] += 1
                elif change == "serial":
                    disks["data1"]["serial"] = disks["data2"]["serial"]
                elif change == "missing":
                    del disks["spare"]
                elif change == "extra":
                    disks["unknown"] = copy.deepcopy(disks["spare"])
                else:
                    disks["os"]["bytes"] = float(disks["os"]["bytes"])
                self.file("manifest.json", json.dumps(manifest))
                with self.assertRaisesRegex(RuntimeError, "mapa ról"), patch.object(vm, "run") as external:
                    vm.load_manifest(self.path)
                external.assert_not_called()

    def test_non_storage_profiles_refuse_storage_and_detach_before_state_or_external_effects(self):
        with patch.object(vm, "read_state") as state, patch.object(vm, "run") as external, \
             patch.object(vm, "durable_new") as intent, patch.object(vm, "save_state") as save:
            for profile in ("e2", "e2-cache", "block"):
                self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"], profile)
                for phase in ("preflight", "prepare", "exercise", "verify", "corruption",
                              "replacement-preflight", "replacement-prepare", "replacement-recover",
                              "enospc-preflight", "enospc"):
                    with self.subTest(profile=profile, phase=phase), \
                         self.assertRaisesRegex(RuntimeError, f"Fazy storage zabronione dla profilu {profile}$"):
                        vm.storage(self.path, self.manifest, phase)
                with self.subTest(profile=profile), \
                     self.assertRaisesRegex(RuntimeError, f"Odłączenie data2 zabronione dla profilu {profile}$"):
                    vm.detach_data2(self.path, self.manifest)
            state.assert_not_called()
            external.assert_not_called()
            intent.assert_not_called()
            save.assert_not_called()

    def test_create_cli_selects_only_closed_profiles(self):
        for args, expected in ((["create"], "storage"), (["create", "--profile", "storage"], "storage"),
                               (["create", "--profile", "e2"], "e2"),
                               (["create", "--profile", "e2-cache"], "e2-cache"),
                               (["create", "--profile", "block"], "block")):
            with self.subTest(args=args), patch.object(vm.sys, "argv", ["vm.py", *args]), \
                 patch.object(vm, "create") as create, patch.object(vm.os, "getuid", return_value=1000):
                vm.main()
                create.assert_called_once_with(expected)
        with patch.object(vm.sys, "argv", ["vm.py", "create", "--profile", "arbitrary"]), \
             patch.object(vm, "create") as create, patch.object(vm.os, "getuid", return_value=1000), \
             contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as failure:
            vm.main()
        self.assertEqual(failure.exception.code, 2)
        create.assert_not_called()

    def test_create_uses_selected_sizes_in_real_controller_and_manifest(self):
        for profile, sizes in (("storage", [12, 1, 1, 2, 1, 1]), ("e2", [12, 32, 32, 40, 1, 40]),
                               ("e2-cache", [12, 32, 32, 40, 32, 40]), ("block", [12])):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as directory:
                path = Path(directory)
                calls = []

                def run(args, **kwargs):
                    calls.append(args)
                    if args[0] == "/usr/bin/mktemp":
                        return subprocess.CompletedProcess(args, 0, str(path) + "\n")
                    if args[0] == vm.QEMU_IMG and args[1] == "info":
                        return subprocess.CompletedProcess(args, 0, '{"virtual-size": 1073741824}')
                    if args[0] == vm.QEMU_IMG:
                        target = args[-1] if args[1] == "convert" else args[-2]
                        Path(target).write_text("obraz zastąpiony w teście wyboru profilu")
                        Path(target).chmod(0o600)
                    elif args[0] == "/usr/bin/ssh-keygen":
                        Path(args[-1]).write_text("klucz testowy")
                        Path(args[-1] + ".pub").write_text("ssh-ed25519 TEST")
                    elif args[0] == "/usr/bin/xorriso":
                        Path(args[args.index("-output") + 1]).write_text("seed testowy")
                    return subprocess.CompletedProcess(args, 0, "")

                with patch.object(vm, "run", side_effect=run), \
                     patch.object(vm, "runtime_path", return_value=path), \
                     patch.object(vm.os, "getuid", return_value=1000), \
                     patch.object(vm.os, "access", return_value=True), \
                     patch.object(vm, "verify_download"), patch.object(vm, "check_images"), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    vm.create(profile)
                manifest = json.loads((path / "manifest.json").read_text())
                self.assertEqual([disk["bytes"] for disk in manifest["disks"].values()],
                                 [size * vm.GIB for size in sizes])
                self.assertEqual(vm.disk_profile(manifest), profile)
                self.assertIn([vm.QEMU_IMG, "resize", "-f", "qcow2", str(path / "os.qcow2"), "12G"], calls)
                create_calls = [args for args in calls if args[:2] == [vm.QEMU_IMG, "create"]]
                self.assertEqual(create_calls, [[vm.QEMU_IMG, "create", "-f", "qcow2",
                                 str(path / f"{role}.qcow2"), f"{size}G"]
                                 for role, size in zip(("data1", "data2", "parity", "cache", "spare"), sizes[1:])])
                self.assertEqual(set(manifest["image_inodes"]), set(manifest["disks"]))
                if profile in ("e2", "e2-cache"):
                    self.assertIs(type(manifest["api_port"]), int)
                    self.assertNotEqual(manifest["api_port"], manifest["ssh_port"])
                else:
                    self.assertNotIn("api_port", manifest)

    def test_e2_api_forward_is_fixed_loopback_and_part_of_process_identity(self):
        for profile in ("e2", "e2-cache"):
            self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"], profile)
            self.manifest["api_port"] = 32124
            expected = ("user,id=net0,restrict=on,ipv6=off,"
                        "hostfwd=tcp:127.0.0.1:32123-:22,hostfwd=tcp:127.0.0.1:32124-:8090")
            args = vm.qemu_command(self.path, self.manifest)
            self.assertEqual(args[args.index("-netdev") + 1], expected)
            bootstrap = vm.qemu_command(self.path, self.manifest, bootstrap=True)
            self.assertEqual(bootstrap[bootstrap.index("-netdev") + 1], expected.replace("restrict=on", "restrict=off"))
        original = {"pid": 1234, "start_ticks": 123, "executable": str(Path(vm.QEMU).resolve()), "argv": args}
        self.manifest["api_port"] = 32125
        with patch.object(vm, "process_identity", return_value=original), \
             self.assertRaisesRegex(RuntimeError, "argumenty VM"):
            vm.running_identity(self.path, self.manifest, {"status": "running", "process": original})

    def test_manifest_refuses_invalid_api_port_or_api_on_storage(self):
        manifest = {**self.manifest, "schema": 1, "uid": os.getuid(), "runtime": str(self.path),
                    "image_url": vm.IMAGE_URL, "image_sha512": vm.IMAGE_SHA512, "machine": vm.MACHINE}
        manifest["disks"] = vm.disk_manifest(manifest["uuid"], "e2")
        for port in (None, "32124", True, 32124.0, 1024, 65536, manifest["ssh_port"]):
            with self.subTest(port=port):
                if port is not None:
                    manifest["api_port"] = port
                self.file("manifest.json", json.dumps(manifest))
                with self.assertRaisesRegex(RuntimeError, "port API"):
                    vm.load_manifest(self.path)
                with self.assertRaisesRegex(RuntimeError, "port API"):
                    vm.qemu_command(self.path, manifest)
        manifest["disks"] = vm.disk_manifest(manifest["uuid"], "storage")
        manifest["api_port"] = 32124
        self.file("manifest.json", json.dumps(manifest))
        with self.assertRaisesRegex(RuntimeError, "storage"):
            vm.load_manifest(self.path)

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
                patch.object(vm, "running_identity"), patch.object(vm, "read_state", return_value={"status": "running"}), \
                patch.object(vm, "ssh_command", return_value=["ssh", "guest"]), \
                patch.object(vm, "run") as external:
            lock.return_value.__enter__.return_value = (self.path, self.manifest)
            vm.main()
            external.assert_called_once_with(["ssh", "guest", vm.shlex.join(args)])

    def test_storage_main_passes_only_manifest_contract_and_real_source(self):
        for phase in ("preflight", "prepare", "exercise", "verify", "corruption"):
            with self.subTest(phase=phase), \
                    patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", phase]), \
                    patch.object(vm, "locked_runtime") as lock, \
                    patch.object(vm, "read_state", return_value={"status": "running"}), \
                    patch.object(vm, "running_identity") as identity, \
                    patch.object(vm, "inventory") as empty_inventory, \
                    patch.object(vm, "ssh_command", return_value=["ssh", "guest"]), \
                    patch.object(vm, "run") as external:
                lock.return_value.__enter__.return_value = (self.path, self.manifest)
                vm.main()
                identity.assert_called_once_with(self.path, self.manifest, {"status": "running"})
                empty_inventory.assert_not_called()
                external.assert_called_once()
                actual = external.call_args
                self.assertEqual(actual.args[0][:2], ["ssh", "guest"])
                self.assertEqual(len(actual.args[0]), 3)
                remote = vm.shlex.split(actual.args[0][2])
                self.assertEqual(remote[:4], ["sudo", "-n", "python3", "-"])
                self.assertEqual(len(remote), 5)
                self.assertEqual(json.loads(remote[4]), {"phase": phase,
                    "uuid": self.manifest["uuid"], "disks": self.manifest["disks"]})
                self.assertEqual(actual.kwargs, {"input": Path(vm.__file__).with_name("guest_storage.py").read_text(),
                                               "timeout": 900})

    def test_storage_refuses_open_network_and_unknown_phase_before_ssh(self):
        with patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", "prepare"]), \
                patch.object(vm, "locked_runtime") as lock, \
                patch.object(vm, "read_state", return_value={"status": "bootstrap"}), \
                patch.object(vm, "run") as external:
            lock.return_value.__enter__.return_value = (self.path, self.manifest)
            with self.assertRaisesRegex(RuntimeError, "izolowanej"):
                vm.main()
            external.assert_not_called()
        with patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", "/dev/vdb"]), \
                patch.object(vm, "locked_runtime") as lock, \
                patch.object(vm, "run") as external:
            with self.assertRaises(SystemExit) as refusal:
                vm.main()
            self.assertEqual(refusal.exception.code, 2)
            lock.assert_not_called()
            external.assert_not_called()


class RetirementGuards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-retirement-guards-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.manifest = {"uuid": str(uuid.uuid4()), "ssh_port": 32123}
        self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"], "storage")
        self.manifest["image_inodes"] = {role: [10, index + 1] for index, role in enumerate(self.manifest["disks"])}
        self.original = {"pid": 23456, "start_ticks": "12345", "executable": str(Path(vm.QEMU).resolve()),
                         "argv": vm.qemu_command(self.path, self.manifest)}
        self.intent = {"schema": 1, "uuid": self.manifest["uuid"], "operation_id": str(uuid.uuid4()),
                       "source": "data2", "target": "spare", "process": self.original,
                       "disks": {role: self.manifest["disks"][role] for role in ("data2", "spare")},
                       "image_inode": self.manifest["image_inodes"]["data2"]}

    def file(self, name, content=""):
        path = self.path / name
        path.write_text(content)
        path.chmod(0o600)
        return path

    def arm(self, operation_id):
        return {"phase": "replacement-arm", "result": "ok", "state": {
            "contract": {"uuid": self.manifest["uuid"], "disks": self.manifest["disks"]},
            "stage": "replacement_armed", "format_count": 3, "format_pending": None,
            "replacement": {"operation_id": operation_id, "missing": "data2", "target": "spare"}}}

    def records(self, phase="detached", status="stopped"):
        arm = self.arm(self.intent["operation_id"])
        record = {"phase": phase, "intent": self.intent}
        if phase == "detached":
            record.update(profile="data2_absent", stopped_process=self.original, image_sha256="a" * 64,
                          arm=arm, arm_sha256=vm.hashlib.sha256(json.dumps(arm, sort_keys=True).encode()).hexdigest())
            self.file("retired-data2.json", json.dumps(record))
        self.file("detach-intent.json", json.dumps(self.intent))
        state = {"status": status, "retirement": record, "previous_process": self.original}
        if status == "running":
            state["process"] = self.original if phase == "intent" else self.detached_process()
        self.file("state.json", json.dumps(state))
        return record

    def detached_process(self):
        return {"pid": 34567, "start_ticks": "23456", "executable": str(Path(vm.QEMU).resolve()),
                "argv": vm.qemu_command(self.path, self.manifest, data2_absent=True)}

    def test_real_start_stop_restart_preserves_retirement_before_identity(self):
        record = self.records()
        process = self.detached_process()
        boots = []

        def boot(args, **kwargs):
            boots.append(args)
            self.file("qemu.pid", str(process["pid"]))

        with patch.object(vm, "check_images"), patch.object(vm, "retired_image_hash", return_value="a" * 64), \
                patch.object(vm, "run", side_effect=boot), patch.object(vm, "process_identity", return_value=process), \
                patch.object(vm, "qmp") as qmp, patch.object(vm, "process_finished", side_effect=[False, True]):
            vm.start(self.path, self.manifest)
            self.assertEqual(vm.read_state(self.path)["retirement"], record)
            vm.stop(self.path, self.manifest)
            self.assertEqual(vm.read_state(self.path)["retirement"], record)
            vm.start(self.path, self.manifest)
            self.assertEqual(vm.read_state(self.path)["retirement"], record)
            self.assertIn(((self.path, self.manifest, "system_powerdown"),), qmp.call_args_list)
        self.assertEqual(boots, [process["argv"], process["argv"]])
        self.assertFalse(any("data2" in arg for args in boots for arg in args))
        self.assertEqual(sum(arg == "-drive" for arg in boots[0]), 6)
        self.assertTrue(any("restrict=on" in arg for arg in boots[0]))

    def test_recover_start_preserves_record_with_and_without_pid(self):
        record = self.records(status="starting")
        process = self.detached_process()
        self.file("qemu.pid", str(process["pid"]))
        with patch.object(vm, "process_identity", return_value=process), patch.object(vm, "qmp"):
            vm.recover_start(self.path, self.manifest)
        self.assertEqual(vm.read_state(self.path), {"status": "running", "process": process, "retirement": record})
        (self.path / "qemu.pid").unlink()
        vm.save_state(self.path, {"status": "starting", "retirement": record})
        with patch.object(vm, "run") as run, patch.object(vm, "qmp") as qmp:
            vm.recover_start(self.path, self.manifest)
            run.assert_not_called()
            qmp.assert_not_called()
        self.assertEqual(vm.read_state(self.path), {"status": "stopped", "retirement": record})

    def test_pending_intent_allows_identity_stop_but_no_start_or_packages(self):
        record = self.records(phase="intent", status="running")
        with patch.object(vm, "process_identity", return_value=self.original), \
                patch.object(vm, "process_finished", side_effect=[False, True]), patch.object(vm, "qmp"):
            vm.running_identity(self.path, self.manifest, vm.read_state(self.path))
            vm.stop(self.path, self.manifest)
        self.assertEqual(vm.read_state(self.path)["retirement"], record)
        with patch.object(vm, "run") as external, patch.object(vm, "stop") as stop:
            for call in (lambda: vm.start(self.path, self.manifest),
                         lambda: vm.packages(self.path, self.manifest, True),
                         lambda: vm.packages(self.path, self.manifest, False),
                         lambda: vm.inventory(self.path, self.manifest),
                         lambda: vm.detach_data2(self.path, self.manifest)):
                with self.assertRaises(RuntimeError):
                    call()
            external.assert_not_called()
            stop.assert_not_called()

    def test_partial_deleted_or_changed_records_refuse_before_qemu(self):
        record = self.records()
        originals = {name: (self.path / name).read_text() for name in
                     ("state.json", "detach-intent.json", "retired-data2.json")}
        variants = [(name, None) for name in ("detach-intent.json", "retired-data2.json")]
        variants += [("state.json", json.dumps({"status": "stopped"})),
                     ("detach-intent.json", "{"),
                     ("detach-intent.json", json.dumps({**self.intent, "operation_id": str(uuid.uuid4())})),
                     ("retired-data2.json", json.dumps({**record, "image_sha256": "b" * 64}))]
        for name, value in variants:
            for original_name, content in originals.items():
                self.file(original_name, content)
            if value is None:
                (self.path / name).unlink()
            else:
                self.file(name, value)
            with self.subTest(name=name, value=value), patch.object(vm, "run") as external:
                with self.assertRaises((RuntimeError, ValueError)):
                    vm.start(self.path, self.manifest)
                external.assert_not_called()

    def test_changed_retired_image_hash_and_bootstrap_refuse_before_qemu(self):
        self.records()
        with patch.object(vm, "retired_image_hash", return_value="b" * 64), patch.object(vm, "run") as external:
            with self.assertRaisesRegex(RuntimeError, "SHA"):
                vm.start(self.path, self.manifest)
            with self.assertRaisesRegex(RuntimeError, "Pakiety"):
                vm.start(self.path, self.manifest, bootstrap=True)
            external.assert_not_called()

    def test_save_state_refuses_accidental_retirement_loss(self):
        self.records()
        before = (self.path / "state.json").read_bytes()
        with self.assertRaisesRegex(RuntimeError, "retirement"):
            vm.save_state(self.path, {"status": "stopped"})
        self.assertEqual((self.path / "state.json").read_bytes(), before)

    def test_main_replacement_transport_uses_pinned_record_and_full_original_contract(self):
        self.records(status="running")
        for phase in ("replacement-preflight", "replacement-prepare", "replacement-recover", "verify",
                      "enospc-preflight", "enospc"):
            with self.subTest(phase=phase), patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", phase]), \
                    patch.object(vm, "locked_runtime") as lock, \
                    patch.object(vm, "process_identity", return_value=self.detached_process()), \
                    patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), patch.object(vm, "run") as external, \
                    patch.object(vm.os, "statvfs") as space:
                space.return_value.f_frsize = 4096
                space.return_value.f_bavail = 3 * vm.GIB // 4096
                lock.return_value.__enter__.return_value = (self.path, self.manifest)
                vm.main()
                args = external.call_args.args[0]
                self.assertEqual(len(args), 3)
                remote = vm.shlex.split(args[2])
                self.assertEqual(remote[:4], ["sudo", "-n", "python3", "-"])
                self.assertEqual(json.loads(remote[4]), {"phase": phase, "uuid": self.manifest["uuid"],
                    "disks": self.manifest["disks"], "replacement_id": self.intent["operation_id"]})
                self.assertEqual(external.call_args.kwargs["input"], Path(vm.__file__).with_name("guest_storage.py").read_text())
                if phase == "enospc":
                    space.assert_called_once_with(self.path)
                else:
                    space.assert_not_called()
        with patch.object(vm, "run") as external:
            for phase in ("preflight", "prepare", "exercise", "corruption"):
                with self.assertRaises(RuntimeError):
                    vm.storage(self.path, self.manifest, phase)
            external.assert_not_called()

    def test_enospc_host_margin_and_failed_measurement_refuse_before_mutating_ssh(self):
        self.records(status="running")
        before = (self.path / "state.json").read_bytes()
        for variant in ("below_margin", "zero_fragment", "failed_measurement"):
            with self.subTest(variant=variant), patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", "enospc"]), \
                    patch.object(vm, "locked_runtime") as lock, \
                    patch.object(vm, "process_identity", return_value=self.detached_process()), \
                    patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), \
                    patch.object(vm.os, "statvfs") as space, patch.object(vm, "run") as external:
                lock.return_value.__enter__.return_value = (self.path, self.manifest)
                space.return_value.f_frsize = 0 if variant == "zero_fragment" else 4096
                space.return_value.f_bavail = 3 * vm.GIB // 4096 - 1
                space.return_value.f_bfree = 100 * vm.GIB // 4096
                if variant == "failed_measurement":
                    space.side_effect = OSError("Nie można zmierzyć filesystemu runtime")
                with self.assertRaises((RuntimeError, OSError)):
                    vm.main()
                external.assert_not_called()
                self.assertEqual((self.path / "state.json").read_bytes(), before)

    def test_enospc_phases_require_detached_restricted_profile_before_ssh(self):
        for variant in ("attached", "pending", "bootstrap"):
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as directory:
                path = Path(directory)
                state = {"status": "bootstrap" if variant == "bootstrap" else "running"}
                if variant == "pending":
                    vm.write_new(path / "detach-intent.json", json.dumps(self.intent))
                vm.write_new(path / "state.json", json.dumps(state))
                for phase in ("enospc-preflight", "enospc"):
                    with patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", phase]), \
                            patch.object(vm, "locked_runtime") as lock, patch.object(vm, "ssh_command") as ssh, \
                            patch.object(vm, "run") as external, patch.object(vm.os, "statvfs") as space:
                        lock.return_value.__enter__.return_value = (path, self.manifest)
                        with self.assertRaises(RuntimeError):
                            vm.main()
                        ssh.assert_not_called()
                        external.assert_not_called()
                        space.assert_not_called()

    def test_detach_crash_boundaries_never_allow_new_six_disk_boot(self):
        for boundary in ("before_ssh", "remote_arm_before_response", "before_stop", "after_stop", "before_receipt"):
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as directory:
                path = Path(directory)
                original = {**self.original, "argv": vm.qemu_command(path, self.manifest)}
                vm.write_new(path / "state.json", json.dumps({"status": "running", "process": original}))
                real_persist = vm.persist_state
                real_durable = vm.durable_new

                def ssh(args, **kwargs):
                    saved = vm.read_state(path)
                    self.assertEqual(saved["retirement"]["phase"], "intent")
                    self.assertTrue((path / "detach-intent.json").exists())
                    if boundary == "remote_arm_before_response":
                        raise subprocess.TimeoutExpired(args, 900)
                    operation_id = json.loads(vm.shlex.split(args[-1])[-1])["replacement_id"]
                    return subprocess.CompletedProcess(args, 0, json.dumps(self.arm(operation_id)), "")

                def stopping(*args):
                    if boundary == "before_stop":
                        raise RuntimeError("Przerwanie przed stop")
                    state = vm.read_state(path)
                    vm.save_state(path, vm.transition(state, status="stopped", previous_process=original))

                def persist(target, state):
                    if boundary == "before_ssh" and state.get("retirement", {}).get("phase") == "intent":
                        raise OSError("Przerwanie zapisu state przed SSH")
                    if boundary == "after_stop" and state.get("retirement", {}).get("phase") == "detached":
                        raise OSError("Przerwanie po stop")
                    return real_persist(target, state)

                def durable(target, value):
                    if boundary == "before_receipt" and target.name == "retired-data2.json":
                        raise OSError("Przerwanie przed receipt")
                    return real_durable(target, value)

                with patch.object(vm, "process_identity", return_value=original), \
                        patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), \
                        patch.object(vm, "run", side_effect=ssh), patch.object(vm, "stop", side_effect=stopping), \
                        patch.object(vm, "process_finished", return_value=True), \
                        patch.object(vm, "retired_image_hash", return_value="a" * 64), \
                        patch.object(vm, "persist_state", side_effect=persist), \
                        patch.object(vm, "durable_new", side_effect=durable):
                    with self.assertRaises((RuntimeError, OSError, subprocess.SubprocessError)):
                        vm.detach_data2(path, self.manifest)
                self.assertTrue((path / "detach-intent.json").exists())
                state = vm.read_state(path)
                if state["status"] == "running":
                    state = vm.transition(state, status="stopped", previous_process=original)
                    vm.persist_state(path, state)
                with patch.object(vm, "run") as external:
                    with self.assertRaises(RuntimeError):
                        vm.start(path, self.manifest)
                    external.assert_not_called()

    def test_real_failed_arm_process_preserves_diagnostics_and_pending_intent(self):
        self.file("state.json", json.dumps({"status": "running", "process": self.original}))
        failure = subprocess.run(["/usr/bin/python3", "-c",
                                  "import sys; print('journal-pending'); print('arm-refusal', file=sys.stderr); sys.exit(1)"],
                                 capture_output=True, text=True)
        error = subprocess.CalledProcessError(failure.returncode, failure.args, failure.stdout, failure.stderr)
        log = io.StringIO()
        with patch.object(vm, "process_identity", return_value=self.original), \
                patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), \
                patch.object(vm, "run", side_effect=error), patch.object(vm, "stop") as stop, \
                contextlib.redirect_stderr(log):
            with self.assertRaises(subprocess.CalledProcessError):
                vm.detach_data2(self.path, self.manifest)
            stop.assert_not_called()
        self.assertIn("journal-pending", log.getvalue())
        self.assertIn("arm-refusal", log.getvalue())
        self.assertEqual(vm.read_state(self.path)["retirement"]["phase"], "intent")
        self.assertFalse((self.path / "retired-data2.json").exists())

    def test_successful_detach_uses_real_stop_and_final_records_without_boot(self):
        self.file("state.json", json.dumps({"status": "running", "process": self.original}))
        previous_manifest = copy.deepcopy(self.manifest)

        def arm(args, **kwargs):
            self.assertEqual(args[:2], ["ssh", "pinned"])
            contract = json.loads(vm.shlex.split(args[-1])[-1])
            intent = json.loads((self.path / "detach-intent.json").read_text())
            self.assertEqual(contract, {"phase": "replacement-arm", "uuid": self.manifest["uuid"],
                "disks": self.manifest["disks"], "replacement_id": intent["operation_id"]})
            self.assertEqual(vm.read_state(self.path)["retirement"]["intent"], intent)
            return subprocess.CompletedProcess(args, 0, json.dumps(self.arm(intent["operation_id"])), "")

        def image_hash(*args):
            stopped = vm.read_state(self.path)
            self.assertEqual(stopped["status"], "stopped")
            self.assertEqual(stopped["previous_process"], self.original)
            self.assertEqual(stopped["retirement"]["phase"], "intent")
            self.assertFalse((self.path / "retired-data2.json").exists())
            return "a" * 64

        with patch.object(vm, "process_identity", return_value=self.original), \
                patch.object(vm, "process_finished", side_effect=[False, True, True]), \
                patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), \
                patch.object(vm, "run", side_effect=arm) as external, patch.object(vm, "qmp") as qmp, \
                patch.object(vm, "retired_image_hash", side_effect=image_hash):
            vm.detach_data2(self.path, self.manifest)
        state = vm.read_state(self.path)
        self.assertEqual(state["status"], "stopped")
        self.assertEqual(state["retirement"], json.loads((self.path / "retired-data2.json").read_text()))
        self.assertEqual(vm.retirement(self.path, self.manifest, state)["phase"], "detached")
        external.assert_called_once()
        qmp.assert_called_once_with(self.path, self.manifest, "system_powerdown")
        self.assertEqual(self.manifest, previous_manifest)

    def test_fsync_failure_before_arm_never_executes_guest_or_qemu(self):
        self.file("state.json", json.dumps({"status": "running", "process": self.original}))
        with patch.object(vm, "process_identity", return_value=self.original), \
                patch.object(vm.os, "fsync", side_effect=OSError("fsync")), \
                patch.object(vm, "run") as external:
            with self.assertRaisesRegex(OSError, "fsync"):
                vm.detach_data2(self.path, self.manifest)
            external.assert_not_called()
        self.assertTrue((self.path / "detach-intent.json").exists())
        with self.assertRaises(RuntimeError):
            vm.retirement(self.path, self.manifest, vm.read_state(self.path))

    def test_zero_exit_arm_with_invalid_response_preserves_pending_before_stop(self):
        for variant in ("json", "operation_id", "uuid", "stage", "count"):
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as directory:
                path = Path(directory)
                process = {**self.original, "argv": vm.qemu_command(path, self.manifest)}
                vm.write_new(path / "state.json", json.dumps({"status": "running", "process": process}))

                def response(args, **kwargs):
                    operation_id = json.loads(vm.shlex.split(args[-1])[-1])["replacement_id"]
                    arm = self.arm(operation_id)
                    if variant == "operation_id":
                        arm["state"]["replacement"]["operation_id"] = str(uuid.uuid4())
                    elif variant == "uuid":
                        arm["state"]["contract"]["uuid"] = str(uuid.uuid4())
                    elif variant == "stage":
                        arm["state"]["stage"] = "exercised"
                    elif variant == "count":
                        arm["state"]["format_count"] = 4
                    return subprocess.CompletedProcess(args, 0, "{" if variant == "json" else json.dumps(arm), "")

                with patch.object(vm, "process_identity", return_value=process), \
                        patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), \
                        patch.object(vm, "run", side_effect=response), patch.object(vm, "stop") as stop, \
                        patch.object(vm, "retired_image_hash") as image_hash:
                    with self.assertRaises((RuntimeError, ValueError)):
                        vm.detach_data2(path, self.manifest)
                    stop.assert_not_called()
                    image_hash.assert_not_called()
                state = vm.read_state(path)
                self.assertEqual(state["retirement"]["phase"], "intent")
                self.assertEqual(state["status"], "running")
                self.assertEqual(state["retirement"]["intent"], json.loads((path / "detach-intent.json").read_text()))
                self.assertFalse((path / "retired-data2.json").exists())


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


class BlockProfile(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-block-profile-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        vm_uuid = str(uuid.uuid4())
        self.manifest = {"schema": 1, "uid": os.getuid(), "runtime": str(self.path), "uuid": vm_uuid,
                         "ssh_port": 32123, "image_url": vm.IMAGE_URL, "image_sha512": vm.IMAGE_SHA512,
                         "machine": vm.MACHINE, "disks": vm.disk_manifest(vm_uuid, "block")}

    def write_manifest(self, manifest):
        target = self.path / "manifest.json"
        target.write_text(json.dumps(manifest))
        target.chmod(0o600)

    def test_block_manifest_has_only_os_disk_and_no_api_forward(self):
        self.write_manifest(self.manifest)
        actual = vm.load_manifest(self.path)
        self.assertEqual(vm.disk_profile(actual), "block")
        prefix = uuid.UUID(actual["uuid"]).hex[:10]
        self.assertEqual(actual["disks"], {"os": {"serial": f"tn-{prefix}-os", "bytes": 12 * vm.GIB}})
        args = vm.qemu_command(self.path, actual)
        self.assertEqual(args[args.index("-netdev") + 1],
                         "user,id=net0,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:32123-:22")
        self.assertEqual([args[index + 1].split(",")[1] for index, arg in enumerate(args) if arg == "-drive"],
                         ["id=os", "id=seed"])
        self.assertFalse(any(arg.startswith("nvme,") for arg in args))
        self.write_manifest({**self.manifest, "api_port": 32124})
        with self.assertRaisesRegex(RuntimeError, "Profil block nie dopuszcza portu API"):
            vm.load_manifest(self.path)

    def test_block_inventory_accepts_only_the_os_disk(self):
        disk = self.manifest["disks"]["os"]
        system = {"serial": disk["serial"], "size": disk["bytes"], "type": "disk", "name": "vda",
                  "contains_root": True, "used": True}
        vm.validate_inventory(self.manifest, {"uuid": self.manifest["uuid"], "disks": [system]})
        for extra in ({"serial": "", "size": vm.GIB, "type": "loop", "name": "loop0",
                       "contains_root": False, "used": False},
                      {"serial": "beaf11", "size": vm.GIB, "type": "disk", "name": "sda",
                       "contains_root": False, "used": False}):
            with self.subTest(extra=extra["name"]), self.assertRaisesRegex(RuntimeError, "Nieoczekiwane dyski"):
                vm.validate_inventory(self.manifest, {"uuid": self.manifest["uuid"], "disks": [system, extra]})

    def test_package_contract_carries_profile_derived_from_disk_map(self):
        source = Path(vm.__file__).with_name("guest_packages.py").read_text()
        for profile in ("storage", "e2", "e2-cache", "block"):
            manifest = {"uuid": self.manifest["uuid"], "ssh_port": 32123,
                        "disks": vm.disk_manifest(self.manifest["uuid"], profile)}
            with self.subTest(profile=profile), patch.object(vm, "read_state", return_value={"status": "running"}), \
                 patch.object(vm, "retirement", return_value=None), patch.object(vm, "running_identity"), \
                 patch.object(vm, "ssh_command", return_value=["ssh"]), patch.object(vm, "run") as external:
                vm.package_phase(self.path, manifest, "download")
                args, options = external.call_args
                self.assertEqual(args[0][0], "ssh")
                remote = shlex.split(args[0][1])
                self.assertEqual(remote[:4], ["sudo", "-n", "python3", "-"])
                self.assertEqual(json.loads(remote[4]),
                                 {"phase": "download", "uuid": self.manifest["uuid"], "profile": profile})
                self.assertEqual(options, {"input": source, "timeout": 900})


STORAGE_PACKAGES = ["snapraid", "mergerfs", "xfsprogs", "e2fsprogs", "nvme-cli"]
STORAGE_UNITS = (
    "e2scrub_all.timer", "e2scrub_all.service", "e2scrub_reap.service", "e2scrub@.service",
    "e2scrub_fail@.service", "xfs_scrub_all.timer", "xfs_scrub_all.service",
    "xfs_scrub@.service", "xfs_scrub_fail@.service", "nvmf-autoconnect.service",
    "nvmf-connect-nbft.service", "nvmf-connect@.service", "nvmefc-boot-connections.service",
    "xfs_scrub_all_fail.service", "xfs_scrub_media@.service", "xfs_scrub_media_fail@.service",
)
ISCSI_UNITS = ("iscsid.socket", "iscsid.service", "open-iscsi.service")


def tar_bytes(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w") as archive:
        for name, content in files.items():
            data = content.encode()
            info = tarfile.TarInfo("./" + name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return stream.getvalue()


class PackageProfiles(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-package-profiles-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.uuid = str(uuid.uuid4())

    def test_storage_profiles_keep_exact_package_set_units_and_tools(self):
        for name in ("storage", "e2", "e2-cache"):
            with self.subTest(name=name):
                profile = guest_packages.PROFILES[name]
                self.assertEqual(list(profile["packages"]), STORAGE_PACKAGES)
                self.assertEqual(profile["units"], STORAGE_UNITS)
                self.assertEqual(profile["unit_patterns"], ("*e2scrub*", "*xfs_scrub*", "*nvmf*", "*nvmefc*"))
                self.assertEqual(profile["binaries"], (("snapraid", "--version"), ("mergerfs", "--version"),
                                                       ("mkfs.xfs", "-V"), ("mke2fs", "-V"), ("nvme", "version")))
                self.assertEqual(profile["aliases"], ())

    def test_block_profile_is_exactly_open_iscsi_and_nvme_cli(self):
        profile = guest_packages.PROFILES["block"]
        self.assertEqual(profile["packages"], ("open-iscsi", "nvme-cli"))
        self.assertEqual(profile["units"], STORAGE_UNITS + ISCSI_UNITS)
        self.assertEqual(profile["binaries"], (("iscsiadm", "--version"), ("iscsid", "--version"),
                                               ("nvme", "version")))
        self.assertEqual(profile["aliases"], ("iscsi.service",))
        self.assertFalse(any("targetcli" in package for value in guest_packages.PROFILES.values()
                             for package in value["packages"]))

    def test_package_profiles_match_host_disk_profiles(self):
        self.assertEqual(set(guest_packages.PROFILES), set(vm.DISK_PROFILES))

    def test_main_refuses_missing_or_unknown_profile_before_touching_guest(self):
        for profile in (None, "arbitrary", "", "BLOCK", ["block"]):
            contract = {"phase": "prepare", "uuid": self.uuid}
            if profile is not None:
                contract["profile"] = profile
            with self.subTest(profile=profile), \
                 patch.object(guest_packages.sys, "argv", ["-", json.dumps(contract)]), \
                 patch.object(guest_packages.os, "umask"), \
                 patch.object(guest_packages, "check_guest") as guest, \
                 patch.object(guest_packages, "command") as command, \
                 patch.object(guest_packages, "prepare") as prepare:
                with self.assertRaisesRegex(RuntimeError, "Nieznany profil pakietów"):
                    guest_packages.main()
                guest.assert_not_called()
                command.assert_not_called()
                prepare.assert_not_called()

    def test_main_passes_the_contract_profile_to_each_phase(self):
        directory = Path("/var/lib/tentanas-vm-packages") / self.uuid
        for name in ("storage", "block"):
            profile = guest_packages.PROFILES[name]
            expected = {"prepare": ("prepare", (directory, profile["units"])),
                        "download": ("download", (directory, profile)),
                        "install": ("install", (directory, profile)),
                        "seal": ("seal", (directory, self.uuid, profile))}
            for phase, (function, args) in expected.items():
                contract = json.dumps({"phase": phase, "uuid": self.uuid, "profile": name})
                with self.subTest(name=name, phase=phase), \
                     patch.object(guest_packages.sys, "argv", ["-", contract]), \
                     patch.object(guest_packages.os, "umask"), \
                     patch.object(guest_packages.os, "getuid", return_value=0), \
                     patch.object(guest_packages, "check_guest") as guest, \
                     patch.object(guest_packages, function) as target:
                    guest_packages.main()
                    guest.assert_called_once_with(self.uuid)
                    target.assert_called_once_with(*args)

    def test_main_probe_child_drops_privileges_then_runs_the_profile_probe(self):
        account = unittest.mock.Mock(pw_uid=1000, pw_gid=1001)
        for name, expected in (("storage", "probe"), ("e2", "probe"), ("e2-cache", "probe"),
                               ("block", "block_probe")):
            events = []

            def leave(code):
                events.append(("_exit", code))
                raise SystemExit(code)

            contract = json.dumps({"phase": "probe", "uuid": self.uuid, "profile": name})
            with self.subTest(name=name), patch.object(guest_packages.sys, "argv", ["-", contract]), \
                 patch.object(guest_packages.os, "umask"), \
                 patch.object(guest_packages.os, "getuid", return_value=0), \
                 patch.object(guest_packages, "check_guest"), \
                 patch.object(guest_packages.pwd, "getpwnam", return_value=account) as getpwnam, \
                 patch.object(guest_packages.os, "fork", return_value=0), \
                 patch.object(guest_packages.os, "waitpid") as waitpid, \
                 patch.object(guest_packages.os, "setgroups", side_effect=lambda groups: events.append(("setgroups", groups))), \
                 patch.object(guest_packages.os, "setgid", side_effect=lambda gid: events.append(("setgid", gid))), \
                 patch.object(guest_packages.os, "setuid", side_effect=lambda uid: events.append(("setuid", uid))), \
                 patch.object(guest_packages.os, "_exit", side_effect=leave), \
                 patch.object(guest_packages, "probe", side_effect=lambda: events.append(("probe",))), \
                 patch.object(guest_packages, "block_probe", side_effect=lambda: events.append(("block_probe",))):
                with self.assertRaises(SystemExit) as child:
                    guest_packages.main()
                self.assertEqual(child.exception.code, 0)
                getpwnam.assert_called_once_with("tentanas")
                waitpid.assert_not_called()
                self.assertEqual(events, [("setgroups", []), ("setgid", 1001), ("setuid", 1000),
                                          (expected,), ("_exit", 0)])

    def test_masks_keep_storage_commands_and_add_iscsi_units_for_block(self):
        result = subprocess.CompletedProcess([], 0, stdout="")
        timers = ["apt-daily.timer", "apt-daily-upgrade.timer", "e2scrub_all.timer", "xfs_scrub_all.timer"]
        services = [unit for unit in guest_packages.APT_UNITS + STORAGE_UNITS if unit.endswith(".service")]
        for units, masked in ((STORAGE_UNITS, services), (STORAGE_UNITS + ISCSI_UNITS, services + list(ISCSI_UNITS))):
            with self.subTest(units=len(units)), patch.object(guest_packages, "command", return_value=result) as command, \
                 patch.object(guest_packages, "active", return_value=False) as active:
                guest_packages.mask_units(guest_packages.APT_UNITS + units)
                self.assertEqual(command.call_args_list, [
                    unittest.mock.call(["systemctl", "mask", *timers]),
                    unittest.mock.call(["systemctl", "stop", *timers]),
                    unittest.mock.call(["systemctl", "mask", *masked]),
                ])
                self.assertEqual([call.args[0] for call in active.call_args_list], masked)
        with patch.object(guest_packages, "command", return_value=result), \
             patch.object(guest_packages, "active", side_effect=lambda unit: unit == "iscsid.socket"), \
             self.assertRaisesRegex(RuntimeError, "iscsid.socket działa"):
            guest_packages.mask_units(guest_packages.APT_UNITS + STORAGE_UNITS + ISCSI_UNITS)

    def test_download_uses_only_the_profile_package_set(self):
        for name, packages in (("storage", STORAGE_PACKAGES), ("block", ["open-iscsi", "nvme-cli"])):
            directory = self.root / name
            directory.mkdir()
            profile = guest_packages.PROFILES[name]
            with self.subTest(name=name), patch.object(guest_packages, "validate_sources"), \
                 patch.object(guest_packages.socket, "getaddrinfo",
                              return_value=[(None, None, None, None, ("151.101.2.132", 443))]), \
                 patch.object(guest_packages.socket, "create_connection"), \
                 patch.object(guest_packages, "command",
                              return_value=subprocess.CompletedProcess([], 0, stdout="")) as command, \
                 patch.object(guest_packages, "inspect_archives") as inspect, \
                 contextlib.redirect_stdout(io.StringIO()):
                guest_packages.download(directory, profile)
                self.assertEqual([call.args[0] for call in command.call_args_list], [
                    ["apt-get", "-o", "APT::Update::Error-Mode=any", "update"],
                    ["apt-cache", "policy", *packages],
                    ["apt-get", "--simulate", "--no-install-recommends", "install", *packages],
                    ["apt-get", "--yes", "--download-only", "--no-install-recommends", "--no-remove",
                     "install", *packages]])
                inspect.assert_called_once_with(directory, profile)

    def inspect(self, profile, archives):
        base = Path(tempfile.mkdtemp(dir=self.root))
        cache, rules, state = base / "archives", base / "rules", base / "state"
        for path in (cache, rules, state):
            path.mkdir()
        names = {}
        for package, files in archives.items():
            archive = cache / f"{package}_1.0_amd64.deb"
            archive.write_bytes(package.encode())
            names[str(archive)] = (package, tar_bytes(files))

        def command(args, **kwargs):
            stdout = names[args[-2]][0] + "\n" if args[:2] == ["dpkg-deb", "--field"] else ""
            return subprocess.CompletedProcess(args, 0, stdout=stdout)

        def run(args, **kwargs):
            payload = names[args[-1]][1] if args[1] == "--fsys-tarfile" else tar_bytes({})
            return subprocess.CompletedProcess(args, 0, stdout=payload)

        with patch.object(guest_packages, "ARCHIVES", cache), patch.object(guest_packages, "UDEV_RULES", rules), \
             patch.object(guest_packages, "command", side_effect=command), \
             patch.object(guest_packages.subprocess, "run", side_effect=run), \
             contextlib.redirect_stdout(io.StringIO()):
            guest_packages.inspect_archives(state, guest_packages.PROFILES[profile])
        return json.loads((state / "downloaded.json").read_text()), rules

    def test_block_archives_accept_masked_iscsi_automation_and_override_its_udev_rules(self):
        units = "usr/lib/systemd/system/"
        rules_dir = "usr/lib/udev/rules.d/"
        receipt, rules = self.inspect("block", {
            "open-iscsi": {units + "iscsid.socket": "[Socket]", units + "iscsid.service": "[Service]",
                           units + "open-iscsi.service": "[Service]", "etc/init.d/iscsid": "#!/bin/sh",
                           "etc/init.d/open-iscsi": "#!/bin/sh", rules_dir + "70-open-iscsi.rules": "RULE",
                           rules_dir + "70-iscsi-network-interface.rules": "RULE", "usr/sbin/iscsiadm": "ELF"},
            "nvme-cli": {units + "nvmf-autoconnect.service": "[Service]", units + "nvmf-connect.target": "[Unit]",
                         rules_dir + "70-nvmf-autoconnect.rules": "RULE"},
            "libopeniscsiusr": {"usr/lib/x86_64-linux-gnu/libopeniscsiusr.so.0": "ELF"}})
        expected = ["70-iscsi-network-interface.rules", "70-nvmf-autoconnect.rules", "70-open-iscsi.rules"]
        self.assertEqual(sorted(path.name for path in rules.iterdir()), expected)
        self.assertTrue(all(os.readlink(path) == "/dev/null" for path in rules.iterdir()))
        self.assertEqual(sorted(receipt["udev_overrides"]), [str(rules / name) for name in expected])
        self.assertEqual(receipt["packages"], ["open-iscsi", "nvme-cli"])
        self.assertEqual(len(receipt["archives"]), 3)

    def test_storage_archives_keep_nvme_cli_inspection_and_skip_foreign_packages(self):
        receipt, rules = self.inspect("storage", {
            "nvme-cli": {"usr/lib/systemd/system/nvmf-connect@.service": "[Service]",
                         "usr/lib/systemd/system/nvmf-connect.target": "[Unit]",
                         "usr/lib/udev/rules.d/70-nvmf-autoconnect.rules": "RULE"},
            "open-iscsi": {"usr/lib/systemd/system/iscsid.socket": "[Socket]",
                           "usr/lib/udev/rules.d/70-open-iscsi.rules": "RULE"}})
        self.assertEqual([path.name for path in rules.iterdir()], ["70-nvmf-autoconnect.rules"])
        self.assertEqual(receipt["packages"], STORAGE_PACKAGES)

    def test_archives_refuse_unknown_socket_path_or_init_script(self):
        for profile, package, name in (
                ("storage", "nvme-cli", "usr/lib/systemd/system/nvmf-foreign.socket"),
                ("block", "open-iscsi", "usr/lib/systemd/system/iscsiuio.socket"),
                ("block", "open-iscsi", "usr/lib/systemd/system/iscsi-watch.path"),
                ("block", "open-iscsi", "etc/init.d/iscsiuio")):
            with self.subTest(profile=profile, name=name), self.assertRaisesRegex(RuntimeError, "Nieznana automatyka"):
                self.inspect(profile, {package: {name: "automation"}})

    def test_install_refuses_profile_other_than_downloaded_before_marker_or_apt(self):
        for receipt in ({"packages": STORAGE_PACKAGES, "archives": {}, "udev_overrides": []},
                        {"archives": {}, "udev_overrides": []}):
            (self.root / "downloaded.json").write_text(json.dumps(receipt))
            with self.subTest(receipt=sorted(receipt)), patch.object(guest_packages, "validate_sources") as sources, \
                 patch.object(guest_packages, "command") as command, \
                 patch.object(guest_packages, "check_automation") as automation:
                with self.assertRaisesRegex(RuntimeError, "niezgodny z pobraniem"):
                    guest_packages.install(self.root, guest_packages.PROFILES["block"])
                sources.assert_not_called()
                command.assert_not_called()
                automation.assert_not_called()
                self.assertFalse((self.root / "installation-started").exists())

    def test_install_accepts_pre_profile_receipt_only_as_the_storage_set(self):
        (self.root / "downloaded.json").write_text(json.dumps({"archives": {}, "udev_overrides": []}))
        for name in ("storage", "e2", "e2-cache"):
            with self.subTest(name=name), \
                 patch.object(guest_packages, "validate_sources",
                              side_effect=RuntimeError("po kontroli profilu")) as sources, \
                 patch.object(guest_packages, "command") as command:
                with self.assertRaisesRegex(RuntimeError, "po kontroli profilu"):
                    guest_packages.install(self.root, guest_packages.PROFILES[name])
                sources.assert_called_once_with()
                command.assert_not_called()
                self.assertFalse((self.root / "installation-started").exists())

    def test_install_uses_profile_packages_units_tools_and_alias_check(self):
        storage_tools = (("snapraid", "--version"), ("mergerfs", "--version"), ("mkfs.xfs", "-V"),
                         ("mke2fs", "-V"), ("nvme", "version"))
        block_tools = (("iscsiadm", "--version"), ("iscsid", "--version"), ("nvme", "version"))
        alias = ["systemctl", "show", "--property=LoadState", "--value", "--", "iscsi.service"]
        for name, packages, tools, aliases in (("storage", STORAGE_PACKAGES, storage_tools, []),
                                               ("block", ["open-iscsi", "nvme-cli"], block_tools, [alias])):
            profile = guest_packages.PROFILES[name]
            base = Path(tempfile.mkdtemp(dir=self.root))
            cache, tool_dir, state = base / "archives", base / "bin", base / "state"
            for path in (cache, tool_dir, state):
                path.mkdir()
            (cache / "tool_1.0_amd64.deb").write_bytes(b"deb")
            overrides = ["/etc/udev/rules.d/70-nvmf-autoconnect.rules"]
            (state / "downloaded.json").write_text(json.dumps({
                "packages": packages, "udev_overrides": overrides,
                "archives": {"tool_1.0_amd64.deb": vm.hashlib.sha256(b"deb").hexdigest()}}))
            for binary, _ in tools:
                (tool_dir / binary).write_text(binary)
            tool_paths = {str((tool_dir / binary).resolve()) for binary, _ in tools}
            calls = []

            def command(args, **kwargs):
                calls.append(args)
                return subprocess.CompletedProcess(args, 0, stdout="not-found\n" if args[:2] == ["systemctl", "show"] else "")

            with self.subTest(name=name), patch.object(guest_packages, "ARCHIVES", cache), \
                 patch.object(guest_packages, "validate_sources"), \
                 patch.object(guest_packages, "command", side_effect=command), \
                 patch.object(guest_packages, "active", return_value=False) as active, \
                 patch.object(guest_packages, "check_automation") as automation, \
                 patch.object(guest_packages.shutil, "which", side_effect=lambda binary: str(tool_dir / binary)), \
                 contextlib.redirect_stdout(io.StringIO()):
                guest_packages.install(state, profile)
                automation.assert_called_once_with(profile["units"], [], overrides)
                self.assertIn(["apt-get", "--yes", "--no-download", "--no-install-recommends", "--no-remove",
                               "install", *packages], calls)
                self.assertEqual([args for args in calls if args[:2] == ["dpkg", "-L"]],
                                 [["dpkg", "-L", package] for package in packages])
                self.assertIn(["systemctl", "list-unit-files", "--no-pager", *profile["unit_patterns"]], calls)
                self.assertEqual([args for args in calls if args[0] in tool_paths],
                                 [[str((tool_dir / binary).resolve()), flag] for binary, flag in tools])
                self.assertEqual([args for args in calls if args[:2] == ["systemctl", "show"]], aliases)
                self.assertEqual([call.args[0] for call in active.call_args_list], list(profile["units"]))
                self.assertEqual(json.loads((state / "installed.json").read_text()),
                                 {"units": list(profile["units"]), "disabled_cron": [], "udev_overrides": overrides})

    def test_block_alias_must_stay_masked_or_absent(self):
        for state, accepted in (("not-found", True), ("masked", True), ("loaded", False), ("", False)):
            result = subprocess.CompletedProcess([], 0, stdout=state + "\n")
            with self.subTest(state=state), patch.object(guest_packages, "command", return_value=result):
                if accepted:
                    guest_packages.check_aliases(("iscsi.service",))
                else:
                    with self.assertRaisesRegex(RuntimeError, "Alias jednostki iscsi.service"):
                        guest_packages.check_aliases(("iscsi.service",))
        (self.root / "probe-complete").touch()
        (self.root / "installed.json").write_text(json.dumps(
            {"units": list(STORAGE_UNITS + ISCSI_UNITS), "disabled_cron": [], "udev_overrides": []}))
        with patch.object(guest_packages, "check_automation"), \
             patch.object(guest_packages, "command",
                          return_value=subprocess.CompletedProcess([], 0, stdout="loaded\n")), \
             self.assertRaisesRegex(RuntimeError, "Alias jednostki iscsi.service"):
            guest_packages.seal(self.root, self.uuid, guest_packages.PROFILES["block"])
        self.assertFalse((self.root / "ready.json").exists())

    def test_seal_records_the_profile_package_set(self):
        for name, packages in (("storage", STORAGE_PACKAGES), ("block", ["open-iscsi", "nvme-cli"])):
            directory = self.root / name
            directory.mkdir()
            (directory / "probe-complete").touch()
            installed = {"units": list(guest_packages.PROFILES[name]["units"]),
                         "disabled_cron": [], "udev_overrides": ["/etc/udev/rules.d/70-nvmf-autoconnect.rules"]}
            (directory / "installed.json").write_text(json.dumps(installed))
            with self.subTest(name=name), patch.object(guest_packages, "check_automation") as automation, \
                 patch.object(guest_packages, "command",
                              return_value=subprocess.CompletedProcess([], 0, stdout="not-found\n")) as command, \
                 contextlib.redirect_stdout(io.StringIO()):
                guest_packages.seal(directory, self.uuid, guest_packages.PROFILES[name])
                automation.assert_called_once_with(installed["units"], [], installed["udev_overrides"])
                self.assertEqual([call.args[0][-1] for call in command.call_args_list],
                                 ["iscsi.service"] if name == "block" else [])
                self.assertEqual(json.loads((directory / "ready.json").read_text()),
                                 {**installed, "schema": 1, "uuid": self.uuid, "packages": packages,
                                  "network": "restricted"})

    def test_seal_refuses_profile_other_than_installed(self):
        (self.root / "probe-complete").touch()
        (self.root / "installed.json").write_text(json.dumps(
            {"units": list(STORAGE_UNITS), "disabled_cron": [], "udev_overrides": []}))
        with patch.object(guest_packages, "check_automation") as automation, \
             self.assertRaisesRegex(RuntimeError, "niezgodny z instalacją"):
            guest_packages.seal(self.root, self.uuid, guest_packages.PROFILES["block"])
        automation.assert_not_called()
        self.assertFalse((self.root / "ready.json").exists())

    def probe_root(self):
        root = Path(tempfile.mkdtemp(dir=self.root))
        (root / "proc/1").mkdir(parents=True)
        (root / "proc/1/comm").write_text("systemd\n")
        (root / "proc/self").mkdir()
        (root / "proc/self/mountinfo").write_text(
            "22 27 0:21 / /sys rw,nosuid,nodev,noexec,relatime shared:7 - sysfs sysfs rw\n"
            "35 22 0:32 / /sys/kernel/config rw,nosuid,nodev,noexec,relatime shared:14 - configfs configfs rw\n")
        (root / "sys/class/iscsi_session").mkdir(parents=True)
        (root / "sys/kernel/config").mkdir(parents=True)
        (root / "sys/module/loop").mkdir(parents=True)
        return root

    def test_block_probe_accepts_inert_guest_and_refuses_autonomous_state(self):
        with patch.object(guest_packages.os, "getuid", return_value=1000), \
             contextlib.redirect_stdout(io.StringIO()):
            guest_packages.block_probe(self.probe_root())
        changes = (("iscsid", lambda root: ((root / "proc/812").mkdir(),
                                            (root / "proc/812/comm").write_text("iscsid\n"))),
                   ("session", lambda root: (root / "sys/class/iscsi_session/session1").mkdir()),
                   ("nvme", lambda root: (root / "sys/class/nvme/nvme0").mkdir(parents=True)),
                   ("lio", lambda root: (root / "sys/kernel/config/target").mkdir()),
                   ("nvmet", lambda root: (root / "sys/kernel/config/nvmet").mkdir()),
                   ("target_core_mod", lambda root: (root / "sys/module/target_core_mod").mkdir()),
                   ("nvmet_module", lambda root: (root / "sys/module/nvmet").mkdir()),
                   ("configfs_unmounted", lambda root: (root / "proc/self/mountinfo").write_text(
                       "22 27 0:21 / /sys rw,nosuid shared:7 - sysfs sysfs rw\n")))
        for label, change in changes:
            root = self.probe_root()
            change(root)
            with self.subTest(change=label), patch.object(guest_packages.os, "getuid", return_value=1000), \
                 contextlib.redirect_stdout(io.StringIO()), \
                 self.assertRaisesRegex(RuntimeError, "nie jest czysty"):
                guest_packages.block_probe(root)

    @unittest.skipIf(os.geteuid() == 0, "root ignoruje prawa katalogu")
    def test_block_probe_fails_closed_when_configfs_is_unreadable(self):
        root = self.probe_root()
        config = root / "sys/kernel/config"
        config.chmod(0)
        self.addCleanup(config.chmod, 0o755)
        with patch.object(guest_packages.os, "getuid", return_value=1000), \
             contextlib.redirect_stdout(io.StringIO()), self.assertRaises(PermissionError):
            guest_packages.block_probe(root)
        with patch.object(guest_packages.os, "getuid", return_value=0), \
             self.assertRaisesRegex(RuntimeError, "konto testowe"):
            guest_packages.block_probe(self.root)


if __name__ == "__main__":
    unittest.main()
