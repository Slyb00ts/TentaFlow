# =============================================================================
# Plik: tests/infra/tentanas-vm/guest_storage.py
# Opis: Jednorazowy cykl ext4, mergerfs i SnapRAID w prywatnym gościu V01.
# Przykład: python3 - '<JSON phase/uuid/disks>' z kodem na stdin, jako root gościa.
# =============================================================================

import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import uuid
import errno
import time


STATE_ROOT = Path("/var/lib/tentanas-vm-storage")
PACKAGES_ROOT = Path("/var/lib/tentanas-vm-packages")
SIZES = {"os": 12, "data1": 1, "data2": 1, "parity": 2, "cache": 1, "spare": 1}
TARGETS = ("data1", "data2", "parity")
PHASES = ("preflight", "prepare", "exercise", "verify", "corruption", "replacement-arm",
          "replacement-preflight", "replacement-prepare", "replacement-recover", "enospc-preflight", "enospc")
ENOSPC_MAX_BYTES = 1024**3
ENOSPC_CHUNK_BYTES = 1024**2
ENOSPC_MAX_SECONDS = 300
ENOSPC_MIN_BYTES = 64 * 1024**2
ENOSPC_BRANCH_MIN_BYTES = 128 * 1024**2


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def command(args, expected=0, *, empty_exit=None):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=300,
                                env={**os.environ, "LC_ALL": "C"})
    except subprocess.TimeoutExpired:
        print(json.dumps({"command": args, "timeout": True}), file=sys.stderr, flush=True)
        raise
    print(json.dumps({"command": args, "exit": result.returncode,
                      "stdout": result.stdout, "stderr": result.stderr}), file=sys.stderr, flush=True)
    if empty_exit is not None:
        require(not result.stderr, f"Diagnostyka sondy {args[0]}")
        if result.returncode == empty_exit:
            require(result.stdout == "", f"Niepusty wynik sondy {args[0]}")
            return None
    require(result.returncode == expected, f"Nieoczekiwany wynik {args[0]}: {result.returncode}")
    return result.stdout


def filesystem(device):
    output = command(["/usr/sbin/blkid", "-p", "-o", "export", str(device)], empty_exit=2)
    if output is None:
        return None, None
    fields = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        require(separator and re.fullmatch(r"[A-Z0-9_]+", key) and key not in fields,
                "Niepoprawny lub powtórzony wpis blkid")
        fields[key] = value
    require(fields.get("DEVNAME") == str(device)
            and ("TYPE" in fields) == ("UUID" in fields)
            and ("PTTYPE" in fields) == ("PTUUID" in fields)
            and any(fields.get(key) for key in ("TYPE", "PTTYPE"))
            and all(fields[key] for key in ("TYPE", "UUID", "PTTYPE", "PTUUID") if key in fields),
            "Niepełna lub obca tożsamość blkid")
    return fields.get("TYPE"), fields.get("UUID")


def descendants(node):
    yield node
    for child in node.get("children", []):
        yield from descendants(child)


def observe():
    disks = json.loads(command(["/usr/bin/lsblk", "--json", "--bytes", "--output",
                               "NAME,SIZE,TYPE,SERIAL,FSTYPE,UUID,MOUNTPOINTS,MAJ:MIN,RO"]))["blockdevices"]
    mounts = json.loads(command(["/usr/bin/findmnt", "--json", "--list", "--output",
                                "SOURCE,TARGET,FSTYPE,MAJ:MIN"]))["filesystems"]
    for disk in disks:
        if disk["type"] != "disk":
            continue
        name = disk["name"]
        require(re.fullmatch(r"vd[a-z]+|nvme\d+n\d+", name), "Nieoczekiwane urządzenie gościa")
        device = Path("/dev") / name
        device_stat = device.stat()
        require(stat.S_ISBLK(device_stat.st_mode), "Cel nie jest urządzeniem blokowym")
        require(f"{os.major(device_stat.st_rdev)}:{os.minor(device_stat.st_rdev)}" == disk["maj:min"],
                "Urządzenie zmieniło tożsamość")
        disk["fstype"], disk["uuid"] = filesystem(device)
        holders = Path("/sys/dev/block") / disk["maj:min"] / "holders"
        disk["holders"] = [entry.name for entry in holders.iterdir()]
        disk["signatures"] = json.loads(command([
            "/usr/sbin/wipefs", "--no-act", "--json", str(device)]))["signatures"]
    swaps = []
    for line in Path("/proc/swaps").read_text().splitlines()[1:]:
        info = Path(line.split()[0]).stat()
        number = info.st_rdev if stat.S_ISBLK(info.st_mode) else info.st_dev
        swaps.append(f"{os.major(number)}:{os.minor(number)}")
    return {"uuid": Path("/sys/class/dmi/id/product_uuid").read_text().strip().lower(),
            "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
            "disks": disks, "mounts": mounts, "swaps": swaps}


def contract(value):
    fields = {"phase", "uuid", "disks"}
    if value.get("phase", "").startswith("replacement-") or value.get("phase") in ("enospc-preflight", "enospc") or "replacement_id" in value:
        require(value["phase"].startswith("replacement-") or value["phase"] in ("verify", "enospc-preflight", "enospc"),
                "Identyfikator replacement poza właściwą fazą")
        fields.add("replacement_id")
        require(str(uuid.UUID(value["replacement_id"])) == value["replacement_id"], "Niekanoniczny replacement_id")
    require(set(value) == fields and value["phase"] in PHASES,
            "Niepoprawny kontrakt fazy")
    identity = str(uuid.UUID(value["uuid"]))
    require(identity == value["uuid"], "Niekanoniczny UUID")
    prefix = uuid.UUID(identity).hex[:10]
    expected = {role: {"serial": f"tn-{prefix}-{role}", "bytes": size * 1024**3}
                for role, size in SIZES.items()}
    require(value["disks"] == expected, "Kontrakt dysków różni się od profilu V01")
    return {"uuid": identity, "disks": expected}


def validate(expected, observation, filesystems, base, replacement=None):
    require(observation["uuid"] == expected["uuid"], "Niewłaściwy UUID gościa")
    disks = [disk for disk in observation["disks"] if disk["type"] != "rom"]
    wanted_disks = expected["disks"].copy()
    if replacement is not None:
        require(replacement["missing"] == "data2" and replacement["target"] == "spare",
                "Obce mapowanie replacement")
        require(all(node.get("serial") != expected["disks"]["data2"]["serial"]
                    and node.get("uuid") != replacement["before"]["filesystems"]["data2"]
                    for disk in disks for node in descendants(disk)), "Utracony data2 nadal obecny")
        require(all(filesystems[role] == replacement["before"]["filesystems"][role]
                    for role in ("data1", "parity")), "Zmieniony ocalały UUID")
        wanted_disks["data2"] = wanted_disks.pop("spare")
        filesystems = filesystems.copy()
        if "new_uuid" in replacement:
            require(filesystems["data2"] == replacement["new_uuid"], "Obcy UUID replacement")
        else:
            require(filesystems["data2"] == replacement["before"]["filesystems"]["data2"], "Zmieniony stary UUID")
            del filesystems["data2"]
    require(len(disks) == len(wanted_disks) and all(disk["type"] == "disk" for disk in disks),
            "Nieoczekiwany zestaw całych dysków")
    require(len({disk["serial"] for disk in disks}) == len(disks)
            and len({disk["maj:min"] for disk in disks}) == len(disks), "Nieunikalny serial lub urządzenie")
    roots = [mount for mount in observation["mounts"] if mount["target"] == "/"]
    require(len(roots) == 1, "Nieznany dysk systemowy")
    found = {}
    for role, wanted in wanted_disks.items():
        matches = [disk for disk in disks if disk["serial"] == wanted["serial"]]
        require(len(matches) == 1, f"Niewłaściwy serial {role}")
        disk = matches[0]
        require(disk["size"] == wanted["bytes"] and not disk["ro"], f"Rozmiar lub tryb dysku {role}")
        pattern = r"nvme\d+n\d+" if role == "cache" else r"vd[a-z]+"
        require(re.fullmatch(pattern, disk["name"]), f"Niewłaściwa magistrala {role}")
        tree = list(descendants(disk))
        numbers = {node["maj:min"] for node in tree}
        system_mounts = [mount for mount in observation["mounts"]
                         if mount["target"] in ("/", "/boot", "/boot/efi") and mount["maj:min"] in numbers]
        if role == "os":
            require(roots[0]["maj:min"] in numbers, "Root nie należy do OS")
        else:
            require(not system_mounts and not numbers.intersection(observation["swaps"]),
                    f"Chroniony dysk systemowy/swap: {role}")
            require(len(tree) == 1 and not disk["holders"], f"Partycje lub holdery: {role}")
            actual_mounts = [mount for mount in observation["mounts"] if mount["maj:min"] in numbers]
            saved_uuid = filesystems.get(role)
            if saved_uuid:
                require(role in TARGETS and disk["fstype"] == "ext4" and disk["uuid"] == saved_uuid,
                        f"Niewłaściwy FS/UUID: {role}")
                require(all(signature["type"] == "ext4" for signature in disk["signatures"]),
                        f"Obcy podpis dysku {role}")
                require(len(actual_mounts) <= 1 and all(
                    mount["target"] == str(base / "mnt" / role) and mount["fstype"] == "ext4"
                    for mount in actual_mounts), f"Niewłaściwy mount: {role}")
            else:
                require(not disk["fstype"] and not disk["uuid"] and not disk["signatures"]
                        and not actual_mounts and not any(disk["mountpoints"] or []),
                        f"Dysk nie jest pusty: {role}")
        found[role] = disk
    require(found["parity"]["size"] >= max(found[role]["size"] for role in ("data1", "data2")),
            "Parity za małe")
    fs_ids = [disk["uuid"] for disk in disks if disk["uuid"]]
    require(len(set(fs_ids)) == len(fs_ids), "Powtórzony UUID filesystemu")
    return found


def private(path, directory=False):
    info = path.lstat()
    require((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))
            and info.st_uid == 0 and not info.st_mode & 0o077
            and (directory or info.st_nlink == 1), f"Nieprywatny plik stanu: {path}")
    require(path.resolve() == path, "Dowiązanie w ścieżce stanu")


def flush_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def save(base, state):
    next_path = base / "state.next"
    with next_path.open("x") as stream:
        json.dump(state, stream, sort_keys=True)
        stream.flush()
        os.fsync(stream.fileno())
    next_path.replace(base / "state.json")
    flush_directory(base)


def digest(path):
    require(path.is_file() and not path.is_symlink() and path.stat().st_nlink == 1,
            f"Nieoczekiwany plik korpusu: {path}")
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def scrub_blocks(output, log, expected_errors=0):
    blocks = re.findall(r"(?m)^block_count:(\d+)$", log)
    require(len(blocks) == 1 and int(blocks[0]) > 0
            and re.search(r"(?m)^\s*100% completed, [1-9][0-9]* MB accessed\b", output)
            and bool(re.search(r"(?m)^Everything OK$", output)) == (expected_errors == 0)
            and re.findall(r"(?m)^summary:exit:(\w+)$", log) == ["error" if expected_errors else "ok"]
            and all(re.findall(r"(?m)^summary:error_" + kind + r":(\d+)$", log) == ["0"]
                    for kind in ("file", "io"))
            and re.findall(r"(?m)^summary:error_data:(\d+)$", log) == [str(expected_errors)],
            "Brak dowodu pełnego scrub z oczekiwanym wynikiem")
    return int(blocks[0])


def corruption_block(output, log, disk):
    count = scrub_blocks(output, log, 1)
    errors = re.findall(r"(?m)^error:(.*)$", log)
    require(len(errors) == 1 and not re.search(r"(?m)^parity_error:", log), "Obce błędy scrub")
    match = re.fullmatch(r"(\d+):" + re.escape(disk)
                         + r":restore\.bin: Data error at position 0, diff bits (\d+)/(\d+)", errors[0])
    require(match and int(match[1]) < count and 0 < int(match[2]) <= int(match[3]),
            "Scrub nie wskazał zmienionego bloku restore.bin")
    return int(match[1])


def recovered_block(log, disk, block):
    expected = f"{block}:{disk}:restore.bin: Fixed data error at position 0"
    errors = re.findall(r"(?m)^error:(.*)$", log)
    error = re.fullmatch(re.escape(f"{block}:{disk}:restore.bin:")
                         + r" Data error at position 0, diff bits (\d+)/(\d+)", errors[0]) if len(errors) == 1 else None
    require(re.findall(r"(?m)^fixed:(.*)$", log) == [expected]
            and error and 0 < int(error[1]) <= int(error[2])
            and not re.search(r"(?m)^(?:parity_fixed|parity_error|unrecoverable):", log)
            and all(re.findall(r"(?m)^summary:" + key + r":(\w+)$", log) == [value]
                    for key, value in (("error", "1"), ("error_recovered", "1"),
                                       ("error_unrecoverable", "0"), ("exit", "recovered"))),
            "Fix nie potwierdził odzyskania wskazanego bloku")


def recovered_disk(output, log, original, block_limit):
    require(set(original) == {"restore.bin"} and original["restore.bin"]["role"] == "data2",
            "Nieoczekiwany korpus utraconego dysku")
    require(re.findall(r"(?m)^blocksize:(\d+)$", log) == ["262144"], "Obcy rozmiar bloku odzysku")
    count = (original["restore.bin"]["bytes"] + 262143) // 262144
    pairs = []
    for tag, description in (("error", "Read error"), ("fixed", "Fixed data error")):
        entries = re.findall(r"(?m)^" + tag + r":(.*)$", log)
        matches = [re.fullmatch(r"(\d+):d2:restore\.bin: " + description + r" at position (\d+)", entry)
                   for entry in entries]
        require(count > 0 and len(entries) == count and all(matches), "Niepełne lub obce bloki odzysku")
        parsed = [(int(match[1]), int(match[2])) for match in matches]
        require(len({block for block, _ in parsed}) == count
                and {position for _, position in parsed} == set(range(count))
                and all(block < block_limit for block, _ in parsed), "Powtórzone lub obce pozycje odzysku")
        pairs.append(set(parsed))
    require(pairs[0] == pairs[1]
            and re.search(r"(?m)^\s*100% completed, [1-9][0-9]* MB accessed\b", output)
            and re.findall(r"(?m)^status:(.*)$", log) == ["recovered:d2:restore.bin"]
            and not re.search(r"(?m)^(?:parity_fixed|parity_error|unrecoverable):", log)
            and all(re.findall(r"(?m)^summary:" + key + r":(\w+)$", log) == [value]
                    for key, value in (("error", str(count)), ("error_recovered", str(count)),
                                       ("error_unrecoverable", "0"), ("exit", "recovered"))),
            "Brak pełnego dowodu odzysku dysku")
    return count


def flip_byte(path, expected_sha, before):
    descriptor = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
    try:
        actual = os.fstat(descriptor)
        require(stat.S_ISREG(actual.st_mode) and actual.st_nlink == 1 and actual.st_size > 0
                and all(getattr(actual, key) == getattr(before, key)
                        for key in ("st_dev", "st_ino", "st_size", "st_mtime_ns")),
                "Cel korupcji zmienił tożsamość lub metadane")
        with os.fdopen(os.dup(descriptor), "rb") as stream:
            require(hashlib.file_digest(stream, "sha256").hexdigest() == expected_sha,
                    "SHA celu korupcji niezgodne z oryginałem")
        original_byte = os.pread(descriptor, 1, 0)
        require(len(original_byte) == 1, "Nie odczytano bajtu korpusu")
        changed_byte = bytes([original_byte[0] ^ 1])
        require(os.pwrite(descriptor, changed_byte, 0) == 1, "Nie zapisano jednego bajtu")
        os.fsync(descriptor)
        os.utime(descriptor, ns=(before.st_atime_ns, before.st_mtime_ns))
        os.fsync(descriptor)
        after = os.fstat(descriptor)
        require(after.st_size == before.st_size and after.st_mtime_ns == before.st_mtime_ns
                and os.pread(descriptor, 1, 0) == changed_byte, "Korupcja nie zachowała kontraktu bajtu i metadanych")
    finally:
        os.close(descriptor)


def space(path):
    value = os.statvfs(path)
    return {"device": path.stat().st_dev, "free": value.f_bfree * value.f_frsize,
            "available": value.f_bavail * value.f_frsize, "total": value.f_blocks * value.f_frsize,
            "free_inodes": value.f_favail}


def fill_file(descriptor, state_path):
    payload = bytes(range(256)) * (ENOSPC_CHUNK_BYTES // 256 + 1)
    accepted = 0
    started = time.monotonic()
    while accepted < ENOSPC_MAX_BYTES:
        require(time.monotonic() - started < ENOSPC_MAX_SECONDS, "Przekroczony czas fill")
        require(space(state_path)["available"] >= 1024**3, "Brak rezerwy miejsca OS")
        operation = "write"
        requested = min(ENOSPC_CHUNK_BYTES, ENOSPC_MAX_BYTES - accepted)
        try:
            offset = accepted % 256
            size = os.write(descriptor, payload[offset:offset + requested])
            require(0 < size <= requested, "Niepoprawny częściowy zapis")
            accepted += size
            operation = "fsync"
            os.fsync(descriptor)
        except OSError as error:
            require(error.errno == errno.ENOSPC, f"Nieoczekiwany błąd {operation}: errno={error.errno}")
            require(accepted > 0, "ENOSPC przed dodatnim zapisem")
            return {"errno": error.errno, "operation": operation, "accepted_bytes": accepted,
                    "last_requested_bytes": requested}
    raise RuntimeError("Osiągnięto maxbytes bez ENOSPC")


def enospc_allocation(before, after, allocated):
    consumed = before["free"] - after["free"]
    return after["available"] == 0 and abs(consumed - allocated) <= 2 * ENOSPC_CHUNK_BYTES


def automation_guard(identity):
    directory = PACKAGES_ROOT / identity
    for path in (PACKAGES_ROOT, directory):
        private(path, True)
    receipt = directory / "ready.json"
    private(receipt)
    value = json.loads(receipt.read_text())
    require(value["schema"] == 1 and value["uuid"] == identity and value["network"] == "restricted"
            and sorted(value["packages"]) == sorted(("snapraid", "mergerfs", "xfsprogs", "e2fsprogs", "nvme-cli")),
            "Brak potwierdzenia instalacji w odizolowanym gościu")
    for key, pattern in (("units", r"[A-Za-z0-9_@.-]+\.(?:service|timer)"),
                         ("disabled_cron", r"/etc/cron\.[A-Za-z]+/[A-Za-z0-9_.-]+"),
                         ("udev_overrides", r"/etc/udev/rules\.d/[A-Za-z0-9_.-]+\.rules")):
        entries = value[key]
        require(isinstance(entries, list) and entries and len(entries) == len(set(entries))
                and all(isinstance(entry, str) and re.fullmatch(pattern, entry)
                        and ".." not in entry for entry in entries), "Niepoprawny manifest automatyki")
    for unit in value["units"]:
        if "@." in unit:
            require(command(["/usr/bin/systemctl", "is-enabled", "--", unit], 1).strip() == "masked",
                    f"Szablon nie jest zamaskowany: {unit}")
            pattern = unit.replace("@.", "@*.")
            require(not command(["/usr/bin/systemctl", "list-units", "--all", "--plain", "--no-legend",
                                 "--no-pager", "--", pattern]).strip(),
                    f"Załadowana instancja szablonu: {unit}")
            continue
        output = command(["/usr/bin/systemctl", "show", "--property=UnitFileState,ActiveState", "--", unit])
        properties = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
        require(properties == {"UnitFileState": "masked", "ActiveState": "inactive"},
                f"Automatyka nie jest zamaskowana i nieaktywna: {unit}")
    require(all(not Path(path).exists() and not Path(path).is_symlink() for path in value["disabled_cron"]),
            "Przywrócona automatyka cron")
    require(all(Path(path).is_symlink() and os.readlink(path) == "/dev/null"
                for path in value["udev_overrides"]), "Przywrócona automatyka udev")


class Storage:
    def __init__(self, expected, base, state):
        self.expected = expected
        self.base = base
        self.state = state

    def guard(self):
        observation = observe()
        replacement = self.state.get("replacement")
        if replacement is not None:
            require(self.state["stage"] in ("replacement_armed", "replacement_formatting", "replacement_prepared",
                                            "replacement_recovering", "exercised", "enospc_filling"), "Obcy etap replacement")
            require(self.state["format_count"] == (4 if "new_uuid" in replacement else 3), "Obca liczba formatów")
            require(self.state["stage"] not in ("exercised", "enospc_filling") or replacement.get("completed") is True,
                    "Nieukończony replacement")
        found = validate(self.expected, observation, self.state["filesystems"], self.base, replacement)
        automation_guard(self.expected["uuid"])
        return observation, found

    def mounted(self, union_required=True, roles=TARGETS):
        observation, found = self.guard()
        for role in roles:
            path = self.base / "mnt" / role
            require(path.resolve() == path and path.is_dir(), "Obcy punkt montowania")
            matches = [mount for mount in observation["mounts"] if mount["target"] == str(path)]
            require(len(matches) == 1 and matches[0]["maj:min"] == found[role]["maj:min"]
                    and matches[0]["fstype"] == "ext4", f"Brak właściwego mountu {role}")
        if not union_required:
            return observation
        union = [mount for mount in observation["mounts"] if mount["target"] == str(self.base / "union")]
        branches = ":".join(str(self.base / "mnt" / role / "data") for role in ("data1", "data2"))
        require(len(union) == 1 and union[0]["fstype"] == "fuse.mergerfs"
                and union[0]["source"] == branches, "Brak właściwej unii")
        return observation

    def mount(self, union_required=True, roles=TARGETS):
        for role in roles:
            observation, found = self.guard()
            path = self.base / "mnt" / role
            require(path.resolve() == path and path.is_dir(), "Obcy punkt montowania")
            occupants = [mount for mount in observation["mounts"] if mount["target"] == str(path)]
            if occupants:
                require(len(occupants) == 1 and occupants[0]["maj:min"] == found[role]["maj:min"],
                        "Obcy mount docelowy")
            else:
                require(not any(path.iterdir()), "Niepusty katalog zamiast mountu")
                command(["/usr/bin/mount", "-t", "ext4", "-U", self.state["filesystems"][role], str(path)])
        if not union_required:
            self.mounted(False, roles)
            return
        observation, _ = self.guard()
        union = self.base / "union"
        require(union.resolve() == union and union.is_dir(), "Obca ścieżka unii")
        if not any(mount["target"] == str(union) for mount in observation["mounts"]):
            require(not any(union.iterdir()), "Niepusty katalog unii")
            branches = ":".join(str(self.base / "mnt" / role / "data") for role in ("data1", "data2"))
            self.mounted(False)
            require(all((self.base / "mnt" / role / "data").resolve() == self.base / "mnt" / role / "data"
                        for role in ("data1", "data2")), "Dowiązanie gałęzi")
            command(["/usr/bin/mergerfs", "-o", "category.create=mfs,minfreespace=16M,fsname=" + branches,
                     branches, str(union)])
        self.mounted()

    def prepare(self):
        for role in TARGETS:
            _, found = self.guard()
            self.state["format_pending"] = role
            save(self.base, self.state)
            _, found = self.guard()
            command(["/usr/sbin/mkfs.ext4", "-q", "/dev/" + found[role]["name"]])
            actual = observe()
            disk = next(disk for disk in actual["disks"] if disk["serial"] == found[role]["serial"])
            require(disk["fstype"] == "ext4" and disk["uuid"], "mkfs nie utworzył oczekiwanego ext4")
            self.state["filesystems"][role] = str(uuid.UUID(disk["uuid"]))
            self.state["format_count"] += 1
            self.state["format_pending"] = None
            save(self.base, self.state)
        (self.base / "mnt").mkdir(mode=0o700)
        for role in TARGETS:
            (self.base / "mnt" / role).mkdir(mode=0o700)
        self.mount(False)
        for role in ("data1", "data2"):
            self.mounted(False)
            (self.base / "mnt" / role / "data").mkdir(mode=0o700)
        (self.base / "union").mkdir(mode=0o700)
        (self.base / "logs").mkdir(mode=0o700)
        config = (f"parity {self.base}/mnt/parity/snapraid.parity\n"
                  f"content {self.base}/mnt/data1/snapraid.content\n"
                  f"content {self.base}/mnt/data2/snapraid.content\n"
                  f"data d1 {self.base}/mnt/data1/data/\n"
                  f"data d2 {self.base}/mnt/data2/data/\n")
        with (self.base / "snapraid.conf").open("x") as stream:
            stream.write(config)
            stream.flush()
            os.fsync(stream.fileno())
        self.state["config_sha256"] = digest(self.base / "snapraid.conf")
        self.mount()
        self.state["stage"] = "prepared"
        save(self.base, self.state)

    def snap(self, label, args, expected=0):
        self.mounted()
        require(digest(self.base / "snapraid.conf") == self.state["config_sha256"], "Zmieniona konfiguracja")
        log = self.base / "logs" / (label + ".log")
        require(not log.exists() and not log.is_symlink(), "Log operacji już istnieje")
        return command(["/usr/bin/snapraid", "-c", str(self.base / "snapraid.conf"),
                        "-l", str(log)] + args, expected)

    def hashes(self, roles=("data1", "data2"), union_required=True):
        result = {}
        for role in roles:
            branch = self.base / "mnt" / role / "data"
            require(branch.resolve() == branch, "Dowiązanie gałęzi")
            for path in sorted(branch.iterdir()):
                require(path.name not in result, "Powielona ścieżka w unii")
                result[path.name] = {"role": role, "bytes": path.stat().st_size, "sha256": digest(path)}
                require(not union_required or digest(self.base / "union" / path.name) == result[path.name]["sha256"],
                        "Inne dane przez unię")
        return result

    def exercise(self):
        self.mounted()
        require(self.state["stage"] == "prepared", "Exercise jest jednokrotne")
        require(not self.hashes(), "Korpus nie jest pusty")
        self.state["stage"] = "exercising"
        self.state["statvfs"] = {}
        for role, path in (("data1", self.base / "mnt/data1"), ("data2", self.base / "mnt/data2"),
                           ("union", self.base / "union")):
            info = os.statvfs(path)
            self.state["statvfs"][role] = {"fragment_size": info.f_frsize, "blocks": info.f_blocks,
                                          "free": info.f_bfree, "available": info.f_bavail,
                                          "device": path.stat().st_dev}
        save(self.base, self.state)
        for name, size in (("restore.bin", 64), ("second.bin", 67)):
            self.mounted()
            with (self.base / "union" / name).open("xb") as stream:
                for _ in range(size):
                    stream.write(os.urandom(1024**2))
                stream.flush()
                os.fsync(stream.fileno())
        original = self.hashes()
        require(len(original) == 2 and {item["role"] for item in original.values()} == {"data1", "data2"}
                and len({item["sha256"] for item in original.values()}) == 2,
                "Korpus nie obejmuje dwóch różnych gałęzi")
        with (self.base / "original.json").open("x") as stream:
            json.dump(original, stream, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())
        self.state["original_sha256"] = digest(self.base / "original.json")
        save(self.base, self.state)
        print(json.dumps({"original": original, "sha256": self.state["original_sha256"]}),
              file=sys.stderr, flush=True)
        self.snap("01-diff", ["diff"], 2)
        self.snap("02-sync", ["sync"])
        self.snap("03-diff", ["diff"])
        scrub = self.snap("04-scrub", ["-p", "full", "scrub"])
        log = (self.base / "logs" / "04-scrub.log").read_text()
        self.state["scrub_blocks"] = scrub_blocks(scrub, log)
        self.snap("05-check", ["check"])
        require(self.hashes() == original, "Dane zmieniły się przed odzyskaniem")
        self.mounted()
        target = self.base / "union" / "restore.bin"
        require(digest(target) == original["restore.bin"]["sha256"], "Zmieniony cel usunięcia")
        target.unlink()
        require(not target.exists() and all(not (self.base / "mnt" / role / "data" / "restore.bin").exists()
                                           for role in ("data1", "data2")), "Plik nadal istnieje")
        self.snap("06-deleted-diff", ["diff"], 2)
        disk_name = "d1" if original["restore.bin"]["role"] == "data1" else "d2"
        self.snap("07-fix", ["-d", disk_name, "-m", "-f", "/restore.bin", "fix"])
        self.snap("08-check", ["check"])
        require(self.hashes() == original, "Odzyskane SHA niezgodne z oryginałem")
        self.snap("09-sync", ["sync"])
        self.snap("10-diff", ["diff"])
        require(self.hashes() == original, "Niezgodny korpus końcowy")
        self.state["baseline"] = {str(path.relative_to(self.base)): digest(path) for path in (
            self.base / "snapraid.conf", self.base / "mnt/parity/snapraid.parity",
            self.base / "mnt/data1/snapraid.content", self.base / "mnt/data2/snapraid.content")}
        self.state["boot_id"] = self.mounted()["boot_id"]
        self.state["stage"] = "exercised"
        save(self.base, self.state)

    def verify(self):
        replacement = self.state.get("replacement")
        require(self.state["stage"] == "exercised"
                and self.state["format_count"] == (4 if replacement else 3)
                and (not replacement or replacement.get("completed") is True),
                "Brak ukończonego cyklu")
        require(self.guard()[0]["boot_id"] != self.state["boot_id"], "Gość nie został zrestartowany")
        self.mount()
        require(digest(self.base / "original.json") == self.state["original_sha256"], "Zmieniony manifest SHA")
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes() == original, "Dane nie przetrwały restartu")
        require(all(digest(self.base / path) == expected for path, expected in self.state["baseline"].items()),
                "Parity/content/config nie przetrwały restartu")
        self.snap("verify-" + str(uuid.uuid4()), ["check"])


    def replacement_arm(self, operation_id):
        require("replacement" not in self.state and self.state["format_count"] == 3
                and self.state.get("corruption", {}).get("clean_blocks", 0) > 0,
                "Replacement wymaga ukończonej korupcji i jest jednokrotny")
        observation = self.mounted()
        require(digest(self.base / "original.json") == self.state["original_sha256"], "Zmieniony manifest SHA")
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes() == original and {name for name, info in original.items() if info["role"] == "data2"}
                == {"restore.bin"}, "Niezgodny korpus przed odłączeniem")
        require(all(digest(self.base / path) == value for path, value in self.state["baseline"].items()),
                "Zmieniony baseline przed odłączeniem")
        self.state["replacement"] = {
            "operation_id": operation_id, "missing": "data2", "target": "spare",
            "armed_boot_id": observation["boot_id"],
            "before": {"baseline": self.state["baseline"].copy(), "filesystems": self.state["filesystems"].copy(),
                       "boot_id": self.state["boot_id"], "original_sha256": self.state["original_sha256"]}}
        self.state["stage"] = "replacement_armed"
        save(self.base, self.state)

    def replacement_preflight(self):
        observation, found = self.guard()
        require(observation["boot_id"] != self.state["replacement"]["armed_boot_id"],
                "Brak nowego startu po odłączeniu")
        for name in ("mnt/data2", "union"):
            path = self.base / name
            require(path.resolve() == path and path.is_dir()
                    and not any(mount["target"] == str(path) for mount in observation["mounts"])
                    and not any(path.iterdir()), "Obcy mount lub niepusty punkt zastąpienia")
        require(found["data2"]["serial"] == self.expected["disks"]["spare"]["serial"], "Cel nie jest spare")

    def replacement_sources(self):
        self.mounted(False, ("data1", "parity"))
        before = self.state["replacement"]["before"]
        require(digest(self.base / "original.json") == self.state["original_sha256"] == before["original_sha256"],
                "Zmieniony manifest SHA replacement")
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes(("data1",), False) == {name: info for name, info in original.items() if info["role"] == "data1"},
                "Zmieniony ocalały korpus")
        require(all(digest(self.base / path) == before["baseline"][path]
                    for path in ("snapraid.conf", "mnt/data1/snapraid.content", "mnt/parity/snapraid.parity")),
                "Zmienione lub brakujące źródła odzysku")
        return original

    def replacement_prepare(self):
        self.replacement_preflight()
        self.state["stage"] = "replacement_formatting"
        save(self.base, self.state)
        self.mount(False, ("data1", "parity"))
        self.replacement_sources()
        self.state["format_pending"] = "spare"
        save(self.base, self.state)
        self.replacement_preflight()
        self.mounted(False, ("data1", "parity"))
        _, found = self.guard()
        command(["/usr/sbin/mkfs.ext4", "-q", "/dev/" + found["data2"]["name"]])
        observation = observe()
        matches = [disk for disk in observation["disks"] if disk.get("serial") == found["data2"]["serial"]]
        require(len(matches) == 1 and matches[0]["fstype"] == "ext4" and matches[0]["uuid"],
                "mkfs spare nie utworzył oczekiwanego ext4")
        new_uuid = str(uuid.UUID(matches[0]["uuid"]))
        self.state["replacement"]["new_uuid"] = new_uuid
        self.state["filesystems"]["data2"] = new_uuid
        self.state["format_count"] = 4
        self.guard()
        self.state["format_pending"] = None
        save(self.base, self.state)
        self.mount(False)
        self.mounted(False)
        (self.base / "mnt/data2/data").mkdir(mode=0o700)
        self.mount()
        original = self.replacement_sources()
        require(self.hashes() == {name: info for name, info in original.items() if info["role"] == "data1"},
                "Nowy d2 nie jest pusty")
        self.state["stage"] = "replacement_prepared"
        save(self.base, self.state)

    def replacement_recover(self):
        self.mounted()
        original = self.replacement_sources()
        require(self.hashes() == {name: info for name, info in original.items() if info["role"] == "data1"},
                "Nowy d2 nie jest pusty przed fix")
        content = self.base / "mnt/data2/snapraid.content"
        require(not content.exists() and not content.is_symlink(), "Nieoczekiwana kopia content na spare")
        self.state["stage"] = "replacement_recovering"
        save(self.base, self.state)
        output = self.snap("replacement-01-fix", ["-d", "d2", "fix"])
        self.state["replacement"]["recovered_blocks"] = recovered_disk(
            output, (self.base / "logs/replacement-01-fix.log").read_text(),
            {name: info for name, info in original.items() if info["role"] == "data2"}, self.state["scrub_blocks"])
        require(self.hashes() == original, "Odzyskane SHA dysku niezgodne z oryginałem")
        self.snap("replacement-02-check", ["check"])
        self.replacement_sources()
        require(self.hashes() == original, "Dane zmieniły się przed sync replacement")
        self.snap("replacement-03-sync", ["sync"])
        self.snap("replacement-04-diff", ["diff"])
        output = self.snap("replacement-05-scrub", ["-p", "full", "scrub"])
        self.state["replacement"]["clean_blocks"] = scrub_blocks(
            output, (self.base / "logs/replacement-05-scrub.log").read_text())
        self.snap("replacement-06-check", ["check"])
        require(self.hashes() == original and digest(self.base / "original.json") == self.state["original_sha256"],
                "Niezgodny korpus końcowy replacement")
        before = self.state["replacement"]["before"]["baseline"]
        require(all(digest(self.base / path) == before[path] for path in ("snapraid.conf", "mnt/parity/snapraid.parity")),
                "Config/parity zmienione przez replacement")
        self.state["baseline"] = {path: digest(self.base / path) for path in before}
        require(self.state["baseline"]["mnt/data1/snapraid.content"]
                == self.state["baseline"]["mnt/data2/snapraid.content"], "Niezgodne kopie content")
        self.state["boot_id"] = self.mounted()["boot_id"]
        self.state["replacement"]["completed"] = True
        self.state["stage"] = "exercised"
        save(self.base, self.state)


    def enospc_options(self):
        expected = {"branches": ":".join(str(self.base / "mnt" / role / "data") + "=RW" for role in ("data1", "data2")),
                    "category.create": "mfs", "minfreespace": "16777216", "moveonenospc": "false",
                    "nullrw": "false", "cache.files": "libfuse", "cache.writeback": "false",
                    "direct_io": "false", "kernel_cache": "false", "auto_cache": "false", "cache.statfs": "0"}
        options = {key: os.getxattr(self.base / "union/.mergerfs", "user.mergerfs." + key).decode()
                   for key in expected}
        require(options == expected, "Nieoczekiwane runtime opcje mergerfs")
        return options

    def enospc_preflight(self):
        require("enospc" not in self.state and self.state["format_count"] == 4
                and self.state.get("replacement", {}).get("completed") is True,
                "ENOSPC wymaga ukończonego replacement i jest jednokrotny")
        observation = self.mounted()
        original = json.loads((self.base / "original.json").read_text())
        require(digest(self.base / "original.json") == self.state["original_sha256"]
                and self.hashes() == original, "Zmieniony pierwotny korpus ENOSPC")
        require(all(digest(self.base / path) == value for path, value in self.state["baseline"].items()),
                "Zmieniony baseline ENOSPC")
        options = self.enospc_options()
        spaces = {role: space(self.base / "mnt" / role) for role in TARGETS}
        spaces.update(os=space(self.base), union=space(self.base / "union"))
        require(len({spaces[role]["device"] for role in (*TARGETS, "os")}) == 4,
                "Dane i OS nie są różnymi filesystemami")
        require(spaces["os"]["available"] >= 1024**3
                and all(spaces[role]["available"] >= ENOSPC_BRANCH_MIN_BYTES for role in ("data1", "data2"))
                and all(info["free_inodes"] > 8 for info in spaces.values()), "Brak rezerwy miejsca lub inode")
        result = {"boot_id": observation["boot_id"], "options": options, "space": spaces}
        print(json.dumps({"enospc_preflight": result}), file=sys.stderr, flush=True)
        return result

    def enospc_owner(self):
        self.mounted()
        self.enospc_options()
        entry = self.state["enospc"]
        require(entry["name"] == "enospc-" + str(uuid.UUID(entry["operation_id"])) + ".bin", "Obca nazwa fill")
        path = self.base / "mnt/data2/data" / entry["name"]
        info = path.lstat()
        require(path.resolve() == path and stat.S_ISREG(info.st_mode) and info.st_nlink == 1
                and info.st_ino == entry["owner"]["inode"] and info.st_dev == entry["owner"]["device"],
                "Podmieniony właściciel pliku fill")
        other = self.base / "mnt/data1/data" / entry["name"]
        require(not other.exists() and not other.is_symlink(), "Fill pojawił się na data1")
        union_path = self.base / "union" / entry["name"]
        require(os.getxattr(union_path, "user.mergerfs.fullpath").decode() == str(path)
                and os.getxattr(union_path, "user.mergerfs.basepath").decode() == str(path.parent)
                and os.getxattr(union_path, "user.mergerfs.allpaths").decode().rstrip("\0").split("\0") == [str(path)],
                "Inny właściciel fill przez unię")
        return path, info

    def enospc(self):
        measured = self.enospc_preflight()
        identity = str(uuid.uuid4())
        name = f"enospc-{identity}.bin"
        for directory in (self.base / "mnt/data1/data", self.base / "mnt/data2/data", self.base / "union"):
            require(not (directory / name).exists() and not (directory / name).is_symlink(), "Nazwa fill już istnieje")
        entry = {"operation_id": identity, "name": name, "max_bytes": ENOSPC_MAX_BYTES,
                 "before": {"baseline": self.state["baseline"].copy(), "boot_id": self.state["boot_id"],
                            "original_sha256": self.state["original_sha256"], "filesystems": self.state["filesystems"].copy()},
                 "measurement": measured}
        self.state["enospc"] = entry
        self.state["stage"] = "enospc_filling"
        save(self.base, self.state)
        path = self.base / "mnt/data2/data" / name
        try:
            self.mounted()
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            try:
                info = os.fstat(descriptor)
                require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size == 0, "Obcy nowy plik fill")
                entry["owner"] = {"inode": info.st_ino, "device": info.st_dev}
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            flush_directory(path.parent)
            save(self.base, self.state)
            self.enospc_owner()
            descriptor = os.open(self.base / "union" / name, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW)
            try:
                require(os.fstat(descriptor).st_size == 0, "Fill nie jest pusty przy otwarciu unii")
                entry["result"] = fill_file(descriptor, self.base)
            finally:
                os.close(descriptor)
            path, info = self.enospc_owner()
            result = entry["result"]
            result.update(visible_bytes=info.st_size, allocated_bytes=info.st_blocks * 512)
            require(ENOSPC_MIN_BYTES <= info.st_size <= ENOSPC_MAX_BYTES
                    and result["accepted_bytes"] <= info.st_size
                    <= result["accepted_bytes"] + result["last_requested_bytes"]
                    and result["allocated_bytes"] >= ENOSPC_MIN_BYTES, "Brak dowodu rzeczywistej alokacji fill")
            expected = bytes(range(256)) * (ENOSPC_CHUNK_BYTES // 256)
            checksum = hashlib.sha256()
            with path.open("rb") as stream:
                while chunk := stream.read(ENOSPC_CHUNK_BYTES):
                    require(chunk == expected[:len(chunk)], "Niepoprawny fizyczny payload fill")
                    checksum.update(chunk)
            result["visible_sha256"] = checksum.hexdigest()
            result["space"] = {role: space(self.base / "mnt" / role) for role in TARGETS}
            result["space"].update(os=space(self.base), union=space(self.base / "union"))
            require(enospc_allocation(measured["space"]["data2"], result["space"]["data2"], result["allocated_bytes"])
                    and result["space"]["data1"]["available"] >= ENOSPC_BRANCH_MIN_BYTES
                    and result["space"]["union"]["available"] > 0
                    and result["space"]["os"]["available"] >= 1024**3, "Brak dowodu wyczerpania miejsca dostępnego dla zapisu data2")
            save(self.base, self.state)
            print(json.dumps({"enospc_result": result}), file=sys.stderr, flush=True)
        except Exception as error:
            print(json.dumps({"enospc_failure": str(error)}), file=sys.stderr, flush=True)
            raise
        finally:
            if "owner" in entry:
                path, _ = self.enospc_owner()
                (self.base / "union" / name).unlink()
                flush_directory(path.parent)
                require(not path.exists() and not (self.base / "union" / name).exists(), "Fill pozostał po cleanup")
                entry["cleaned"] = True
                save(self.base, self.state)
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes() == original and digest(self.base / "original.json") == entry["before"]["original_sha256"],
                "Zmienione oryginalne dane po cleanup")
        check = self.snap("enospc-check", ["check"])
        require(re.search(r"(?m)^\s*100% completed, [1-9][0-9]* MB accessed\b", check)
                and re.search(r"(?m)^Everything OK$", check), "Niepełny check po ENOSPC")
        require(self.hashes() == original
                and all(digest(self.base / name) == value for name, value in entry["before"]["baseline"].items()),
                "Zmieniony baseline po ENOSPC")
        entry["space_after_cleanup"] = space(self.base / "mnt/data2")
        require(entry["space_after_cleanup"]["available"] >= ENOSPC_BRANCH_MIN_BYTES, "Nieodzyskane miejsce po cleanup")
        self.state["boot_id"] = self.mounted()["boot_id"]
        require(self.state["boot_id"] == measured["boot_id"], "Boot zmieniony podczas ENOSPC")
        entry["completed"] = True
        self.state["stage"] = "exercised"
        save(self.base, self.state)


    def corruption(self):
        require(self.state["stage"] == "exercised" and self.state["format_count"] == 3
                and "corruption" not in self.state, "Korupcja jest jednokrotna i wymaga ukończonego cyklu")
        observation = self.mounted()
        require(digest(self.base / "original.json") == self.state["original_sha256"], "Zmieniony manifest SHA")
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes() == original and "restore.bin" in original, "Niezgodny pierwotny korpus")
        baseline = self.state["baseline"].copy()
        require(all(digest(self.base / path) == expected for path, expected in baseline.items()),
                "Zmieniony baseline przed korupcją")
        role = original["restore.bin"]["role"]
        require(role in ("data1", "data2"), "Obca rola celu korupcji")
        target = self.base / "mnt" / role / "data" / "restore.bin"
        before = target.stat()
        require(before.st_size == original["restore.bin"]["bytes"], "Zmieniony rozmiar celu korupcji")
        self.state["corruption"] = {
            "before": {"baseline": baseline, "boot_id": self.state["boot_id"],
                       "original_sha256": self.state["original_sha256"]},
            "boot_id": observation["boot_id"], "role": role, "offset": 0,
            "bytes": before.st_size, "mtime_ns": before.st_mtime_ns, "atime_ns": before.st_atime_ns,
            "inode": before.st_ino, "device": before.st_dev}
        self.state["stage"] = "corrupting"
        save(self.base, self.state)
        print(json.dumps({"corruption": self.state["corruption"]}), file=sys.stderr, flush=True)
        self.mounted()
        flip_byte(target, original["restore.bin"]["sha256"], before)
        changed = self.hashes()
        require(set(changed) == set(original)
                and changed["restore.bin"]["sha256"] != original["restore.bin"]["sha256"]
                and changed["restore.bin"]["bytes"] == original["restore.bin"]["bytes"]
                and changed["restore.bin"]["role"] == role
                and all(changed[name] == original[name] for name in original if name != "restore.bin"),
                "Korupcja nie jest pojedynczą zmianą znanego pliku")
        self.state["corruption"]["changed_sha256"] = changed["restore.bin"]["sha256"]
        save(self.base, self.state)
        self.snap("corruption-00-diff", ["diff"])
        disk = "d1" if role == "data1" else "d2"
        output = self.snap("corruption-01-scrub", ["-p", "full", "scrub"], 1)
        block = corruption_block(output, (self.base / "logs/corruption-01-scrub.log").read_text(), disk)
        self.state["corruption"]["detected_block"] = block
        save(self.base, self.state)
        self.snap("corruption-02-fix", ["-d", disk, "-f", "/restore.bin", "fix"])
        recovered_block((self.base / "logs/corruption-02-fix.log").read_text(), disk, block)
        self.snap("corruption-03-check", ["check"])
        require(self.hashes() == original, "Odzyskane SHA niezgodne z oryginałem")
        clean = self.snap("corruption-04-scrub", ["-p", "full", "scrub"])
        self.state["corruption"]["clean_blocks"] = scrub_blocks(
            clean, (self.base / "logs/corruption-04-scrub.log").read_text())
        require(self.hashes() == original and digest(self.base / "original.json") == self.state["original_sha256"],
                "Korpus lub pierwotny manifest zmieniony po czystym scrub")
        for path in ("snapraid.conf", "mnt/parity/snapraid.parity"):
            require(digest(self.base / path) == baseline[path], "Config/parity zmienione przez test korupcji")
        self.state["baseline"] = {path: digest(self.base / path) for path in baseline}
        self.state["boot_id"] = self.mounted()["boot_id"]
        require(self.state["boot_id"] == self.state["corruption"]["boot_id"], "Boot zmieniony podczas korupcji")
        self.state["stage"] = "exercised"
        save(self.base, self.state)


def main():
    os.umask(0o077)
    require(os.geteuid() == 0, "Wymagany root wyłącznie prywatnego gościa")
    require(len(sys.argv) == 2, "Wymagany jeden argument JSON")
    raw = sys.argv[1]
    require(len(raw) <= 65536, "Za duży kontrakt")
    request = json.loads(raw)
    expected = contract(request)
    base = STATE_ROOT / expected["uuid"]
    phase = request["phase"]
    if phase in ("preflight", "prepare"):
        validate(expected, observe(), {}, base)
        automation_guard(expected["uuid"])
        require(not base.exists() and not base.is_symlink(), "Prepare nie może być ponawiane")
    if phase == "preflight":
        print(json.dumps({"phase": phase, "result": "ok", "uuid": expected["uuid"]}))
        return
    if phase == "prepare":
        require(STATE_ROOT.parent.resolve() == STATE_ROOT.parent, "Dowiązanie katalogu nadrzędnego")
        STATE_ROOT.mkdir(mode=0o700, exist_ok=True)
        private(STATE_ROOT, True)
        base.mkdir(mode=0o700)
        flush_directory(STATE_ROOT)
        state = {"contract": expected, "stage": "preparing", "filesystems": {},
                 "format_count": 0, "format_pending": None}
        save(base, state)
    private(STATE_ROOT, True)
    private(base, True)
    lock = base / "lock"
    descriptor = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        private(lock)
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        private(base / "state.json")
        state = json.loads((base / "state.json").read_text())
        require(state["contract"] == expected and state["format_pending"] is None,
                "Niepełny lub obcy journal")
        require(state["stage"] == {"prepare": "preparing", "exercise": "prepared", "verify": "exercised",
                                   "corruption": "exercised", "replacement-arm": "exercised",
                                   "replacement-preflight": "replacement_armed", "replacement-prepare": "replacement_armed",
                                   "replacement-recover": "replacement_prepared", "enospc-preflight": "exercised",
                                   "enospc": "exercised"}[phase],
                "Faza nie może być ponawiana lub journal jest niepełny")
        if phase != "replacement-arm":
            require(request.get("replacement_id") == state.get("replacement", {}).get("operation_id"),
                    "Brak lub obcy replacement_id")
        operation = Storage(expected, base, state)
        action = getattr(operation, phase.replace("-", "_"))
        if phase == "replacement-arm":
            action(request["replacement_id"])
        else:
            action()
        print(json.dumps({"phase": phase, "result": "ok", "state": operation.state}, sort_keys=True))
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"Odmowa/błąd storage: {error}", file=sys.stderr)
        sys.exit(1)
