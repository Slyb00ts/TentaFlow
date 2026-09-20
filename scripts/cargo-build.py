#!/usr/bin/env python3
# =============================================================================
# Plik: cargo-build.py
# Opis: Uruchamia Cargo i ogranicza cache, zachowując artefakty ostatnich buildów.
# Przykład: python3 scripts/cargo-build.py build --profile release-fast
# =============================================================================

import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import stat

if sys.version_info < (3, 11):
    sys.exit("Wymagany Python 3.11 lub nowszy; zaktualizuj Python przed uruchomieniem wrappera Cargo.")

import tomllib

REPO_ROOT = Path(__file__).resolve().parent.parent
CONFIG_PATH = REPO_ROOT / "scripts/config/build-cache.toml"
ARTIFACT_NAME = re.compile(r"^(?P<name>.+)-(?P<hash>[a-f0-9]{16})(?:\..*)?$")
INCREMENTAL_NAME = re.compile(r"^(?P<name>.+)-[a-z0-9]+$")
GIB = 1024 ** 3


class CacheError(RuntimeError):
    pass


def is_link(path):
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISLNK(metadata.st_mode) or bool(getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def checked_path(root, path):
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(root):
        raise CacheError(f"Ścieżka poza katalogiem cache: {path}")
    current = path
    while True:
        if is_link(current):
            raise CacheError(f"Dowiązanie w katalogu cache: {current}")
        if current == root:
            return path
        current = current.parent


def validate_target(target, initialize=False):
    target = Path(os.path.abspath(target))
    for parent in [target, *target.parents]:
        if is_link(parent):
            raise CacheError(f"Katalog target nie może prowadzić przez dowiązanie: {parent}")
    if target in (Path(target.anchor), Path.home(), REPO_ROOT):
        raise CacheError(f"To nie jest wydzielony katalog artefaktów Cargo: {target}")
    if (target / "Cargo.toml").exists() or (target / "src").exists():
        raise CacheError(f"Katalog zawiera źródła projektu: {target}")
    tag = target / "CACHEDIR.TAG"
    checked_path(target, tag)
    if initialize:
        target.mkdir(parents=True, exist_ok=True)
        if not tag.exists():
            tag.write_text("Signature: 8a477f597d28d172789f06886806bc55\n# Cache artefaktów Cargo.\n", encoding="utf-8")
    if not target.is_dir() or not tag.is_file() or not tag.read_bytes().startswith(b"Signature: 8a477f597d28d172789f06886806bc55"):
        raise CacheError(f"Brak prawidłowego znacznika Cargo CACHEDIR.TAG w {target}")
    return target


@contextlib.contextmanager
def file_lock(path):
    # Pierwszy bajt pokrywa się z zakresem LockFileEx używanym przez Cargo.
    handle = open(path, "a+b")
    try:
        if os.name == "nt":
            import msvcrt
            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        handle.close()
        raise CacheError(f"Cache zajęty albo blokada niedostępna: {path}: {error}") from error
    try:
        yield
    finally:
        handle.close()


def cargo_locks(target):
    locks = []
    for directory, names, files in os.walk(target, followlinks=False):
        names[:] = [n for n in names if n not in ("deps", "incremental", ".fingerprint") and not is_link(Path(directory) / n)]
        if ".cargo-lock" in files:
            locks.append(checked_path(target, Path(directory) / ".cargo-lock"))
    return sorted(locks)


def read_config(path=CONFIG_PATH):
    with open(path, "rb") as handle:
        document = tomllib.load(handle)
        config = document["retention"]
    config["nested_targets"] = document.get("nested_targets", {})
    for name, limit in config["nested_targets"].items():
        if not re.fullmatch(r"target-[a-z0-9-]+", name) or not isinstance(limit, (int, float)) or limit <= 0:
            raise CacheError(f"Nieprawidłowy katalog lub limit zagnieżdżonego cache: {name}")
    for key in ("successful_builds", "bootstrap_variants", "incremental_variants"):
        if type(config[key]) is not int or config[key] < 1:
            raise CacheError(f"{key} musi być dodatnią liczbą całkowitą")
    for key in ("max_gib", "incremental_max_gib", "older_than_days"):
        if not isinstance(config[key], (int, float)) or isinstance(config[key], bool) or config[key] <= 0:
            raise CacheError(f"{key} musi być dodatnią liczbą")
    return config


def read_history(target):
    path = checked_path(target, target / ".build-cache-history.json")
    if not path.exists():
        return []
    history = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(history, list):
        raise CacheError("Nieprawidłowa historia buildów; czyszczenie wstrzymane")
    for record in history:
        for name in record["artifacts"]:
            checked_path(target, target / name)
    return history


def write_json(target, name, value):
    path = checked_path(target, target / name)
    temporary = checked_path(target, target / (name + ".tmp"))
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def save_build(target, artifacts, config, locked=False):
    with contextlib.nullcontext() if locked else file_lock(checked_path(target, target / ".build-cache.lock")):
        history = read_history(target)
        record = {"finished": time.time(), "artifacts": sorted(artifacts)}
        write_json(target, ".build-cache-history.json", (history + [record])[-config["successful_builds"]:])


def inventory(target):
    files = {}
    directories = {}
    links = set()
    for directory, names, filenames in os.walk(target, followlinks=False):
        base = Path(directory)
        checked_path(target, base)
        for name in list(names):
            path = base / name
            if is_link(path):
                links.add(path)
                names.remove(name)
        directories[base] = base.stat().st_mtime
        for name in filenames:
            path = base / name
            if is_link(path):
                links.add(path)
                continue
            checked_path(target, path)
            stat = path.stat()
            if not path.is_file():
                raise CacheError(f"Nieregularny plik w cache: {path}")
            files[path] = (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime)
    return files, directories, links


def collect_groups(target, files, directories, locked_profiles):
    groups = {}
    for path in [*directories, *files]:
        parts = path.relative_to(target).parts
        for index, part in enumerate(parts):
            if part not in ("deps", ".fingerprint", "build", "incremental") or index + 1 >= len(parts):
                continue
            profile = target.joinpath(*parts[:index])
            if profile not in locked_profiles:
                continue
            name = parts[index + 1]
            match = (INCREMENTAL_NAME if part == "incremental" else ARTIFACT_NAME).match(name)
            if match is None:
                break
            kind = "incremental" if part == "incremental" else "artifact"
            key = (profile, kind, name if kind == "incremental" else match["hash"])
            group = groups.setdefault(key, {"kind": kind, "family": (profile, kind, match["name"]), "roots": set(), "files": set(), "modified": 0})
            group["roots"].add(profile / part / name)
            group["modified"] = max(group["modified"], files[path][3] if path in files else directories[path])
            if path in files:
                group["files"].add(path)
            break
    for group in groups.values():
        roles = sorted({p.name.removesuffix(".json") for p in group["files"] if p.parent.parent.name == ".fingerprint" and p.suffix == ".json"})
        group["family"] += (tuple(roles),)
    return list(groups.values())


def make_plan(target, config, history, now=None, locked_profiles=None):
    files, directories, links = inventory(target)
    if locked_profiles is None:
        locked_profiles = {p.parent for p in files if p.name == ".cargo-lock"}
    groups = collect_groups(target, files, directories, locked_profiles)
    now = time.time() if now is None else now
    owners = {}
    sizes = {}
    for path, (device, inode, size, _) in files.items():
        key = (device, inode)
        owners.setdefault(key, set()).add(path)
        sizes[key] = size
    total = sum(sizes.values())
    initial = total
    protected = {target / name for record in history for name in record["artifacts"]}
    protected_inodes = {files[p][:2] for p in protected if p in files}
    protected.update(p for p, info in files.items() if info[:2] in protected_inodes)
    protected_ancestors = set()
    unlocked = {p for p in files if p.name == ".cargo-lock" and p.parent not in locked_profiles}
    for path in protected | links | unlocked:
        protected_ancestors.add(path)
        protected_ancestors.update(parent for parent in path.parents if parent.is_relative_to(target))
    families = {}
    for group in groups:
        families.setdefault(group["family"], []).append(group)
        group["protected"] = not group["roots"].isdisjoint(protected_ancestors)
    for family in families.values():
        family.sort(key=lambda item: item["modified"], reverse=True)
        keep = 1 if family[0]["kind"] == "incremental" else config["bootstrap_variants"]
        for group in family[:keep]:
            group["protected"] = True
        for rank, group in enumerate(family):
            group["rank"] = rank
    incremental_paths = {p for p in files if "incremental" in p.relative_to(target).parts}
    incremental = sum(sizes[key] for key, paths in owners.items() if not paths.isdisjoint(incremental_paths))
    candidates = []
    for group in sorted(groups, key=lambda item: (item["kind"] != "incremental", item["modified"])):
        if group["protected"]:
            continue
        old = now - group["modified"] > config["older_than_days"] * 86400
        oversized = total > config["max_gib"] * GIB
        extra_incremental = group["kind"] == "incremental" and (incremental > config["incremental_max_gib"] * GIB or group["rank"] >= config["incremental_variants"])
        if not (old or oversized or extra_incremental):
            continue
        reclaimed = 0
        incremental_removed = 0
        for path in group["files"]:
            key = files[path][:2]
            owners[key].discard(path)
            if not owners[key]:
                reclaimed += sizes[key]
            if path in incremental_paths and owners[key].isdisjoint(incremental_paths):
                incremental_removed += sizes[key]
        total -= reclaimed
        if group["kind"] == "incremental":
            incremental -= incremental_removed
        candidates.append({"kind": group["kind"], "paths": sorted(str(p.relative_to(target)) for p in group["roots"]), "bytes": reclaimed})
    return {"target": str(target), "created": now, "before_bytes": initial, "after_bytes": total, "max_bytes": int(config["max_gib"] * GIB), "incremental_after_bytes": incremental, "incremental_max_bytes": int(config["incremental_max_gib"] * GIB), "candidates": candidates}


def prune(target, config, apply=False, locked=False):
    target = validate_target(target)
    with contextlib.ExitStack() as stack:
        if not locked:
            stack.enter_context(file_lock(checked_path(target, target / ".build-cache.lock")))
        locks = cargo_locks(target)
        if not locks:
            raise CacheError("Nie znaleziono blokad profili Cargo; czyszczenie wstrzymane")
        for path in locks:
            stack.enter_context(file_lock(path))
        plan = make_plan(target, config, read_history(target), locked_profiles={p.parent for p in locks})
        write_json(target, ".build-cache-plan.json", plan)
        if apply:
            for candidate in plan["candidates"]:
                for name in candidate["paths"]:
                    path = checked_path(target, target / name)
                    if path.is_dir():
                        # Blokady zagnieżdżonych Cargo zostają na swoich miejscach.
                        for directory, names, filenames in os.walk(path, topdown=False):
                            for filename in filenames:
                                if filename != ".cargo-lock":
                                    checked_path(target, Path(directory) / filename).unlink()
                            for dirname in names:
                                child = checked_path(target, Path(directory) / dirname)
                                if not any(child.iterdir()):
                                    child.rmdir()
                        if not any(path.iterdir()):
                            path.rmdir()
                    elif path.exists():
                        path.unlink()
        prefix = "Usunięto" if apply else "Plan usunięcia"
        print(f"{prefix}: {len(plan['candidates'])} grup; cache {plan['before_bytes'] / GIB:.2f} → {plan['after_bytes'] / GIB:.2f} GiB (rozmiar logiczny, hardlinki raz).", file=sys.stderr)
        if plan["after_bytes"] > plan["max_bytes"] or plan["incremental_after_bytes"] > plan["incremental_max_bytes"]:
            print("Limit miękki przekroczony: pozostawiono najnowsze warianty, ostatnie udane buildy i pliki spoza zakresu retencji.", file=sys.stderr)
        print(f"Szczegóły: {target / '.build-cache-plan.json'}", file=sys.stderr)
        return plan


def metadata_arguments(arguments):
    result = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--":
            break
        value_options = ("--manifest-path", "--config", "--features", "-F")
        if any(argument.startswith(option + "=") for option in value_options):
            result.append(argument)
        elif argument in value_options:
            index += 1
            if index == len(arguments):
                raise CacheError(f"Brak wartości dla {argument}")
            result.extend([argument, arguments[index]])
        elif argument in ("--offline", "--locked", "--frozen", "--all-features", "--no-default-features"):
            result.append(argument)
        index += 1
    return result


def target_override(arguments):
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument.startswith("--target-dir="):
            return Path(argument.split("=", 1)[1])
        if argument == "--target-dir":
            return Path(arguments[index + 1])
    return None


def run_cargo(command, arguments, config):
    cargo_arguments = arguments[:arguments.index("--")] if "--" in arguments else arguments
    if any(argument == "--message-format" or argument.startswith("--message-format=") for argument in cargo_arguments):
        raise CacheError("Wrapper sam ustawia --message-format; usuń tę opcję z polecenia")
    cwd = Path.cwd()
    metadata = subprocess.run(["cargo", "metadata", "--format-version=1", "--no-deps", *metadata_arguments(arguments)], cwd=cwd, text=True, encoding="utf-8", stdout=subprocess.PIPE, check=True)
    info = json.loads(metadata.stdout)
    override = target_override(arguments)
    target = Path(os.path.abspath(cwd / override)) if override else Path(info["target_directory"])
    target = validate_target(target, initialize=True)
    separate_build_dir = os.environ.get("CARGO_BUILD_BUILD_DIR")
    if info.get("build_directory") is not None and info["build_directory"] != info["target_directory"]:
        separate_build_dir = info.get("build_directory")
    if separate_build_dir and Path(os.path.abspath(cwd / separate_build_dir)) != target:
        raise CacheError("Osobny build.build-dir nie jest obsługiwany przez retencję; użyj wspólnego target-dir")
    if any("build.build-dir" in arg for arg in cargo_arguments):
        raise CacheError("Opcja build.build-dir nie jest obsługiwana przez retencję")
    with file_lock(checked_path(target, target / ".build-cache.lock")):
        code, artifacts = stream_cargo(command, arguments, cwd, target)
        if code == 0:
            try:
                target = validate_target(target)
                save_build(target, artifacts, config, locked=True)
                prune(target, config, apply=True, locked=True)
            except (CacheError, OSError, ValueError, KeyError) as error:
                print(f"Cargo zakończone poprawnie; retencja wstrzymana: {error}", file=sys.stderr)
            for name, limit in config.get("nested_targets", {}).items():
                nested = Path(info["workspace_root"]) / name
                if nested == target or not nested.exists():
                    continue
                nested_config = dict(config, max_gib=limit, incremental_max_gib=min(limit, 4))
                try:
                    prune(nested, nested_config, apply=True)
                except (CacheError, OSError, ValueError, KeyError) as error:
                    print(f"Retencja {name} wstrzymana: {error}", file=sys.stderr)
        return code


def stream_cargo(command, arguments, cwd, target):
    artifacts = set()
    cargo = subprocess.Popen(["cargo", command, "--message-format=json-render-diagnostics", *arguments], cwd=cwd, env=dict(os.environ, TENTAFLOW_PYTHON=sys.executable), stdout=subprocess.PIPE, text=True, encoding="utf-8", errors="replace", bufsize=1)
    try:
        for line in cargo.stdout:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if not isinstance(event, dict) or "reason" not in event:
                print(line, end="", flush=True)
                continue
            if event["reason"] == "compiler-message":
                rendered = event["message"].get("rendered")
                if rendered:
                    print(rendered, end="", file=sys.stderr, flush=True)
            elif event["reason"] in ("compiler-artifact", "build-script-executed"):
                paths = event.get("filenames", []) + ([event["out_dir"]] if event.get("out_dir") else [])
                for name in paths:
                    path = Path(os.path.abspath(name))
                    if path.is_relative_to(target):
                        artifacts.add(str(checked_path(target, path).relative_to(target)))
        code = cargo.wait()
    except BaseException:
        cargo.terminate()
        cargo.wait()
        raise
    return code, artifacts


def main(argv=None):
    parser = argparse.ArgumentParser(description="Cargo z retencją cache; prune domyślnie tylko zapisuje plan.")
    parser.add_argument("command", choices=("build", "test", "check", "prune", "report"))
    args, remaining = parser.parse_known_args(argv)
    config = read_config()
    if args.command in ("prune", "report"):
        cleanup = argparse.ArgumentParser()
        cleanup.add_argument("--target-dir", type=Path, default=REPO_ROOT / "target_shared")
        cleanup.add_argument("--apply", action="store_true")
        options = cleanup.parse_args(remaining)
        if args.command == "report" and options.apply:
            raise CacheError("report nie obsługuje --apply")
        prune(options.target_dir, config, apply=options.apply)
        return 0
    return run_cargo(args.command, remaining, config)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (CacheError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Błąd: {error}", file=sys.stderr)
        sys.exit(1)
