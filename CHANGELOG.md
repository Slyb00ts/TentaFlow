# Changelog

All notable changes to TentaFlow are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) /
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.2-alpha] - 2026-04-23

### Added
- TBD — describe the changes before tagging.


### Added
- Added the new `www/` dashboard SPA and migrated the app to the binary WebSocket protocol with generated browser codecs.
- Added the service manifest registry, universal service catalog, and deploy wizard for Docker, native, and external engines.
- Added embedded and bundled deployment flows for AI engines, including live deployment progress streaming and shared model storage.
- Added a full meeting-bot stack with protocol support, database persistence, per-session container lifecycle, and dedicated frontend screens.
- Added multi-hop mesh topology propagation, route awareness, peer liveness tracking, and richer model and service visibility across nodes.
- Added QR-based pairing flows for mobile and tablet devices, including camera scanning and invite/PIN confirmation improvements.
- Added mobile-focused improvements across iOS and Android, including native discovery integration, QR scanning fallback, and mobile web packaging groundwork.
- Added installer and release automation, including GitHub Releases, packaged artifacts, and install scripts for Unix and Windows.
- Added IAM foundations with users, groups, role metadata, and resource permission protocol and handler support.

### Changed
- Switched the main dashboard static asset pipeline from `wwwroot/` to `www/`.
- Reworked deployment execution to use manifest-driven jobs and streamed deployment status instead of the older direct service deploy path.
- Upgraded the deploy wizard UI from simple radio inputs to richer option cards and per-GPU selection controls.
- Expanded mesh model and topology views so the UI can show backend, size, route, and peer-derived fallback data more consistently.

### Fixed
- Fixed native embedded deploys so `llama.cpp`, `MLX`, and `Whisper` create persistent service records, reappear in `Services`, and restore correctly after app restart.
- Fixed iOS and Xcode build issues around toolchain setup, Metal platform support, and mobile startup behavior.
- Fixed container bundle deployment path resolution and Docker build context handling for manifest-based deploys.
- Fixed multiple mesh pairing and discovery regressions, including duplicate connect/disconnect events, pairing completion handling, peer identity propagation, and reconnect behavior.
- Fixed mobile window sizing, fullscreen handling, and QR scanner error behavior.

## [0.0.1-alpha] - 2026-04-14

First public alpha. Everything listed below has been implemented,
compiled on Linux x86_64 + RTX 4090, and test-bootstrapped.

### Added — deploy and containers
- Generic `tentaflow-sidecar` crate (role-based QUIC bridge) with
  built-in keep-alive, idle detection, graceful shutdown. 7
  integration tests cover request/response, server shutdown notifying
  clients, client disconnect, handler errors, parallel streams, and
  long-idle keepalive.
- `ReverseProxy` sidecar role translating `ModelRequest` ↔ OpenAI /
  llama.cpp / sherpa / raw HTTP, with SSE → rkyv stream passthrough.
- Dockerfile + config + entrypoint for every model container:
  `llm-llamacpp`, `llm-vllm`, `llm-sglang`, `llm-ollama`, `stt-whisper`,
  `stt-parakeet`, `stt-qwen-asr`, `tts-sherpa`, `tts-xtts`, `tts-voxcpm`,
  `embeddings`, `reranker`, `comfyui`.
- `tentaflow-core/build.rs` embeds the container contexts as a single
  `tar.gz` (~26 MB) so a vanilla tentaflow binary can build and run any
  of them without git clone.
- `tentaflow-core/src/deploy/` module: `bundle::extract_to`,
  `docker::deploy` (bollard build + run), REST endpoints
  `GET /api/deploy/containers` and `POST /api/deploy/<name>`.

### Added — Docker-free deploy (Python bundles)
- `tentaflow-containers/python-bundles/` with one `bundle.toml` per
  engine (vLLM, SGLang, XTTS, VoxCPM, Parakeet, Qwen-ASR, ComfyUI) that
  pins python version, source (git head or pypi), launch command with
  `${MODEL}` / `${VENV_DIR}` substitution, required platforms, and per-
  backend install variants (CUDA / ROCm 7 / Metal / XPU).
- `deploy::python_venv::bootstrap` and `deploy::python_venv::deploy`:
  downloads `python-build-standalone` and `uv` into
  `~/.cache/tentaflow/`, creates a venv, installs the engine with the
  correct `--extra-index-url` and extras, then spawns it. All 7 bundles
  bootstrap end-to-end on a host with only system Python 3.14 present.
- Upstream compatibility fixes: `install_subdir` (SGLang's `python/`),
  `install_mode = "requirements_txt"` (ComfyUI), `extras_no_build_isolation`
  (flash-attn needs torch to be installed first), and a defensive
  `patch_pyproject_if_needed` that strips the `license` field so both
  old and new setuptools can build the cloned repos.

### Added — Docker-free deploy (native C/C++ binaries)
- `tentaflow-containers/native-binaries/` build scripts for
  llama.cpp, whisper.cpp, sherpa-onnx, text-embeddings-inference, and
  stable-diffusion.cpp. Each script auto-detects CUDA / Metal / Vulkan /
  CPU and produces a tarball of binary + required shared libs.
- Successful builds on the reference host: `llama-server` (CUDA, 27 MB),
  `whisper-server` (CUDA, 2 MB), `sd-server` (CUDA, 58 MB), sherpa CLI
  bundle (CPU, 36 MB).

### Added — system detection
- `system_check::collect()` reports CPU features (AVX2/AVX512/NEON), RAM,
  NVIDIA GPUs (via `nvidia-smi`), AMD GPUs (via `rocminfo` and
  `/opt/rocm/.info/version`), Intel XPU (via `sycl-ls`), Metal, Vulkan,
  plus runtime versions (`docker`, `podman`, `python`, `nvcc`).
- `GpuBackend` enum with `preferred_backend` resolution
  (CUDA → ROCm → Metal → XPU → CPU) used by `pick_install_variant`.
- Per-engine capability matrix returned to the GUI wizard so users see
  what will and will not run on their hardware.
- REST endpoint `GET /api/system/capabilities`.
- `cargo run --example system_check` CLI helper.

### Added — GUI integration
- `ws_deploy.rs` recognises both backends: for engines mapped to an
  embedded container it builds and runs via `deploy::docker::deploy`; if
  `deploy_mode == "native"` it hands off to `deploy::python_venv::deploy`.
  Falls back to legacy `docker compose` path when the engine is not
  recognised.
- Respects every wizard field by parsing the wizard's generated
  `compose_yaml` — container name, ports (TCP/UDP mix), volumes, env
  (`HF_TOKEN`, `MODEL_ID`, `GPU_MEMORY_UTILIZATION`, `GGUF_PATH`,
  `shm_size`) and GPU selection.
- LLM deploy wizard GPU picker replaced with a multi-checkbox dropdown
  — users can target any subset of their cards; the compose emits
  `device_ids: ['0','4']` and the sidecar passes `NVIDIA_VISIBLE_DEVICES`
  through.
- Three unit tests covering GPU multi-select + compose parsing.

### Added — meeting bot persistence
- Transcripts are now stored in SQLite (tables `meeting_sessions` and
  `meeting_transcripts`) instead of process memory or a JSONL file.
  Survives restart, indexed by `(session_id, timestamp_ms)`.
- Endpoints `GET /api/meeting-bot/sessions`,
  `GET /api/meeting-bot/sessions/{id}/transcripts`,
  `GET /api/meeting-bot/sessions/{id}/download`.
- Meeting bot GUI panel: download button fetches the full session;
  transcript list re-renders incrementally without resetting scroll.
- Speaker match thresholds retuned to cut false positives
  (`MATCH_CONFIDENT 0.55`, `MATCH_VERY_CONFIDENT 0.70`, strict
  `is_match()`, `INCREMENTAL_LEARN_THRESHOLD 0.65`, tracker
  similarity 0.50).

### Added — release pipeline
- `.github/workflows/release.yml`: tag `v*` triggers a matrix build
  (`x86_64-linux`, `aarch64-linux`, `aarch64-macos`, `x86_64-windows`)
  and publishes a GitHub Release with tarballs, SHA-256 sidecars,
  `install.sh`, and `install.ps1`. Tags with `-alpha`/`-beta`/`-rc`
  are marked as pre-release automatically.
- `scripts/install/install.sh` + `install.ps1` one-liner installers
  that detect platform, download the archive, verify SHA-256, install
  to `/opt/tentaflow` (or user path), and register auto-start via
  systemd / launchd / Scheduled Task.
- `scripts/release.sh` helper that bumps `tentaflow/Cargo.toml`, adds
  a CHANGELOG section, commits, tags, and pushes.
- `tentaflow update [--check|--force]` subcommand using `axoupdater` to
  swap the running binary from the latest GitHub Release.
- `RELEASING.md` documents the whole flow.

### Added — shutdown hardening
- SIGTERM + SIGINT both handled in `tentaflow/src/main.rs`.
- Unified HTTPS server now selects on the service-manager shutdown
  channel, so port 8090 is released immediately instead of sitting in
  `TIME_WAIT`.
- `MetricsCollector` background tasks join on the shutdown channel
  instead of looping forever.
- `db::checkpoint_wal` invoked on exit so SQLite WAL is flushed before
  the process dies.

### Changed
- Container images use `FROM rust:slim-bookworm` (no pinned Rust
  version) so sidecar builds always use the current stable toolchain.

### Fixed
- `tentaflow-voice` build no longer requires a system `protoc`; the
  build script falls back to `protobuf-src` when `PROTOC` is not set.
