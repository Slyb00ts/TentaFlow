# Releasing TentaFlow

## TL;DR — one command

```bash
./scripts/release.sh            # bumps patch + tags + pushes
./scripts/release.sh --minor    # bumps minor
./scripts/release.sh --major    # bumps major
./scripts/release.sh --finalize # strips -alpha/-beta/-rc suffix
./scripts/release.sh --set 0.1.0-beta.1   # set an exact version
./scripts/release.sh --dry-run  # show what would happen
```

The script updates `tentaflow/Cargo.toml`, writes a new section in
`CHANGELOG.md`, commits, creates a tag `vX.Y.Z...`, and pushes both
branch and tag. GitHub Actions does the rest.

## What GitHub Actions does on a `v*` tag

The workflow in `.github/workflows/release.yml` runs automatically and:

1. Builds release binaries in parallel for:
   - `x86_64-unknown-linux-gnu`
   - `aarch64-unknown-linux-gnu`
   - `aarch64-apple-darwin` (Apple Silicon)
   - `x86_64-pc-windows-msvc`
2. Packages each as `tentaflow-vX.Y.Z-<triple>.tar.gz` (Linux/macOS) or
   `.zip` (Windows) including `config.example.toml`, `tentaflow.service`,
   `ai.tentaflow.plist`, `LICENSE`, `README.md`.
3. Generates a SHA-256 sidecar for each archive.
4. Creates a GitHub Release (pre-release flag when tag ends with
   `-alpha`/`-beta`/`-rc`) and uploads all archives, checksums,
   `install.sh`, and `install.ps1`.

## End-user flow

```bash
# Linux / macOS
curl -fsSL https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.sh | sh

# Windows PowerShell
irm https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.ps1 | iex
```

The installer:

1. Detects OS and architecture.
2. Downloads the matching archive (`tentaflow-vX.Y.Z-<triple>.tar.gz` / `.zip`).
3. Verifies SHA-256.
4. Extracts to `/opt/tentaflow` (Linux), `~/Library/Application Support/TentaFlow` (macOS user install), or `C:\Program Files\TentaFlow` (Windows).
5. Registers auto-start (systemd unit, launchd agent, or Scheduled Task).
6. Links `tentaflow` into `PATH`.

## User updates

```bash
tentaflow update --check   # is a new version available?
tentaflow update           # download and swap binary
tentaflow update --force   # reinstall even if already current
```

`tentaflow update` uses [axoupdater](https://github.com/axodotdev/axoupdater):
fetches the new archive from GitHub Releases, verifies the hash, swaps
the binary through a short-lived external updater process so Windows
file locks do not block the copy. After the swap the user restarts the
service manager (`systemctl restart tentaflow`, `launchctl unload/load`,
or `Restart-ScheduledTask TentaFlow`).

## Pre-releases

Any tag that ends with `-alpha`, `-beta`, or `-rc` (for example
`v0.0.1-alpha`, `v0.1.0-beta.3`, `v1.0.0-rc.1`) is published as a
GitHub "pre-release" and is not returned by `/releases/latest`. Users
who want to pin to a specific pre-release can set `TENTAFLOW_VERSION`
before running the installer.

## Manually re-running the release job

GitHub → repository → **Actions** → **release** → **Run workflow**
(`workflow_dispatch`). Useful if a runner hit a transient failure.

## Rolling back a bad release

```bash
git tag -d vX.Y.Z                     # delete local tag
git push origin :refs/tags/vX.Y.Z     # delete remote tag
# Delete the corresponding Release on github.com manually
```
