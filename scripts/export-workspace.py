#!/usr/bin/env python3
# =============================================================================
# Plik: export-workspace.py
# Opis: Eksportuje archiwum podzbioru workspace z centralnego katalogu zależności.
# Przykład: python3 scripts/export-workspace.py --output /tmp/context --archive /tmp/context.tar.gz
# =============================================================================

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import stat

if sys.version_info < (3, 11):
    sys.exit("Eksport workspace wymaga Python 3.11 lub nowszego.")

import tomllib

CONTAINER_MEMBERS = [
    "tentaflow-containers/agents/native/coding-agent-bridge",
    "tentaflow-containers/agents/native/teams-bot",
    "tentaflow-containers/sidecar",
]
EXTRA_FILES = [
    "tentaflow-core/www/js/components/tf-face.js",
    "tentaflow-core/www/js/lib/face-speech.js",
    "tentaflow-core/www/js/data/face-data.js",
    "tentaflow-core/www/js/data/face-edges.js",
    "scripts/export-workspace.py",
]


def is_link(path):
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISLNK(metadata.st_mode) or bool(getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def reject_links(path):
    path = Path(os.path.abspath(path))
    if any(is_link(parent) for parent in [path, *path.parents]):
        raise ValueError(f"Dowiązanie w ścieżce eksportu: {path}")


def load_manifest(path):
    reject_links(path)
    return tomllib.loads(path.read_text(encoding="utf-8"))


def member_closure(root, members):
    catalog = load_manifest(root / "Cargo.toml")["workspace"]["dependencies"]
    pending = [root / member for member in members]
    found = set()
    while pending:
        folder = pending.pop()
        reject_links(folder)
        folder = folder.resolve()
        if not folder.is_relative_to(root):
            raise ValueError(f"Zależność ścieżkowa wychodzi poza workspace: {folder}")
        if folder in found:
            continue
        manifest = load_manifest(folder / "Cargo.toml")
        found.add(folder)
        tables = [manifest, *manifest.get("target", {}).values()]
        for table in tables:
            for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
                for name, dependency in table.get(kind, {}).items():
                    if not isinstance(dependency, dict):
                        continue
                    base = folder
                    if dependency.get("workspace"):
                        dependency = catalog[name]
                        base = root
                    if isinstance(dependency, dict) and "path" in dependency:
                        pending.append(base / dependency["path"])
    return sorted(str(path.relative_to(root)).replace("\\", "/") for path in found)


def exported_manifest(root, members):
    source = (root / "Cargo.toml").read_text(encoding="utf-8")
    workspace = load_manifest(root / "Cargo.toml")["workspace"]
    settings = {"resolver": workspace["resolver"], "members": members, "default-members": members}
    if "exclude" in workspace:
        settings["exclude"] = workspace["exclude"]
    header = "[workspace]\n" + "".join(f"{key} = {json.dumps(value, ensure_ascii=False)}\n" for key, value in settings.items()) + "\n"
    result, count = re.subn(r"(?ms)^\[workspace\]\s*\n.*?(?=^\[|\Z)", lambda _: header, source, count=1)
    if count != 1:
        raise ValueError("Nie znaleziono głównej tabeli [workspace]")
    return result


def include_source(info):
    parts = Path(info.name).parts
    if len(parts) > 1 and parts[-1] == "Cargo.lock":
        return None
    for name in parts:
        if name in ("target", "node_modules", ".git", ".venv", "venv", "__pycache__", "bundle-instances", "bundle-templates") or name.startswith(("target-", "target_", ".build-")):
            return None
        if name == ".env" or (name.startswith(".env.") and name not in (".env.example", ".env.sample", ".env.template")):
            return None
    if info.name.endswith((".pyc", ".pth", ".onnx", ".gguf", ".safetensors")):
        return None
    if info.name == "tentaflow-containers/output" or info.name.startswith("tentaflow-containers/output/"):
        return None
    if info.issym() or info.islnk():
        link = Path(info.linkname)
        if link.is_absolute() or ".." in link.parts:
            raise ValueError(f"Dowiązanie poza eksportowanym drzewem: {info.name}")
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info


def copy_sources(root, source, destination):
    reject_links(source)
    reject_links(destination)
    def ignored(directory, names):
        skipped = []
        for name in names:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if include_source(tarfile.TarInfo(relative)) is None:
                skipped.append(name)
            elif is_link(path):
                raise ValueError(f"Zrodla eksportu zawieraja dowiazanie: {path}")
        return skipped
    shutil.copytree(source, destination, ignore=ignored)


def export_workspace(root, output, members, archive=None, offline=True, in_place=False):
    if archive is None and not in_place:
        raise ValueError("Wymagane --archive albo --in-place; sam katalog metadanych nie jest workspace źródłowym")
    for path in [root, output, *([archive] if archive is not None else [])]:
        reject_links(path)
    root = root.resolve()
    output = output.resolve()
    if root.is_relative_to(output) and root != output:
        raise ValueError("Katalog eksportu nie moze byc rodzicem workspace")
    if output == root:
        if not in_place or (root / ".git").exists():
            raise ValueError("Eksport w miejscu wymaga --in-place i przygotowanego kontekstu poza repo")
    elif output.is_relative_to(root) and not output.relative_to(root).parts[0].startswith("target"):
        raise ValueError("Eksport wewnatrz repo moze trafic tylko do katalogu target")
    if output.exists() and any(output.iterdir()) and output != root and not (output / ".tentaflow-workspace-export").is_file():
        raise ValueError("Katalog eksportu zawiera pliki niezarzadzane przez eksporter")
    closure = member_closure(root, members)
    manifest = exported_manifest(root, closure)
    original_packages = load_manifest(root / "Cargo.lock")["package"]
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="workspace-source-", dir=output) as temporary:
        staging = root if output == root else Path(temporary)
        if staging != root:
            for member in [*closure, "vendor"]:
                if (root / member).exists():
                    copy_sources(root, root / member, staging / member)
            shutil.copy2(root / "Cargo.lock", staging / "Cargo.lock")
        (staging / "Cargo.toml").write_text(manifest, encoding="utf-8")
        # Eksport wymaga podzbioru lockfile, nie pobrania źródeł wszystkich targetów.
        command = ["cargo", "update", "--workspace", "--quiet"]
        if offline:
            command.append("--offline")
        subprocess.run(command, cwd=staging, env=dict(os.environ, CARGO_TARGET_DIR=str(staging / "target")), check=True, stdout=subprocess.DEVNULL)
        allowed = {(p["name"], p["version"], p.get("source"), p.get("checksum")) for p in original_packages}
        resolved = load_manifest(staging / "Cargo.lock")["package"]
        unexpected = [(p["name"], p["version"]) for p in resolved if (p["name"], p["version"], p.get("source"), p.get("checksum")) not in allowed]
        if unexpected:
            raise ValueError(f"Eksport wymaga wersji spoza glownego Cargo.lock: {unexpected}")
        if staging != root:
            for name in ("Cargo.toml", "Cargo.lock"):
                shutil.copy2(staging / name, output / name)
    (output / ".tentaflow-workspace-export").touch()
    if archive is not None:
        container_export = any(member.startswith("tentaflow-containers/") for member in members)
        roots = ["vendor", "scripts/export-workspace.py"]
        if container_export:
            roots.extend(["tentaflow-containers", *[name for name in EXTRA_FILES if name != "scripts/export-workspace.py"]])
        roots.extend(member for member in closure if not container_export or not member.startswith("tentaflow-containers/"))
        temporary = archive.with_suffix(archive.suffix + ".tmp")
        reject_links(temporary)
        def filter_archive(info):
            selected = include_source(info)
            if selected is not None:
                source_root = output if info.name in ("Cargo.toml", "Cargo.lock") else root
                reject_links(source_root / info.name)
            return selected
        with tarfile.open(temporary, "w:gz", dereference=False) as bundle:
            for name in ("Cargo.toml", "Cargo.lock"):
                bundle.add(output / name, arcname=name, filter=filter_archive)
            for name in roots:
                bundle.add(root / name, arcname=name, filter=filter_archive)
        temporary.replace(archive)
    return closure


def main():
    parser = argparse.ArgumentParser(description="Eksport archiwum podzbioru workspace albo zawężenie kontekstu Dockera.")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--output", type=Path, required=True, help="Katalog roboczy metadanych; samodzielne źródła zapisuje --archive")
    parser.add_argument("--member", action="append")
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--in-place", action="store_true", help="Zawęża już skopiowany kontekst Dockera")
    parser.add_argument("--allow-network", action="store_true", help="Pozwala Cargo pobrać metadane paczek podczas przygotowania kontekstu Dockera")
    args = parser.parse_args()
    members = export_workspace(args.root, args.output, args.member or CONTAINER_MEMBERS, args.archive, offline=not args.allow_network, in_place=args.in_place)
    print(json.dumps(members))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        sys.exit(f"Eksport workspace nieudany: {error}")
