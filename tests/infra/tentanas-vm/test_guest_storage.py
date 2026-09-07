# =============================================================================
# Plik: tests/infra/tentanas-vm/test_guest_storage.py
# Opis: Odmowy głównych faz storage oraz trwałość journalu bez urządzeń i VM.
# Przykład: python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_storage.py -v
# =============================================================================

import copy
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import uuid
from types import SimpleNamespace

import guest_storage as storage


class StorageGuards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tentanas-storage-unit-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "state"
        identity = str(uuid.uuid4())
        prefix = uuid.UUID(identity).hex[:10]
        self.request = {"phase": "prepare", "uuid": identity,
                        "disks": {role: {"serial": f"tn-{prefix}-{role}", "bytes": size * 1024**3}
                                  for role, size in storage.SIZES.items()}}
        self.expected = storage.contract(self.request)
        self.base = self.root / identity
        self.observation = {"uuid": identity, "boot_id": str(uuid.uuid4()), "swaps": [], "disks": [],
                            "mounts": [{"source": "/dev/vda1", "target": "/", "fstype": "ext4",
                                        "maj:min": "252:1"}]}
        for index, (role, disk) in enumerate(self.request["disks"].items()):
            self.observation["disks"].append({
                "name": "nvme0n1" if role == "cache" else "vd" + chr(97 + index),
                "serial": disk["serial"], "size": disk["bytes"], "type": "disk", "ro": False,
                "maj:min": f"252:{index * 16}", "fstype": None, "uuid": None,
                "mountpoints": [None], "holders": [], "signatures": [],
                "children": [{"maj:min": "252:1", "type": "part"}] if role == "os" else []})
        self.calls = []
        self.patch(storage, "STATE_ROOT", self.root)
        self.patch(storage.os, "geteuid", return_value=0)
        self.real_observe = storage.observe
        self.patch(storage, "observe", side_effect=lambda: copy.deepcopy(self.observation))
        self.real_command = storage.command
        self.patch(storage, "command", side_effect=self.command)
        self.patch(storage, "private", side_effect=self.private_fixture)
        self.real_automation = storage.automation_guard
        self.automation = self.patch(storage, "automation_guard")

    def patch(self, obj, name, *args, **kwargs):
        handle = patch.object(obj, name, *args, **kwargs)
        self.addCleanup(handle.stop)
        return handle.start()

    def private_fixture(self, path, directory=False):
        info = path.lstat()
        self.assertTrue(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
        self.assertEqual(path.resolve(), path)
        self.assertFalse(info.st_mode & 0o077)

    def command(self, args, expected=0):
        self.calls.append((args, expected))
        if args[0] == "/usr/sbin/mkfs.ext4":
            journal = json.loads((self.base / "state.json").read_text())
            role = journal["format_pending"]
            self.assertIn(role, storage.TARGETS)
            self.assertEqual(args[-1], "/dev/" + self.disk(role)["name"])
            self.disk(role).update(fstype="ext4", uuid=str(uuid.uuid4()), signatures=[{"type": "ext4"}])
        elif args[0] == "/usr/bin/mount":
            disk = next(disk for disk in self.observation["disks"] if disk["uuid"] == args[-2])
            self.observation["mounts"].append({"target": args[-1], "source": "/dev/" + disk["name"],
                                               "fstype": "ext4", "maj:min": disk["maj:min"]})
        elif args[0] == "/usr/bin/mergerfs":
            self.observation["mounts"].append({"target": args[-1], "source": args[-2],
                                               "fstype": "fuse.mergerfs", "maj:min": "0:44"})
        return ""

    def disk(self, role):
        serial = self.request["disks"][role]["serial"]
        return next(disk for disk in self.observation["disks"] if disk["serial"] == serial)

    def main(self, phase):
        self.request["phase"] = phase
        with patch.object(storage.sys, "argv", ["guest_storage.py", json.dumps(self.request)]), \
                patch.object(storage.sys, "stdout", io.StringIO()):
            storage.main()

    def assert_no_format(self):
        self.assertFalse(any(args[0] == "/usr/sbin/mkfs.ext4" for args, _ in self.calls))

    def test_preflight_does_not_create_state_or_execute_mutations(self):
        self.main("preflight")
        self.assertFalse(self.root.exists())
        self.assertEqual(self.calls, [])

    def test_main_prepare_rejects_every_identity_boundary_before_any_format(self):
        original = copy.deepcopy(self.observation)
        variants = []
        for field, value in (("serial", "wrong"), ("size", 1), ("ro", True),
                             ("type", "part"), ("maj:min", "252:0"), ("holders", ["dm-0"]),
                             ("fstype", "ext4"), ("signatures", [{"type": "gpt"}]),
                             ("children", [{"type": "part", "maj:min": "252:17"}])):
            altered = copy.deepcopy(original)
            altered["disks"][1][field] = value
            variants.append(altered)
        for update in ({"uuid": str(uuid.uuid4())}, {"swaps": ["252:16"]}):
            altered = copy.deepcopy(original)
            altered.update(update)
            variants.append(altered)
        wrong_root = copy.deepcopy(original)
        wrong_root["mounts"][0]["maj:min"] = "252:16"
        variants.append(wrong_root)
        wrong_mount = copy.deepcopy(original)
        wrong_mount["mounts"].append({"source": "/dev/vdb", "target": "/foreign",
                                     "fstype": "ext4", "maj:min": "252:16"})
        variants.append(wrong_mount)
        duplicate = copy.deepcopy(original)
        duplicate["disks"][2]["serial"] = duplicate["disks"][1]["serial"]
        variants.append(duplicate)
        for observation in variants:
            with self.subTest(observation=observation):
                self.observation = observation
                with self.assertRaises(RuntimeError):
                    self.main("prepare")
                self.assert_no_format()
                self.assertFalse(self.root.exists())

    def test_main_rejects_modified_contract_and_existing_state(self):
        self.request["disks"]["parity"]["bytes"] = 1
        with self.assertRaisesRegex(RuntimeError, "profilu"):
            self.main("prepare")
        self.assert_no_format()

    def test_main_prepare_records_pending_before_each_of_exactly_three_formats(self):
        self.main("prepare")
        journal = json.loads((self.base / "state.json").read_text())
        self.assertEqual(journal["stage"], "prepared")
        self.assertEqual(journal["format_count"], 3)
        self.assertEqual(set(journal["filesystems"]), set(storage.TARGETS))
        formatted = [args[-1] for args, _ in self.calls if args[0] == "/usr/sbin/mkfs.ext4"]
        self.assertEqual(formatted, ["/dev/" + self.disk(role)["name"] for role in storage.TARGETS])
        self.assertTrue(all(self.disk(role)["fstype"] is None for role in ("os", "cache", "spare")))

    def test_repeat_prepare_and_interrupted_format_never_format_again(self):
        original_command = self.command

        def fail_first_format(args, expected=0):
            if args[0] == "/usr/sbin/mkfs.ext4":
                self.calls.append((args, expected))
                raise RuntimeError("przerwane mkfs")
            return original_command(args, expected)

        with patch.object(storage, "command", side_effect=fail_first_format):
            with self.assertRaisesRegex(RuntimeError, "przerwane"):
                self.main("prepare")
        self.assertEqual(json.loads((self.base / "state.json").read_text())["format_pending"], "data1")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "ponawiane"):
            self.main("prepare")
        self.assert_no_format()

    def test_changed_identity_immediately_before_format_refuses(self):
        reads = 0

        def observe():
            nonlocal reads
            reads += 1
            result = copy.deepcopy(self.observation)
            if reads >= 3:
                result["disks"][1]["serial"] = "changed"
            return result

        with patch.object(storage, "observe", side_effect=observe):
            with self.assertRaisesRegex(RuntimeError, "serial"):
                self.main("prepare")
        self.assert_no_format()

    def direct_observation_fixture(self, probe_result=None):
        self.patch(storage, "observe", self.real_observe)
        actual_stat, actual_read, actual_iterdir = Path.stat, Path.read_text, Path.iterdir

        def device_stat(path, *args, **kwargs):
            if path.parent == Path("/dev"):
                disk = next(disk for disk in self.observation["disks"] if disk["name"] == path.name)
                major, minor = map(int, disk["maj:min"].split(":"))
                return SimpleNamespace(st_mode=stat.S_IFBLK, st_rdev=storage.os.makedev(major, minor))
            return actual_stat(path, *args, **kwargs)

        def read(path, *args, **kwargs):
            values = {"/proc/swaps": "Filename Type Size Used Priority\n",
                      "/sys/class/dmi/id/product_uuid": self.observation["uuid"],
                      "/proc/sys/kernel/random/boot_id": self.observation["boot_id"]}
            return values[str(path)] if str(path) in values else actual_read(path, *args, **kwargs)

        self.patch(Path, "stat", device_stat)
        self.patch(Path, "read_text", read)
        self.patch(Path, "iterdir", lambda path: iter(()) if str(path).startswith("/sys/dev/block/") else actual_iterdir(path))
        original_command = self.command

        def command(args, expected=0, **kwargs):
            if args[0] == "/usr/bin/lsblk":
                disks = copy.deepcopy(self.observation["disks"])
                for disk in disks:
                    disk.update(fstype=None, uuid=None)
                return json.dumps({"blockdevices": disks})
            if args[0] == "/usr/bin/findmnt":
                return json.dumps({"filesystems": self.observation["mounts"]})
            if args[0] == "/usr/sbin/wipefs":
                disk = next(disk for disk in self.observation["disks"] if "/dev/" + disk["name"] == args[-1])
                return json.dumps({"signatures": disk["signatures"]})
            if args[0] == "/usr/sbin/blkid":
                disk = next(disk for disk in self.observation["disks"] if "/dev/" + disk["name"] == args[-1])
                if probe_result is not None:
                    result = subprocess.CompletedProcess(args, *probe_result)
                elif disk["fstype"]:
                    result = subprocess.CompletedProcess(args, 0, f"DEVNAME={args[-1]}\nTYPE={disk['fstype']}\nUUID={disk['uuid']}\n", "")
                elif disk["serial"] == self.request["disks"]["os"]["serial"]:
                    result = subprocess.CompletedProcess(args, 0, f"DEVNAME={args[-1]}\nPTTYPE=gpt\nPTUUID={self.request['uuid']}\n", "")
                else:
                    result = subprocess.CompletedProcess(args, 2, "", "")
                with patch.object(storage.subprocess, "run", return_value=result):
                    return self.real_command(args, expected, **kwargs)
            return original_command(args, expected)

        self.patch(storage, "command", side_effect=command)

    def test_real_observe_uses_direct_probe_despite_stale_udev_and_preserves_serials(self):
        self.direct_observation_fixture()
        with patch.object(storage.sys, "stderr", io.StringIO()):
            self.main("prepare")
        journal = json.loads((self.base / "state.json").read_text())
        self.assertEqual(journal["stage"], "prepared")
        self.assertEqual(journal["format_count"], 3)
        self.assertEqual(journal["filesystems"], {role: self.disk(role)["uuid"] for role in storage.TARGETS})
        self.assertTrue(all(self.disk(role)["serial"] == self.request["disks"][role]["serial"] for role in storage.SIZES))

    def test_real_observe_probe_errors_refuse_before_any_format(self):
        for result in ((2, "", "permission denied"), (2, "partial\n", ""), (8, "", ""),
                       (4, "", ""), (0, "", ""), (0, "DEVNAME=/dev/vda\nTYPE=ext4\n", "")):
            with self.subTest(result=result), patch.object(storage.sys, "stderr", io.StringIO()):
                self.direct_observation_fixture(result)
                with self.assertRaises(RuntimeError):
                    self.main("prepare")
                self.assert_no_format()
                self.assertFalse(self.root.exists())

    def test_automation_failure_immediately_before_format_refuses(self):
        self.automation.side_effect = [None, None, RuntimeError("Automatyka aktywna")]
        with self.assertRaisesRegex(RuntimeError, "Automatyka"):
            self.main("prepare")
        self.assert_no_format()
        self.assertEqual(json.loads((self.base / "state.json").read_text())["format_pending"], "data1")

    def receipt_fixture(self):
        package_root = Path(self.temp.name) / "packages"
        directory = package_root / self.request["uuid"]
        package_root.mkdir(mode=0o700)
        directory.mkdir(mode=0o700)
        self.receipt_path = directory / "ready.json"
        self.receipt = {"schema": 1, "uuid": self.request["uuid"], "network": "restricted",
                        "packages": ["snapraid", "mergerfs", "xfsprogs", "e2fsprogs", "nvme-cli"],
                        "units": ["e2scrub_all.timer", "nvmf-autoconnect.service"],
                        "disabled_cron": ["/etc/cron.d/tentanas-unit-fixture"],
                        "udev_overrides": ["/etc/udev/rules.d/999-tentanas-unit-fixture.rules"]}
        self.receipt_path.write_text(json.dumps(self.receipt))
        self.receipt_path.chmod(0o600)
        self.patch(storage, "PACKAGES_ROOT", package_root)
        self.patch(storage, "automation_guard", self.real_automation)
        actual_symlink = Path.is_symlink
        self.patch(Path, "is_symlink", lambda path: True if str(path) in self.receipt["udev_overrides"] else actual_symlink(path))
        self.patch(storage.os, "readlink", return_value="/dev/null")
        self.system_state = "UnitFileState=masked\nActiveState=inactive\n"

        def system_command(args, expected=0):
            self.calls.append((args, expected))
            return self.system_state

        self.patch(storage, "command", side_effect=system_command)

    def test_receipt_rechecks_actual_systemd_state_and_configuration_before_prepare(self):
        self.receipt_fixture()
        self.main("preflight")
        self.assertFalse(self.root.exists())
        variants = ({"uuid": str(uuid.uuid4())}, {"network": "temporary-egress"}, {"packages": []},
                    {"units": []}, {"units": ["../foreign.service"]}, {"disabled_cron": []},
                    {"udev_overrides": []}, {"schema": 9})
        for change in variants:
            with self.subTest(change=change):
                self.receipt_path.write_text(json.dumps({**self.receipt, **change}))
                with self.assertRaises(RuntimeError):
                    self.main("prepare")
                self.assert_no_format()
                self.assertFalse(self.root.exists())
        self.receipt_path.write_text(json.dumps(self.receipt))
        for state in ("UnitFileState=enabled\nActiveState=inactive\n",
                      "UnitFileState=masked\nActiveState=active\n",
                      "UnitFileState=masked\nActiveState=failed\n", ""):
            with self.subTest(state=state):
                self.system_state = state
                with self.assertRaisesRegex(RuntimeError, "Automatyka"):
                    self.main("prepare")
                self.assert_no_format()
                self.assertFalse(self.root.exists())

    def test_receipt_rejects_restored_cron_or_udev_before_prepare(self):
        self.receipt_fixture()
        exists = Path.exists
        with patch.object(Path, "exists", lambda path: True if str(path) in self.receipt["disabled_cron"] else exists(path)):
            with self.assertRaisesRegex(RuntimeError, "cron"):
                self.main("prepare")
        with patch.object(storage.os, "readlink", return_value="/foreign"):
            with self.assertRaisesRegex(RuntimeError, "udev"):
                self.main("prepare")
        self.assert_no_format()
        self.assertFalse(self.root.exists())

    def test_template_checks_mask_and_instances_without_invalid_show(self):
        self.receipt_fixture()
        self.receipt["units"].append("e2scrub@.service")
        self.receipt_path.write_text(json.dumps(self.receipt))
        listing = ""
        enabled = (1, "masked\n")

        def systemctl(args, **kwargs):
            self.calls.append((args, None))
            if args[1] == "show" and args[-1] == "e2scrub@.service":
                return subprocess.CompletedProcess(args, 1, "", "neither a valid invocation ID nor unit name")
            if args[1] == "is-enabled":
                return subprocess.CompletedProcess(args, enabled[0], enabled[1], "")
            if args[1] == "list-units":
                self.assertEqual(args[-1], "e2scrub@*.service")
                self.assertIn("--all", args)
                self.assertFalse(any(arg.startswith("--state") for arg in args))
                return subprocess.CompletedProcess(args, 0, listing, "")
            return subprocess.CompletedProcess(args, 0, self.system_state, "")

        with patch.object(storage, "command", self.real_command), \
                patch.object(storage.subprocess, "run", side_effect=systemctl), \
                patch.object(storage.sys, "stderr", io.StringIO()):
            with self.assertRaises(RuntimeError):
                self.real_command(["/usr/bin/systemctl", "show", "e2scrub@.service"])
            self.calls.clear()
            self.main("preflight")
            self.assertFalse(any(args[1] == "show" and args[-1] == "e2scrub@.service" for args, _ in self.calls))
            for listing in ("e2scrub@dev.service loaded active running scrub\n",
                            "e2scrub@dev.service loaded inactive dead scrub\n",
                            "e2scrub@dev.service loaded failed failed scrub\n"):
                with self.subTest(listing=listing), self.assertRaisesRegex(RuntimeError, "instancja"):
                    self.main("prepare")
            listing = ""
            for enabled in ((0, "static\n"), (1, "disabled\n"), (1, "masked-runtime\n"), (4, "not-found\n")):
                with self.subTest(enabled=enabled), self.assertRaises(RuntimeError):
                    self.main("prepare")
        self.assert_no_format()
        self.assertFalse(self.root.exists())

    def test_missing_mount_refuses_snap_before_command(self):
        self.main("prepare")
        state = json.loads((self.base / "state.json").read_text())
        self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                      if mount["target"] != str(self.base / "mnt/data1")]
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            storage.Storage(self.expected, self.base, state).snap("not-run", ["sync"])
        self.assertEqual(self.calls, [])

    def test_foreign_destination_mount_refuses_before_mount_command(self):
        self.main("prepare")
        state = json.loads((self.base / "state.json").read_text())
        for mount in self.observation["mounts"]:
            if mount["target"] == str(self.base / "mnt/data1"):
                mount["maj:min"] = "252:1"
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "mount"):
            storage.Storage(self.expected, self.base, state).mount()
        self.assertEqual(self.calls, [])

    def test_prepare_foreign_mount_prevents_mount_and_data_directory_creation(self):
        def observe():
            result = copy.deepcopy(self.observation)
            if (self.base / "mnt").exists():
                result["mounts"].append({"source": "/dev/vda1", "target": str(self.base / "mnt/data1"),
                                        "fstype": "ext4", "maj:min": "252:1"})
            return result

        with patch.object(storage, "observe", side_effect=observe):
            with self.assertRaisesRegex(RuntimeError, "Obcy mount"):
                self.main("prepare")
        self.assertFalse(any(args[0] == "/usr/bin/mount" for args, _ in self.calls))
        self.assertFalse((self.base / "mnt/data1/data").exists())

    def exercise_fixture(self, lose_mount=False, corrupt_fix=False):
        self.main("prepare")
        self.calls.clear()
        phase_stderr = io.StringIO()
        self.patch(storage.sys, "stderr", phase_stderr)
        original = {"restore.bin": {"role": "data1", "bytes": 64, "sha256": "first"},
                    "second.bin": {"role": "data2", "bytes": 67, "sha256": "second"}}
        hashes = [{}, original, original, original, original]
        if corrupt_fix:
            hashes[3] = {"restore.bin": {"sha256": "wrong"}}
        self.patch(storage.Storage, "hashes", side_effect=hashes)
        self.patch(storage.os, "urandom", return_value=b"x")
        real_digest = storage.digest
        self.patch(storage, "digest", side_effect=lambda path: "first" if path.name == "restore.bin" else real_digest(path))
        original_command = self.command

        def snap_command(args, expected=0):
            if args[0] != "/usr/bin/snapraid":
                return original_command(args, expected)
            self.calls.append((args, expected))
            log = Path(args[args.index("-l") + 1])
            if args[-1] == "scrub":
                log.write_text("block_count:268\nsummary:error_file:0\nsummary:error_io:0\n"
                               "summary:error_data:0\nsummary:exit:ok\n")
                return "100% completed, 131 MB accessed in 0:00\nEverything OK\n"
            if log.name == "05-check.log" and lose_mount:
                self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                              if mount["target"] != str(self.base / "mnt/data1")]
            if args[-1] == "fix":
                self.assertFalse((self.base / "union/restore.bin").exists())
                (self.base / "union/restore.bin").write_bytes(b"x" * 64)
            if args[-1] == "sync":
                evidence = json.loads(phase_stderr.getvalue().splitlines()[0])
                self.assertEqual(evidence["original"], original)
                self.assertEqual(evidence["sha256"], real_digest(self.base / "original.json"))
                for path in ("mnt/parity/snapraid.parity", "mnt/data1/snapraid.content", "mnt/data2/snapraid.content"):
                    (self.base / path).write_bytes(b"stan")
            return ""

        self.patch(storage, "command", side_effect=snap_command)

    def test_exercise_command_sequence_requires_scrub_recovery_and_final_hashes(self):
        self.exercise_fixture()
        self.main("exercise")
        sequence = [(args[-1], expected) for args, expected in self.calls]
        self.assertEqual(sequence, [("diff", 2), ("sync", 0), ("diff", 0), ("scrub", 0),
                                    ("check", 0), ("diff", 2), ("fix", 0), ("check", 0),
                                    ("sync", 0), ("diff", 0)])
        fix = next(args for args, _ in self.calls if args[-1] == "fix")
        self.assertEqual(fix[-6:], ["-d", "d1", "-m", "-f", "/restore.bin", "fix"])
        journal = json.loads((self.base / "state.json").read_text())
        self.assertEqual(journal["stage"], "exercised")
        self.assertEqual(journal["scrub_blocks"], 268)
        self.assertEqual(set(journal["statvfs"]), {"data1", "data2", "union"})
        self.assertEqual(len(journal["baseline"]), 4)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "ponawiana"):
            self.main("exercise")
        self.assertEqual(self.calls, [])

    def test_mount_lost_after_check_prevents_unlink_and_fix(self):
        self.exercise_fixture(lose_mount=True)
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("exercise")
        self.assertTrue((self.base / "union/restore.bin").exists())
        self.assertFalse(any(args[-1] == "fix" for args, _ in self.calls))
        self.assertEqual([args[-1] for args, _ in self.calls].count("sync"), 1)

    def test_wrong_restored_hash_prevents_final_sync_and_completion(self):
        self.exercise_fixture(corrupt_fix=True)
        with self.assertRaisesRegex(RuntimeError, "Odzyskane SHA"):
            self.main("exercise")
        self.assertEqual([args[-1] for args, _ in self.calls].count("sync"), 1)
        self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "exercising")

    def corruption_fixture(self, write_mode="normal", wrong_scrub=False, wrong_recovery=False):
        self.main("prepare")
        self.calls.clear()
        self.original_files = {"restore.bin": b"pierwotny korpus restore" * 3,
                               "second.bin": b"druga niezalezna galaz" * 5}
        original = {}
        for name, payload in self.original_files.items():
            role = "data2" if name == "restore.bin" else "data1"
            path = self.base / "mnt" / role / "data" / name
            path.write_bytes(payload)
            storage.os.utime(path, ns=(1700000000123456789, 1700000000987654321))
            (self.base / "union" / name).write_bytes(payload)
            original[name] = {"role": role, "bytes": len(payload), "sha256": storage.digest(path)}
        (self.base / "original.json").write_text(json.dumps(original))
        baseline_paths = ("snapraid.conf", "mnt/data1/snapraid.content", "mnt/data2/snapraid.content",
                          "mnt/parity/snapraid.parity")
        for name in baseline_paths[1:]:
            (self.base / name).write_bytes(b"pierwotny checkpoint")
        state = json.loads((self.base / "state.json").read_text())
        state.update(stage="exercised", boot_id="historyczny-boot", original_sha256=storage.digest(self.base / "original.json"),
                     baseline={name: storage.digest(self.base / name) for name in baseline_paths})
        storage.save(self.base, state)
        self.before_corruption = copy.deepcopy(state)
        self.original_manifest = (self.base / "original.json").read_bytes()
        self.target = self.base / "mnt/data2/data/restore.bin"
        self.target_before = self.target.stat()
        self.byte_writes = []
        real_write = storage.os.pwrite

        def write(descriptor, payload, offset):
            journal = json.loads((self.base / "state.json").read_text())
            self.assertEqual(journal["stage"], "corrupting")
            self.assertEqual(journal["corruption"]["before"]["baseline"], self.before_corruption["baseline"])
            self.byte_writes.append((payload, offset))
            if write_mode == "noop":
                return len(payload)
            if write_mode == "short":
                return 0
            result = real_write(descriptor, payload, offset)
            (self.base / "union/restore.bin").write_bytes(self.target.read_bytes())
            if write_mode == "crash":
                raise RuntimeError("Przerwanie po zapisie")
            return result

        self.patch(storage.os, "pwrite", side_effect=write)
        self.patch(storage.sys, "stderr", io.StringIO())

        def snap_command(args, expected=0):
            self.calls.append((args, expected))
            self.assertEqual(args[0], "/usr/bin/snapraid")
            self.assertNotIn(args[-1], ("sync", "mkfs.ext4"))
            log = Path(args[args.index("-l") + 1])
            if args[-1] == "diff":
                after = self.target.stat()
                self.assertEqual(after.st_size, self.target_before.st_size)
                self.assertEqual(after.st_mtime_ns, self.target_before.st_mtime_ns)
                self.assertEqual(after.st_ino, self.target_before.st_ino)
                self.assertEqual(self.target.read_bytes(), bytes([self.original_files["restore.bin"][0] ^ 1])
                                 + self.original_files["restore.bin"][1:])
            if log.name == "corruption-01-scrub.log":
                self.assertNotEqual(storage.digest(self.target), original["restore.bin"]["sha256"])
                disk = "foreign" if wrong_scrub else "d2"
                log.write_text(f"block_count:2\nerror:0:{disk}:restore.bin: Data error at position 0, diff bits 64/128\n"
                               "summary:error_file:0\nsummary:error_io:0\nsummary:error_data:1\nsummary:exit:error\n")
                for role in ("data1", "data2"):
                    (self.base / "mnt" / role / "snapraid.content").write_bytes(b"oznaczony zly blok")
                return "100% completed, 1 MB accessed\n"
            if args[-1] == "fix":
                self.assertEqual(args[-5:], ["-d", "d2", "-f", "/restore.bin", "fix"])
                payload = b"uszkodzone" if wrong_recovery else self.original_files["restore.bin"]
                self.target.write_bytes(payload)
                (self.base / "union/restore.bin").write_bytes(payload)
                log.write_text("error:0:d2:restore.bin: Data error at position 0, diff bits 64/128\n"
                               "fixed:0:d2:restore.bin: Fixed data error at position 0\nsummary:error:1\n"
                               "summary:error_recovered:1\nsummary:error_unrecoverable:0\nsummary:exit:recovered\n")
            if log.name == "corruption-04-scrub.log":
                log.write_text("block_count:2\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n")
                for role in ("data1", "data2"):
                    (self.base / "mnt" / role / "snapraid.content").write_bytes(b"nowy czysty checkpoint")
                return "100% completed, 1 MB accessed\nEverything OK\n"
            return ""

        self.patch(storage, "command", side_effect=snap_command)

    def test_corruption_main_flips_real_byte_once_and_preserves_original_checkpoint(self):
        self.corruption_fixture()
        self.main("corruption")
        state = json.loads((self.base / "state.json").read_text())
        self.assertEqual(self.byte_writes, [(bytes([self.original_files["restore.bin"][0] ^ 1]), 0)])
        self.assertEqual([(args[-1], expected) for args, expected in self.calls],
                         [("diff", 0), ("scrub", 1), ("fix", 0), ("check", 0), ("scrub", 0)])
        self.assertEqual(state["corruption"]["before"]["baseline"], self.before_corruption["baseline"])
        self.assertEqual(state["boot_id"], self.observation["boot_id"])
        self.assertEqual(state["stage"], "exercised")
        self.assertEqual(state["format_count"], 3)
        self.assertEqual(state["original_sha256"], self.before_corruption["original_sha256"])
        self.assertEqual((self.base / "original.json").read_bytes(), self.original_manifest)
        self.assertEqual(self.target.read_bytes(), self.original_files["restore.bin"])
        self.assertNotEqual(state["baseline"]["mnt/data1/snapraid.content"], self.before_corruption["baseline"]["mnt/data1/snapraid.content"])
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "jednokrotna"):
            self.main("corruption")
        self.assertEqual(len(self.byte_writes), 1)
        self.assertEqual(self.calls, [])

    def test_corruption_noop_short_write_and_crash_leave_pending_without_fix_or_retry(self):
        for mode in ("noop", "short", "crash"):
            with self.subTest(mode=mode):
                case = StorageGuards()
                case.setUp()
                try:
                    case.corruption_fixture(write_mode=mode)
                    with self.assertRaises(RuntimeError):
                        case.main("corruption")
                    self.assertEqual(case.calls, [])
                    self.assertEqual(json.loads((case.base / "state.json").read_text())["stage"], "corrupting")
                    for phase in ("corruption", "verify"):
                        with self.assertRaisesRegex(RuntimeError, "ponawiana"):
                            case.main(phase)
                    self.assertEqual(len(case.byte_writes), 1)
                    self.assertEqual(case.calls, [])
                finally:
                    case.doCleanups()

    def test_corruption_rejects_missing_mount_before_any_byte_write(self):
        self.corruption_fixture()
        self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                      if mount["target"] != str(self.base / "mnt/data2")]
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("corruption")
        self.assertEqual(self.byte_writes, [])
        self.assertEqual(self.target.read_bytes(), self.original_files["restore.bin"])

    def test_corruption_wrong_scrub_location_prevents_fix(self):
        self.corruption_fixture(wrong_scrub=True)
        with self.assertRaisesRegex(RuntimeError, "wskazał"):
            self.main("corruption")
        self.assertEqual([args[-1] for args, _ in self.calls], ["diff", "scrub"])
        self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "corrupting")

    def test_corruption_wrong_recovered_sha_does_not_advance_checkpoint(self):
        self.corruption_fixture(wrong_recovery=True)
        with self.assertRaisesRegex(RuntimeError, "Odzyskane SHA"):
            self.main("corruption")
        state = json.loads((self.base / "state.json").read_text())
        self.assertEqual(state["stage"], "corrupting")
        self.assertEqual(state["baseline"], self.before_corruption["baseline"])
        self.assertEqual(state["boot_id"], self.before_corruption["boot_id"])

    def test_corruption_changed_baseline_rejects_before_journal_and_write(self):
        self.corruption_fixture()
        (self.base / "mnt/parity/snapraid.parity").write_bytes(b"obca parity")
        with self.assertRaisesRegex(RuntimeError, "baseline"):
            self.main("corruption")
        self.assertEqual(self.byte_writes, [])
        self.assertEqual(self.calls, [])
        self.assertNotIn("corruption", json.loads((self.base / "state.json").read_text()))

    def test_corruption_changed_original_manifest_rejects_before_write(self):
        self.corruption_fixture()
        (self.base / "original.json").write_text("{}")
        with self.assertRaisesRegex(RuntimeError, "manifest SHA"):
            self.main("corruption")
        self.assertEqual(self.byte_writes, [])
        self.assertEqual(self.calls, [])
        self.assertNotIn("corruption", json.loads((self.base / "state.json").read_text()))

    def test_corruption_mount_lost_after_journal_prevents_write(self):
        self.corruption_fixture()
        real_save = storage.save

        def save_then_lose_mount(base, state):
            real_save(base, state)
            self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                          if mount["target"] != str(self.base / "mnt/data2")]

        self.patch(storage, "save", side_effect=save_then_lose_mount)
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("corruption")
        self.assertEqual(self.byte_writes, [])
        self.assertEqual(self.calls, [])
        self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "corrupting")

    def test_verify_after_corruption_requires_new_boot_not_only_old_exercise_restart(self):
        self.corruption_fixture()
        self.main("corruption")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "zrestartowany"):
            self.main("verify")
        self.assertEqual(self.calls, [])
        self.observation["boot_id"] = str(uuid.uuid4())
        self.main("verify")
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])

    def replacement_fixture(self, arm=True, detach=True, wrong_recovery=False, fail_check=False):
        self.corruption_fixture()
        self.main("corruption")
        state = json.loads((self.base / "state.json").read_text())
        state["scrub_blocks"] = 2
        storage.save(self.base, state)
        self.request["replacement_id"] = str(uuid.uuid4())
        self.calls.clear()
        if arm:
            self.main("replacement-arm")
        self.attached_files = {}
        if detach:
            self.old_disk = copy.deepcopy(self.disk("data2"))
            self.observation["disks"].remove(self.disk("data2"))
            self.observation["boot_id"] = str(uuid.uuid4())
            self.observation["mounts"] = self.observation["mounts"][:1]
            for role in storage.TARGETS:
                path = self.base / "mnt" / role
                destination = Path(self.temp.name) / ("detached-" + role)
                path.rename(destination)
                path.mkdir(mode=0o700)
                if role != "data2":
                    self.attached_files[role] = destination
            for path in (self.base / "union").iterdir():
                path.unlink()

        def replacement_command(args, expected=0):
            if args[0] == "/usr/sbin/mkfs.ext4":
                self.calls.append((args, expected))
                journal = json.loads((self.base / "state.json").read_text())
                self.assertEqual(journal["stage"], "replacement_formatting")
                self.assertEqual(journal["format_pending"], "spare")
                self.assertEqual(journal["format_count"], 3)
                self.assertEqual(args, ["/usr/sbin/mkfs.ext4", "-q", "/dev/" + self.disk("spare")["name"]])
                self.disk("spare").update(fstype="ext4", uuid=str(uuid.uuid4()), signatures=[{"type": "ext4"}])
                return ""
            if args[0] == "/usr/bin/mount":
                role = Path(args[-1]).name
                if role in self.attached_files:
                    Path(args[-1]).rmdir()
                    self.attached_files.pop(role).rename(args[-1])
                return self.command(args, expected)
            if args[0] == "/usr/bin/mergerfs":
                for role in ("data1", "data2"):
                    for path in (self.base / "mnt" / role / "data").iterdir():
                        (self.base / "union" / path.name).write_bytes(path.read_bytes())
                return self.command(args, expected)
            self.calls.append((args, expected))
            self.assertEqual(args[0], "/usr/bin/snapraid")
            log = Path(args[args.index("-l") + 1])
            if args[-1] == "fix":
                self.assertEqual(args[-3:], ["-d", "d2", "fix"])
                self.assertFalse((self.base / "mnt/data2/snapraid.content").exists())
                self.assertEqual(storage.digest(self.base / "mnt/data1/snapraid.content"),
                                 json.loads((self.base / "state.json").read_text())["replacement"]["before"]["baseline"]["mnt/data1/snapraid.content"])
                payload = b"zly odzysk" if wrong_recovery else self.original_files["restore.bin"]
                (self.base / "mnt/data2/data/restore.bin").write_bytes(payload)
                (self.base / "union/restore.bin").write_bytes(payload)
                log.write_text("blocksize:262144\nerror:0:d2:restore.bin: Read error at position 0\n"
                               "fixed:0:d2:restore.bin: Fixed data error at position 0\nstatus:recovered:d2:restore.bin\n"
                               "summary:error:1\nsummary:error_recovered:1\nsummary:error_unrecoverable:0\nsummary:exit:recovered\n")
                return "100% completed, 1 MB accessed\n"
            if args[-1] == "check" and fail_check:
                raise RuntimeError("Nieudany check")
            if args[-1] == "sync":
                self.assertEqual([call[-1] for call, _ in self.calls][-3:], ["fix", "check", "sync"])
                self.assertEqual((self.base / "mnt/data2/data/restore.bin").read_bytes(), self.original_files["restore.bin"])
                for role in ("data1", "data2"):
                    (self.base / "mnt" / role / "snapraid.content").write_bytes(b"nowa mapa FS")
            if args[-1] == "scrub":
                log.write_text("block_count:2\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n")
                return "100% completed, 1 MB accessed\nEverything OK\n"
            return ""

        self.patch(storage, "command", side_effect=replacement_command)

    def test_replacement_main_formats_only_spare_and_recovers_before_sync(self):
        self.replacement_fixture()
        armed = json.loads((self.base / "state.json").read_text())
        self.main("replacement-preflight")
        self.assertEqual(self.calls, [])
        self.assertEqual(json.loads((self.base / "state.json").read_text()), armed)
        self.main("replacement-prepare")
        prepared = json.loads((self.base / "state.json").read_text())
        self.assertEqual(prepared["format_count"], 4)
        self.assertEqual(prepared["filesystems"]["data2"], self.disk("spare")["uuid"])
        self.assertNotEqual(prepared["filesystems"]["data2"], armed["filesystems"]["data2"])
        self.assertEqual(sum(args[0] == "/usr/sbin/mkfs.ext4" for args, _ in self.calls), 1)
        self.calls.clear()
        self.main("replacement-recover")
        self.assertEqual([args[-1] for args, _ in self.calls], ["fix", "check", "sync", "diff", "scrub", "check"])
        state = json.loads((self.base / "state.json").read_text())
        self.assertTrue(state["replacement"]["completed"])
        self.assertEqual(state["stage"], "exercised")
        self.assertEqual(state["replacement"]["before"]["filesystems"], armed["filesystems"])
        self.assertEqual((self.base / "original.json").read_bytes(), self.original_manifest)
        self.assertEqual(state["baseline"]["snapraid.conf"], armed["baseline"]["snapraid.conf"])
        self.calls.clear()
        for phase in ("replacement-arm", "replacement-prepare", "replacement-recover"):
            with self.assertRaises(RuntimeError):
                self.main(phase)
        with self.assertRaisesRegex(RuntimeError, "zrestartowany"):
            self.main("verify")
        self.assertEqual(self.calls, [])
        self.observation["boot_id"] = str(uuid.uuid4())
        self.main("verify")
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])
        self.calls.clear()
        self.observation["disks"].append(self.old_disk)
        with self.assertRaisesRegex(RuntimeError, "nadal obecny"):
            self.main("verify")
        self.assertEqual(self.calls, [])

    def test_replacement_missing_or_foreign_id_refuses_before_mutations(self):
        self.replacement_fixture()
        for identity in (None, str(uuid.uuid4()), "../obcy"):
            with self.subTest(identity=identity):
                if identity is None:
                    self.request.pop("replacement_id", None)
                else:
                    self.request["replacement_id"] = identity
                with self.assertRaises((RuntimeError, KeyError, ValueError)):
                    self.main("replacement-prepare")
                self.assertEqual(self.calls, [])

    def test_replacement_follows_serials_after_device_names_and_numbers_change(self):
        self.replacement_fixture()
        names = {"os": "vdb", "data1": "vde", "parity": "vdf", "cache": "nvme0n1", "spare": "vda"}
        for index, (role, name) in enumerate(names.items()):
            self.disk(role).update(name=name, **{"maj:min": f"253:{index * 16}"})
        self.disk("os")["children"][0]["maj:min"] = "253:1"
        self.observation["mounts"][0].update(source="/dev/vdb1", **{"maj:min": "253:1"})
        self.main("replacement-prepare")
        formats = [args for args, _ in self.calls if args[0] == "/usr/sbin/mkfs.ext4"]
        self.assertEqual(formats, [["/usr/sbin/mkfs.ext4", "-q", "/dev/vda"]])
        self.assertEqual(self.disk("os")["name"], "vdb")
        self.main("replacement-recover")
        self.observation["boot_id"] = str(uuid.uuid4())
        self.calls.clear()
        self.main("verify")
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])
        self.assertEqual(json.loads((self.base / "state.json").read_text())["filesystems"]["data2"], self.disk("spare")["uuid"])

    def test_five_disks_without_replacement_journal_never_relax_base_guard(self):
        self.replacement_fixture(arm=False)
        self.request.pop("replacement_id")
        with self.assertRaisesRegex(RuntimeError, "zestaw całych dysków"):
            self.main("verify")
        self.assertEqual(self.calls, [])

    def test_replacement_arm_wrong_original_or_baseline_never_writes_readiness(self):
        self.replacement_fixture(arm=False, detach=False)
        original = self.base / "original.json"
        original.write_text("{}")
        with self.assertRaisesRegex(RuntimeError, "manifest SHA"):
            self.main("replacement-arm")
        original.write_bytes(self.original_manifest)
        (self.base / "mnt/parity/snapraid.parity").write_bytes(b"obca parity")
        with self.assertRaisesRegex(RuntimeError, "baseline"):
            self.main("replacement-arm")
        self.assertNotIn("replacement", json.loads((self.base / "state.json").read_text()))
        self.assertEqual(self.calls, [])

    def test_replacement_prepare_rejects_all_five_disk_boundaries_without_mkfs(self):
        self.replacement_fixture()
        original = copy.deepcopy(self.observation)
        variants = []
        for field, value in (("serial", "wrong"), ("size", 1), ("ro", True), ("holders", ["dm-0"]),
                             ("fstype", "ext4"), ("signatures", [{"type": "gpt"}]),
                             ("children", [{"type": "part", "maj:min": "252:99"}])):
            altered = copy.deepcopy(original)
            next(disk for disk in altered["disks"] if disk["serial"] == self.request["disks"]["spare"]["serial"])[field] = value
            variants.append(altered)
        for update in ({"uuid": str(uuid.uuid4())}, {"swaps": [self.disk("spare")["maj:min"]]},
                       {"boot_id": json.loads((self.base / "state.json").read_text())["replacement"]["armed_boot_id"]},
                       {"disks": original["disks"] + [self.old_disk]}, {"disks": original["disks"][:-1]}):
            altered = copy.deepcopy(original)
            altered.update(update)
            variants.append(altered)
        altered = copy.deepcopy(original)
        altered["mounts"][0]["maj:min"] = self.disk("spare")["maj:min"]
        variants.append(altered)
        altered = copy.deepcopy(original)
        altered["mounts"].append({"source": "/dev/foreign", "target": "/foreign", "fstype": "ext4",
                                  "maj:min": self.disk("spare")["maj:min"]})
        variants.append(altered)
        altered = copy.deepcopy(original)
        altered["disks"][1]["uuid"] = str(uuid.uuid4())
        variants.append(altered)
        for altered in variants:
            with self.subTest(observation=altered):
                self.observation = altered
                with self.assertRaises(RuntimeError):
                    self.main("replacement-prepare")
                self.assertEqual(self.calls, [])
                self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "replacement_armed")

    def test_replacement_surviving_content_changed_refuses_before_format(self):
        self.replacement_fixture()
        (self.attached_files["data1"] / "snapraid.content").write_bytes(b"obcy content")
        with self.assertRaisesRegex(RuntimeError, "źródła odzysku"):
            self.main("replacement-prepare")
        self.assert_no_format()
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.main("replacement-prepare")
        self.assertEqual(self.calls, [])

    def test_replacement_crash_after_mkfs_preserves_pending_and_refuses_retry(self):
        self.replacement_fixture()
        original_command = storage.command.side_effect

        def crash_after_format(args, expected=0):
            output = original_command(args, expected)
            if args[0] == "/usr/sbin/mkfs.ext4":
                raise RuntimeError("Przerwanie po mkfs")
            return output

        self.patch(storage, "command", side_effect=crash_after_format)
        with self.assertRaisesRegex(RuntimeError, "Przerwanie"):
            self.main("replacement-prepare")
        state = json.loads((self.base / "state.json").read_text())
        self.assertEqual(state["format_pending"], "spare")
        self.assertEqual(state["format_count"], 3)
        self.assertEqual(sum(args[0] == "/usr/sbin/mkfs.ext4" for args, _ in self.calls), 1)
        self.calls.clear()
        for phase in ("replacement-prepare", "replacement-recover", "verify"):
            with self.assertRaises(RuntimeError):
                self.main(phase)
        self.assertEqual(self.calls, [])

    def test_replacement_wrong_sha_blocks_sync_and_checkpoint(self):
        self.replacement_fixture(wrong_recovery=True)
        self.main("replacement-prepare")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "Odzyskane SHA"):
            self.main("replacement-recover")
        self.assertEqual([args[-1] for args, _ in self.calls], ["fix"])
        self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "replacement_recovering")

    def test_replacement_check_failure_blocks_sync(self):
        self.replacement_fixture(fail_check=True)
        self.main("replacement-prepare")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "Nieudany check"):
            self.main("replacement-recover")
        self.assertEqual([args[-1] for args, _ in self.calls], ["fix", "check"])

    def test_replacement_truncated_success_log_blocks_check_sync_and_retry(self):
        self.replacement_fixture()
        self.main("replacement-prepare")
        original_command = storage.command.side_effect

        def truncate_successful_fix(args, expected=0):
            output = original_command(args, expected)
            if args[-1] == "fix":
                self.assertEqual(expected, 0)
                self.assertEqual((self.base / "mnt/data2/data/restore.bin").read_bytes(), self.original_files["restore.bin"])
                log = Path(args[args.index("-l") + 1])
                log.write_text(log.read_text().replace("summary:exit:recovered\n", ""))
            return output

        self.patch(storage, "command", side_effect=truncate_successful_fix)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "dowodu odzysku"):
            self.main("replacement-recover")
        self.assertEqual([args[-1] for args, _ in self.calls], ["fix"])
        self.assertEqual(json.loads((self.base / "state.json").read_text())["stage"], "replacement_recovering")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "ponawiana"):
            self.main("replacement-recover")
        self.assertEqual(self.calls, [])

    def test_replacement_mkfs_ambiguous_observation_leaves_pending_without_retry(self):
        self.replacement_fixture()
        original_command = storage.command.side_effect

        def format_without_identity(args, expected=0):
            output = original_command(args, expected)
            if args[0] == "/usr/sbin/mkfs.ext4":
                self.disk("spare")["uuid"] = None
            return output

        self.patch(storage, "command", side_effect=format_without_identity)
        with self.assertRaisesRegex(RuntimeError, "mkfs spare"):
            self.main("replacement-prepare")
        self.assertEqual(json.loads((self.base / "state.json").read_text())["format_pending"], "spare")
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.main("replacement-prepare")
        self.assertEqual(self.calls, [])

    def test_replacement_verify_rejects_uncompleted_checkpoint_before_mount(self):
        self.replacement_fixture()
        self.main("replacement-prepare")
        state = json.loads((self.base / "state.json").read_text())
        state["stage"] = "exercised"
        storage.save(self.base, state)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "ukończonego cyklu"):
            self.main("verify")
        self.assertEqual(self.calls, [])

    def test_replacement_mount_lost_after_pending_prevents_mkfs(self):
        self.replacement_fixture()
        original_save = storage.save

        def save_then_lose_mount(base, state):
            original_save(base, state)
            if state["format_pending"] == "spare":
                self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                              if mount["target"] != str(self.base / "mnt/data1")]

        self.patch(storage, "save", side_effect=save_then_lose_mount)
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("replacement-prepare")
        self.assert_no_format()

    def test_replacement_mount_lost_after_check_prevents_sync(self):
        self.replacement_fixture()
        self.main("replacement-prepare")
        original_command = storage.command.side_effect

        def check_then_lose_mount(args, expected=0):
            output = original_command(args, expected)
            if args[-1] == "check":
                self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                              if mount["target"] != str(self.base / "mnt/data1")]
            return output

        self.patch(storage, "command", side_effect=check_then_lose_mount)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("replacement-recover")
        self.assertEqual([args[-1] for args, _ in self.calls], ["fix", "check"])

    def enospc_fixture(self, failure="write", mutate_owner=False, lose_mount=False, wrong_options=False):
        self.replacement_fixture()
        self.main("replacement-prepare")
        self.main("replacement-recover")
        self.calls.clear()
        self.before_enospc = (self.base / "state.json").read_bytes()
        self.fill_writes = []
        self.fill_error = False
        self.fill_path = None
        self.runtime_options = {
            "branches": ":".join(str(self.base / "mnt" / role / "data") + "=RW" for role in ("data1", "data2")),
            "category.create": "mfs", "minfreespace": "16777216", "moveonenospc": "false", "nullrw": "false",
            "cache.files": "libfuse", "cache.writeback": "false", "direct_io": "false", "kernel_cache": "false",
            "auto_cache": "false", "cache.statfs": "0"}
        self.patch(storage, "ENOSPC_MAX_BYTES", 16384)
        self.patch(storage, "ENOSPC_CHUNK_BYTES", 4096)
        self.patch(storage, "ENOSPC_MIN_BYTES", 4096)
        self.patch(storage, "ENOSPC_BRANCH_MIN_BYTES", 4096)

        def measured_space(path):
            role = path.name if path.parent.name == "mnt" else ("os" if path == self.base else "union")
            value = {"device": {"os": 1, "data1": 2, "data2": 3, "parity": 4, "union": 5}[role],
                     "free": 2 * 1024**3, "available": 2 * 1024**3, "total": 3 * 1024**3, "free_inodes": 1000}
            if role == "data2":
                value.update(free=16 * 1024**2 + 8192, available=8192)
                if self.fill_error and self.fill_path.exists():
                    value.update(free=value["free"] - self.fill_path.stat().st_blocks * 512, available=0)
            return value

        self.patch(storage, "space", side_effect=measured_space)

        def getxattr(path, key):
            if path.name == ".mergerfs":
                return self.runtime_options[key.removeprefix("user.mergerfs.")].encode()
            physical = self.base / "mnt/data2/data" / path.name
            return {"user.mergerfs.fullpath": str(physical), "user.mergerfs.basepath": str(physical.parent),
                    "user.mergerfs.allpaths": str(physical)}[key].encode()

        self.patch(storage.os, "getxattr", side_effect=getxattr)
        real_open, real_write, real_fsync, real_unlink = storage.os.open, storage.os.write, storage.os.fsync, storage.os.unlink

        def open_file(path, flags, *args, **kwargs):
            path = Path(path)
            if path.name.startswith("enospc-") and path.suffix == ".bin":
                journal = json.loads((self.base / "state.json").read_text())
                self.assertEqual(journal["stage"], "enospc_filling")
                if path.parent == self.base / "union":
                    self.assertFalse(flags & (storage.os.O_CREAT | storage.os.O_TRUNC))
                    self.assertIn("owner", journal["enospc"])
                    return real_open(self.fill_path, flags, *args, **kwargs)
                self.assertTrue(flags & storage.os.O_EXCL)
                self.assertTrue(flags & storage.os.O_NOFOLLOW)
                self.fill_path = path
            return real_open(path, flags, *args, **kwargs)

        def is_fill(descriptor):
            return self.fill_path is not None and storage.os.readlink(f"/proc/self/fd/{descriptor}") == str(self.fill_path)

        def damage_context():
            if mutate_owner:
                self.fill_path.rename(self.fill_path.with_suffix(".saved"))
                self.fill_path.write_bytes(b"obcy plik")
            if lose_mount:
                self.observation["mounts"] = [mount for mount in self.observation["mounts"]
                                              if mount["target"] != str(self.base / "mnt/data2")]
            if wrong_options:
                self.runtime_options["moveonenospc"] = "true"

        def write_file(descriptor, payload):
            self.assertTrue(is_fill(descriptor))
            self.fill_writes.append(bytes(payload))
            if len(self.fill_writes) == 1:
                return real_write(descriptor, payload[:17])
            if failure == "noop":
                if len(self.fill_writes) == 2:
                    return len(payload)
            elif len(self.fill_writes) == 2 or failure == "max":
                return real_write(descriptor, payload)
            else:
                real_write(descriptor, payload[:13])
            self.fill_error = True
            damage_context()
            code = storage.errno.EIO if failure == "eio" else storage.errno.ENOSPC
            raise OSError(code, "testowa granica zapisu")

        def fsync_file(descriptor):
            if is_fill(descriptor) and failure == "fsync" and len(self.fill_writes) >= 2:
                self.fill_error = True
                raise OSError(storage.errno.ENOSPC, "testowa granica fsync")
            return real_fsync(descriptor)

        def unlink_file(path, *args, **kwargs):
            path = Path(path)
            if path.parent == self.base / "union" and path.name.startswith("enospc-"):
                return real_unlink(self.fill_path, *args, **kwargs)
            return real_unlink(path, *args, **kwargs)

        self.patch(storage.os, "open", side_effect=open_file)
        self.patch(storage.os, "write", side_effect=write_file)
        self.patch(storage.os, "fsync", side_effect=fsync_file)
        self.patch(storage.os, "unlink", side_effect=unlink_file)
        self.patch(storage, "command", side_effect=lambda args, expected=0:
                   self.calls.append((args, expected)) or "100% completed, 138 MB accessed\nEverything OK\n")

    def test_enospc_preflight_is_read_only_and_reports_real_profile(self):
        self.enospc_fixture()
        self.main("enospc-preflight")
        self.assertEqual((self.base / "state.json").read_bytes(), self.before_enospc)
        self.assertIsNone(self.fill_path)
        self.assertEqual(self.calls, [])
        self.assertIn('"cache.files": "libfuse"', storage.sys.stderr.getvalue())

    def test_enospc_main_accepts_partial_unreported_write_then_cleans_only_owned_file(self):
        self.enospc_fixture()
        self.main("enospc")
        state = json.loads((self.base / "state.json").read_text())
        entry = state["enospc"]
        self.assertEqual(entry["result"]["accepted_bytes"], 4113)
        self.assertEqual(entry["result"]["visible_bytes"], 4126)
        self.assertGreaterEqual(entry["result"]["allocated_bytes"], 4096)
        self.assertEqual(entry["result"]["space"]["data2"]["free"], 16 * 1024**2)
        self.assertEqual(entry["measurement"]["space"]["data2"]["free"]
                         - entry["result"]["space"]["data2"]["free"], entry["result"]["allocated_bytes"])
        self.assertEqual(self.fill_writes[1][0], 17)
        self.assertTrue(entry["cleaned"] and entry["completed"])
        self.assertFalse(self.fill_path.exists())
        self.assertEqual(state["baseline"], json.loads(self.before_enospc)["baseline"])
        self.assertEqual((self.base / "original.json").read_bytes(), self.original_manifest)
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])
        self.calls.clear()
        for phase in ("enospc", "enospc-preflight", "verify"):
            with self.assertRaises(RuntimeError):
                self.main(phase)
        self.assertEqual(self.calls, [])
        self.observation["boot_id"] = str(uuid.uuid4())
        self.main("verify")
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])

    def test_enospc_fsync_error_is_distinguished_from_write_error(self):
        self.enospc_fixture(failure="fsync")
        self.main("enospc")
        result = json.loads((self.base / "state.json").read_text())["enospc"]["result"]
        self.assertEqual(result["operation"], "fsync")
        self.assertEqual(result["errno"], storage.errno.ENOSPC)
        self.assertEqual(result["visible_bytes"], 4113)

    def test_enospc_runtime_options_refuse_before_intent_or_create(self):
        self.enospc_fixture()
        original = self.runtime_options.copy()
        for key in original:
            with self.subTest(key=key):
                self.runtime_options = {**original, key: "obca-wartosc"}
                with self.assertRaisesRegex(RuntimeError, "runtime opcje"):
                    self.main("enospc")
                self.assertEqual((self.base / "state.json").read_bytes(), self.before_enospc)
                self.assertIsNone(self.fill_path)

    def test_enospc_wrong_errno_cleans_owned_file_but_never_completes_or_retries(self):
        self.enospc_fixture(failure="eio")
        with self.assertRaisesRegex(RuntimeError, "errno=5"):
            self.main("enospc")
        self.assertFalse(self.fill_path.exists())
        state = json.loads((self.base / "state.json").read_text())
        self.assertEqual(state["stage"], "enospc_filling")
        self.assertNotIn("completed", state["enospc"])
        with self.assertRaisesRegex(RuntimeError, "ponawiana"):
            self.main("enospc")
        self.assertEqual(self.calls, [])

    def test_enospc_noop_write_does_not_pass_physical_allocation_proof(self):
        self.enospc_fixture(failure="noop")
        with self.assertRaisesRegex(RuntimeError, "alokacji"):
            self.main("enospc")
        self.assertFalse(self.fill_path.exists())
        self.assertEqual(self.calls, [])

    def test_enospc_replaced_inode_is_not_unlinked(self):
        self.enospc_fixture(mutate_owner=True)
        with self.assertRaisesRegex(RuntimeError, "właściciel"):
            self.main("enospc")
        self.assertEqual(self.fill_path.read_bytes(), b"obcy plik")
        self.assertTrue(self.fill_path.with_suffix(".saved").exists())
        self.assertEqual(self.calls, [])

    def test_enospc_lost_mount_refuses_cleanup_and_check(self):
        self.enospc_fixture(lose_mount=True)
        with self.assertRaisesRegex(RuntimeError, "mountu"):
            self.main("enospc")
        self.assertTrue(self.fill_path.exists())
        self.assertEqual(self.calls, [])

    def test_enospc_changed_runtime_options_refuse_result_and_cleanup(self):
        self.enospc_fixture(wrong_options=True)
        with self.assertRaisesRegex(RuntimeError, "runtime opcje"):
            self.main("enospc")
        self.assertTrue(self.fill_path.exists())
        self.assertEqual(self.calls, [])

    def test_enospc_check_failure_after_cleanup_keeps_pending_checkpoint(self):
        self.enospc_fixture()
        self.patch(storage, "command", side_effect=lambda args, expected=0:
                   self.calls.append((args, expected)) or "99% completed, 137 MB accessed\nEverything OK\n")
        with self.assertRaisesRegex(RuntimeError, "Niepełny check"):
            self.main("enospc")
        state = json.loads((self.base / "state.json").read_text())
        self.assertTrue(state["enospc"]["cleaned"])
        self.assertFalse(self.fill_path.exists())
        self.assertEqual(state["stage"], "enospc_filling")
        self.assertNotIn("completed", state["enospc"])
        self.assertEqual(state["boot_id"], json.loads(self.before_enospc)["boot_id"])
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])
        self.calls.clear()
        with self.assertRaises(RuntimeError):
            self.main("enospc")
        self.assertEqual(self.calls, [])

    def test_enospc_unbalanced_allocation_refuses_after_owned_cleanup(self):
        self.enospc_fixture()
        measured_space = storage.space.side_effect

        def invalid_space(path):
            result = measured_space(path)
            if path == self.base / "mnt/data2" and self.fill_error and self.fill_path.exists():
                result["free"] -= 2 * storage.ENOSPC_CHUNK_BYTES + 1
            return result

        self.patch(storage, "space", side_effect=invalid_space)
        with self.assertRaisesRegex(RuntimeError, "wyczerpania miejsca"):
            self.main("enospc")
        self.assertFalse(self.fill_path.exists())
        self.assertEqual(self.calls, [])
        state = json.loads((self.base / "state.json").read_text())
        self.assertNotIn("completed", state["enospc"])

    def test_enospc_positive_available_refuses_before_check(self):
        self.enospc_fixture()
        measured_space = storage.space.side_effect

        def available_space(path):
            result = measured_space(path)
            if path == self.base / "mnt/data2" and self.fill_error and self.fill_path.exists():
                result["available"] = 1
            return result

        self.patch(storage, "space", side_effect=available_space)
        with self.assertRaisesRegex(RuntimeError, "wyczerpania miejsca"):
            self.main("enospc")
        self.assertFalse(self.fill_path.exists())
        self.assertEqual(self.calls, [])

    def test_exercise_repeat_and_pending_journal_rejected_through_main(self):
        self.main("prepare")
        state = json.loads((self.base / "state.json").read_text())
        state["stage"] = "exercising"
        storage.save(self.base, state)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "ponawiana"):
            self.main("exercise")
        self.assertEqual(self.calls, [])

    def test_verify_requires_real_new_boot_and_never_formats(self):
        self.main("prepare")
        state = json.loads((self.base / "state.json").read_text())
        state.update(stage="exercised", boot_id=self.observation["boot_id"])
        storage.save(self.base, state)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "zrestartowany"):
            self.main("verify")
        self.assert_no_format()

    def test_verify_checks_original_hash_and_baseline_without_sync(self):
        self.main("prepare")
        state = json.loads((self.base / "state.json").read_text())
        original = self.base / "original.json"
        original.write_text("{}")
        state.update(stage="exercised", boot_id="old-boot", original_sha256=storage.digest(original),
                     baseline={"snapraid.conf": storage.digest(self.base / "snapraid.conf")})
        storage.save(self.base, state)
        self.calls.clear()
        self.main("verify")
        self.assert_no_format()
        self.assertEqual([args[-1] for args, _ in self.calls], ["check"])
        (self.base / "snapraid.conf").write_text("zmieniony")
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "przetrwały"):
            self.main("verify")
        self.assertEqual(self.calls, [])


class ScrubAndCommandTests(unittest.TestCase):
    def test_enospc_retrospective_real_measurements_require_available_zero_and_allocation_balance(self):
        before = {"free": 953290752, "available": 882827264}
        after = {"free": 16777216, "available": 0}
        allocated = 936513536
        self.assertEqual(before["free"] - after["free"], allocated)
        self.assertTrue(storage.enospc_allocation(before, after, allocated))
        self.assertFalse(storage.enospc_allocation(before, {**after, "available": 1}, allocated))
        for offset in (-1, 1):
            self.assertFalse(storage.enospc_allocation(before, after,
                             allocated + offset * (2 * storage.ENOSPC_CHUNK_BYTES + 1)))

    def test_fill_caps_actual_writes_and_fsync_without_claiming_enospc(self):
        with tempfile.TemporaryFile() as stream, patch.object(storage, "ENOSPC_MAX_BYTES", 8193), \
                patch.object(storage, "ENOSPC_CHUNK_BYTES", 4096), \
                patch.object(storage, "space", return_value={"available": 2 * 1024**3}), \
                patch.object(storage.os, "write", wraps=storage.os.write) as writes, \
                patch.object(storage.os, "fsync", wraps=storage.os.fsync) as syncs:
            with self.assertRaisesRegex(RuntimeError, "maxbytes bez ENOSPC"):
                storage.fill_file(stream.fileno(), Path("/unused"))
            self.assertEqual([len(call.args[1]) for call in writes.call_args_list], [4096, 4096, 1])
            self.assertEqual(syncs.call_count, 3)
            stream.seek(0)
            self.assertEqual(stream.read(), (bytes(range(256)) * 33)[:8193])

    def test_fill_time_and_os_reserve_stop_before_next_actual_write(self):
        for reason in ("time", "space"):
            with self.subTest(reason=reason), tempfile.TemporaryFile() as stream, \
                    patch.object(storage, "ENOSPC_CHUNK_BYTES", 4096), \
                    patch.object(storage.time, "monotonic", side_effect=[0, 0, 301] if reason == "time" else [0, 0, 0]), \
                    patch.object(storage, "space", side_effect=[{"available": 2 * 1024**3}, {"available": 1024**3 - 1}]), \
                    patch.object(storage.os, "write", wraps=storage.os.write) as writes, \
                    patch.object(storage.os, "fsync", wraps=storage.os.fsync) as syncs:
                with self.assertRaisesRegex(RuntimeError, "czas fill" if reason == "time" else "rezerwy miejsca OS"):
                    storage.fill_file(stream.fileno(), Path("/unused"))
                self.assertEqual(writes.call_count, 1)
                self.assertEqual(syncs.call_count, 1)
                self.assertEqual(storage.os.fstat(stream.fileno()).st_size, 4096)

    def test_fill_rejects_zero_write_and_edquot_without_fsync(self):
        for result in (0, OSError(storage.errno.EDQUOT, "limit kwoty")):
            with self.subTest(result=result), tempfile.TemporaryFile() as stream, \
                    patch.object(storage, "space", return_value={"available": 2 * 1024**3}), \
                    patch.object(storage.os, "write", **({"side_effect": result} if isinstance(result, OSError)
                                                        else {"return_value": result})) as writes, \
                    patch.object(storage.os, "fsync") as syncs:
                with self.assertRaisesRegex(RuntimeError, "errno=122" if isinstance(result, OSError) else "częściowy zapis"):
                    storage.fill_file(stream.fileno(), Path("/unused"))
                self.assertEqual(writes.call_count, 1)
                syncs.assert_not_called()
                self.assertEqual(storage.os.fstat(stream.fileno()).st_size, 0)

    def test_disk_recovery_requires_all_256_read_fixed_pairs_and_no_conflicts(self):
        original = {"restore.bin": {"role": "data2", "bytes": 64 * 1024**2}}
        output = "100% completed, 135 MB accessed\nEverything OK\n"
        lines = ["blocksize:262144"]
        for position in range(256):
            block = (position + 7) % 256
            lines.extend((f"error:{block}:d2:restore.bin: Read error at position {position}",
                          f"fixed:{block}:d2:restore.bin: Fixed data error at position {position}"))
        lines.extend(("status:recovered:d2:restore.bin", "summary:error:256", "summary:error_recovered:256",
                      "summary:error_unrecoverable:0", "summary:exit:recovered"))
        log = "\n".join(lines) + "\n"
        self.assertEqual(storage.recovered_disk(output, log, original, 268), 256)
        variants = [(output.replace("100%", "99%"), log), ("Everything OK\n", log)]
        for old, new in (("blocksize:262144", "blocksize:131072"),
                         ("Read error at position 0", "Open error at position 0"),
                         ("Read error at position 0", "Data error at position 0, diff bits 8/128"),
                         ("position 255", "position 0"), ("error:7:", "error:268:"),
                         ("fixed:7:", "fixed:8:"), ("d2:restore.bin", "d1:restore.bin"),
                         ("restore.bin", "foreign.bin"), ("summary:error:256", "summary:error:0"),
                         ("summary:error_recovered:256", "summary:error_recovered:255"),
                         ("summary:error_unrecoverable:0", "summary:error_unrecoverable:1"),
                         ("summary:exit:recovered\n", ""), (lines[1] + "\n", ""),
                         (lines[2] + "\n", "")):
            variants.append((output, log.replace(old, new)))
        for extra in (lines[1], lines[2], "status:unrecoverable:d2:restore.bin", "status:recovered:d1:other.bin",
                      "parity_fixed:0:parity", "unrecoverable:0:d2:restore.bin", "summary:error:256"):
            variants.append((output, log + extra + "\n"))
        for text, tags in variants:
            with self.subTest(text=text, tags=tags[-180:]), self.assertRaises(RuntimeError):
                storage.recovered_disk(text, tags, original, 268)

    def test_flip_real_byte_preserves_length_inode_and_nanosecond_mtime(self):
        with tempfile.TemporaryDirectory(prefix="tentanas-byte-unit-") as directory:
            path = Path(directory) / "restore.bin"
            original = b"\x52\x91\xff\x00oryginalny korpus"
            path.write_bytes(original)
            storage.os.utime(path, ns=(1700000000123456789, 1700000000987654321))
            expected_sha = storage.digest(path)
            before = path.stat()
            storage.flip_byte(path, expected_sha, before)
            after = path.stat()
            self.assertEqual(path.read_bytes(), b"\x53" + original[1:])
            self.assertEqual(after.st_size, len(original))
            self.assertEqual(after.st_mtime_ns, 1700000000987654321)
            self.assertEqual(after.st_ino, before.st_ino)
            self.assertEqual(after.st_dev, before.st_dev)
            self.assertNotEqual(storage.digest(path), expected_sha)

    def test_flip_rejects_wrong_sha_and_changed_identity_without_write(self):
        with tempfile.TemporaryDirectory(prefix="tentanas-byte-unit-") as directory:
            path = Path(directory) / "restore.bin"
            path.write_bytes(b"pierwotny plik")
            before = path.stat()
            with patch.object(storage.os, "pwrite") as write:
                with self.assertRaisesRegex(RuntimeError, "SHA"):
                    storage.flip_byte(path, "obcy SHA", before)
                path.write_bytes(b"inny rozmiar")
                with self.assertRaisesRegex(RuntimeError, "tożsamość"):
                    storage.flip_byte(path, storage.digest(path), before)
                write.assert_not_called()

    def test_flip_rejects_links_and_replaced_inode_without_write(self):
        for variant in ("symlink", "hardlink", "inode"):
            with self.subTest(variant=variant), tempfile.TemporaryDirectory(prefix="tentanas-byte-unit-") as directory:
                path = Path(directory) / "restore.bin"
                path.write_bytes(b"pierwotny plik")
                expected_sha, before = storage.digest(path), path.stat()
                other = Path(directory) / "other.bin"
                if variant == "hardlink":
                    storage.os.link(path, other)
                else:
                    path.rename(other)
                    if variant == "symlink":
                        path.symlink_to(other)
                    else:
                        path.write_bytes(other.read_bytes())
                        storage.os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
                        self.assertNotEqual(path.stat().st_ino, before.st_ino)
                with patch.object(storage.os, "pwrite") as write:
                    with self.assertRaises((RuntimeError, OSError)):
                        storage.flip_byte(path, expected_sha, before)
                    write.assert_not_called()
                self.assertEqual(other.read_bytes(), b"pierwotny plik")

    def test_corrupt_scrub_requires_exact_single_data_error_and_complete_work(self):
        output = "100% completed, 131 MB accessed\n"
        error = "error:0:d2:restore.bin: Data error at position 0, diff bits 64/128\n"
        log = ("block_count:268\n" + error + "summary:error_file:0\nsummary:error_io:0\n"
               "summary:error_data:1\nsummary:exit:error\n")
        self.assertEqual(storage.corruption_block(output, log, "d2"), 0)
        variants = [(output, log + error), (output, log + "parity_error:0:parity\n"),
                    (output + "Everything OK\n", log), (output.replace("100%", "99%"), log)]
        for old, new in (("block_count:268", "block_count:0"), ("error:0:", "error:268:"),
                         ("d2:restore.bin", "d1:restore.bin"), ("restore.bin", "other.bin"),
                         ("position 0", "position 1"), ("64/128", "0/128"), ("64/128", "129/128"),
                         ("error_file:0", "error_file:1"), ("error_io:0", "error_io:1"),
                         ("error_data:1", "error_data:2"), ("summary:exit:error\n", "")):
            variants.append((output, log.replace(old, new)))
        for text, tags in variants:
            with self.subTest(text=text, tags=tags), self.assertRaises(RuntimeError):
                storage.corruption_block(text, tags, "d2")

    def test_fix_requires_exact_recovery_without_conflicting_actions(self):
        error = "error:0:d2:restore.bin: Data error at position 0, diff bits 64/128\n"
        fixed = "fixed:0:d2:restore.bin: Fixed data error at position 0\n"
        log = (error + fixed + "summary:error:1\nsummary:error_recovered:1\n"
               "summary:error_unrecoverable:0\nsummary:exit:recovered\n")
        storage.recovered_block(log, "d2", 0)
        variants = [log + fixed, log + error, log.replace(fixed, ""), log.replace(error, "")]
        for tag in ("parity_fixed:0:parity", "unrecoverable:0:d2:restore.bin",
                    "parity_error:0:parity", "error:0:d1:other.bin: Open error"):
            variants.append(log + tag + "\n")
        for old, new in (("fixed:0:", "fixed:1:"), ("d2:restore.bin", "d1:restore.bin"),
                         ("restore.bin", "foreign.bin"), ("position 0", "position 1"),
                         ("64/128", "0/128"), ("summary:error:1", "summary:error:0"),
                         ("error_recovered:1", "error_recovered:0"),
                         ("error_unrecoverable:0", "error_unrecoverable:1"),
                         ("summary:exit:recovered\n", "")):
            variants.append(log.replace(old, new))
        for tags in variants:
            with self.subTest(tags=tags), self.assertRaises(RuntimeError):
                storage.recovered_block(tags, "d2", 0)

    def test_filesystem_probe_accepts_only_complete_identity_or_exact_empty_exit(self):
        device = Path("/dev/vdb")
        identity = str(uuid.uuid4())
        outputs = [(0, f"DEVNAME={device}\nTYPE=ext4\nUUID={identity}\n", "", ("ext4", identity)),
                   (0, f"DEVNAME={device}\nPTTYPE=gpt\nPTUUID={identity}\n", "", (None, None)),
                   (2, "", "", (None, None))]
        for code, output, error, expected in outputs:
            with self.subTest(code=code, output=output), \
                    patch.object(storage.subprocess, "run", return_value=subprocess.CompletedProcess([], code, output, error)) as run, \
                    patch.object(storage.sys, "stderr", io.StringIO()):
                self.assertEqual(storage.filesystem(device), expected)
                self.assertEqual(run.call_args.args, (["/usr/sbin/blkid", "-p", "-o", "export", str(device)],))
                self.assertTrue(run.call_args.kwargs["capture_output"])
                self.assertTrue(run.call_args.kwargs["text"])
                self.assertEqual(run.call_args.kwargs["timeout"], 300)
        invalid = [(2, "\n", ""), (2, "", "error"), (8, "", ""), (4, "", ""), (0, "", ""),
                   (0, f"DEVNAME={device}\nTYPE=ext4\nUUID={identity}\n", "warning")]
        for tail in ("TYPE=ext4\n", f"UUID={identity}\n", "PTTYPE=gpt\n",
                     f"TYPE=ext4\nUUID={identity}\nTYPE=xfs\n", f"TYPE=ext4\nUUID={identity}\nUUID={identity}\n",
                     "TYPE=\nUUID=\n", "TYPE ext4\n", "UNKNOWN=value\n"):
            invalid.append((0, f"DEVNAME={device}\n" + tail, ""))
        invalid.append((0, f"DEVNAME=/dev/foreign\nTYPE=ext4\nUUID={identity}\n", ""))
        for code, output, error in invalid:
            with self.subTest(code=code, output=output, error=error), \
                    patch.object(storage.subprocess, "run", return_value=subprocess.CompletedProcess([], code, output, error)), \
                    patch.object(storage.sys, "stderr", io.StringIO()), self.assertRaises(RuntimeError):
                storage.filesystem(device)

    def test_scrub_requires_positive_complete_error_free_result(self):
        output = "100% completed, 131 MB accessed in 0:00\nEverything OK\n"
        log = "block_count:268\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n"
        self.assertEqual(storage.scrub_blocks(output, log), 268)
        variants = [(output, log.replace("268", "0")), (output.replace("100%", "99%"), log),
                    (output.replace("100%", "1100%"), log), ("Everything OK\n", log),
                    (output, log.replace("summary:exit:ok\n", "")),
                    (output, log.replace("error_data:0", "error_data:1")),
                    (output, log.replace("summary:exit:ok", "summary:exit:error"))]
        for text, tags in variants:
            with self.subTest(text=text, tags=tags), self.assertRaises(RuntimeError):
                storage.scrub_blocks(text, tags)

    def test_command_accepts_only_explicit_diff_two_and_rejects_signal(self):
        with patch.object(storage.subprocess, "run", return_value=subprocess.CompletedProcess(["snapraid"], 2, "diff", "")), \
                patch.object(storage.sys, "stderr", io.StringIO()):
            self.assertEqual(storage.command(["snapraid", "diff"], 2), "diff")
            with self.assertRaises(RuntimeError):
                storage.command(["snapraid", "sync"])
        with patch.object(storage.subprocess, "run", return_value=subprocess.CompletedProcess(["snapraid"], -11, "", "")), \
                patch.object(storage.sys, "stderr", io.StringIO()), self.assertRaises(RuntimeError):
            storage.command(["snapraid", "status"])

    def test_corrupt_scrub_command_requires_rc_one_and_rejects_timeout(self):
        for code in (0, 1, 2, -11):
            with self.subTest(code=code), patch.object(storage.subprocess, "run", return_value=
                    subprocess.CompletedProcess(["snapraid"], code, "wynik", "")), \
                    patch.object(storage.sys, "stderr", io.StringIO()):
                if code == 1:
                    self.assertEqual(storage.command(["snapraid", "scrub"], 1), "wynik")
                else:
                    with self.assertRaises(RuntimeError):
                        storage.command(["snapraid", "scrub"], 1)
        with patch.object(storage.subprocess, "run", side_effect=subprocess.TimeoutExpired("snapraid", 300)), \
                patch.object(storage.sys, "stderr", io.StringIO()), self.assertRaises(subprocess.TimeoutExpired):
            storage.command(["snapraid", "scrub"], 1)


if __name__ == "__main__":
    unittest.main()
