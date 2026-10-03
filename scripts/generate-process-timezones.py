#!/usr/bin/env python3
# ============ File: generate-process-timezones.py — reproduce the reviewed finite IANA offset dataset offline ============

import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import tempfile


RELEASE = "2026e"
SOURCE_SHA256 = "b26882805f26aac59d5b222978e6580484b834ccdc98be89df2f05a6dc53a652"
COMPILER_SHA256 = "05925eb7afffea7c53ee5561497e4d17852f6d0bb1c3a76ca452b3bc846c0934"
DATASET_SHA256 = "17ca1435b3eebef27b70e8964b4e0347c41fbbcda938249f4e816c885662dc0b"
START = 1703980800
END = 2240697600
SOURCES = ("africa", "antarctica", "asia", "australasia", "backward", "etcetera", "europe", "northamerica", "southamerica")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def write_changed(path, data):
    if path.is_file() and path.read_bytes() == data:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def tzif_offsets(data, start, end):
    require(data[:5] in (b"TZif2", b"TZif3"), "expected a complete TZif v2/v3 file")
    counts = struct.unpack_from(">6I", data, 20)
    gmt, standard, leap, times, types, chars = counts
    second = 44 + times * 5 + types * 6 + chars + leap * 8 + standard + gmt
    require(data[second:second + 4] == b"TZif", "missing TZif64 header")
    gmt, standard, leap, times, types, chars = struct.unpack_from(">6I", data, second + 20)
    require(0 < types <= 256 and 0 < times < 1000 and leap == 0, "unsupported TZif layout")
    cursor = second + 44
    transitions = struct.unpack_from(f">{times}q", data, cursor)
    cursor += times * 8
    indices = data[cursor:cursor + times]
    cursor += times
    offsets = [struct.unpack_from(">iBB", data, cursor + i * 6)[0] for i in range(types)]
    cursor += types * 6 + chars + leap * 12 + standard + gmt
    require(data[cursor:] == b"\n\n", "finite output must not retain an implicit POSIX tail")
    require(transitions[-1] == end, "TZif64 lacks the requested upper sentinel")
    require(all(a < b for a, b in zip(transitions, transitions[1:])), "unordered TZif records")
    require(all(i < types for i in indices), "invalid TZif type index")
    initial = offsets[0]
    for at, index in zip(transitions, indices):
        if at <= start:
            initial = offsets[index]
    require(-86400 < initial < 86400, "invalid initial UTC offset")
    current = initial
    changes = []
    for at, index in zip(transitions, indices):
        offset = offsets[index]
        require(-86400 < offset < 86400, "invalid UTC offset")
        if start < at < end and offset != current:
            changes.append({"at_utc_ms": at * 1000, "offset_seconds": offset})
            current = offset
    require(len(changes) <= 128, "zone exceeds the pin transition budget")
    return {"initial_offset_seconds": initial, "transitions": changes}


def generate(args):
    archive = args.archive.read_bytes()
    require(sha256(archive) == SOURCE_SHA256, "official tzdata archive checksum mismatch")
    require(sha256(args.zic.read_bytes()) == COMPILER_SHA256, "reviewed zic compiler checksum mismatch")
    version = subprocess.run([str(args.zic), "--version"], check=True, capture_output=True, text=True).stdout.strip()
    require(version == "zic (tzcode) 2022g", "unexpected compiler version")
    require(args.work_dir.is_dir(), "an existing durable work directory is required")
    with tempfile.TemporaryDirectory(prefix="process-timezones-", dir=args.work_dir) as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir()
        names = set()
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as bundle:
            for name in (*SOURCES, "version", "LICENSE"):
                member = bundle.getmember(name)
                require(member.isfile() and member.size < 2 * 1024 * 1024, f"invalid archive member {name}")
                content = bundle.extractfile(member).read()
                (source / name).write_bytes(content)
                if name in SOURCES:
                    for line in content.decode().splitlines():
                        fields = line.split()
                        if fields and fields[0] == "Zone":
                            names.add(fields[1])
                        elif fields and fields[0] == "Link":
                            names.add(fields[2])
        require((source / "version").read_text().strip() == RELEASE, "archive release mismatch")
        require(len(names) == 597, "expected the complete reviewed 597-name set")
        require(all(re.fullmatch(r"[A-Za-z0-9_+.-]+(?:/[A-Za-z0-9_+.-]+)*", name) and ".." not in name for name in names), "invalid zone name")
        output = root / "tzif"
        command = [str(args.zic), "-b", "fat", "-r", f"@{START}/@{END}", "-d", str(output), *[str(source / name) for name in SOURCES]]
        completed = subprocess.run(command, check=True, capture_output=True, text=True)
        require(not completed.stderr, f"zic diagnostics: {completed.stderr}")
        zones = {}
        for name in sorted(names):
            path = output / name
            require(path.is_file() and not path.is_symlink(), f"missing regular TZif for {name}")
            zones[name] = tzif_offsets(path.read_bytes(), START, END)
        data = {"tzdb_release": RELEASE, "valid_from_utc_ms": START * 1000, "valid_until_utc_ms": END * 1000, "zones": zones}
        encoded = json.dumps(data, ensure_ascii=False, separators=(",", ":")).encode()
        require(sha256(encoded) == DATASET_SHA256, "complete numeric output differs from the reviewed dataset")
        manifest = {
            "release_id": RELEASE,
            "source_url": f"https://data.iana.org/time-zones/releases/tzdata{RELEASE}.tar.gz",
            "source_sha256": SOURCE_SHA256,
            "dataset_sha256": DATASET_SHA256,
            "horizon_start_ms": START * 1000,
            "horizon_end_ms": END * 1000,
            "zone_count": len(zones),
            "names_sha256": sha256("\n".join(sorted(zones)).encode()),
            "compiler_version": version,
            "compiler_sha256": COMPILER_SHA256,
            "arguments": ["-b", "fat", "-r", f"@{START}/@{END}"],
        }
        write_changed(args.output / "zones.json", encoded)
        write_changed(args.output / "manifest.json", (json.dumps(manifest, indent=2) + "\n").encode())
        write_changed(args.output / "LICENSE", (source / "LICENSE").read_bytes())
        historical_start, historical_end = 1325116800, 1325289600
        historical = root / "historical-tzif"
        subprocess.run([str(args.zic), "-b", "fat", "-r", f"@{historical_start}/@{historical_end}", "-d", str(historical), str(source / "australasia")], check=True, capture_output=True)
        apia = {
            "iana_name": "Pacific/Apia", "source_sha256": SOURCE_SHA256,
            "horizon_start_ms": historical_start * 1000, "horizon_end_ms": historical_end * 1000,
            **tzif_offsets((historical / "Pacific/Apia").read_bytes(), historical_start, historical_end),
        }
        write_changed(args.output / "apia-2011-test.json", (json.dumps(apia, indent=2) + "\n").encode())
        return {"release": RELEASE, "zones": len(zones), "bytes": len(encoded), "sha256": sha256(encoded), "max_transitions": max(len(zone["transitions"]) for zone in zones.values())}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--zic", required=True, type=Path)
    parser.add_argument("--work-dir", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    print(json.dumps(generate(parser.parse_args()), sort_keys=True))
