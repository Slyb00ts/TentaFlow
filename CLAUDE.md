# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

No workspace Cargo.toml — each crate builds independently. The main binary is `tentaflow`.

```bash
# Build main binary (from tentaflow/)
cd tentaflow && cargo build --release

# Build core library (from tentaflow-core/)
cd tentaflow-core && cargo build

# Run
./tentaflow/target/release/tentaflow --config config.toml

# WASM addons require this target
rustup target add wasm32-wasip1
```

Feature flags on `tentaflow-core`:

| Flag | Purpose |
|------|---------|
| `docker` | Docker management (bollard) |
| `inference-llamacpp` | llama.cpp backend |
| `inference-mlx` | Apple MLX (macOS only) |
| `dashboard-api` | Axum HTTP dashboard + API |
| `metrics-prometheus` | Prometheus metrics |

The main binary enables `docker`, `dashboard-api`, `metrics-prometheus` by default. macOS additionally enables `inference-mlx`.

## Tests

```bash
# All tests (from tentaflow-core/)
cd tentaflow-core && cargo test --lib --tests

# Specific module
cargo test --lib mesh::security

# Single test
cargo test --lib mesh::security::tests::pair_two_nodes_full_flow

# Skip MLX example (fails without feature flag)
cargo test --lib    # not cargo test (which includes examples)
```

## Architecture

### Crate Dependency Graph

```
tentaflow (binary: API gateway + mesh node)
  └── tentaflow-core (shared library)
        └── tentaflow-protocol (QUIC protocol types, rkyv zero-copy)

tentaflow-desktop/{linux,macos,windows} (native desktop apps)
  ├── tentaflow-desktop/core (shared desktop logic)
  │     ├── tentaflow-core
  │     └── tentaflow-ui (egui/wgpu GUI)
  └── tentaflow-ui

tentaflow-mobile (Android JNI + iOS Swift bridge)
  ├── tentaflow-core
  └── tentaflow-ui

tentaflow-client/native (Rust FFI → .NET P/Invoke)
  └── tentaflow-protocol

tentaflow-client/dotnet (C# wrapper over native)

tentaflow-models (training pipeline for Qwen 3.5-0.8B orchestrator)

mlx-models (Apple MLX inference bindings)
```

### Core Modules (tentaflow-core/src/)

- **mesh/** — P2P networking: mDNS discovery, QUIC transport, CRDT state sync, gossip (SWIM), node pairing (PIN + Ed25519/X25519/ChaCha20-Poly1305), key rotation epochs, trust revocation broadcast
- **addon/** — WASM plugins: Wasmtime (desktop) / wasmi (mobile), permission system, event bus, host functions, instance pooling, rate limiting
- **routing/** — Request routing: load balancer with circuit breaker, chat/embeddings/TTS/STT handlers, local inference, mesh forwarding
- **services/manifest/** — Service Manifest registry: ładowanie wygenerowanego rejestru z `services_generated.rs`, walidacja semantyczna (9 reguł), katalog silników udostępniany przez `/api/services/manifest`. Patrz sekcja `## Service Manifest`.
- **license/** — Sprawdzanie tieru licencji (Free/Pro/Enterprise), gating opcji `download` w manifestach
- **api/** — HTTP: OpenAI-compatible `/v1/*`, Dashboard `/api/*` (JWT), WebSocket metrics
- **flow_engine/** — DAG-based workflow execution with typed adapters
- **inference/** — LLM backends: llama.cpp, MLX, model manager
- **net/quic/** — QUIC client/server with TLS 1.3
- **db/** — SQLite (rusqlite, bundled), migrations, repository pattern

### Key Design Patterns

**Protocol serialization**: All QUIC messages use rkyv (zero-copy binary), not JSON. Protocol types live in `tentaflow-protocol/src/`. Two ALPN protocols: `tentaflow` (client→node) and `tentaflow-mesh` (node↔node).

**build.rs does two things**: (1) compiles WASM addons from `addons/` and `addons-pro/` to `wasm32-wasip1` and embeds them via `include_bytes!`, (2) embeds `wwwroot/` static files into the binary with MIME detection. Changes to `wwwroot/` require recompilation.

**Mesh security layers**: TLS 1.3 (transport) → Ed25519 identity → X25519 DH key exchange → ChaCha20-Poly1305 AEAD with epoch-based key rotation (24h interval, 7-day grace period) and replay protection (sequential nonce + sliding window).

**Dashboard**: Vanilla JS/HTML/CSS SPA in `tentaflow-core/wwwroot/`, no framework. i18n via `i18n/pl.json` and `i18n/en.json`.

### Mesh Protocol Discriminants

| Byte | Message | Status |
|------|---------|--------|
| 0x01-0x03 | ModelRequest, IngestRequest, CancelRequest | Client→Node |
| 0x10-0x18 | Heartbeat, CRDT, FullState, Forward, Models, Containers, Services, NodeInfo | Node↔Node |
| 0x20-0x22 | PairingRequest/Confirm/Reject | Pairing flow |
| 0x23 | TrustRevoked | Revocation broadcast |
| 0x24 | TrustedKeysSync | Post-pairing key sync |
| 0x25 | KeyRotation | Epoch key rotation |
| 0x30-0x33 | MeshCommand/Response/DeployProgress/LogChunk | Management (trusted only) |

## Service Manifest

Single source of truth dla wszystkich silników AI (LLM, TTS, STT, embeddings, vision, image-gen itd.). Każdy silnik = jeden plik TOML w `tentaflow-containers/<category>/_services/<engine_id>.toml`. Build.rs `tentaflow-core` waliduje manifesty przy `cargo build` i generuje:

- Rust const w `$OUT_DIR/services_generated.rs` — statyczny rejestr używany przez `tentaflow-core/src/services/manifest/registry.rs`
- JS module `tentaflow-core/wwwroot/js/generated/services-manifest.js` — importowany dynamicznie przez `wwwroot/js/modules/catalog/ManifestStore.js` w GUI

Pełna specyfikacja: `tentaflow-containers/_schema/SCHEMA.md`. Schema JSON: `tentaflow-containers/_schema/schema.json`.

### Struktura katalogu

| Katalog | Kategoria | Przykładowe silniki |
|---------|-----------|---------------------|
| `tentaflow-containers/llm/_services/` | Large Language Models | llama-cpp, vllm, sglang, ollama |
| `tentaflow-containers/stt/_services/` | Speech-to-Text | whisper, parakeet, qwen-asr |
| `tentaflow-containers/tts/_services/` | Text-to-Speech | sherpa-onnx, xtts, voxcpm |
| `tentaflow-containers/embeddings/_services/` | Wektoryzacja tekstu | hf-tei |
| `tentaflow-containers/reranker/_services/` | Rerankowanie | bge-reranker |
| `tentaflow-containers/vision/_services/` | OCR / detection / captioning | – |
| `tentaflow-containers/image-gen/_services/` | Generowanie obrazów | comfyui, stable-diffusion-cpp |
| `tentaflow-containers/video-gen/_services/` | Generowanie wideo | – |
| `tentaflow-containers/music-gen/_services/` | Generowanie muzyki | – |
| `tentaflow-containers/model-3d-gen/_services/` | Generowanie modeli 3D | – |
| `tentaflow-containers/agents/_services/` | Autonomiczne agenty | teams-bot |
| `tentaflow-containers/tools/_services/` | Function calling, MCP servers | – |

### Anatomia pliku TOML

```toml
[engine]
id = "vllm"
category = "llm"
name = "vLLM"
api = "openai-compatible"
default_port = 8000
version = "0.6.3"

[[variant]]
id = "linux-x64-cuda"
deploy_mode = "docker"
target_os = "linux"
target_arch = "x86_64"
gpu_backend = "cuda"
status = "stable"
vram_gb_min = 8

[variant.build]
context_path = "llm/docker/vllm"

[variant.download]
image = "ghcr.io/slyb00ts/tentaflow-pro/vllm:linux-x64-cuda-v0.6.3"
digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
enabled = false

[[model_preset]]
id = "qwen3-7b"
display_name = "Qwen 3 7B"
repo = "Qwen/Qwen3-7B"
recommended = true
```

Inne podsekcje wariantu wybierane wg `deploy_mode`: `[variant.feature_flag]` (embedded), `[variant.detection]` (external). Pełny opis pól, podsekcji i wartości enum w `tentaflow-containers/_schema/SCHEMA.md`.

### Tryby deploymentu (deploy_mode)

| Tryb | Opis | Kiedy używać |
|------|------|--------------|
| `native` | Natywna binarka kompilowana przez `build-natives.sh`, artefakt `.tar.gz` w `tentaflow-containers/output/` | Lekki silnik bez Pythona (whisper.cpp, sherpa-onnx, llama.cpp CLI) |
| `docker` | Obraz Docker budowany przez `build-containers.sh`, sidecar QUIC + silnik w jednym kontenerze | Silnik z ciężkimi zależnościami systemowymi (vLLM, sglang, comfyui) |
| `python-bundle` | Bundle Pythona definiowany przez `bundle.toml`, uruchamiany przez `server.py` | Silnik czysto pythonowy bez Dockera (parakeet, xtts) |
| `embedded` | Wkompilowany w binarkę `tentaflow` przez Cargo feature flag | Silnik z natywnymi bindingami Rust (llama.cpp, MLX) |
| `external` | Zewnętrzny serwis wykrywany w `PATH` + health check | Procesy uruchamiane osobno przez użytkownika (ollama daemon) |

### Build vs Download

Każdy `variant` typu `docker` ma DWIE opcje instalacji:

- **Build** — lokalny `docker build` z `[variant.build].context_path`. Zawsze dostępne (Free).
- **Download** — pull prebuilt image z `[variant.download].image` (registry). Wymaga TentaFlow Pro (sprawdzane przez `tentaflow-core/src/license/checker.rs`). W v1 wszystkie `download.enabled = false` — infrastruktura przygotowana pod Pro.

### Walidacja

Build.rs waliduje 9 reguł semantycznych przy każdej kompilacji (np. `gpu_backend = "metal"` → `target_os ∈ {macos, ios}`, `deploy_mode = "docker"` → `target_os ∈ {linux, windows}` bo brak GPU passthrough w Docker macOS). Naruszenie reguły łamie build z czytelnym komunikatem `cargo build`. Reguły walidują iloczyn kartezjański: gdy `target_os = ["linux","macos"]` i `gpu_backend = ["cuda","metal"]`, KAŻDA para musi być dopuszczalna — inaczej build odrzucony.

### API endpoints

| Endpoint | Opis |
|----------|------|
| `GET /api/services/manifest` | Cały manifest jako JSON (lista silników) |
| `GET /api/services/manifest/:engine_id` | Pojedynczy silnik |
| `GET /api/license/info` | Tier licencji (`{tier, allows_pro, allows_enterprise}`) |
| `POST /api/services/deploy` | Deploy silnika (body: `engine_id`, `variant_id`, `deploy_method`, `node_id`, `config`) |
| `GET /api/services/deployed` | Lista uruchomionych deploymentów |

Implementacja: `tentaflow-core/src/api/dashboard/api_services_manifest.rs`.

### Jak dodać nowy silnik

1. Wybierz kategorię (`llm`, `tts`, `stt`, ...) i utwórz `tentaflow-containers/<category>/_services/<engine-id>.toml` zgodnie z `_schema/SCHEMA.md`
2. Dla wariantu docker: dodaj `<category>/docker/<engine-id>/{Dockerfile, entrypoint.sh, config.default.toml, build.sh}`
3. Dla wariantu native: dodaj `<category>/native/<engine-id>/build.sh`
4. Dla wariantu python-bundle: dodaj `<category>/python/<engine-id>/{bundle.toml, server.py}`
5. Dla wariantu embedded: tylko TOML manifest (kod siedzi w `tentaflow-core/`, włączany Cargo featurem `feature_flag.name`)
6. `cargo build` z `tentaflow-core/` — walidacja + auto-generacja Rust + JS rejestru
7. Reload GUI — kafelek silnika pojawi się dynamicznie z manifestu

## Configuration

`config.toml` at project root. Key sections: `[server]`, `[protocols.quic]`, `[mesh]`, `[load_balancing]`, `[monitoring]`. Default ports: HTTPS/QUIC on 8090, Prometheus on 9090.

## Conventions

- Comments in code: Polish only
- Variable/function names: English
- Commit messages: English, format `[type]: description`
- Rust: `rustfmt` defaults, `snake_case` functions, `PascalCase` types
- JS/HTML/CSS: 2-space indent, `camelCase` JS, `kebab-case` CSS
- C#: 4-space indent, `PascalCase` public, `_camelCase` private fields

## gstack

For all web browsing, use the `/browse` skill from gstack. Never use `mcp__claude-in-chrome__*` tools.

Available gstack skills:

| Skill | Purpose |
|-------|---------|
| `/browse` | Headless browser for web browsing, QA testing, screenshots |
| `/connect-chrome` | Launch real Chrome controlled by gstack with Side Panel |
| `/qa` | Systematic QA testing + fix bugs found |
| `/qa-only` | QA testing report only (no fixes) |
| `/design-review` | Visual QA: find and fix spacing, hierarchy, AI slop issues |
| `/design-consultation` | Product design system creation |
| `/design-shotgun` | Generate multiple design variants for comparison |
| `/review` | Pre-landing PR review |
| `/ship` | Ship workflow: tests, review, changelog, PR |
| `/land-and-deploy` | Merge PR, wait for CI, verify production |
| `/canary` | Post-deploy canary monitoring |
| `/benchmark` | Performance regression detection |
| `/investigate` | Systematic debugging with root cause analysis |
| `/office-hours` | YC-style forcing questions for startups/builders |
| `/plan-ceo-review` | CEO/founder-mode plan review |
| `/plan-eng-review` | Eng manager plan review |
| `/plan-design-review` | Designer's eye plan review |
| `/autoplan` | Auto-review pipeline (CEO + design + eng) |
| `/retro` | Weekly engineering retrospective |
| `/document-release` | Post-ship documentation update |
| `/codex` | OpenAI Codex CLI: review, challenge, consult |
| `/cso` | Chief Security Officer audit |
| `/setup-browser-cookies` | Import browser cookies for authenticated testing |
| `/setup-deploy` | Configure deployment settings |
| `/careful` | Safety guardrails for destructive commands |
| `/freeze` | Restrict edits to a specific directory |
| `/unfreeze` | Clear freeze boundary |
| `/guard` | Full safety: careful + freeze combined |
| `/gstack-upgrade` | Upgrade gstack to latest version |

## Skill routing

When the user's request matches an available skill, ALWAYS invoke it using the Skill
tool as your FIRST action. Do NOT answer directly, do NOT use other tools first.
The skill has specialized workflows that produce better results than ad-hoc answers.

Key routing rules:
- Product ideas, "is this worth building", brainstorming → invoke office-hours
- Bugs, errors, "why is this broken", 500 errors → invoke investigate
- Ship, deploy, push, create PR → invoke ship
- QA, test the site, find bugs → invoke qa
- Code review, check my diff → invoke review
- Update docs after shipping → invoke document-release
- Weekly retro → invoke retro
- Design system, brand → invoke design-consultation
- Visual audit, design polish → invoke design-review
- Architecture review → invoke plan-eng-review
- Save progress, checkpoint, resume → invoke checkpoint
- Code quality, health check → invoke health
