# =============================================================================
# Plik: tests/infra/tentanas-vm/guest_packages.py
# Opis: Instalacja pięciu narzędzi Debian wyłącznie na systemie prywatnej VM.
# Przykład: vm.py bootstrap-packages RUNTIME przekazuje kod i kontrakt przez SSH.
# =============================================================================

import hashlib
import io
import json
import os
import pwd
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
import re
import socket
import ipaddress


PACKAGES = ("snapraid", "mergerfs", "xfsprogs", "e2fsprogs", "nvme-cli")
STORAGE_UNITS = (
    "e2scrub_all.timer", "e2scrub_all.service", "e2scrub_reap.service", "e2scrub@.service",
    "e2scrub_fail@.service", "xfs_scrub_all.timer", "xfs_scrub_all.service",
    "xfs_scrub@.service", "xfs_scrub_fail@.service", "nvmf-autoconnect.service",
    "nvmf-connect-nbft.service", "nvmf-connect@.service", "nvmefc-boot-connections.service",
    "xfs_scrub_all_fail.service", "xfs_scrub_media@.service", "xfs_scrub_media_fail@.service",
)
APT_UNITS = ("apt-daily.timer", "apt-daily-upgrade.timer", "apt-daily.service",
             "apt-daily-upgrade.service")
SOURCES = """Types: deb
URIs: https://deb.debian.org/debian
Suites: trixie trixie-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

Types: deb
URIs: https://security.debian.org/debian-security
Suites: trixie-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def command(args, *, accepted=(0,), timeout=600):
    print(json.dumps({"command": args}), flush=True)
    result = subprocess.run(args, text=True, capture_output=True, timeout=timeout,
                            env={**os.environ, "LC_ALL": "C", "DEBIAN_FRONTEND": "noninteractive"})
    print(json.dumps({"rc": result.returncode, "stdout": result.stdout, "stderr": result.stderr}),
          flush=True)
    require(result.returncode in accepted, f"Polecenie nieudane: {args[0]}, rc={result.returncode}")
    return result


def active(unit):
    if "@." in unit:
        result = command(["systemctl", "list-units", "--all", "--plain", "--no-legend",
                          "--state=active,activating,reloading,deactivating,failed",
                          unit.replace("@.", "@*.")], timeout=15)
        return bool(result.stdout.strip())
    result = command(["systemctl", "is-active", unit], accepted=(0, 3, 4), timeout=15)
    return result.stdout.strip() not in ("inactive", "unknown")


def check_guest(expected_uuid):
    require(str(uuid.UUID(expected_uuid)) == expected_uuid, "Niepoprawny UUID kontraktu")
    actual = Path("/sys/class/dmi/id/product_uuid").read_text().strip().lower()
    require(actual == expected_uuid, "Odmowa: obca VM")
    release = Path("/etc/os-release").read_text()
    require('VERSION_CODENAME=trixie' in release and 'ID=debian' in release, "Nie jest to Debian trixie")
    require(command(["dpkg", "--print-architecture"]).stdout.strip() == "amd64", "Obca architektura")


def check_sources_list(path):
    require(not path.is_symlink(), "Symlink sources.list wymaga przeglądu")
    if path.exists():
        require(path.is_file(), "Nieprawidłowy typ sources.list")
        require(all(not line.strip() or line.lstrip().startswith("#") for line in path.read_text().splitlines()),
                "Dodatkowe aktywne sources.list wymaga przeglądu")


def prepare(directory):
    require(not directory.exists(), "Bootstrap pakietów już podjęty; bez automatycznego ponawiania")
    require(command(["cloud-init", "status", "--wait"]).returncode == 0, "Cloud-init niegotowy")
    require(not command(["dpkg", "--audit"]).stdout.strip(), "dpkg wymaga diagnostyki")
    for unit in ("cron.service", "anacron.service", "crond.service"):
        require(not active(unit), "Aktywny planista cron wymaga odrębnego przeglądu")
    require(shutil.disk_usage("/").free > 1024 ** 3, "Za mało miejsca systemowego na pakiety")
    sources = Path("/etc/apt/sources.list.d/debian.sources")
    require(sources.is_file() and not sources.is_symlink(), "Nieoczekiwany plik źródeł apt")
    check_sources_list(Path("/etc/apt/sources.list"))
    require(set(Path("/etc/apt/sources.list.d").iterdir()) == {sources}, "Dodatkowe źródła apt")
    require(Path("/usr/share/keyrings/debian-archive-keyring.gpg").is_file(), "Brak keyringu Debian")
    command(["dpkg-query", "-W", "debian-archive-keyring", "ca-certificates"])
    directory.mkdir(mode=0o700, parents=True)
    previous = {unit: {"active": active(unit), "masked": command(
        ["systemctl", "is-enabled", unit], accepted=(0, 1, 4)).stdout.strip().startswith("masked")}
        for unit in APT_UNITS}
    (directory / "apt-active.json").write_text(json.dumps(previous))
    shutil.copy2(sources, directory / "debian.sources.original")
    print(json.dumps({"sources_before": sources.read_text(), "sources_after": SOURCES}), flush=True)
    timers = [unit for unit in APT_UNITS + STORAGE_UNITS if unit.endswith(".timer")]
    command(["systemctl", "mask", *timers])
    command(["systemctl", "stop", *timers])
    command(["systemctl", "mask", *[unit for unit in APT_UNITS + STORAGE_UNITS if unit.endswith(".service")]])
    for unit in APT_UNITS + STORAGE_UNITS:
        if unit.endswith(".service"):
            require(not active(unit), f"Usługa {unit} działa; poczekaj zamiast ją zabijać")
    sources.write_text(SOURCES)
    command(["dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${Status}\n"])


def validate_sources():
    sources = Path("/etc/apt/sources.list.d/debian.sources")
    check_sources_list(Path("/etc/apt/sources.list"))
    require(sources.read_text() == SOURCES and set(sources.parent.iterdir()) == {sources}, "Zmienione źródła apt")
    require(not os.environ.get("APT_CONFIG"), "Nieoczekiwany APT_CONFIG")
    validate_apt_config(command(["apt-config", "dump"]).stdout)


def validate_apt_config(config):
    config = config.lower()
    for line in config.splitlines():
        parsed = re.fullmatch(r'\s*(\S+)\s+"([^"]*)";\s*', line)
        if any(key in line for key in ("allowunauthenticated", "allowinsecurerepositories",
                                       "allowdowngradetoinsecurerepositories", "allowweakrepositories")):
            require(parsed and parsed[2] in ("false", "0", "no", "off"), "Osłabiona autentyczność apt")
        if any(key in line for key in ("check-valid-until", "verify-peer", "verify-host")):
            require(parsed and parsed[2] in ("true", "1", "yes", "on"), "Osłabiona weryfikacja apt/TLS")
        if "dir::etc::sourcelist" in line:
            require('"sources.list"' in line, "Obce źródło apt-config")
        if "dir::etc::sourceparts" in line:
            require('"sources.list.d"' in line, "Obcy katalog źródeł apt-config")


def inspect_archives(directory):
    archives = sorted(Path("/var/cache/apt/archives").glob("*.deb"))
    require(archives, "Brak pobranych pakietów")
    overrides = []
    for archive in archives:
        package = command(["dpkg-deb", "--field", str(archive), "Package"]).stdout.strip()
        command(["dpkg-deb", "--info", str(archive)])
        command(["dpkg-deb", "--contents", str(archive)])
        control = subprocess.run(["dpkg-deb", "--ctrl-tarfile", str(archive)],
                                 check=True, capture_output=True)
        with tarfile.open(fileobj=io.BytesIO(control.stdout)) as files:
            for member in files.getmembers():
                if member.isfile() and Path(member.name).name in ("preinst", "postinst", "prerm", "postrm", "triggers"):
                    print(json.dumps({"package": package, "control": member.name,
                                      "content": files.extractfile(member).read().decode()}), flush=True)
        if package not in PACKAGES:
            continue
        payload = subprocess.run(["dpkg-deb", "--fsys-tarfile", str(archive)],
                                 check=True, capture_output=True)
        with tarfile.open(fileobj=io.BytesIO(payload.stdout)) as files:
            for member in files.getmembers():
                name = member.name.removeprefix("./")
                if member.isfile() and ("systemd/system/" in name or "/udev/rules.d/" in name
                                        or name.startswith("etc/cron")):
                    print(json.dumps({"package": package, "automation": name,
                                      "content": files.extractfile(member).read().decode()}), flush=True)
                    if name.endswith((".service", ".timer")):
                        unit = Path(name).name
                        require(unit in STORAGE_UNITS, f"Nieznana automatyka pakietu: {unit}")
                    if package == "nvme-cli" and "/udev/rules.d/" in name:
                        override = Path("/etc/udev/rules.d") / Path(name).name
                        require(not override.exists() and not override.is_symlink(), "Zastana reguła udev")
                        override.symlink_to("/dev/null")
                        overrides.append(str(override))
    receipt = {"archives": {archive.name: hashlib.sha256(archive.read_bytes()).hexdigest() for archive in archives},
               "udev_overrides": overrides, "apt_sources_sha256": hashlib.sha256(SOURCES.encode()).hexdigest()}
    (directory / "downloaded.json").write_text(json.dumps(receipt, indent=2))
    print(json.dumps({"downloaded": receipt}), flush=True)


def download(directory):
    require(directory.is_dir(), "Brak przygotowania bootstrapu")
    validate_sources()
    address = socket.getaddrinfo("deb.debian.org", 443, socket.AF_INET, socket.SOCK_STREAM)[0][4][0]
    require(ipaddress.ip_address(address).is_global, "Endpoint apt nie jest publicznym IP")
    with socket.create_connection((address, 443), timeout=5):
        print(json.dumps({"egress_tcp443": address, "connected": True}), flush=True)
    (directory / "egress-ip").write_text(address)
    command(["apt-get", "-o", "APT::Update::Error-Mode=any", "update"])
    command(["apt-cache", "policy", *PACKAGES])
    simulation = command(["apt-get", "--simulate", "--no-install-recommends", "install", *PACKAGES])
    require(not any(line.startswith("Remv ") for line in simulation.stdout.splitlines()),
            "Transakcja usuwa pakiety")
    command(["apt-get", "--yes", "--download-only", "--no-install-recommends", "--no-remove",
             "install", *PACKAGES])
    inspect_archives(directory)


def install(directory):
    receipt = json.loads((directory / "downloaded.json").read_text())
    validate_sources()
    actual = {archive.name: hashlib.sha256(archive.read_bytes()).hexdigest()
              for archive in Path("/var/cache/apt/archives").glob("*.deb")}
    require(actual == receipt["archives"], "Podmieniony zestaw lub SHA256 archiwów")
    check_automation(STORAGE_UNITS, [], receipt["udev_overrides"])
    require(not (directory / "installation-started").exists(), "Instalacja już podjęta; wymagana diagnostyka")
    (directory / "installation-started").touch(exist_ok=False)
    command(["apt-get", "--yes", "--no-download", "--no-install-recommends", "--no-remove",
             "install", *PACKAGES])
    disabled_cron = []
    for package in PACKAGES:
        listing = command(["dpkg", "-L", package]).stdout.splitlines()
        for name in listing:
            path = Path(name)
            if name.startswith("/etc/cron") and path.is_file():
                destination = directory / (package + "-" + path.name + ".cron-disabled")
                require(not destination.exists(), "Powtórzony wpis cron")
                shutil.move(path, destination)
                disabled_cron.append(name)
    command(["systemctl", "daemon-reload"])
    command(["systemctl", "list-unit-files", "--no-pager", "*e2scrub*", "*xfs_scrub*", "*nvmf*", "*nvmefc*"])
    command(["systemctl", "list-timers", "--all", "--no-pager"])
    require(not command(["dpkg", "--audit"]).stdout.strip(), "Niekompletna instalacja dpkg")
    for unit in STORAGE_UNITS:
        require(not active(unit), "Uruchomiona automatyka storage")
    command(["dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${Architecture}\t${Status}\n", *PACKAGES])
    binaries = (("snapraid", "--version"), ("mergerfs", "--version"), ("mkfs.xfs", "-V"),
                ("mke2fs", "-V"), ("nvme", "version"))
    for binary, flag in binaries:
        path = Path(shutil.which(binary)).resolve()
        print(json.dumps({"binary": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}), flush=True)
        command([str(path), flag])
    (directory / "installed.json").write_text(json.dumps({"units": STORAGE_UNITS,
        "disabled_cron": disabled_cron, "udev_overrides": receipt["udev_overrides"]}, indent=2))


def finalize(directory):
    previous = directory / "apt-active.json"
    if not previous.exists():
        return
    states = json.loads(previous.read_text())
    restore = [unit for unit, state in states.items() if not state["masked"]]
    if restore:
        command(["systemctl", "unmask", *restore])
    for unit, state in states.items():
        if state["active"] and not state["masked"]:
            command(["systemctl", "start", unit])
    print(json.dumps({"installation_complete": (directory / "installed.json").exists()}), flush=True)


def seal(directory, vm_uuid):
    require((directory / "probe-complete").is_file(), "Brak poprawnej sondy nieuprzywilejowanej")
    receipt = json.loads((directory / "installed.json").read_text())
    check_automation(receipt["units"], receipt["disabled_cron"], receipt["udev_overrides"])
    receipt.update({"schema": 1, "uuid": vm_uuid, "packages": PACKAGES, "network": "restricted"})
    with (directory / "ready.json").open("x") as output:
        json.dump(receipt, output, indent=2)
    print(json.dumps({"ready": receipt}), flush=True)


def check_automation(units, disabled_cron, overrides):
    for scheduler in ("cron.service", "anacron.service", "crond.service"):
        require(not active(scheduler), "Aktywny planista cron wymaga przeglądu")
    for unit in units:
        require(command(["systemctl", "is-enabled", unit], accepted=(0, 1)).stdout.strip() == "masked",
                f"Brak maski jednostki {unit}")
        require(not active(unit), f"Jednostka {unit} jest aktywna")
    require(not any(Path(name).exists() or Path(name).is_symlink() for name in disabled_cron), "Cron storage nadal obecny")
    for name in overrides:
        require(Path(name).is_symlink() and os.readlink(name) == "/dev/null", "Brak blokady udev")


def verify_network(directory):
    address = (directory / "egress-ip").read_text().strip()
    require(ipaddress.ip_address(address).is_global, "Sonda izolacji wymaga tego samego publicznego IP")
    try:
        with socket.create_connection((address, 443), timeout=3):
            raise RuntimeError("Egress nadal otwarty mimo żądanej izolacji")
    except (TimeoutError, ConnectionRefusedError, OSError) as error:
        print(json.dumps({"isolated_tcp443": address, "connected": False, "error": str(error)}), flush=True)


def probe():
    require(os.getuid() != 0, "Sonda status ma działać jako konto testowe bez sudo")
    print(json.dumps({"probe_uid": os.getuid()}), flush=True)
    with tempfile.TemporaryDirectory(prefix="tentanas-snapraid-probe-", dir=pwd.getpwuid(os.getuid()).pw_dir) as value:
        directory = Path(value)
        (directory / "data").mkdir()
        config = directory / "snapraid.conf"
        config.write_text(f"parity {directory}/parity\ncontent {directory}/content\ndata probe {directory}/data\n")
        result = command(["snapraid", "-c", str(config), "status"], accepted=(0, 1), timeout=15)
        diagnostic = result.stdout + result.stderr
        require(result.returncode == 1 and "at least 2 'content' files in different disks" in diagnostic,
                "Nierozpoznana diagnostyka status")
        require(not (directory / "parity").exists() and not (directory / "content").exists()
                and not list((directory / "data").iterdir()), "Sonda zmieniła dane")


def main():
    os.umask(0o077)
    contract = json.loads(sys.argv[1])
    phase, vm_uuid = contract["phase"], contract["uuid"]
    check_guest(vm_uuid)
    require(os.getuid() == 0, "Operacja pakietowa wymaga root wyłącznie gościa")
    directory = Path("/var/lib/tentanas-vm-packages") / vm_uuid
    if phase == "prepare":
        prepare(directory)
    elif phase == "download":
        download(directory)
    elif phase == "install":
        install(directory)
    elif phase == "finalize":
        finalize(directory)
    elif phase == "probe":
        account = pwd.getpwnam("tentanas")
        require(account.pw_uid > 0, "Konto sondy nie może być root")
        child = os.fork()
        if child == 0:
            try:
                os.setgroups([])
                os.setgid(account.pw_gid)
                os.setuid(account.pw_uid)
                probe()
            except Exception as error:
                print(str(error), file=sys.stderr, flush=True)
                os._exit(1)
            os._exit(0)
        _, status = os.waitpid(child, 0)
        require(os.waitstatus_to_exitcode(status) == 0, "Nieudana sonda jako konto testowe")
        (directory / "probe-complete").touch(exist_ok=False)
    elif phase == "seal":
        seal(directory, vm_uuid)
    elif phase == "verify-network":
        verify_network(directory)
    else:
        raise RuntimeError("Nieznana faza pakietów")


if __name__ == "__main__":
    main()
