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


STATE_ROOT = Path("/var/lib/tentanas-vm-storage")
PACKAGES_ROOT = Path("/var/lib/tentanas-vm-packages")
SIZES = {"os": 12, "data1": 1, "data2": 1, "parity": 2, "cache": 1, "spare": 1}
TARGETS = ("data1", "data2", "parity")
PHASES = ("preflight", "prepare", "exercise", "verify", "corruption")


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
    require(set(value) == {"phase", "uuid", "disks"} and value["phase"] in PHASES,
            "Niepoprawny kontrakt fazy")
    identity = str(uuid.UUID(value["uuid"]))
    require(identity == value["uuid"], "Niekanoniczny UUID")
    prefix = uuid.UUID(identity).hex[:10]
    expected = {role: {"serial": f"tn-{prefix}-{role}", "bytes": size * 1024**3}
                for role, size in SIZES.items()}
    require(value["disks"] == expected, "Kontrakt dysków różni się od profilu V01")
    return {"uuid": identity, "disks": expected}


def validate(expected, observation, filesystems, base):
    require(observation["uuid"] == expected["uuid"], "Niewłaściwy UUID gościa")
    disks = [disk for disk in observation["disks"] if disk["type"] != "rom"]
    require(len(disks) == 6 and all(disk["type"] == "disk" for disk in disks),
            "Nieoczekiwany zestaw całych dysków")
    require(len({disk["serial"] for disk in disks}) == 6
            and len({disk["maj:min"] for disk in disks}) == 6, "Nieunikalny serial lub urządzenie")
    roots = [mount for mount in observation["mounts"] if mount["target"] == "/"]
    require(len(roots) == 1, "Nieznany dysk systemowy")
    found = {}
    for role, wanted in expected["disks"].items():
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
        found = validate(self.expected, observation, self.state["filesystems"], self.base)
        automation_guard(self.expected["uuid"])
        return observation, found

    def mounted(self, union_required=True):
        observation, found = self.guard()
        for role in TARGETS:
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

    def mount(self, union_required=True):
        for role in TARGETS:
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
            self.mounted(False)
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

    def hashes(self):
        result = {}
        for role in ("data1", "data2"):
            branch = self.base / "mnt" / role / "data"
            require(branch.resolve() == branch, "Dowiązanie gałęzi")
            for path in sorted(branch.iterdir()):
                require(path.name not in result, "Powielona ścieżka w unii")
                result[path.name] = {"role": role, "bytes": path.stat().st_size, "sha256": digest(path)}
                require(digest(self.base / "union" / path.name) == result[path.name]["sha256"],
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
        require(self.state["stage"] == "exercised" and self.state["format_count"] == 3,
                "Brak ukończonego cyklu")
        require(self.guard()[0]["boot_id"] != self.state["boot_id"], "Gość nie został zrestartowany")
        self.mount()
        require(digest(self.base / "original.json") == self.state["original_sha256"], "Zmieniony manifest SHA")
        original = json.loads((self.base / "original.json").read_text())
        require(self.hashes() == original, "Dane nie przetrwały restartu")
        require(all(digest(self.base / path) == expected for path, expected in self.state["baseline"].items()),
                "Parity/content/config nie przetrwały restartu")
        self.snap("verify-" + str(uuid.uuid4()), ["check"])


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
                                   "corruption": "exercised"}[phase],
                "Faza nie może być ponawiana lub journal jest niepełny")
        operation = Storage(expected, base, state)
        getattr(operation, phase)()
        print(json.dumps({"phase": phase, "result": "ok", "state": operation.state}, sort_keys=True))
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"Odmowa/błąd storage: {error}", file=sys.stderr)
        sys.exit(1)
