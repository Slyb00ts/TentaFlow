#!/usr/bin/env python3
# =============================================================================
# Plik: tests/infra/tentanas-vm/vm.py
# Opis: Prywatna maszyna QEMU/KVM do operacyjnych testów TentaNas.
# Przykład: python3 vm.py create; python3 vm.py start /mnt/d/repos/tentanas-vm.ABC123
# =============================================================================

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import socket
import stat
import subprocess
import sys
import time
import uuid
import signal


QEMU = "/usr/bin/qemu-system-x86_64"
QEMU_IMG = "/usr/bin/qemu-img"
MACHINE = "q35,accel=kvm,smm=off"
IMAGE_URL = "https://cloud.debian.org/images/cloud/trixie/20260831-2587/debian-13-generic-amd64-20260831-2587.qcow2"
IMAGE_SHA512 = "5a069019420fb9441ad4f8004c661fadb747edd5662ca54a17c8f923dee7d717e21dbdaa4ba72d6fce7f920e0217f0a9af382298a7d46ed4bc9dc33ac19181b6"
GIB = 1024 ** 3
DISK_PROFILES = {
    "storage": {"os": 12, "data1": 1, "data2": 1, "parity": 2, "cache": 1, "spare": 1},
    "e2": {"os": 12, "data1": 32, "data2": 32, "parity": 40, "cache": 1, "spare": 40},
    "e2-cache": {"os": 12, "data1": 32, "data2": 32, "parity": 40, "cache": 32, "spare": 40},
    # iSCSI/NVMe-oF probes back their LUNs with loop files in /var/tmp on the OS disk, so the
    # profile carries no data role that a storage phase or an operator could format by mistake.
    "block": {"os": 12},
}
API_PROFILES = ("e2", "e2-cache")
FAULT_PROFILES = ("e2-cache",)
FAULT_ROLE = "cache"
FAULT_CONFIG = "blkdebug-cache.cfg"
POWERCUT_PROFILES = ("e2-cache",)
POWERCUT_ROLE = "cache"
POWERCUT_LOG = "cache-writelog.img"
# Receipt c2_replay.py leaves in the runtime around the write-back, and the facts of it that the
# disarming entry keeps for good. Without them a skipped or interrupted reconstruction would be
# indistinguishable from a finished one, and the next boot would use the crashed image.
POWERCUT_REPLAY = "powercut-replay.json"
POWERCUT_BASELINE = "cache-baseline.qcow2"
# Append-only journal of cut marks. Owner decision 2026-09-12: `blockstats --record-cut-mark` is the
# one shared-lock command allowed to write the runtime, because at the cut instant the mover's ssh
# session holds the shared lock and an exclusive command could not run at all.
POWERCUT_MARKS = "powercut-marks.jsonl"
# Append-only journal of reconstructions, written by c2_replay.py --apply before it touches the
# image. A restored image hashes exactly like one that was never reconstructed, so the image pin
# alone cannot tell them apart; this journal says that a reconstruction happened at all.
POWERCUT_REPLAYS = "powercut-replays.jsonl"
# Why a cut was released, and whether the case survives it. Only the plain reason leaves the runtime
# usable; both forfeiting reasons mean the image no longer stands for anything the case can cite.
DISCARD_DONE = "odtworzenie odrzucone: sprawa unieważniona"
DISCARD_REWRITTEN = "odtworzenie zapisane w dzienniku: sprawa unieważniona"
DISCARD_PLAIN = "porzucone bez odtworzenia obrazu"
DISCARD_REASONS = (DISCARD_DONE, DISCARD_REWRITTEN, DISCARD_PLAIN)
REPLAY_SCHEMA = 2
REPLAY_COUNTERS = ("kept", "dropped_before_limit", "dropped_after_limit")
REPLAY_FIELDS = ("limit_bytes", "cut_mark_bytes", "cut_entry_index", "no_durable_point",
                 "torn_tail_bytes",
                 "baseline_sha256", "image_sha256_before", "image_sha256_after") + REPLAY_COUNTERS
# dm-log-writes sector unit: the log addresses the disk in log-sector-size units, so the pinned 512
# keeps one log sector equal to one disk sector and the replayer needs no rescaling.
POWERCUT_LOG_SECTOR_SIZE = 512
# Single source of the lock class: every subcommand is exclusive unless it is named here, so a new
# subcommand cannot silently become shared. Shared commands never write the runtime directory.
SHARED_COMMANDS = ("ssh", "status", "blockstats", "reset")
RUNTIME_COMMANDS = ("start", "stop", "status", "inventory", "ssh", "bootstrap-packages",
                    "install-packages", "storage", "detach-data2", "reset", "blockstats", "fault",
                    "powercut")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def private_file(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
            and info.st_nlink == 1 and not info.st_mode & 0o077,
            f"Plik nie jest prywatnym zwykłym plikiem właściciela: {path}")
    return info


def runtime_path(value):
    path = Path(value)
    require(re.fullmatch(r"/mnt/d/repos/tentanas-vm\.[A-Za-z0-9]{6}", value)
            and str(path.resolve()) == value, "Niedozwolona ścieżka runtime")
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid()
            and stat.S_IMODE(info.st_mode) == 0o700,
            "Runtime musi być własnym katalogiem mode 700 bez symlinków")
    return path


def write_new(path, text):
    with path.open("x") as stream:
        stream.write(text)
    path.chmod(0o600)


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def durable_text(path, text):
    write_new(path, text)
    with path.open("rb") as stream:
        os.fsync(stream.fileno())
    sync_directory(path.parent)


def durable_new(path, value):
    durable_text(path, json.dumps(value, indent=2) + "\n")


def durable_replace(path, name, text):
    temp = path / Path(name).with_suffix(".next").name
    if temp.exists() or temp.is_symlink():
        private_file(temp)
        temp.unlink()
    durable_text(temp, text)
    temp.replace(path / name)
    sync_directory(path)


def transition(state, **fields):
    for key in ("retirement", "fault", "fault_history", "powercut", "powercut_history"):
        if key in state:
            fields[key] = state[key]
    return fields


def save_state(path, state):
    if (path / "state.json").exists():
        previous = read_state(path)
        if "retirement" in previous:
            require(state.get("retirement") == previous["retirement"],
                    "Zmiana retirement poza zamkniętym odłączeniem")
        require(state.get("fault") == previous.get("fault")
                and state.get("fault_history") == previous.get("fault_history"),
                "Zmiana profilu błędów poza podkomendą fault")
        require(state.get("powercut") == previous.get("powercut")
                and state.get("powercut_history") == previous.get("powercut_history"),
                "Zmiana odcięcia zasilania poza podkomendą powercut")
    persist_state(path, state)


def persist_state(path, state):
    durable_replace(path, "state.json", json.dumps(state, indent=2) + "\n")


def retirement(path, manifest, state, allow_pending=False):
    records = []
    for name in ("detach-intent.json", "retired-data2.json"):
        target = path / name
        if target.exists() or target.is_symlink():
            private_file(target)
            records.append(json.loads(target.read_text()))
        else:
            records.append(None)
    intent, receipt = records
    record = state.get("retirement")
    if intent is None and receipt is None and record is None:
        return None
    require(isinstance(intent, dict) and isinstance(record, dict)
            and record.get("intent") == intent, "Niekompletny intent odłączenia; wymagana diagnostyka")
    require(intent.get("schema") == 1 and intent.get("uuid") == manifest["uuid"]
            and intent.get("source") == "data2" and intent.get("target") == "spare"
            and intent.get("disks") == {role: manifest["disks"][role] for role in ("data2", "spare")}
            and intent.get("image_inode") == manifest["image_inodes"]["data2"],
            "Obca tożsamość intent odłączenia")
    require(str(uuid.UUID(intent["operation_id"])) == intent["operation_id"], "Niepoprawny replacement_id")
    process = intent["process"]
    require(type(process["pid"]) is int and process["pid"] > 1
            and process["executable"] == str(Path(QEMU).resolve())
            and process["argv"] == qemu_command(path, manifest), "Obcy pierwotny proces odłączenia")
    if record.get("phase") == "intent":
        require(receipt is None and allow_pending, "Odłączenie nieukończone; nowy start i ponowienie zabronione")
        if state["status"] == "running":
            require(state.get("process") == process, "Pending intent nie dotyczy aktualnego procesu")
        else:
            require(state["status"] == "stopped" and state.get("previous_process") == process,
                    "Nieznany stan procesu pending intent")
        return record
    require(record.get("phase") == "detached" and record.get("profile") == "data2_absent"
            and receipt == record and record.get("stopped_process") == process,
            "Niekompletne potwierdzenie wycofania data2")
    arm = record["arm"]
    validate_arm(manifest, intent["operation_id"], arm)
    require(record.get("arm_sha256") == hashlib.sha256(json.dumps(arm, sort_keys=True).encode()).hexdigest()
            and re.fullmatch(r"[0-9a-f]{64}", record.get("image_sha256", "")),
            "Niezgodne SHA dowodu odłączenia")
    return record


def validate_arm(manifest, operation_id, arm):
    state = arm["state"]
    replacement = state["replacement"]
    require(arm.get("phase") == "replacement-arm" and arm.get("result") == "ok"
            and state.get("stage") == "replacement_armed"
            and state["contract"]["uuid"] == manifest["uuid"]
            and state["contract"]["disks"] == manifest["disks"]
            and state.get("format_count") == 3 and state.get("format_pending") is None
            and replacement.get("operation_id") == operation_id
            and replacement.get("missing") == "data2" and replacement.get("target") == "spare",
            "Niezgodne potwierdzenie replacement-arm")


def retired_image_hash(path, manifest):
    image = path / "data2.qcow2"
    info = private_file(image)
    require([info.st_dev, info.st_ino] == manifest["image_inodes"]["data2"], "Podmieniony wycofany data2")
    inspect_image(image, manifest["disks"]["data2"]["bytes"])
    with image.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def disk_manifest(vm_uuid, profile):
    prefix = uuid.UUID(vm_uuid).hex[:10]
    return {role: {"serial": f"tn-{prefix}-{role}", "bytes": size * GIB}
            for role, size in DISK_PROFILES[profile].items()}


def disk_profile(manifest):
    matches = [profile for profile in DISK_PROFILES
               if manifest["disks"] == disk_manifest(manifest["uuid"], profile)]
    require(len(matches) == 1
            and all(type(disk["bytes"]) is int for disk in manifest["disks"].values()),
            "Zmieniona mapa ról, seriali lub wielkości dysków")
    return matches[0]


def api_forward(manifest):
    profile = disk_profile(manifest)
    if profile not in API_PROFILES:
        require("api_port" not in manifest, f"Profil {profile} nie dopuszcza portu API")
        return ""
    port = manifest.get("api_port")
    require(type(port) is int and 1024 < port < 65536 and port != manifest["ssh_port"],
            "Niepoprawny port API profilu E2")
    return f",hostfwd=tcp:127.0.0.1:{port}-:8090"


def load_manifest(path):
    private_file(path / "manifest.json")
    manifest = json.loads((path / "manifest.json").read_text())
    require(manifest["schema"] == 1 and manifest["uid"] == os.getuid()
            and manifest["runtime"] == str(path), "Obcy manifest runtime")
    require(str(uuid.UUID(manifest["uuid"])) == manifest["uuid"], "Niepoprawny UUID")
    api_forward(manifest)
    require(manifest["image_url"] == IMAGE_URL and manifest["image_sha512"] == IMAGE_SHA512,
            "Manifest nie używa zatwierdzonego obrazu")
    require(manifest["machine"] == MACHINE, "Niezgodny profil maszyny")
    require(type(manifest["ssh_port"]) is int and 1024 < manifest["ssh_port"] < 65536,
            "Niepoprawny port SSH")
    return manifest


@contextlib.contextmanager
def locked_runtime(value, shared=False):
    path = runtime_path(value)
    private_file(path / "lock")
    with (path / "lock").open("r+") as lock:
        try:
            fcntl.flock(lock, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError("Runtime zajęty przez inne polecenie; odmowa bez czekania") from None
        yield path, load_manifest(path)


def file_sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inspect_image(path, expected_size=None):
    private_file(path)
    info = json.loads(run([QEMU_IMG, "info", "--output=json", "--backing-chain", str(path)],
                          capture_output=True).stdout)
    require(len(info) == 1, "Obraz ma niedozwolony łańcuch backing")
    image = info[0]
    specific = image.get("format-specific", {}).get("data", {})
    require(image.get("format") == "qcow2" and not image.get("backing-filename")
            and not image.get("full-backing-filename") and not specific.get("data-file"),
            "Dopuszczalny wyłącznie samodzielny QCOW2 bez external data file")
    if expected_size is not None:
        require(image["virtual-size"] == expected_size, "Niezgodna wielkość obrazu")


def verify_download(image):
    private_file(image)
    with image.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha512").hexdigest()
    require(digest == IMAGE_SHA512, "SHA512 obrazu niezgodne; brak konwersji i bootu")
    inspect_image(image)


def check_images(path, manifest):
    for role, disk in manifest["disks"].items():
        image = path / f"{role}.qcow2"
        info = private_file(image)
        require([info.st_dev, info.st_ino] == manifest["image_inodes"][role],
                f"Podmieniony plik obrazu {role}")
        inspect_image(image, disk["bytes"])
    private_file(path / "seed.iso")
    digest = hashlib.sha256((path / "seed.iso").read_bytes()).hexdigest()
    require(digest == manifest["seed_sha256"], "Podmieniony seed")


def fault_config(sectors):
    header = "# vm.py fault: prywatny profil błędów odczytu blkdebug; wyłącznie testy harnessu.\n"
    return header + "".join(
        '\n[inject-error]\nevent = "read_aio"\niotype = "read"\nerrno = "5"\n'
        f'sector = "{sector}"\nonce = "off"\nimmediately = "off"\n' for sector in sectors)


def validate_fault_entry(manifest, entry):
    require(set(entry) == {"schema", "enabled", "role", "sectors", "config_sha256"}
            and entry["schema"] == 1 and type(entry["enabled"]) is bool, "Obcy wpis profilu błędów")
    if not entry["enabled"]:
        require(entry["role"] is None and entry["sectors"] == [] and entry["config_sha256"] is None,
                "Wyłączony profil błędów z pozostałą regułą")
        return
    sectors = entry["sectors"]
    require(entry["role"] == FAULT_ROLE and entry["role"] in manifest["disks"],
            f"Profil błędów dopuszcza wyłącznie rolę {FAULT_ROLE}")
    # Structural check only: the sector must address the host qcow2 file, whose current length is
    # verified in fault() against the pinned image. History entries stay valid as that file grows.
    require(type(sectors) is list and sectors and len(set(sectors)) == len(sectors)
            and all(type(sector) is int and sector >= 0 for sector in sectors),
            "Niepoprawne sektory błędu odczytu")
    require(entry["config_sha256"] == hashlib.sha256(fault_config(sectors).encode()).hexdigest(),
            "Niezgodne SHA konfiguracji blkdebug")


def fault_record(path, manifest, state, pending=None):
    # `pending` is the entry the running fault() is about to write. It makes exactly the artefacts of
    # an interrupted identical command acceptable, so re-running that command finishes the write;
    # every other mismatch stays a refusal.
    receipt_path = path / "fault.json"
    history = state.get("fault_history", [])
    record = state.get("fault")
    accepted = [history] if pending is None else [history, history + [pending]]
    if not (receipt_path.exists() or receipt_path.is_symlink()):
        require(record is None and history == [], "Brak prywatnego dowodu profilu błędów")
        return None
    private_file(receipt_path)
    receipt = json.loads(receipt_path.read_text())
    require(receipt.get("schema") == 1 and receipt.get("uuid") == manifest["uuid"]
            and receipt.get("runtime") == str(path) and type(history) is list
            and receipt.get("history") in accepted, "Obcy dowód profilu błędów")
    for entry in history:
        validate_fault_entry(manifest, entry)
    require(record == (history[-1] if history and history[-1]["enabled"] else None),
            "Zapisany profil błędów niezgodny z historią")
    if record is None:
        return None
    require(disk_profile(manifest) in FAULT_PROFILES, "Profil błędów zapisany poza profilem cache")
    config = path / FAULT_CONFIG
    require(config.exists() or config.is_symlink(), "Brak przypiętej konfiguracji blkdebug")
    private_file(config)
    text = config.read_text()
    pinned = {record["config_sha256"]: fault_config(record["sectors"])}
    if pending is not None and pending["enabled"]:
        pinned[pending["config_sha256"]] = fault_config(pending["sectors"])
    require(pinned.get(hashlib.sha256(text.encode()).hexdigest()) == text,
            "Podmieniona konfiguracja blkdebug")
    return record


def digest_value(value):
    return type(value) is str and bool(re.fullmatch(r"[0-9a-f]{64}", value))


def validate_counters(value):
    require(type(value) is dict and set(value) == {"entries", "writes", "discards", "bytes"}
            and all(type(value[key]) is int and value[key] >= 0 for key in value),
            "Niepoprawne liczniki odtworzenia")


def validate_replay_summary(summary):
    require(type(summary) is dict and set(summary) == set(REPLAY_FIELDS), "Obcy dowód odtworzenia")
    # Entries and bytes, always separately and on both sides of the cut: the plan reads the loss as
    # both numbers, so a summary that carries only entry counts cannot stand as its evidence.
    for key in REPLAY_COUNTERS:
        validate_counters(summary[key])
    require(type(summary["no_durable_point"]) is bool
            and type(summary["torn_tail_bytes"]) is int and summary["torn_tail_bytes"] >= 0,
            "Niepoprawny opis granicy odtworzenia")
    # The limit is the recorded mark, never absent: an apply without one reconstructs the whole log,
    # which is the crashed image, and reports that nothing was lost.
    require(type(summary["limit_bytes"]) is int and summary["limit_bytes"] >= 0
            and summary["cut_mark_bytes"] == summary["limit_bytes"], "Odtworzenie bez znacznika odcięcia")
    require(summary["cut_entry_index"] is None or (type(summary["cut_entry_index"]) is int
                                                   and summary["cut_entry_index"] >= 0),
            "Niepoprawna granica odtworzenia")
    require(all(digest_value(summary[key]) for key in
                ("baseline_sha256", "image_sha256_before", "image_sha256_after")),
            "Niepoprawne SHA odtworzenia")


def validate_discard_summary(summary):
    require(type(summary) is dict and set(summary) == {"reason", "log_bytes", "log_sha256",
                                                       "replay_phase", "limit_bytes",
                                                       "cut_mark_bytes", "image_sha256",
                                                       "forfeited"},
            "Obcy dowód porzucenia")
    require(type(summary["reason"]) is str and summary["reason"]
            and type(summary["log_bytes"]) is int and summary["log_bytes"] > 0
            and digest_value(summary["log_sha256"]) and digest_value(summary["image_sha256"])
            and summary["replay_phase"] in (None, "pending", "done")
            and type(summary["forfeited"]) is bool,
            "Niepoprawny dowód porzucenia logu zapisów")
    require(all(summary[key] is None or (type(summary[key]) is int and summary[key] >= 0)
                for key in ("limit_bytes", "cut_mark_bytes")),
            "Niepoprawny znacznik w dowodzie porzucenia")
    # Discarding a reconstruction repudiates it: the image in the runtime is neither the crashed one
    # nor an accepted reconstruction, so the case has to be re-run on a rebuilt runtime. That holds
    # whether the receipt said `done` or the image alone shows it was rewritten.
    require(summary["reason"] in DISCARD_REASONS, "Nieznany powód porzucenia logu zapisów")
    require((summary["reason"] == DISCARD_DONE) == (summary["replay_phase"] == "done"),
            "Powód porzucenia niezgodny z fazą odtworzenia")
    require(summary["forfeited"] == (summary["reason"] != DISCARD_PLAIN),
            "Niezgodne unieważnienie sprawy w dowodzie porzucenia")


def validate_powercut_entry(manifest, entry, index):
    require(set(entry) == {"schema", "enabled", "role", "log", "log_inode", "log_sector_size",
                           "cut_mark_bytes", "image_sha256", "baseline", "baseline_bytes",
                           "log_bytes",
                           "log_sha256", "log_archive", "baseline_archive", "replay",
                           "replay_archive", "discarded"}
            and entry["schema"] == 1 and type(entry["enabled"]) is bool,
            "Obcy wpis odcięcia zasilania")
    archive, replay_archive, baseline_archive = powercut_archive(index)
    # The mark is the log length the host recorded at the cut instant. It is absent only between
    # arming and the cut itself; from then on every reconstruction must match it exactly.
    require(entry["cut_mark_bytes"] is None or (type(entry["cut_mark_bytes"]) is int and entry["cut_mark_bytes"] >= 0),
            "Niepoprawny znacznik odcięcia")
    require(digest_value(entry["image_sha256"]) and type(entry["baseline_bytes"]) is int
            and entry["baseline_bytes"] >= 0, "Niepoprawny pin obrazu bazowego")
    if not entry["enabled"]:
        require(entry["role"] is None and entry["log"] is None and entry["log_inode"] is None
                and entry["log_sector_size"] is None and entry["baseline"] is None
                and entry["baseline_archive"] == baseline_archive,
                "Rozbrojone odcięcie zasilania z pozostałym logiem")
        # The disarmed entry is the permanent evidence of the cut: size and SHA of the log, the name
        # it was archived under — carrying the entry's own position, so one entry cannot borrow
        # another cut's evidence — and how the log was accounted for.
        require(type(entry["log_bytes"]) is int and entry["log_bytes"] >= 0
                and digest_value(entry["log_sha256"]) and entry["log_archive"] == archive,
                "Niepoprawny dowód rozbrojonego logu zapisów")
        released = [value for value in (entry["replay"], entry["discarded"]) if value is not None]
        require(len(released) == (0 if entry["log_bytes"] == 0 else 1),
                "Niepusty log zapisów bez rozliczenia: wymagane odtworzenie albo porzucenie")
        # A boundary longer than the log it was taken on cannot describe that log. Truncating the
        # live log keeps its inode, so this length check is what catches the substitution.
        require(entry["cut_mark_bytes"] is None or entry["cut_mark_bytes"] <= entry["log_bytes"],
                "Znacznik odcięcia większy niż zwolniony log zapisów")
        if entry["replay"] is not None:
            validate_replay_summary(entry["replay"])
            require(entry["replay_archive"] == replay_archive,
                    "Niespójne archiwum dowodu odtworzenia")
        if entry["discarded"] is not None:
            validate_discard_summary(entry["discarded"])
        require(entry["replay_archive"] in (None, replay_archive),
                "Niespójne archiwum dowodu odtworzenia")
        return
    require(entry["role"] == POWERCUT_ROLE and entry["role"] in manifest["disks"],
            f"Odcięcie zasilania dopuszcza wyłącznie rolę {POWERCUT_ROLE}")
    # Baseline pin: the cache image as it stood when the log was armed, kept as a private copy in the
    # runtime. The reconstruction must start from exactly this file, not from anything the operator
    # happens to point at.
    require(entry["log"] == POWERCUT_LOG and entry["log_sector_size"] == POWERCUT_LOG_SECTOR_SIZE
            and entry["baseline"] == POWERCUT_BASELINE
            and entry["log_bytes"] is None and entry["log_sha256"] is None
            and entry["log_archive"] is None and entry["baseline_archive"] is None
            and entry["replay"] is None and entry["replay_archive"] is None
            and entry["discarded"] is None, "Niepoprawny opis logu zapisów")
    inode = entry["log_inode"]
    require(type(inode) is list and len(inode) == 2 and all(type(value) is int for value in inode),
            "Niepoprawny inode logu zapisów")


def powercut_archive(index):
    return (f"cache-writelog-{index}.img", f"powercut-replay-{index}.json",
            f"cache-baseline-{index}.qcow2")


def copy_private(source, target):
    """Private copy of an image: server-side (btrfs reflink) where the filesystem supports it."""
    descriptor = os.open(target, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    try:
        with source.open("rb") as origin:
            remaining = source.stat().st_size
            try:
                while remaining:
                    copied = os.copy_file_range(origin.fileno(), descriptor, remaining)
                    if not copied:
                        break
                    remaining -= copied
            except OSError:
                origin.seek(0)
                os.lseek(descriptor, 0, os.SEEK_SET)
                os.ftruncate(descriptor, 0)
                while chunk := origin.read(4 * 1024 * 1024):
                    os.write(descriptor, chunk)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    sync_directory(target.parent)


def powercut_marks(path, manifest):
    """Reads the append-only mark journal. Every record is validated, so a corrupt or foreign line
    refuses every later command instead of being skipped over."""
    target = path / POWERCUT_MARKS
    if not (target.exists() or target.is_symlink()):
        return []
    private_file(target)
    records = []
    lines = target.read_text().splitlines()
    for number, line in enumerate(lines, start=1):
        require(line.strip(), f"Pusty wiersz {number} w dzienniku znaczników odcięcia")
        record = torn_line(line, number, len(lines), "znaczników odcięcia")
        if record is None:
            break
        require(set(record) == {"schema", "uuid", "index", "log_bytes", "recorded_at"}
                and record["schema"] == 1 and record["uuid"] == manifest["uuid"]
                and type(record["index"]) is int and record["index"] >= 0
                and type(record["log_bytes"]) is int and record["log_bytes"] >= 0
                and type(record["recorded_at"]) in (int, float) and record["recorded_at"] > 0,
                f"Obcy wpis {number} w dzienniku znaczników odcięcia")
        # A later, smaller line is not a threat and must not refuse on read: the boundary is the
        # first record for the cut, so a duplicate measurement is documentation. The decreasing
        # check lives only where it can still prevent one, in the pre-append guard.
        records.append(record)
    return records


def torn_line(line, number, total, journal):
    """A hard crash mid-append leaves a torn last line. That is an append that never completed, so
    it is ignored — but only as the last line, and the refusal is named, never a raw JSON error."""
    try:
        return json.loads(line)
    except ValueError:
        require(number == total, f"Uszkodzony wiersz {number} w dzienniku {journal}")
        return None


def powercut_replays(path, manifest):
    """Reads the append-only journal of reconstructions, validated exactly like the mark journal."""
    target = path / POWERCUT_REPLAYS
    if not (target.exists() or target.is_symlink()):
        return []
    private_file(target)
    records = []
    lines = target.read_text().splitlines()
    for number, line in enumerate(lines, start=1):
        require(line.strip(), f"Pusty wiersz {number} w dzienniku odtworzeń")
        record = torn_line(line, number, len(lines), "odtworzeń")
        if record is None:
            break
        require(set(record) == {"schema", "uuid", "index", "log_sha256", "limit_bytes",
                                "image_sha256_before", "recorded_at"}
                and record["schema"] == 1 and record["uuid"] == manifest["uuid"]
                and type(record["index"]) is int and record["index"] >= 0
                and type(record["limit_bytes"]) is int and record["limit_bytes"] >= 0
                and digest_value(record["log_sha256"])
                and digest_value(record["image_sha256_before"])
                and type(record["recorded_at"]) in (int, float) and record["recorded_at"] > 0,
                f"Obcy wpis {number} w dzienniku odtworzeń")
        records.append(record)
    return records


def powercut_mark(path, manifest, state, mark):
    """Records the cut mark — the live log length at the cut instant — for the armed cut.

    Owner decision of 2026-09-12: this is the one narrow, documented exception to "shared commands
    never write the runtime". It is reached only from `blockstats --record-cut-mark`, because at the
    cut instant the mover's ssh session holds the shared lock and no exclusive command could run.

    The journal only ever grows: a repeated measurement is appended, never substituted, and the
    boundary stays the first value recorded for that cut — so the mark cannot be moved after the
    fact, and an attempt to move it stays visible in the journal."""
    armed = state.get("powercut")
    require(armed is not None, "Znacznik odcięcia wymaga uzbrojonego runtime")
    require(type(mark) is int and mark >= 0, "Niepoprawny znacznik odcięcia")
    log = path / armed["log"]
    require(private_file(log).st_size >= mark, "Znacznik wykracza poza log zapisów")
    index = len(state["powercut_history"]) - 1
    # `blockstats` is a shared-lock command by design, so two markers can run at once. Only
    # serialising read-guard-append keeps the journal ordered: without this, the process that
    # measured the larger log could commit first and the other append a smaller line behind it.
    # Own lock, taken after the runtime lock, never blocking — the same shape as the QMP monitor.
    descriptor = os.open(path / POWERCUT_MARKS, os.O_CREAT | os.O_WRONLY | os.O_APPEND, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError("Dziennik znaczników zajęty przez inne polecenie; "
                               "odmowa bez czekania") from None
        # Read and validate the journal before extending it: an append that the next read would
        # refuse would leave the runtime with no way forward and no way out.
        earlier = [past for past in powercut_marks(path, manifest) if past["index"] == index]
        require(not earlier or earlier[-1]["log_bytes"] <= mark,
                "Znacznik odcięcia mniejszy niż już zapisany w dzienniku")
        committed = armed["cut_mark_bytes"] is not None
        # An interrupted attempt can leave the boundary journalled with nothing in the state. The
        # first journalled record IS the boundary, so the repeat adopts it instead of appending a
        # second value the state would contradict — that pairing used to brick the runtime for good.
        adopting = not committed and bool(earlier)
        if adopting:
            mark = earlier[0]["log_bytes"]
        # The append and the state write are one region: no signal may separate journal from state.
        previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        try:
            if not adopting:
                record = {"schema": 1, "uuid": manifest["uuid"], "index": index, "log_bytes": mark,
                          "recorded_at": time.time()}
                os.write(descriptor, (json.dumps(record, sort_keys=True) + "\n").encode())
                os.fsync(descriptor)
                sync_directory(path)
            if committed:
                require(armed["cut_mark_bytes"] == mark,
                        f"Znacznik odcięcia jest już zapisany: {armed['cut_mark_bytes']}")
                return armed
            entry = {**armed, "cut_mark_bytes": mark}
            history = state["powercut_history"][:-1] + [entry]
            validate_powercut_entry(manifest, entry, index)
            powercut_record(path, manifest, state, entry)
            receipt = json.dumps({"schema": 1, "uuid": manifest["uuid"], "runtime": str(path),
                                  "history": history}, indent=2) + "\n"
            state["powercut"] = entry
            state["powercut_history"] = history
            durable_replace(path, "powercut.json", receipt)
            persist_state(path, state)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    finally:
        os.close(descriptor)
    return entry


def powercut_discard(path, manifest, armed, index, log_bytes, log_sha256, image_sha256):
    """Sanctioned exit for a cut that cannot be accounted for: a log QEMU never finished writing (no
    superblock at all), a reconstruction that cannot be completed, or one that finished against the
    wrong boundary. It releases the runtime and records for good what was thrown away.

    Over a finished reconstruction it is a repudiation, not a cleanup: the image in the runtime is
    neither the crashed one nor an accepted reconstruction, so the entry is marked forfeited and this
    runtime can never arm another cut — the case is re-run on a rebuilt one."""
    phase = None
    limit = None
    for name in (POWERCUT_REPLAY, powercut_archive(index)[1]):
        target = path / name
        if target.exists() or target.is_symlink():
            private_file(target)
            record = json.loads(target.read_text())
            require(record.get("uuid") == manifest["uuid"] and record.get("runtime") == str(path),
                    "Obcy dowód odtworzenia w porzucanym runtime")
            phase = record.get("phase")
            limit = record.get("limit_bytes")
            break
    if phase == "pending":
        # An interrupted convert leaves bytes that are neither the crashed image nor a reconstruction,
        # and nothing would name them. Releasing that is refused until the image has an identity again.
        require(image_sha256 == armed["image_sha256"],
                "Przerwane odtworzenie zostawiło obraz bez tożsamości; przywróć baseline albo "
                "dokończ identyczne odtworzenie przed porzuceniem")
    # Owner decision 2026-09-12: the append-only replay journal is the SOLE witness of the forfeit.
    # The image pin cannot be one — after a real cut the guest's own writes have changed the image,
    # so a mismatch says nothing about whether anyone reconstructed anything, and deriving the
    # verdict from it would cost the whole runtime for a merely botched mark. The image SHA stays in
    # the summary as evidence the plan compares by hand; it simply no longer decides.
    # Keyed by the armed entry's position, one before the disarming entry being written now.
    journalled = [past for past in powercut_replays(path, manifest) if past["index"] == index - 1]
    reason = DISCARD_DONE if phase == "done" else DISCARD_REWRITTEN if journalled else DISCARD_PLAIN
    return {"reason": reason, "log_bytes": log_bytes, "log_sha256": log_sha256,
            "replay_phase": phase, "limit_bytes": limit,
            "cut_mark_bytes": armed["cut_mark_bytes"], "image_sha256": image_sha256,
            "forfeited": reason != DISCARD_PLAIN}


def powercut_replay(path, manifest, armed, index, log_sha256, image_sha256):
    """Reads the reconstruction receipt c2_replay.py left behind and folds it into the history."""
    for name in (POWERCUT_REPLAY, powercut_archive(index)[1]):
        target = path / name
        if target.exists() or target.is_symlink():
            break
    else:
        require(False, "Brak dowodu odtworzenia obrazu; log zapisów nie może zostać zwolniony")
    private_file(target)
    record = json.loads(target.read_text())
    require(record.get("schema") == REPLAY_SCHEMA and record.get("uuid") == manifest["uuid"]
            and record.get("runtime") == str(path) and record.get("phase") == "done",
            "Niedokończony albo obcy dowód odtworzenia obrazu")
    # The boundary is not the operator's to choose at replay time: it is the mark the host recorded
    # at the cut instant. A reconstruction against any other boundary — above all against the whole
    # log, which rebuilds the crashed image — is refused here and cannot be folded into the history.
    require(armed["cut_mark_bytes"] is not None, "Runtime nie ma zapisanego znacznika odcięcia")
    require(record.get("limit_bytes") is not None, "Odtworzenie bez znacznika odcięcia")
    require(record.get("limit_bytes") == armed["cut_mark_bytes"]
            and record.get("cut_mark_bytes") == armed["cut_mark_bytes"],
            f"Odtworzenie nie odpowiada znacznikowi odcięcia {armed['cut_mark_bytes']}")
    require(record.get("log") == armed["log"] and record.get("log_sha256") == log_sha256
            and record.get("log_sector_size") == armed["log_sector_size"],
            "Dowód odtworzenia dotyczy innego logu zapisów")
    require(record.get("baseline_sha256") == armed["image_sha256"],
            "Odtworzenie nie wyszło od obrazu przypiętego przy uzbrojeniu")
    require(record.get("image_sha256_after") == image_sha256,
            "Obraz w runtime nie jest obrazem zapisanym przez odtworzenie")
    # The receipt alone is not enough: the same reconstruction must be in the append-only journal
    # that --apply writes before it touches the image, so deleting the journal is not free either.
    # Journals are keyed by the armed entry's own position; `index` is the disarming entry that is
    # about to be written, one past it.
    require(any(past["log_sha256"] == record.get("log_sha256")
                and past["limit_bytes"] == record.get("limit_bytes")
                for past in powercut_replays(path, manifest) if past["index"] == index - 1),
            "Brak wpisu w dzienniku odtworzeń dla tego dowodu")
    summary = {key: record.get(key) for key in REPLAY_FIELDS}
    validate_replay_summary(summary)
    return summary


def powercut_record(path, manifest, state, pending=None):
    # Same contract as fault_record: `pending` is the entry the running powercut() is about to write,
    # so exactly the artefacts of an interrupted identical command stay acceptable and repeating that
    # command finishes the write. The log cannot be pinned by SHA — QEMU appends to it while the VM
    # runs — so it is pinned by inode, the same way as the disk images.
    receipt_path = path / "powercut.json"
    history = state.get("powercut_history", [])
    record = state.get("powercut")
    accepted = [history]
    if pending is not None:
        accepted.append(history + [pending])
        # Recording the cut mark rewrites the armed entry in place, so a crash between the receipt
        # and the state must leave the same command able to finish its own write.
        if history and pending["enabled"] and history[-1] == {**pending,
                                                              "cut_mark_bytes": history[-1]["cut_mark_bytes"]}:
            accepted.append(history[:-1] + [pending])
    if not (receipt_path.exists() or receipt_path.is_symlink()):
        require(record is None and history == [], "Brak prywatnego dowodu odcięcia zasilania")
        return None
    private_file(receipt_path)
    receipt = json.loads(receipt_path.read_text())
    require(receipt.get("schema") == 1 and receipt.get("uuid") == manifest["uuid"]
            and receipt.get("runtime") == str(path) and type(history) is list
            and receipt.get("history") in accepted, "Obcy dowód odcięcia zasilania")
    replays = powercut_replays(path, manifest)
    for position, past in enumerate(history):
        validate_powercut_entry(manifest, past, position)
        # The journal is the sole witness of the forfeit, so a release that claims the case survived
        # must not sit on top of a journalled reconstruction. Flipping the verdict in the history now
        # takes deleting the journal line too — which the evidence copy taken after every apply, and
        # nothing inside this directory, is what finally catches.
        if past["discarded"] is not None and not past["discarded"]["forfeited"]:
            require(not [line for line in replays if line["index"] == position - 1],
                    "Porzucenie bez unieważnienia przy zapisanym odtworzeniu")
        # The archives are the only copies of what each cut recorded, so name, mode and size are all
        # checked, and the small JSON one is re-read in full against the history it belongs to. A
        # runtime that lost or swapped one is no longer a runtime whose history reads as evidence.
        for name, size in ((past["log_archive"], past["log_bytes"]),
                           (past["baseline_archive"], past["baseline_bytes"])):
            if name:
                archived = path / name
                require(archived.exists() or archived.is_symlink(),
                        f"Brak archiwum odcięcia zasilania: {name}")
                require(private_file(archived).st_size == size,
                        f"Zmieniony rozmiar archiwum odcięcia zasilania: {name}")
        if past["replay_archive"]:
            archived = path / past["replay_archive"]
            require(archived.exists() or archived.is_symlink(),
                    f"Brak archiwum odcięcia zasilania: {past['replay_archive']}")
            private_file(archived)
            stored = json.loads(archived.read_text())
            require(stored.get("schema") == REPLAY_SCHEMA and stored.get("uuid") == manifest["uuid"]
                    and stored.get("runtime") == str(path), "Obce archiwum dowodu odtworzenia")
            if past["replay"] is not None:
                require({key: stored.get(key) for key in REPLAY_FIELDS} == past["replay"],
                        "Archiwum dowodu odtworzenia nie zgadza się z historią")
    require(record == (history[-1] if history and history[-1]["enabled"] else None),
            "Zapisane odcięcie zasilania niezgodne z historią")
    if record is None:
        return None
    require(disk_profile(manifest) in POWERCUT_PROFILES, "Odcięcie zasilania zapisane poza profilem cache")
    log = path / record["log"]
    if pending is not None and not pending["enabled"] and pending["log_archive"]:
        # The disarming command archives the log by rename before it writes the state, so between
        # those two steps the pinned live log is legitimately gone. Only the command that named this
        # archive may see that state; for everyone else a missing log stays a refusal.
        archived = path / pending["log_archive"]
        if not (log.exists() or log.is_symlink()) and (archived.exists() or archived.is_symlink()):
            private_file(archived)
            return record
    require(log.exists() or log.is_symlink(), "Brak przypiętego logu zapisów")
    info = private_file(log)
    require([info.st_dev, info.st_ino] == record["log_inode"], "Podmieniony log zapisów")
    # Truncation preserves the inode, so every pin above still matches an emptied log. The recorded
    # mark is a length: a log shorter than its own boundary is not the log that boundary came from.
    if record["cut_mark_bytes"] is not None:
        require(info.st_size >= record["cut_mark_bytes"],
                f"Log zapisów krótszy niż znacznik odcięcia {record['cut_mark_bytes']}")
    baseline = path / record["baseline"]
    require(baseline.exists() or baseline.is_symlink(), "Brak przypiętego obrazu bazowego")
    require(private_file(baseline).st_size == record["baseline_bytes"],
            "Zmieniony rozmiar przypiętego obrazu bazowego")
    # The boundary is pinned to the FIRST measurement journalled for this cut; later appends document
    # further attempts without moving it. A recorded boundary with no journal behind it is tampering.
    if record["cut_mark_bytes"] is not None:
        journalled = [mark for mark in powercut_marks(path, manifest)
                      if mark["index"] == len(history) - 1]
        require(journalled and journalled[0]["log_bytes"] == record["cut_mark_bytes"],
                "Znacznik odcięcia bez pokrycia w dzienniku znaczników")
    return record


def create(profile):
    disks = DISK_PROFILES[profile]
    require(os.getuid() != 0, "Nie uruchamiaj harnessu jako root hosta")
    require(os.access("/dev/kvm", os.R_OK | os.W_OK), "Brak dostępu do KVM")
    path = runtime_path(run(["/usr/bin/mktemp", "-d", "/mnt/d/repos/tentanas-vm.XXXXXX"],
                            capture_output=True).stdout.strip())
    print(f"Nowy prywatny runtime: {path}", file=sys.stderr, flush=True)
    write_new(path / "lock", "")
    vm_uuid = str(uuid.uuid4())
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        ports = {"ssh_port": port}
        if profile in API_PROFILES:
            with socket.socket() as api_listener:
                api_listener.bind(("127.0.0.1", 0))
                ports["api_port"] = api_listener.getsockname()[1]
    manifest = {"schema": 1, "uid": os.getuid(), "runtime": str(path), "uuid": vm_uuid,
                **ports, "image_url": IMAGE_URL, "image_sha512": IMAGE_SHA512,
                "disks": disk_manifest(vm_uuid, profile), "machine": MACHINE}
    image = path / "download.qcow2"
    run(["/usr/bin/curl", "--fail", "--location", "--silent", "--show-error",
         "--proto", "=https", "--proto-redir", "=https", "--max-time", "1800",
         "--output", str(image), IMAGE_URL])
    verify_download(image)
    require(json.loads(run([QEMU_IMG, "info", "--output=json", str(image)],
                           capture_output=True).stdout)["virtual-size"] <= disks["os"] * GIB,
            "Obraz bazowy większy od dysku systemowego; nie wolno go zmniejszyć")
    run([QEMU_IMG, "convert", "-f", "qcow2", "-O", "qcow2", str(image), str(path / "os.qcow2")])
    run([QEMU_IMG, "resize", "-f", "qcow2", str(path / "os.qcow2"), f"{disks['os']}G"])
    for role, size in disks.items():
        if role != "os":
            run([QEMU_IMG, "create", "-f", "qcow2", str(path / f"{role}.qcow2"), f"{size}G"])
    for name in ("client", "host"):
        run(["/usr/bin/ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "tentanas-vm",
             "-f", str(path / name)])
        (path / f"{name}.pub").chmod(0o600)
    client_public = (path / "client.pub").read_text().strip()
    host_public = (path / "host.pub").read_text().strip()
    host_private = "\n".join("    " + line for line in (path / "host").read_text().splitlines())
    user_data = f"""#cloud-config
hostname: tentanas-vm
disable_root: true
ssh_pwauth: false
ssh_deletekeys: true
ssh_genkeytypes: [ed25519]
ssh_keys:
  ed25519_private: |
{host_private}
  ed25519_public: {host_public}
users:
  - name: tentanas
    lock_passwd: true
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - {client_public}
write_files:
  - path: /etc/ssh/sshd_config.d/90-tentanas-vm.conf
    permissions: '0644'
    content: |
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      PermitRootLogin no
      AllowAgentForwarding no
      X11Forwarding no
      AllowTcpForwarding no
      HostKey /etc/ssh/ssh_host_ed25519_key
"""
    write_new(path / "user-data", user_data)
    write_new(path / "meta-data", f"instance-id: {vm_uuid}\nlocal-hostname: tentanas-vm\n")
    write_new(path / "known_hosts", f"[127.0.0.1]:{port} {host_public}\n")
    run(["/usr/bin/xorriso", "-as", "mkisofs", "-quiet", "-volid", "cidata", "-joliet", "-rock",
         "-output", str(path / "seed.iso"), str(path / "user-data"), str(path / "meta-data")])
    manifest["seed_sha256"] = hashlib.sha256((path / "seed.iso").read_bytes()).hexdigest()
    manifest["client_public"] = client_public
    manifest["host_public"] = host_public
    manifest["image_inodes"] = {role: [info.st_dev, info.st_ino] for role in disks
                                for info in [private_file(path / f"{role}.qcow2")]}
    write_new(path / "manifest.json", json.dumps(manifest, indent=2) + "\n")
    save_state(path, {"status": "prepared"})
    check_images(path, manifest)
    print(path)


def qemu_command(path, manifest, bootstrap=False, data2_absent=False, fault=None, powercut=None):
    require(not (bootstrap and data2_absent), "Pakiety zabronione po wycofaniu data2")
    require(not (bootstrap and fault), "Pakiety zabronione przy włączonym profilu błędów")
    require(not (bootstrap and powercut), "Pakiety zabronione przy uzbrojonym odcięciu zasilania")
    # Both filters would own the same drive, and a run with injected read errors could never be
    # presented as a clean power cut. The exclusion is enforced here as well, so no tampered pair of
    # receipts can render a doubly filtered drive.
    require(not (fault and powercut), "Profil błędów i odcięcie zasilania wykluczają się")
    args = [QEMU, "-machine", MACHINE, "-cpu", "host", "-smp", "2", "-m", "4096",
            "-name", f"tentanas-{manifest['uuid']}", "-uuid", manifest["uuid"],
            "-nodefaults", "-display", "none", "-monitor", "none",
            "-daemonize", "-pidfile", str(path / "qemu.pid"),
            "-qmp", f"unix:{path}/qmp.sock,server=on,wait=off",
            "-serial", f"file:{path}/serial.log",
            "-netdev", f"user,id=net0,restrict={'off' if bootstrap else 'on'},ipv6=off,hostfwd=tcp:127.0.0.1:{manifest['ssh_port']}-:22{api_forward(manifest)}",
            "-device", "virtio-net-pci,netdev=net0"]
    for role, disk in manifest["disks"].items():
        if role == "data2" and data2_absent:
            continue
        if fault and fault["role"] == role:
            args += ["-drive", f"if=none,id={role},format=qcow2,file.driver=blkdebug,"
                               f"file.config={path}/{FAULT_CONFIG},file.image.driver=file,"
                               f"file.image.filename={path}/{role}.qcow2"]
        elif powercut and powercut["role"] == role:
            # The log filter sits under the emulated NVMe and over qcow2, so the guest still sees the
            # whole disk and the helper rule of whole disks stays untouched.
            args += ["-drive", f"if=none,id={role},driver=blklogwrites,"
                               f"file.driver=qcow2,file.file.filename={path}/{role}.qcow2,"
                               f"log.driver=file,log.filename={path}/{powercut['log']},"
                               f"log-sector-size={powercut['log_sector_size']}"]
        else:
            args += ["-drive", f"if=none,id={role},format=qcow2,file={path}/{role}.qcow2"]
        device = "nvme" if role == "cache" else "virtio-blk-pci"
        boot = ",bootindex=1" if role == "os" else ""
        args += ["-device", f"{device},drive={role},serial={disk['serial']}{boot}"]
    args += ["-drive", f"if=none,id=seed,format=raw,readonly=on,file={path}/seed.iso",
             "-device", "virtio-scsi-pci,id=scsi0",
             "-device", "scsi-cd,drive=seed,bus=scsi0.0"]
    return args


def process_identity(pid):
    require(type(pid) is int and pid > 1, "Niepoprawny PID")
    proc = Path(f"/proc/{pid}")
    require(proc.stat().st_uid == os.getuid(), "Obcy właściciel procesu")
    fields = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
    require(fields[0] != "Z", "Proces zakończony")
    return {"pid": pid, "start_ticks": fields[19],
            "executable": str((proc / "exe").resolve()),
            "argv": (proc / "cmdline").read_bytes().rstrip(b"\0").decode().split("\0")}


def running_identity(path, manifest, state, pending=None):
    require(state["status"] in ("running", "bootstrap"), "VM nie ma potwierdzonego działającego procesu")
    record = retirement(path, manifest, state, allow_pending=True)
    profile = fault_record(path, manifest, state)
    # `pending` is the powercut entry the caller is about to write; the argv never depends on it, so
    # passing it through only decides whether a half-finished write of that same entry is acceptable.
    cut = powercut_record(path, manifest, state, pending)
    actual = process_identity(state["process"]["pid"])
    require(actual == state["process"] and actual["executable"] == str(Path(QEMU).resolve())
            and actual["argv"] == qemu_command(path, manifest, state["status"] == "bootstrap",
                                              bool(record and record["phase"] == "detached"), profile, cut),
            "PID/starttime lub argumenty VM niezgodne; odmowa sterowania")


def process_finished(identity):
    try:
        fields = Path(f"/proc/{identity['pid']}/stat").read_text().rsplit(") ", 1)[1].split()
    except (FileNotFoundError, ProcessLookupError):
        return True
    require(fields[19] == identity["start_ticks"], "PID ponownie użyty podczas stop")
    return fields[0] == "Z"


def qmp(path, manifest, command):
    endpoint = path / "qmp.sock"
    info = endpoint.lstat()
    require(stat.S_ISSOCK(info.st_mode) and info.st_uid == os.getuid(), "Obcy socket QMP")
    # Chardev QEMU przyjmuje jednego klienta, a polecenia z lockiem dzielonym (reset, status,
    # blockstats) mogą trafić na siebie; drugi klient czekałby na powitanie do timeoutu. Monitor
    # serializuje więc flock na qemu.pid, którego żadne z nich nie zapisuje. Kolejność locków jest
    # stała: najpierw lock runtime (locked_runtime), potem monitor, nigdy odwrotnie; oba są NB.
    pid_file = path / "qemu.pid"
    require(pid_file.exists() and not pid_file.is_symlink(), "Brak pliku PID; monitor QMP niedostępny")
    private_file(pid_file)
    with pid_file.open("r") as monitor:
        try:
            fcntl.flock(monitor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError("Monitor QMP zajęty przez inne polecenie; odmowa bez czekania") from None
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(endpoint))
            with connection.makefile("rwb") as stream:
                require("QMP" in json.loads(stream.readline()), "Brak powitania QMP")

                def execute(name):
                    stream.write((json.dumps({"execute": name, "id": name}) + "\n").encode())
                    stream.flush()
                    while True:
                        try:
                            line = stream.readline()
                        except ConnectionResetError:
                            if name == "quit":
                                return {"disconnected_after_quit": True}
                            raise
                        if not line and name == "quit":
                            return {"disconnected_after_quit": True}
                        require(line, "Monitor QMP rozłączony w trakcie polecenia")
                        message = json.loads(line)
                        if message.get("id") == name:
                            require("error" not in message, f"QMP odrzucił {name}")
                            return message["return"]

                execute("qmp_capabilities")
                require(execute("query-uuid")["UUID"] == manifest["uuid"], "Obcy UUID QMP")
                return execute(command)


def read_state(path):
    private_file(path / "state.json")
    return json.loads((path / "state.json").read_text())


def start(path, manifest, bootstrap=False):
    state = read_state(path)
    require(state["status"] in ("prepared", "stopped"), "VM już działa lub wymaga diagnostyki")
    record = retirement(path, manifest, state)
    profile = fault_record(path, manifest, state)
    cut = powercut_record(path, manifest, state)
    require(not (record and bootstrap), "Pakiety zabronione po wycofaniu data2")
    require(not (profile and bootstrap), "Pakiety zabronione przy włączonym profilu błędów")
    require(not (cut and bootstrap), "Pakiety zabronione przy uzbrojonym odcięciu zasilania")
    if cut:
        # log-append is off, so QEMU rewrites the log from its first byte at every boot. A log left
        # by a previous boot is the only copy of that boot's writes: refuse instead of erasing it.
        require(private_file(path / cut["log"]).st_size == 0,
                "Log zapisów z poprzedniego bootu; odtwórz obraz i rozbrój odcięcie zasilania")
    if record:
        require(retired_image_hash(path, manifest) == record["image_sha256"], "Zmieniony SHA wycofanego data2")
    for name in ("qemu.pid", "qmp.sock"):
        require(not (path / name).exists() and not (path / name).is_symlink(),
                "Pozostała tożsamość procesu; odmowa nowego bootu")
    if (path / "serial.log").exists() or (path / "serial.log").is_symlink():
        private_file(path / "serial.log")
    else:
        write_new(path / "serial.log", "")
    check_images(path, manifest)
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    try:
        save_state(path, transition(state, status="starting-bootstrap" if bootstrap else "starting"))
        run(qemu_command(path, manifest, bootstrap, bool(record), profile, cut))
        private_file(path / "qemu.pid")
        identity = process_identity(int((path / "qemu.pid").read_text().strip()))
        state = transition(state, status="bootstrap" if bootstrap else "running", process=identity)
        running_identity(path, manifest, state)
        save_state(path, state)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    qmp(path, manifest, "query-status")
    print(json.dumps({"runtime": str(path), "uuid": manifest["uuid"], "pid": identity["pid"]}))


def ssh_command(path, manifest):
    for name in ("client", "known_hosts", "host.pub"):
        private_file(path / name)
    require((path / "host.pub").read_text().strip() == manifest["host_public"],
            "Podmieniony publiczny klucz hosta")
    client_public = run(["/usr/bin/ssh-keygen", "-y", "-f", str(path / "client")],
                        capture_output=True).stdout.strip().split()[:2]
    require(client_public == manifest["client_public"].split()[:2], "Podmieniony klucz klienta")
    require((path / "known_hosts").read_text()
            == f"[127.0.0.1]:{manifest['ssh_port']} {manifest['host_public']}\n",
            "Podmieniony known_hosts")
    return ["/usr/bin/ssh", "-F", "/dev/null", "-T", "-p", str(manifest["ssh_port"]),
            "-i", str(path / "client"), "-o", f"UserKnownHostsFile={path}/known_hosts",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "StrictHostKeyChecking=yes",
            "-o", "IdentitiesOnly=yes", "-o", "IdentityAgent=none", "-o", "BatchMode=yes",
            "-o", "ForwardAgent=no", "-o", "ForwardX11=no", "-o", "ClearAllForwardings=yes",
            "-o", "ConnectTimeout=5", "-o", "HostKeyAlgorithms=ssh-ed25519", "tentanas@127.0.0.1"]


def inventory(path, manifest):
    require(retirement(path, manifest, read_state(path)) is None,
            "Inventory V01 wymaga pustych dysków profilu bez odłączenia")
    running_identity(path, manifest, read_state(path))
    probe = Path(__file__).with_name("guest_probe.py").read_text()
    result = run(ssh_command(path, manifest) + ["sudo", "-n", "python3", "-"],
                 input=probe, capture_output=True, timeout=30)
    actual = json.loads(result.stdout)
    validate_inventory(manifest, actual)
    print(json.dumps(actual, indent=2))


def validate_inventory(manifest, actual):
    require(actual["uuid"].lower() == manifest["uuid"], "UUID gościa niezgodny")
    disks = actual["disks"]
    require(len(disks) == len(manifest["disks"]), "Nieoczekiwane dyski gościa")
    by_serial = {disk["serial"]: disk for disk in disks}
    require(len(by_serial) == len(manifest["disks"]), "Powtórzony serial dysku")
    for role, expected in manifest["disks"].items():
        disk = by_serial.get(expected["serial"])
        require(disk is not None and disk["size"] == expected["bytes"] and disk["type"] == "disk",
                f"Niezgodna tożsamość lub wielkość: {role}")
        require(bool(re.fullmatch(r"nvme\d+n\d+", disk["name"])) if role == "cache"
                else bool(re.fullmatch(r"vd[a-z]+", disk["name"])), "Niepoprawny typ magistrali")
        require(disk["contains_root"] == (role == "os"), "Niezgodny łańcuch dysku systemowego")
        if role != "os":
            require(not disk["used"], f"Dysk {role} nie jest pustym nośnikiem V01")


def stop(path, manifest):
    state = read_state(path)
    retirement(path, manifest, state, allow_pending=True)
    require(state["status"] in ("running", "bootstrap"), "Brak zapisanej tożsamości VM do zatrzymania")
    if not process_finished(state["process"]):
        running_identity(path, manifest, state)
        qmp(path, manifest, "system_powerdown")
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if process_finished(state["process"]):
            break
        actual = process_identity(state["process"]["pid"])
        require(actual == state["process"], "Zmiana tożsamości procesu podczas stop")
        time.sleep(1)
    else:
        raise RuntimeError("Gość nie zakończył się w 90 s; bez kill i bez usuwania stanu")
    for name in ("qemu.pid", "qmp.sock"):
        target = path / name
        if target.exists() or target.is_symlink():
            info = target.lstat()
            require(info.st_uid == os.getuid() and not stat.S_ISLNK(info.st_mode),
                    "Obcy pozostały plik procesu")
            target.unlink()
    save_state(path, transition(state, status="stopped", previous_process=state["process"]))
    print("VM zatrzymana; wszystkie obrazy i klucze pozostają w prywatnym runtime")


def reset(path, manifest):
    state = read_state(path)
    require(state["status"] == "running", "Twardy reset wymaga potwierdzonej działającej VM")
    require(retirement(path, manifest, state) is None, "Twardy reset zabroniony po rozpoczęciu odłączenia")
    # Resetuje wyłącznie gościa: runtime hosta nie jest zapisywany, dlatego wystarczy lock dzielony.
    running_identity(path, manifest, state)
    qmp(path, manifest, "system_reset")
    print(json.dumps({"runtime": str(path), "uuid": manifest["uuid"],
                      "pid": state["process"]["pid"], "reset": True}))


def blockstats(path, manifest, record_mark=False):
    state = read_state(path)
    require(state["status"] == "running", "Liczniki blockstats wymagają działającej VM")
    pending = None
    if record_mark:
        armed = state.get("powercut")
        require(armed is not None, "Znacznik odcięcia wymaga uzbrojonego runtime")
        # A hard interrupt (SIGKILL, host crash) can leave the receipt one step ahead of the state.
        # The repair is to repeat this command, so the identity check has to be told which write is
        # in flight — otherwise the only command that can finish it is the one that refuses first.
        journalled = [past for past in powercut_marks(path, manifest)
                      if past["index"] == len(state["powercut_history"]) - 1]
        if armed["cut_mark_bytes"] is None and journalled:
            pending = {**armed, "cut_mark_bytes": journalled[0]["log_bytes"]}
    running_identity(path, manifest, state, pending)
    cut = state.get("powercut")
    # The log length at this instant. `blockstats` is the shared-lock command the procedure already
    # runs with the helper suspended, so this is the one moment the host can name the cut position
    # before a clean powerdown appends the guest's own shutdown writes and a final flush.
    marker = private_file(path / cut["log"]).st_size if cut else None
    if record_mark:
        # The single documented exception to "shared commands never write the runtime" (owner
        # decision 2026-09-12). Only this flag writes, and only into the append-only journal and the
        # armed entry; without it blockstats touches nothing.
        require(cut is not None, "Znacznik odcięcia wymaga uzbrojonego runtime")
        powercut_mark(path, manifest, state, marker)
        state = read_state(path)
        cut = state.get("powercut")
    stats = qmp(path, manifest, "query-blockstats")
    print(json.dumps({"runtime": str(path), "uuid": manifest["uuid"], "fault": state.get("fault"),
                      "fault_history": state.get("fault_history", []), "powercut": cut,
                      "powercut_history": state.get("powercut_history", []),
                      "powercut_log_bytes": marker,
                      "powercut_cut_mark_bytes": cut["cut_mark_bytes"] if cut else None,
                      "blockstats": stats}, indent=2))


def fault(path, manifest, role, sectors):
    profile = disk_profile(manifest)
    require(profile in FAULT_PROFILES, f"Profil błędów odczytu zabroniony dla profilu {profile}")
    state = read_state(path)
    require(state["status"] in ("prepared", "stopped"), "Profil błędów wymaga zatrzymanej VM")
    require(retirement(path, manifest, state, allow_pending=True) is None,
            "Profil błędów zabroniony po rozpoczęciu odłączenia")
    enabled = role != "none"
    # Called unconditionally: it is also the check that the powercut archives are all still there,
    # and `fault none` must not be the one path that skips it.
    cut = powercut_record(path, manifest, state)
    # Disarming is always allowed, so neither subcommand can lock the runtime out of the other.
    require(not enabled or cut is None,
            "Odcięcie zasilania jest uzbrojone; najpierw powercut cache off")
    if enabled:
        entry = {"schema": 1, "enabled": True, "role": role, "sectors": sectors,
                 "config_sha256": hashlib.sha256(fault_config(sectors).encode()).hexdigest()}
    else:
        require(not sectors, "Wyłączenie profilu błędów nie przyjmuje sektorów")
        entry = {"schema": 1, "enabled": False, "role": None, "sectors": [], "config_sha256": None}
    validate_fault_entry(manifest, entry)
    if enabled:
        # blkdebug dopasowuje sektory hostowego pliku qcow2, nie rozmiaru widzianego przez gościa.
        image = path / f"{FAULT_ROLE}.qcow2"
        info = private_file(image)
        require([info.st_dev, info.st_ino] == manifest["image_inodes"][FAULT_ROLE],
                f"Podmieniony plik obrazu {FAULT_ROLE}")
        limit = info.st_size // 512
        require(all(sector < limit for sector in sectors),
                f"Sektor poza hostowym plikiem {FAULT_ROLE}.qcow2; dopuszczalne poniżej {limit}")
        # VM stoi, więc mapa jest stabilna. Sektor musi trafić w zapisane dane gościa, nie w
        # metadane qcow2 ani dziurę: inaczej profil albo psuje otwarcie obrazu, albo nic nie robi.
        # Zakres `zero` odpada mimo `data`, bo tak mapuje się preallokacja metadanych: QEMU zwraca
        # takie odczyty jako zera, nie sięgając do pliku hosta, więc blkdebug nigdy by nie wystrzelił.
        extents = [extent for extent in json.loads(run([QEMU_IMG, "map", "--output=json", str(image)],
                                                       capture_output=True).stdout)
                   if extent.get("data") and not extent.get("zero") and "offset" in extent]
        require(all(any(extent["offset"] <= sector * 512 < extent["offset"] + extent["length"]
                        for extent in extents) for sector in sectors),
                f"Sektor poza zapisanymi danymi {FAULT_ROLE}.qcow2; wylicz offset przez qemu-img map")
    fault_record(path, manifest, state, entry)
    history = state.get("fault_history", []) + [entry]
    receipt = json.dumps({"schema": 1, "uuid": manifest["uuid"], "runtime": str(path),
                          "history": history}, indent=2) + "\n"
    state["fault_history"] = history
    if enabled:
        state["fault"] = entry
    else:
        state.pop("fault", None)
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    try:
        if enabled:
            durable_replace(path, FAULT_CONFIG, fault_config(sectors))
        durable_replace(path, "fault.json", receipt)
        persist_state(path, state)
        config = path / FAULT_CONFIG
        if not enabled and (config.exists() or config.is_symlink()):
            private_file(config)
            config.unlink()
            sync_directory(path)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    require(fault_record(path, manifest, read_state(path)) == (entry if enabled else None),
            "Nie potwierdzono zapisanego profilu błędów")
    print(json.dumps({"runtime": str(path), "uuid": manifest["uuid"],
                      "fault": entry if enabled else None, "fault_history": history}, indent=2))


def powercut(path, manifest, role, switch):
    profile = disk_profile(manifest)
    require(profile in POWERCUT_PROFILES, f"Odcięcie zasilania zabronione dla profilu {profile}")
    state = read_state(path)
    require(state["status"] in ("prepared", "stopped"), "Odcięcie zasilania wymaga zatrzymanej VM")
    require(retirement(path, manifest, state, allow_pending=True) is None,
            "Odcięcie zasilania zabronione po rozpoczęciu odłączenia")
    enabled = switch == "on"
    require(not enabled or fault_record(path, manifest, state) is None,
            "Profil błędów jest włączony; najpierw fault none")
    log = path / POWERCUT_LOG
    image = path / f"{POWERCUT_ROLE}.qcow2"
    baseline = path / POWERCUT_BASELINE
    receipt_path = path / POWERCUT_REPLAY
    index = len(state.get("powercut_history", []))
    archive, replay_archive, baseline_archive = powercut_archive(index)
    if enabled:
        require(state.get("powercut") is None, "Odcięcie zasilania jest już uzbrojone")
        require(not [past for past in state.get("powercut_history", [])
                     if past["discarded"] and past["discarded"]["forfeited"]],
                "Runtime ma unieważnione odcięcie; sprawę powtarza się na odbudowanym runtime")
        require(not (receipt_path.exists() or receipt_path.is_symlink()),
                "Nierozliczone odtworzenie z poprzedniego odcięcia; najpierw powercut cache off")
        if log.exists() or log.is_symlink():
            # Only the empty log of an interrupted identical command is reused; a used log belongs to
            # a boot whose image has not been reconstructed yet.
            require(private_file(log).st_size == 0,
                    "Log zapisów z poprzedniego bootu; odtwórz obraz i rozbrój odcięcie zasilania")
        else:
            # Nothing to reuse, so nothing was interrupted mid-arm: receipt and state must already
            # agree before this command creates its first file.
            powercut_record(path, manifest, state)
            os.close(os.open(log, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
            sync_directory(path)
        info = private_file(log)
        target = private_file(image)
        require([target.st_dev, target.st_ino] == manifest["image_inodes"][POWERCUT_ROLE],
                f"Podmieniony plik obrazu {POWERCUT_ROLE}")
        # The baseline is the harness's own copy, not something the operator points at later: the
        # reconstruction has to start from exactly the bytes that were on the disk at arming.
        if baseline.exists() or baseline.is_symlink():
            private_file(baseline)
        else:
            copy_private(image, baseline)
        digest = file_sha256(image)
        require(file_sha256(baseline) == digest, "Kopia bazowa nie odpowiada obrazowi cache")
        entry = {"schema": 1, "enabled": True, "role": role, "log": POWERCUT_LOG,
                 "log_inode": [info.st_dev, info.st_ino],
                 "log_sector_size": POWERCUT_LOG_SECTOR_SIZE, "cut_mark_bytes": None,
                 "image_sha256": digest,
                 "baseline": POWERCUT_BASELINE, "baseline_bytes": baseline.stat().st_size,
                 "log_bytes": None, "log_sha256": None, "log_archive": None,
                 "baseline_archive": None, "replay": None, "replay_archive": None,
                 "discarded": None}
    else:
        armed = state.get("powercut")
        require(armed is not None, "Odcięcie zasilania nie jest uzbrojone")
        # After the archiving rename the live log is gone, so repeating an interrupted disarm has to
        # measure the archive instead and still produce the identical entry.
        source = log if (log.exists() or log.is_symlink()) else path / archive
        require(source.exists() or source.is_symlink(),
                "Brak logu zapisów ani jego archiwum; wymagana diagnostyka")
        info = private_file(source)
        digest = file_sha256(source)
        image_digest = file_sha256(image)
        summary = discarded = None
        if info.st_size and switch == "off":
            summary = powercut_replay(path, manifest, armed, index, digest, image_digest)
        elif info.st_size:
            discarded = powercut_discard(path, manifest, armed, index, info.st_size, digest,
                                         image_digest)
        else:
            require(not (receipt_path.exists() or receipt_path.is_symlink()),
                    "Dowód odtworzenia bez zapisów w logu; wymagana diagnostyka")
        entry = {"schema": 1, "enabled": False, "role": None, "log": None, "log_inode": None,
                 "log_sector_size": None, "cut_mark_bytes": armed["cut_mark_bytes"],
                 "image_sha256": armed["image_sha256"], "baseline": None,
                 "baseline_bytes": armed["baseline_bytes"], "log_bytes": info.st_size,
                 "log_sha256": digest, "log_archive": archive,
                 "baseline_archive": baseline_archive, "replay": summary,
                 "replay_archive": replay_archive if summary or (discarded
                                                                 and discarded["replay_phase"]) else None,
                 "discarded": discarded}
    validate_powercut_entry(manifest, entry, index)
    powercut_record(path, manifest, state, entry)
    history = state.get("powercut_history", []) + [entry]
    receipt = json.dumps({"schema": 1, "uuid": manifest["uuid"], "runtime": str(path),
                          "history": history}, indent=2) + "\n"
    state["powercut_history"] = history
    if enabled:
        state["powercut"] = entry
    else:
        state.pop("powercut", None)
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    try:
        durable_replace(path, "powercut.json", receipt)
        if not enabled:
            # Archive before the state write, and by rename: the log and the reconstruction receipt
            # are the only copies of what the cut recorded, so they are never deleted, and a crash
            # between the two steps leaves a runtime the same command can finish.
            for live, archived in ((POWERCUT_LOG, archive), (POWERCUT_REPLAY, replay_archive),
                                   (POWERCUT_BASELINE, baseline_archive)):
                if (path / live).exists() or (path / live).is_symlink():
                    private_file(path / live)
                    (path / live).replace(path / archived)
            sync_directory(path)
        persist_state(path, state)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    require(powercut_record(path, manifest, read_state(path)) == (entry if enabled else None),
            "Nie potwierdzono zapisanego odcięcia zasilania")
    print(json.dumps({"runtime": str(path), "uuid": manifest["uuid"],
                      "powercut": entry if enabled else None, "powercut_history": history}, indent=2))


def wait_ssh(path, manifest):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        running_identity(path, manifest, read_state(path))
        result = subprocess.run(ssh_command(path, manifest) + ["true"], capture_output=True, timeout=10)
        if result.returncode == 0:
            return
        time.sleep(2)
    raise RuntimeError("SSH niegotowy w 90 s")


def package_phase(path, manifest, phase):
    require(retirement(path, manifest, read_state(path)) is None, "Pakiety zabronione po rozpoczęciu odłączenia")
    running_identity(path, manifest, read_state(path))
    contract = json.dumps({"phase": phase, "uuid": manifest["uuid"], "profile": disk_profile(manifest)})
    source = Path(__file__).with_name("guest_packages.py").read_text()
    run(ssh_command(path, manifest) + [shlex.join(["sudo", "-n", "python3", "-", contract])],
        input=source, timeout=900)


def recover_start(path, manifest):
    state = read_state(path)
    retirement(path, manifest, state)
    if state["status"] not in ("starting", "starting-bootstrap"):
        return
    pid_file = path / "qemu.pid"
    if not pid_file.exists():
        require(not (path / "qmp.sock").exists(), "Niepełny start wymaga diagnostyki QMP")
        save_state(path, transition(state, status="stopped"))
        return
    private_file(pid_file)
    identity = process_identity(int(pid_file.read_text().strip()))
    state = transition(state, status="bootstrap" if state["status"] == "starting-bootstrap" else "running",
                       process=identity)
    running_identity(path, manifest, state)
    qmp(path, manifest, "query-status")
    save_state(path, state)


def emergency_stop(path, manifest):
    state = read_state(path)
    running_identity(path, manifest, state)
    qmp(path, manifest, "quit")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process_finished(state["process"]):
            stop(path, manifest)
            print("Awaryjny QMP quit: instalacja nie może być uznana za poprawną", file=sys.stderr)
            return
        time.sleep(0.2)
    raise RuntimeError("Nie potwierdzono zakończenia VM; izolacja nieprzywrócona")


def restore_isolation(path, manifest, restore_apt):
    recover_start(path, manifest)
    state = read_state(path)
    if state["status"] == "running":
        running_identity(path, manifest, state)
        if restore_apt:
            package_phase(path, manifest, "finalize")
        inventory(path, manifest)
        return
    forced = False
    if state["status"] in ("running", "bootstrap"):
        try:
            stop(path, manifest)
        except (RuntimeError, OSError, ValueError):
            emergency_stop(path, manifest)
            forced = True
    require(read_state(path)["status"] == "stopped", "Nie potwierdzono zatrzymania VM")
    start(path, manifest)
    wait_ssh(path, manifest)
    if restore_apt:
        package_phase(path, manifest, "finalize")
    inventory(path, manifest)
    require(not forced, "Przerwano VM awaryjnie; pakiety wymagają diagnostyki")


def packages(path, manifest, download):
    require(retirement(path, manifest, read_state(path)) is None, "Pakiety zabronione po rozpoczęciu odłączenia")
    # Every package stage restarts the VM, and each boot resets the write log; the stages also open
    # egress. Refuse here, before the first phase, instead of failing inside restore_isolation.
    require(powercut_record(path, manifest, read_state(path)) is None,
            "Pakiety zabronione przy uzbrojonym odcięciu zasilania")
    require(read_state(path)["status"] == "running", "Etap pakietów wymaga działającej izolowanej VM")
    inventory(path, manifest)

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"Przerwano etap pakietów sygnałem {signum}")

    previous = signal.signal(signal.SIGTERM, interrupted)
    success = False
    try:
        if download:
            package_phase(path, manifest, "prepare")
            stop(path, manifest)
            start(path, manifest, bootstrap=True)
            wait_ssh(path, manifest)
            inventory(path, manifest)
            package_phase(path, manifest, "download")
        else:
            package_phase(path, manifest, "install")
            package_phase(path, manifest, "probe")
        success = True
    finally:
        old_int = signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        try:
            restore_isolation(path, manifest, restore_apt=not download or not success)
        finally:
            signal.signal(signal.SIGINT, old_int)
            signal.signal(signal.SIGTERM, previous)
    package_phase(path, manifest, "verify-network")
    if not download:
        package_phase(path, manifest, "seal")
    print("Archiwa pobrane; instalacja wymaga osobnego odbioru" if download else "Pakiety gotowe; sieć izolowana")


def storage(path, manifest, phase):
    profile = disk_profile(manifest)
    require(profile == "storage", f"Fazy storage zabronione dla profilu {profile}")
    state = read_state(path)
    require(state["status"] == "running", "Storage wymaga działającej izolowanej VM")
    record = retirement(path, manifest, state)
    replacement_phase = phase in ("replacement-preflight", "replacement-prepare", "replacement-recover")
    enospc_phase = phase in ("enospc-preflight", "enospc")
    require((record is not None and (replacement_phase or enospc_phase or phase == "verify"))
            or (record is None and not replacement_phase and not enospc_phase),
            "Faza storage niezgodna z profilem odłączenia")
    running_identity(path, manifest, state)
    contract = {"phase": phase, "uuid": manifest["uuid"], "disks": manifest["disks"]}
    if record:
        contract["replacement_id"] = record["intent"]["operation_id"]
    source = Path(__file__).with_name("guest_storage.py").read_text()
    command = ssh_command(path, manifest) + [shlex.join(["sudo", "-n", "python3", "-", json.dumps(contract)])]
    if phase == "enospc":
        space = os.statvfs(path)
        require(space.f_frsize > 0 and space.f_bavail * space.f_frsize >= 3 * GIB,
                "ENOSPC wymaga minimum 3 GiB dostępnych na hoście po eksporcie bezpieczeństwa")
    run(command,
        input=source, timeout=900)


def detach_data2(path, manifest):
    profile = disk_profile(manifest)
    require(profile == "storage", f"Odłączenie data2 zabronione dla profilu {profile}")
    state = read_state(path)
    require(retirement(path, manifest, state) is None, "Ponowienie detach zabronione")
    require(state["status"] == "running", "Odłączenie wymaga działającej izolowanej VM")
    running_identity(path, manifest, state)
    intent = {"schema": 1, "uuid": manifest["uuid"], "operation_id": str(uuid.uuid4()),
              "source": "data2", "target": "spare", "process": state["process"],
              "disks": {role: manifest["disks"][role] for role in ("data2", "spare")},
              "image_inode": manifest["image_inodes"]["data2"]}
    durable_new(path / "detach-intent.json", intent)
    state["retirement"] = {"phase": "intent", "intent": intent}
    save_state(path, state)
    running_identity(path, manifest, state)
    contract = {"phase": "replacement-arm", "uuid": manifest["uuid"],
                "disks": manifest["disks"], "replacement_id": intent["operation_id"]}
    source = Path(__file__).with_name("guest_storage.py").read_text()
    try:
        response = run(ssh_command(path, manifest)
                       + [shlex.join(["sudo", "-n", "python3", "-", json.dumps(contract)])],
                       input=source, capture_output=True, timeout=900)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        for output in (error.stdout, error.stderr):
            if output:
                print(output.decode(errors="replace") if isinstance(output, bytes) else output,
                      file=sys.stderr, end="")
        raise
    print(response.stderr, file=sys.stderr, end="")
    arm = json.loads(response.stdout)
    validate_arm(manifest, intent["operation_id"], arm)
    stop(path, manifest)
    stopped = read_state(path)
    retirement(path, manifest, stopped, allow_pending=True)
    require(stopped["status"] == "stopped" and stopped["previous_process"] == intent["process"]
            and process_finished(intent["process"]), "Nie potwierdzono normalnego stop przed odłączeniem")
    record = {"phase": "detached", "profile": "data2_absent", "intent": intent,
              "stopped_process": intent["process"], "image_sha256": retired_image_hash(path, manifest),
              "arm": arm, "arm_sha256": hashlib.sha256(json.dumps(arm, sort_keys=True).encode()).hexdigest()}
    stopped["retirement"] = record
    persist_state(path, stopped)
    durable_new(path / "retired-data2.json", record)
    retirement(path, manifest, read_state(path))
    print(json.dumps({"status": "stopped", "profile": "data2_absent", "operation_id": intent["operation_id"],
                      "image_sha256": record["image_sha256"]}, indent=2))


def main():
    os.umask(0o077)
    require(os.getuid() != 0, "Nie uruchamiaj harnessu jako root hosta")
    parser = argparse.ArgumentParser(description="Prywatna VM TentaNas bez hostowych dysków")
    commands = parser.add_subparsers(dest="command", required=True)
    create_parser = commands.add_parser("create", help="Nowy runtime, pobranie i weryfikacja; bez bootu")
    create_parser.add_argument("--profile", choices=tuple(DISK_PROFILES), default="storage")
    for name in RUNTIME_COMMANDS:
        command = commands.add_parser(name)
        command.add_argument("runtime")
        if name == "ssh":
            command.add_argument("guest_command", nargs=argparse.REMAINDER)
        elif name == "fault":
            command.add_argument("role", choices=(FAULT_ROLE, "none"))
            command.add_argument("--read-error-sector", type=int, action="append", default=None,
                                 dest="sectors", metavar="SEKTOR")
        elif name == "powercut":
            command.add_argument("role", choices=(POWERCUT_ROLE,))
            command.add_argument("switch", choices=("on", "off", "discard"))
        elif name == "blockstats":
            command.add_argument("--record-cut-mark", action="store_true", dest="record_mark")
        elif name == "storage":
            command.add_argument("phase", choices=("preflight", "prepare", "exercise", "verify", "corruption",
                                                   "replacement-preflight", "replacement-prepare", "replacement-recover",
                                                   "enospc-preflight", "enospc"))
    args = parser.parse_args()
    if args.command == "create":
        create(args.profile)
        return
    with locked_runtime(args.runtime, args.command in SHARED_COMMANDS) as (path, manifest):
        if args.command == "start":
            start(path, manifest)
        elif args.command == "stop":
            stop(path, manifest)
        elif args.command == "inventory":
            inventory(path, manifest)
        elif args.command in ("bootstrap-packages", "install-packages"):
            packages(path, manifest, args.command == "bootstrap-packages")
        elif args.command == "storage":
            storage(path, manifest, args.phase)
        elif args.command == "detach-data2":
            detach_data2(path, manifest)
        elif args.command == "reset":
            reset(path, manifest)
        elif args.command == "blockstats":
            blockstats(path, manifest, args.record_mark)
        elif args.command == "fault":
            fault(path, manifest, args.role, args.sectors or [])
        elif args.command == "powercut":
            powercut(path, manifest, args.role, args.switch)
        elif args.command == "status":
            state = read_state(path)
            retirement(path, manifest, state, allow_pending=True)
            fault_record(path, manifest, state)
            powercut_record(path, manifest, state)
            if state["status"] in ("running", "bootstrap"):
                running_identity(path, manifest, state)
                state["qmp"] = qmp(path, manifest, "query-status")
            print(json.dumps(state, indent=2))
        elif args.command == "ssh":
            retirement(path, manifest, read_state(path))
            running_identity(path, manifest, read_state(path))
            require(args.guest_command, "Podaj jawne polecenie wykonywane wyłącznie w gościu")
            run(ssh_command(path, manifest) + [shlex.join(args.guest_command)])
        else:
            require(False, f"Nieobsługiwana podkomenda: {args.command}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"Odmowa/błąd VM: {error}", file=sys.stderr)
        sys.exit(1)
