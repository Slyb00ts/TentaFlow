# =============================================================================
# Plik: tests/infra/tentanas-vm/guest_probe.py
# Opis: Odczyt UUID VM, całych dysków i ich zależności od systemu plików root.
# Przykład: vm.py inventory /mnt/d/repos/tentanas-vm.ABC123 przesyła sondę przez SSH.
# =============================================================================

import json
from pathlib import Path
import subprocess


def descendants(node):
    yield node
    for child in node.get("children", []):
        yield from descendants(child)


def probe():
    result = subprocess.run(
        ["lsblk", "--json", "--bytes", "--output", "NAME,SIZE,TYPE,SERIAL,FSTYPE,MOUNTPOINTS,MAJ:MIN"],
        check=True, text=True, capture_output=True)
    nodes = json.loads(result.stdout)["blockdevices"]
    disks = []
    for node in nodes:
        if node["type"] == "rom":
            continue
        tree = list(descendants(node))
        disks.append({"name": node["name"], "size": node["size"], "type": node["type"],
                      "serial": (node["serial"] or "").strip(), "major_minor": node["maj:min"],
                      "contains_root": any("/" in (child["mountpoints"] or []) for child in tree),
                      "used": any(child["fstype"] or any(child["mountpoints"] or [])
                                  or child["type"] != "disk" for child in tree)})
    return {"uuid": Path("/sys/class/dmi/id/product_uuid").read_text().strip(), "disks": disks}


if __name__ == "__main__":
    print(json.dumps(probe()))
