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


QEMU = "/usr/bin/qemu-system-x86_64"
QEMU_IMG = "/usr/bin/qemu-img"
MACHINE = "q35,accel=kvm,smm=off"
IMAGE_URL = "https://cloud.debian.org/images/cloud/trixie/20260831-2587/debian-13-generic-amd64-20260831-2587.qcow2"
IMAGE_SHA512 = "5a069019420fb9441ad4f8004c661fadb747edd5662ca54a17c8f923dee7d717e21dbdaa4ba72d6fce7f920e0217f0a9af382298a7d46ed4bc9dc33ac19181b6"
GIB = 1024 ** 3
DISKS = {"os": 12, "data1": 1, "data2": 1, "parity": 2, "cache": 1, "spare": 1}


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


def save_state(path, state):
    temp = path / "state.next"
    write_new(temp, json.dumps(state, indent=2) + "\n")
    temp.replace(path / "state.json")


def disk_manifest(vm_uuid):
    prefix = uuid.UUID(vm_uuid).hex[:10]
    return {role: {"serial": f"tn-{prefix}-{role}", "bytes": size * GIB}
            for role, size in DISKS.items()}


def load_manifest(path):
    private_file(path / "manifest.json")
    manifest = json.loads((path / "manifest.json").read_text())
    require(manifest["schema"] == 1 and manifest["uid"] == os.getuid()
            and manifest["runtime"] == str(path), "Obcy manifest runtime")
    require(str(uuid.UUID(manifest["uuid"])) == manifest["uuid"], "Niepoprawny UUID")
    require(manifest["disks"] == disk_manifest(manifest["uuid"]),
            "Zmieniona mapa ról, seriali lub wielkości dysków")
    require(manifest["image_url"] == IMAGE_URL and manifest["image_sha512"] == IMAGE_SHA512,
            "Manifest nie używa zatwierdzonego obrazu")
    require(manifest["machine"] == MACHINE, "Niezgodny profil maszyny")
    require(type(manifest["ssh_port"]) is int and 1024 < manifest["ssh_port"] < 65536,
            "Niepoprawny port SSH")
    return manifest


@contextlib.contextmanager
def locked_runtime(value):
    path = runtime_path(value)
    private_file(path / "lock")
    with (path / "lock").open("r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield path, load_manifest(path)


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


def create():
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
    manifest = {"schema": 1, "uid": os.getuid(), "runtime": str(path), "uuid": vm_uuid,
                "ssh_port": port, "image_url": IMAGE_URL, "image_sha512": IMAGE_SHA512,
                "disks": disk_manifest(vm_uuid), "machine": MACHINE}
    image = path / "download.qcow2"
    run(["/usr/bin/curl", "--fail", "--location", "--silent", "--show-error",
         "--proto", "=https", "--proto-redir", "=https", "--max-time", "1800",
         "--output", str(image), IMAGE_URL])
    verify_download(image)
    require(json.loads(run([QEMU_IMG, "info", "--output=json", str(image)],
                           capture_output=True).stdout)["virtual-size"] <= DISKS["os"] * GIB,
            "Obraz bazowy większy od dysku systemowego; nie wolno go zmniejszyć")
    run([QEMU_IMG, "convert", "-f", "qcow2", "-O", "qcow2", str(image), str(path / "os.qcow2")])
    run([QEMU_IMG, "resize", "-f", "qcow2", str(path / "os.qcow2"), f"{DISKS['os']}G"])
    for role, size in DISKS.items():
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
    manifest["image_inodes"] = {role: [info.st_dev, info.st_ino] for role in DISKS
                                for info in [private_file(path / f"{role}.qcow2")]}
    write_new(path / "manifest.json", json.dumps(manifest, indent=2) + "\n")
    save_state(path, {"status": "prepared"})
    check_images(path, manifest)
    print(path)


def qemu_command(path, manifest):
    args = [QEMU, "-machine", MACHINE, "-cpu", "host", "-smp", "2", "-m", "4096",
            "-name", f"tentanas-{manifest['uuid']}", "-uuid", manifest["uuid"],
            "-nodefaults", "-display", "none", "-monitor", "none",
            "-daemonize", "-pidfile", str(path / "qemu.pid"),
            "-qmp", f"unix:{path}/qmp.sock,server=on,wait=off",
            "-serial", f"file:{path}/serial.log",
            "-netdev", f"user,id=net0,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:{manifest['ssh_port']}-:22",
            "-device", "virtio-net-pci,netdev=net0"]
    for role, disk in manifest["disks"].items():
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


def running_identity(path, manifest, state):
    require(state["status"] == "running", "VM nie ma potwierdzonego działającego procesu")
    actual = process_identity(state["process"]["pid"])
    require(actual == state["process"] and actual["executable"] == str(Path(QEMU).resolve())
            and actual["argv"] == qemu_command(path, manifest),
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
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(endpoint))
        with connection.makefile("rwb") as stream:
            require("QMP" in json.loads(stream.readline()), "Brak powitania QMP")

            def execute(name):
                stream.write((json.dumps({"execute": name, "id": name}) + "\n").encode())
                stream.flush()
                while True:
                    message = json.loads(stream.readline())
                    if message.get("id") == name:
                        require("error" not in message, f"QMP odrzucił {name}")
                        return message["return"]

            execute("qmp_capabilities")
            require(execute("query-uuid")["UUID"] == manifest["uuid"], "Obcy UUID QMP")
            return execute(command)


def read_state(path):
    private_file(path / "state.json")
    return json.loads((path / "state.json").read_text())


def start(path, manifest):
    state = read_state(path)
    require(state["status"] in ("prepared", "stopped"), "VM już działa lub wymaga diagnostyki")
    for name in ("qemu.pid", "qmp.sock"):
        require(not (path / name).exists() and not (path / name).is_symlink(),
                "Pozostała tożsamość procesu; odmowa nowego bootu")
    if (path / "serial.log").exists() or (path / "serial.log").is_symlink():
        private_file(path / "serial.log")
    else:
        write_new(path / "serial.log", "")
    check_images(path, manifest)
    save_state(path, {"status": "starting"})
    run(qemu_command(path, manifest))
    private_file(path / "qemu.pid")
    identity = process_identity(int((path / "qemu.pid").read_text().strip()))
    state = {"status": "running", "process": identity}
    running_identity(path, manifest, state)
    save_state(path, state)
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
    require(len(disks) == len(DISKS), "Nieoczekiwane dyski gościa")
    by_serial = {disk["serial"]: disk for disk in disks}
    require(len(by_serial) == len(DISKS), "Powtórzony serial dysku")
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
    require(state["status"] == "running", "Brak zapisanej tożsamości VM do zatrzymania")
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
    save_state(path, {"status": "stopped", "previous_process": state["process"]})
    print("VM zatrzymana; wszystkie obrazy i klucze pozostają w prywatnym runtime")


def main():
    os.umask(0o077)
    require(os.getuid() != 0, "Nie uruchamiaj harnessu jako root hosta")
    parser = argparse.ArgumentParser(description="Prywatna VM TentaNas bez hostowych dysków")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("create", help="Nowy runtime, pobranie i weryfikacja; bez bootu")
    for name in ("start", "stop", "status", "inventory", "ssh"):
        command = commands.add_parser(name)
        command.add_argument("runtime")
        if name == "ssh":
            command.add_argument("guest_command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command == "create":
        create()
        return
    with locked_runtime(args.runtime) as (path, manifest):
        if args.command == "start":
            start(path, manifest)
        elif args.command == "stop":
            stop(path, manifest)
        elif args.command == "inventory":
            inventory(path, manifest)
        elif args.command == "status":
            state = read_state(path)
            if state["status"] == "running":
                running_identity(path, manifest, state)
                state["qmp"] = qmp(path, manifest, "query-status")
            print(json.dumps(state, indent=2))
        else:
            running_identity(path, manifest, read_state(path))
            require(args.guest_command, "Podaj jawne polecenie wykonywane wyłącznie w gościu")
            run(ssh_command(path, manifest) + [shlex.join(args.guest_command)])


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"Odmowa/błąd VM: {error}", file=sys.stderr)
        sys.exit(1)
