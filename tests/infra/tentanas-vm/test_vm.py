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
import io


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
        self.manifest["disks"] = vm.disk_manifest(self.manifest["uuid"])
        self.manifest["image_inodes"] = {role: [10, index + 1] for index, role in enumerate(vm.DISKS)}
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
        for phase in ("replacement-preflight", "replacement-prepare", "replacement-recover", "verify"):
            with self.subTest(phase=phase), patch.object(vm.sys, "argv", ["vm.py", "storage", "runtime", phase]), \
                    patch.object(vm, "locked_runtime") as lock, \
                    patch.object(vm, "process_identity", return_value=self.detached_process()), \
                    patch.object(vm, "ssh_command", return_value=["ssh", "pinned"]), patch.object(vm, "run") as external:
                lock.return_value.__enter__.return_value = (self.path, self.manifest)
                vm.main()
                args = external.call_args.args[0]
                self.assertEqual(len(args), 3)
                remote = vm.shlex.split(args[2])
                self.assertEqual(remote[:4], ["sudo", "-n", "python3", "-"])
                self.assertEqual(json.loads(remote[4]), {"phase": phase, "uuid": self.manifest["uuid"],
                    "disks": self.manifest["disks"], "replacement_id": self.intent["operation_id"]})
                self.assertEqual(external.call_args.kwargs["input"], Path(vm.__file__).with_name("guest_storage.py").read_text())
        with patch.object(vm, "run") as external:
            for phase in ("preflight", "prepare", "exercise", "corruption"):
                with self.assertRaises(RuntimeError):
                    vm.storage(self.path, self.manifest, phase)
            external.assert_not_called()

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


if __name__ == "__main__":
    unittest.main()
