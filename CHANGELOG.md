# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) +
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Pierwsza pelna sciezka deployu Pythonowych silnikow bez Dockera
  (`python_venv::deploy`) — vLLM/SGLang/XTTS/VoxCPM/Parakeet/Qwen-ASR/ComfyUI
  z wlasnym `python-build-standalone` + `uv` w `~/.cache/tentaflow/`.
- Moduł `system_check` z detekcja CUDA / ROCm 7 / Metal / Intel XPU /
  Vulkan i mapowaniem co ktory silnik moze uruchomic.
- `tentaflow update` — self-update z GitHub Releases (axoupdater).
- `tentaflow system-check` — JSON dump moliwoci hosta.
- Installer `install.sh` (Linux/macOS) + `install.ps1` (Windows) z
  auto-startem przez systemd / launchd / Scheduled Task.
- GitHub Actions workflow `.github/workflows/release.yml` — matrix
  build per platforma na kazdy tag `v*`.

## [0.1.0] - TBD

Pierwszy publiczny release.
