# Changelog

Notable changes to TentaFlow.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) /
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Upgrade notes

- **TentaBus replication safety needs every node on this release.** The
  protection against losing acknowledged records holds only once every node of
  a cluster runs it: an older node sends neither log epochs nor committed
  offsets and is judged by offsets alone, as before. Partitions written by an
  older release open with no recorded epochs and a committed offset of 0, so
  the first leader to reach them re-feeds more than it would otherwise.
- **Upgrade TentaBus followers before leaders.** At RF≥3 with `acks=quorum` or
  `acks=all`, an upgraded leader shows consumers the records it re-fed after a
  failover only once a majority confirms its term, and an older follower never
  confirms. During a rolling upgrade an idle partition whose majority still
  runs the older release keeps those records hidden until the next publish
  reaches a majority.

### TentaBus

- Fixed two leaders serving one partition at once: a node could accept another
  leader's announcement and still promote itself (present in `0.3.0-beta`).
- A topic deleted and created again under the same name is a new incarnation
  on every node: a node wipes the old incarnation's data before opening the new
  one, a leader of the old incarnation cannot take over a replica of the new
  one, and nodes agree on the incarnation under concurrent create and delete.
  Assignments and directories left by topics deleted before this release are
  cleaned up at migration and startup.
- A follower learns a raised commit offset immediately instead of at the next
  heartbeat, removing a delay of up to one heartbeat (about 0.5 s) before a
  record acknowledged without a following batch reaches followers' consumers.
- Replication records, for every record, the leader epoch it was first written
  in (a new `partition.epochs` file per partition) and ranks logs by the epoch
  of their last record; a majority-derived committed offset (appended to
  `partition.meta` after the unchanged 30-byte v1 record, so a downgraded
  binary still reads its own fields) bounds every reconciliation, and an
  election needs replies from a majority of replicas. A deposed or hung leader
  can no longer win with an unreplicated tail, overwrite committed records, or
  keep writing after it lost leadership; a node that lost an election stands
  again when the winner fails.
- Partitions with a replication factor of 2 now favour availability, like
  Kafka: when one replica is down, the surviving in-sync replica keeps leading
  (or is elected) and keeps accepting `acks=leader` and `acks=all` writes;
  `acks=quorum` still needs both replicas. Accepted risk: a write acknowledged
  only by the survivor is lost if the survivor is lost too; and a plain network
  split between the two replicas — no node has to fail — lets both lead at once
  until they reach each other again and one is fenced. The newer leadership
  wins, and the losing side's writes that never reached the winner are dropped.
  Partitions with three or more replicas keep requiring a majority.
- `acks=all` waits for the live in-sync replicas (never fewer than a majority
  at RF≥3) instead of the ISR recorded in the ledger, so a dead follower no
  longer blocks `acks=all` writes.
- At RF≥3 with `acks=quorum` or `acks=all`, consumers no longer read records
  of an earlier leader term that a re-elected leader re-fed to a majority
  before they were committed; a stale later-term replica could still win the
  next election and replace them. A new leader claims its term when it starts
  serving and followers confirm the claim once their logs reach it (an entry
  in `partition.epochs` that holds no record, so consumers see no extra
  message and offsets are unchanged); a majority confirming makes those
  records visible within about one heartbeat, with no publish needed. While
  fewer than a majority of nodes run this release, the records stay hidden
  until a record of the new leader's term reaches a majority.

## [0.3.0-beta] — 2026-09-24

Changes since `0.2.0-beta`. That tag never produced a published release (its
builds failed), so this is the first release published after `0.1.0-beta`: the
`0.2.0-beta` notes below apply to it as well.

### Upgrade notes

- **Update every mesh node together.** The binary protocol moved from schema 29
  to 32; an older and a newer node reject each other's handshake.
- **Sign in to agent CLI accounts again.** Agent CLI accounts now live in the
  provider-account registry. Every migrated account starts as "sign-in
  required": credentials stored in the old places (the Code Studio content
  database and the bridge's on-disk logins) are **not** carried over, because
  they cannot be decrypted with another node's key. At startup the node logs how
  many credential rows it removed, per node and engine and in total, so the
  loss of old entries never looks like silent data loss.
- **TentaBus partitions damaged by the old segment-roll bug no longer open.** A
  partition whose segment files were misnamed by that bug now fails with
  `SegmentOffsetMismatch` instead of being silently truncated; recover it by hand
  from a healthy replica.

### Windows

- Release archives for Windows x86_64 — `slim`, `full-vulkan` and `full-cuda13`
  — built in CI by the same `setup.ps1` → `build-all.ps1` → `build.ps1` scripts
  a developer runs. Each archive carries its Visual C++ runtime (and cuBLAS for
  `full-cuda13`); CI starts every archive from a bare `PATH` before publishing.
- `install.ps1` installs TentaFlow as the `TentaFlow` Windows service: explicit
  edition choice, verified download, the GStreamer runtime the `full` editions
  need (checksum-verified), a virtual service account, automatic start with
  restart on failure, firewall rules and a data directory only the service and
  administrators can read. `uninstall.ps1` removes it (`-Purge` also removes
  the data).
- `tentaflow start|stop|restart|status` drive the Windows service, and
  `tentaflow update` updates a Windows installation, including a newer
  GStreamer runtime when the release needs one.
- CI installs, upgrades, drives and uninstalls every Windows archive on a clean
  runner before a release is published.
- Host telemetry on Windows: GPU utilisation and VRAM (DXGI + PDH, independent
  of the UI language), disks, and network interface details now report real
  values instead of zeros.

### Android

- The Android debug APK (arm64-v8a) is attached to the release. It is
  debug-signed: it installs by sideloading, and a future properly signed build
  will not upgrade it in place.

### TentaNAS and storage

- Elastic Array: isolated mounts for new arrays, a durable service mode, a
  verified single-disk cache tier and a mover that relocates files from the
  cache to the data disks automatically, on a schedule or on demand — without
  freezing the share.
- Scheduled sync and scrub (every array is scrubbed monthly); a failed parity
  run is recorded as a cause instead of wedging the array, a Sync over a fault
  needs an explicit acknowledgement, and an unfinished disk add can be resumed
  or undone.
- Repair, grow and dissolve an array; share and adopt arrays between nodes;
  export an array over NFS; clear a disk only behind a namespace-proof guard
  and a retyped name.
- Disk health: correct SMART and NVMe self-test decoding, one self-test per
  disk at a time, failures that reach the health verdict, and a dashboard that
  tells a failing disk from a warning.
- Multi-tenancy: jobs, alerts, disks, approvals, share sources and targets are
  scoped to the asking organisation; internal configuration rows stay hidden
  from tenants.
- The UI names nodes, users and disks instead of showing identifiers, and
  patches the dashboard, pool and array views in place instead of rebuilding
  them.

### TentaBus

- Package 1: field policies reach the dead-letter queue, per-action ACLs,
  instance isolation, quota enforcement and validation.
- Schema-registry REST API with per-action API-key scopes; the compliance
  retention floor applies to bus topics.
- Replication: followers acknowledge on their own cadence and the leader waits
  without a thread per publish, raising quorum throughput from about 1k to about
  220k messages/s. Fixed a segment-roll bug that could make consumers skip
  records, and leader-term races (term rollback, duplicate leaders after
  failover, unauthenticated leader handshakes). The replication transport is
  hardened and the failover audit names the instance.
- Lag without side effects, truthful deprecation, lag history, and the first
  TentaBus screen (shell and overview).

### Agents, Code Studio and provider accounts

- Provider accounts: an on-demand runtime, CLI sign-in with a login GUI,
  sessions, shared accounts and credentials that sync between nodes.
- The account card no longer claims an account has no sessions when it only
  sees this node's. It tells apart an account homed on this node, one homed on
  another reachable node (named, with its own subset) and one homed on a node
  that does not answer ("no data" instead of an empty list).
- Coding agents run on Linux and Windows; only the Code Harness that Code
  Studio actually runs is kept; agents without a model get a chat model.

### Robotics (Go2)

- Cloud onboarding, the `data2=3` handshake with a per-device AES key and GPU
  camera anonymisation with tighter privacy regions.
- Shared map: a persistent occupancy model, frame carving, chunk storage, a
  control plane and relocalisation against the shared map by branch-and-bound
  scan matching.

### Inference, vision and voice

- ONNX Runtime never falls back to the CPU with device-bound inputs, and cuDNN
  is preloaded for the CUDA provider.
- Addon model requirements can be installed from the addon's settings.
- GPU detection runs in a child process, so a crashing driver cannot take the
  node down.
- A bundled Jarvis voice clone for Supertonic TTS.

### Mesh and sync

- Baseline adoption and node-log catch-up no longer block between nodes; a
  donor that refuses a fuller requester adopts from it instead; a replicated
  flow version whose number is already taken locally is kept.
- Security and mesh audit findings closed.

### Build

- The `slim` edition compiles again; Arch Linux setup and macOS build errors
  are fixed; generated browser assets are no longer tracked in Git.
- Native library and toolchain versions live in one file,
  `scripts/versions.env`, read by every build script on every platform.

## [0.2.0-beta] — 2026-09-08

The main changes since the last published release, `0.1.0-beta`.

### TentaNAS and storage

- A new application for managing disks, ZFS pools, datasets, snapshots, network
  shares and block storage across the fleet, with schedules, an access audit, a
  restricted privileged helper and second-administrator approval for selected
  operations.
- Elastic Array joins data and cache disks through mergerfs with periodic
  SnapRAID parity. Operations run with a durable intent record; data moves off
  the cache, sync and scrub run on demand, mounts can be restored, and data still
  waiting for parity protection is visible.

### Applications, TentaBus and agent accounts

- Multiple application instances with separate data, permissions and lifecycle.
  TentaBus gained a durable log, replication, a schema registry, field policies
  and integration with flows, REST and the SDK; instance isolation and recovery
  after a leader change were fixed.
- Code Studio manages agent CLI accounts, isolated working directories and moving
  accounts between nodes. TentaVM foundations: a host registry, capability
  probing, and granting and requesting access.

### TentaQuant

- A new quantum-circuit studio and notebook with an OpenQASM 3 subset parser,
  CPU/WGPU simulation, in-browser execution through WASM and Qiskit export. Jobs
  can be cancelled, stream their results, visualise the state, and compare and
  export results.

### Mesh, clusters and processing

- The Mesh list shows the local node, devices discovered over mDNS and trusted
  nodes, keeping transitive trust; the relay does not create a global device
  directory.
- iroh update and reconnection fixes after a restart or an address change.
  Cluster sync uses a change log, and model recovery takes peer availability and
  startup time into account.
- One way of handling the default conversation flow. Node configuration refreshes
  correctly in the editor; selected voice and image-processing paths were fixed.

### Build, dependencies and cache

- One Cargo workspace, a shared lockfile and central dependency versions and
  profiles, checked by CI. Source exports keep self-contained container and SDK
  contexts; unused dependencies were removed and code adapted to new APIs.
- ThinLTO for releases, an incremental `release-fast` profile and automatic
  artefact retention in the shared scripts; less duplicated output and fewer
  needless rebuilds. [Measurements and cache rules](docs/build-performance.md)
  are documented separately.

### Addons and integrations

- Outlook, SharePoint RAG and Teams moved into the shared addons directory. The
  separate `teams-bot` WASM addon was removed; the native Meeting Bot remains.

### Installation and distribution

- An explicit Full/Slim choice also when the installer is piped. A new
  configuration has mesh enabled and HTTPS reachable from the LAN; the Linux
  system installer opens the ports in an active UFW/firewalld, keeps an existing
  configuration and warns about a loopback bind.
- The macOS Metal archive requires the Meeting Bot: before publishing, the
  workflow checks it is present, executable, built for the right architecture,
  has its dependencies and starts with `--help`.

## [0.1.0-beta] - 2026-09-02

### Added
- Installer for Linux and macOS (`curl … | sh`) with an explicit edition choice: hardware
  detection proposes, the user decides. Installs into `/opt/tentaflow/versions/<ver>` behind a
  `current` symlink, keeps configuration and data across updates, registers a systemd unit
  (Linux) or a LaunchDaemon (macOS) and starts it.
- `tentaflow start|stop|restart|status` — status reports service state, autostart, PID, config
  and probes `/health`.
- `tentaflow update` — own updater over GitHub Releases: mandatory checksum verification,
  whole-version-directory swap, previous version kept for rollback, service restarted only if it
  was running.
- `tentaflow init-config` — writes the default configuration from `NodeConfig`, pinning all three
  listeners to the chosen bind address.
- `slim` distribution edition (`--no-default-features`): gateway, mesh, flows, dashboard, addons
  and containers with no local inference engine. Its catalog keeps every cloud provider and the
  utility infrastructure — 21 entries instead of 94.
- CI `verify` job installs the built archive on a runner with real systemd and asserts autostart,
  liveness and `/health` before publishing; `scripts/ci-local/` reproduces the whole release build
  and the install test locally, across Ubuntu, Debian, Fedora and Arch.

### Changed
- ROCm/HIP removed from the application: AMD and Intel run on the portable Vulkan path, the same
  one Burn uses for vision. NVIDIA keeps CUDA.
- Release builds on Ubuntu 22.04, making glibc 2.35 / GLIBCXX 3.4.30 the supported floor; the
  installer refuses to install below it instead of leaving an unrunnable service enabled.
- Whisper is target-gated, not only feature-gated, so `full` on Apple no longer links whisper.cpp
  next to MLX.
- Vision model runners moved to `vision/runners.rs`; they are shared by the flow vision node,
  local CV and the inference batcher, and no longer live behind the camera feature.

### Fixed
- `mesh.frame_rejected` from unpaired peers no longer floods the audit log (it was 98.5% of
  1.15M rows).
- Reasoning deltas are forwarded over the mesh reverse stream.
- Mesh peer trust survives pairing, boot prune and `persisted_version` domains.

## [0.0.2-alpha] - 2026-04-23

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
  llama.cpp / sherpa / raw HTTP, with SSE → CBOR stream passthrough.
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
