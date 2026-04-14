# Changelog

All notable changes to TentaFlow are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) /
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1-alpha] - 2026-04-14

First public alpha.

### Added
- Docker-free deploy path for Python AI engines (vLLM, SGLang, XTTS,
  VoxCPM, Parakeet, Qwen-ASR, ComfyUI). Bootstraps each engine with
  bundled `python-build-standalone` + `uv` into
  `~/.cache/tentaflow/envs/<engine>/`.
- `system_check` module that detects CUDA, ROCm 7+, Apple Metal, Intel
  XPU and Vulkan, plus a per-engine capability matrix the GUI wizard
  uses to show what the host can actually run.
- `tentaflow update` (self-update via GitHub Releases + axoupdater) and
  `tentaflow system-check` CLI subcommands.
- One-line installer: `install.sh` for Linux/macOS, `install.ps1` for
  Windows. Detects OS and architecture, downloads the matching archive,
  verifies SHA-256, installs to `/opt/tentaflow` (or a user-writable
  path), and registers auto-start via systemd, launchd, or a Scheduled
  Task.
- GitHub Actions release workflow: push tag `v*` → matrix build for
  `x86_64-linux`, `aarch64-linux`, `aarch64-macos`, `x86_64-windows` →
  GitHub Release with tarballs, SHA-256 sidecars, and install scripts.
- Embedded container contexts for every engine (Docker path) plus native
  C/C++ builds of llama.cpp, whisper.cpp, sherpa-onnx and
  stable-diffusion.cpp for hosts without Docker and without Python.
