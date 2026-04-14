# Wypuszczanie release'a TentaFlow

## Pierwszy raz (jednorazowo)

1. Zsynchronizuj wersje:
   - `tentaflow/Cargo.toml` -> `version = "X.Y.Z"`
   - `CHANGELOG.md` -> nowa sekcja `## [X.Y.Z]`
2. Zacommituj.

## Kazdy release

```bash
# 1. Zaktualizuj wersje w tentaflow/Cargo.toml + CHANGELOG.md
git add tentaflow/Cargo.toml CHANGELOG.md
git commit -m "release: vX.Y.Z"

# 2. Tag (musi zaczynac sie od 'v')
git tag vX.Y.Z
git push origin main
git push origin vX.Y.Z
```

GitHub Actions automatycznie:
- Zbuduje binarki dla:
  - `x86_64-unknown-linux-gnu`
  - `aarch64-unknown-linux-gnu`
  - `aarch64-apple-darwin` (Apple Silicon)
  - `x86_64-pc-windows-msvc`
- Spakuje kazda jako `tentaflow-vX.Y.Z-<triple>.tar.gz` / `.zip` z `config.example.toml`,
  `tentaflow.service`, `ai.tentaflow.plist`, `LICENSE`, `README.md`.
- Wygeneruje SHA256 obok kazdego artefaktu.
- Stworzy Release na GitHub i wrzuci wszystkie pliki + `install.sh` + `install.ps1`.

## Co dostaje uzytkownik

```bash
# Linux / macOS
curl -fsSL https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.sh | sh

# Windows (PowerShell)
irm https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.ps1 | iex
```

Installer:
1. Wykrywa OS i arch.
2. Pobiera odpowiednie `tentaflow-vX.Y.Z-<triple>.tar.gz` (lub `.zip`).
3. Weryfikuje SHA256.
4. Rozpakowuje do `/opt/tentaflow` (Linux), `~/Library/Application Support/TentaFlow` (macOS, user-install)
   lub `C:\Program Files\TentaFlow` (Windows).
5. Tworzy auto-start: systemd unit / launchd plist / Scheduled Task.
6. Tworzy `tentaflow` w `PATH`.

## Aktualizacja przez uzytkownika

```bash
# Sprawdz czy jest nowsza
tentaflow update --check

# Aktualizuj
tentaflow update
```

`tentaflow update` uzywa `axoupdater` — pobiera nowy archiv z GitHub
Releases, weryfikuje hash, podmienia binarke (przez external updater
process zeby Windows nie blokowal pliku w trakcie kopiowania).
Po update uzytkownik restartuje uslug systemd/launchd/scheduled-task
(albo robimy to za niego — TODO).

## Hotfixy / pre-release

Tagi `vX.Y.Z-rc.N` tez triggeruja workflow ale Release jest oznaczony
jako "pre-release" (do zrobienia przez `release-tag` opcje
`prerelease: true` w workflow).

## Re-build artefaktow bez nowego taga

GitHub UI -> Actions -> "release" -> "Run workflow" (workflow_dispatch).
