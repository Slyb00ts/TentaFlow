#!/usr/bin/env python3
# ===== File: llm/patches/dspark/apply.py — DSpark + B12X patch set for vLLM =====
# Applies the DeepSeek V4 DSpark runtime onto a stock vLLM install: six files the
# recipe adds outright, twelve it modifies. Replaces the previous `cp -a` of a
# whole overlay tree, which silently reverted every upstream fix inside the ~24k
# lines it covered and pinned us to one prebuilt image.
#
# Idempotent: re-running is a no-op. Fails loudly and names the file whose patch
# no longer applies, so a vLLM bump reports the drift instead of producing a
# runtime that boots and misbehaves.
#
# Przyklad:
#   apply.py --check                 # czy zestaw nalozy sie na ten vLLM
#   apply.py                         # naloz
#   apply.py --site-packages <path>  # jawna sciezka do vllm/
import argparse
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def find_vllm(explicit: str | None) -> Path:
    """Katalog pakietu `vllm`. Jawna sciezka wygrywa; inaczej pytamy interpreter,
    ktory nas uruchomil — w bundlu to venv, w obrazie /opt/env."""
    if explicit:
        p = Path(explicit)
        return p if p.name == "vllm" else p / "vllm"
    try:
        import vllm  # noqa: PLC0415

        return Path(vllm.__file__).resolve().parent
    except Exception as exc:  # noqa: BLE001
        sys.exit(f"nie znajduje pakietu vllm ({exc}) — podaj --site-packages")


def vllm_version() -> str:
    try:
        import vllm  # noqa: PLC0415

        return getattr(vllm, "__version__", "?")
    except Exception:  # noqa: BLE001
        return "?"


def _supports_3way(root: Path) -> bool:
    """`git apply --3way` dziala TYLKO w repozytorium git, a `site-packages`
    nim nie jest. Sprawdzamy raz, zeby poza repo nie zglaszac dwunastu
    identycznych bledow '--3way outside a repository' zamiast prawdziwej
    przyczyny."""
    r = subprocess.run(
        ["git", "rev-parse", "--is-inside-work-tree"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    return r.returncode == 0 and r.stdout.strip() == "true"


_THREE_WAY: bool | None = None


def git_apply(patch: Path, root: Path, *extra: str) -> subprocess.CompletedProcess:
    # -p2 zdejmuje `a/vllm/` z naglowkow, wiec sciezki w patchu sa wzgledem
    # katalogu pakietu. --3way (gdy dostepne) nakłada latke mimo drobnego dryfu
    # kontekstu — to cala poanta przy bumpie wersji.
    global _THREE_WAY  # noqa: PLW0603
    if _THREE_WAY is None:
        _THREE_WAY = _supports_3way(root)
    flags = ["-p2"] + (["--3way"] if _THREE_WAY else [])
    return subprocess.run(
        ["git", "apply", *flags, *extra, str(patch)],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )


def already_applied(patch: Path, root: Path) -> bool:
    """Latka jest juz nalozona, gdy da sie ja czysto ODWROCIC."""
    return git_apply(patch, root, "--reverse", "--check").returncode == 0


def main() -> int:
    ap = argparse.ArgumentParser(description="DSpark/B12X patch set for vLLM")
    ap.add_argument("--site-packages", default=None)
    ap.add_argument(
        "--check",
        action="store_true",
        help="tylko sprawdz, czy zestaw sie nalozy — nic nie zmienia",
    )
    args = ap.parse_args()

    root = find_vllm(args.site_packages)
    if not root.is_dir():
        sys.exit(f"{root} nie jest katalogiem pakietu vllm")
    base = (HERE / "BASE_COMMIT").read_text().strip()
    print(f"vllm      : {root}  (wersja {vllm_version()})")
    print(f"lat. bazie: upstream vllm @ {base}")

    additive = sorted((HERE / "additive").rglob("*.py"))
    patches = sorted((HERE / "patches").glob("*.patch"))
    if not patches:
        sys.exit("brak latek w patches/ — zestaw jest niekompletny")

    # --- pliki dodawane -----------------------------------------------------
    add_todo = []
    for src in additive:
        rel = src.relative_to(HERE / "additive")
        dst = root / rel
        if dst.exists() and dst.read_bytes() == src.read_bytes():
            continue
        add_todo.append((src, dst, rel))

    # --- latki --------------------------------------------------------------
    ok, done, failed = [], [], []
    for p in patches:
        if already_applied(p, root):
            done.append(p.name)
        elif git_apply(p, root, "--check").returncode == 0:
            ok.append(p)
        else:
            failed.append((p, git_apply(p, root, "--check").stderr.strip()))

    print(
        f"pliki     : {len(add_todo)} do skopiowania, "
        f"{len(additive) - len(add_todo)} juz aktualnych"
    )
    print(
        f"lat.      : {len(ok)} do nalozenia, {len(done)} juz nalozonych, "
        f"{len(failed)} NIE PASUJE"
    )

    if failed:
        print("\nlatki, ktore nie pasuja do tej wersji vLLM:", file=sys.stderr)
        for p, err in failed:
            print(f"  - {p.name}", file=sys.stderr)
            for line in err.splitlines()[:3]:
                print(f"      {line}", file=sys.stderr)
        print(
            "\nUpstream przesunal te pliki. Przenies latke na nowa wersje "
            "i zaktualizuj BASE_COMMIT.",
            file=sys.stderr,
        )
        return 1

    if args.check:
        print("\nOK — zestaw nalozy sie na ten vLLM.")
        return 0

    for src, dst, rel in add_todo:
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        print(f"  + {rel}")
    for p in ok:
        res = git_apply(p, root)
        if res.returncode != 0:
            # Check przeszedl, a aplikacja nie — zatrzymujemy sie z nazwa pliku
            # zamiast zostawiac drzewo w polowie zalatanym.
            print(f"BLAD przy {p.name}:\n{res.stderr}", file=sys.stderr)
            return 1
        print(f"  * {p.name}")

    print("\nOK — DSpark/B12X nalozony.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
