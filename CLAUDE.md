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

# Browser protocol glue (tentaflow-protocol-wasm) requires these two.
# Without them build.rs skips generating www/js/protocol/wasm_glue.{js,wasm}
# and the dashboard fails to load in the browser.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked

# Or one-shot: ./scripts/setup.sh (Linux + macOS)
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
- **services/manifest/** — Service Manifest registry: ładowanie wygenerowanego rejestru z `services_generated.rs`, walidacja semantyczna (4 reguły), katalog silników udostępniany przez `/api/services/manifest`. Patrz sekcja `## Service Manifest`.
- **license/** — Sprawdzanie tieru licencji (Free/Pro/Enterprise), gating opcji `download` w manifestach
- **api/** — HTTP: OpenAI-compatible `/v1/*`, Dashboard `/api/*` (JWT), WebSocket metrics
- **flow_engine/** — DAG-based workflow execution with typed adapters
- **inference/** — LLM backends: llama.cpp, MLX, model manager
- **net/quic/** — QUIC client/server with TLS 1.3
- **db/** — SQLite (rusqlite, bundled), migrations, repository pattern

### Key Design Patterns

**Protocol serialization**: All QUIC messages use rkyv (zero-copy binary), not JSON. Protocol types live in `tentaflow-protocol/src/`. Two ALPN protocols: `tentaflow` (client→node) and `tentaflow-mesh` (node↔node).

**build.rs does two things**: (1) compiles WASM addons from `addons/` and `addons-pro/` to `wasm32-wasip1` and embeds them via `include_bytes!`, (2) embeds `www/` static files into the binary with MIME detection. Changes to `www/` require recompilation.
Bundled addon updates at startup are driven by `bundle_hash` (computed from embedded addon payload), not only by manifest `version`, so manifest-only changes propagate to the installed DB state without a forced version bump.

**Mesh security layers**: TLS 1.3 (transport) → Ed25519 identity → X25519 DH key exchange → ChaCha20-Poly1305 AEAD with epoch-based key rotation (24h interval, 7-day grace period) and replay protection (sequential nonce + sliding window).

**Pairing / First Contact**:
- Pierwszy kontakt nie idzie już przez istniejący `mesh` stream, tylko przez osobny ALPN `tentaflow-pairing/v1`.
- `MeshPairingStartRequest` może nieść hinty transportowe (`remote_addresses`, `remote_relay_url`, `remote_hostname`) z QR albo z autodiscovery.
- QR payload `tentaflow-pair://...` powinien zawierać co najmniej `node_id`, `pin`, oraz gdy są znane także `addr=` i `relay=`.
- Po potwierdzonym parowaniu utrwalamy `trusted_contact:*` w `settings`, żeby reconnect po zmianie sieci mógł iść od razu przez relay/direct hints zamiast czekać na świeże discovery.
- `MeshNodeInfo.connection` raportuje do GUI aktywną ścieżkę iroh (`p2p`/`relay`, `lan`/`wan`, adres, lista pathów), więc ekran Mesh pokazuje realny transport zamiast zgadywać po statusie.
- Receiver zapisuje `pending_contact:*` w settings, żeby późniejsze `confirm/reject` mogły dociągnąć połączenie do inicjatora nawet bez świeżego autodiscovery.
- `mesh` stream jest dalej używany po zestawieniu łączności do `PairingConfirm/Reject`, `NodeInfo` i `TrustedKeysSync`.

**Dashboard**:
- Frontend `www/` używa vanilla JS + custom elements `tf-*` z `tentaflow-core/www/js/components/`.
- Widok Addons (WASM) korzysta z komponentów `tf-chip`, `tf-searchbox`, `tf-toggle`, `tf-button`; układ i style modułu są trzymane w `tentaflow-core/www/css/addons.css`.

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
- JS module `tentaflow-core/www/js/generated/services-manifest.js` — importowany dynamicznie przez `www/js/modules/catalog/manifest-store.js` w GUI

## Legacy Cleanup
- `tentaflow-core/wwwroot/` zostało usunięte; jedynym aktywnym dashboardem jest `tentaflow-core/www/`.
- Binary protocol nie wspiera już legacy `NodeListRequest` ani `NodeInfoRequest`; GUI i backend używają ścieżki `MeshNode*`.

Pełna specyfikacja: `tentaflow-containers/_schema/SCHEMA.md`. Schema JSON: `tentaflow-containers/_schema/schema.json`.

### Struktura katalogu

Kategorie z ≥1 plikiem `*.toml` w `_services/` pokazują się w GUI; puste są ukryte.

| Katalog | Kategoria | Przykładowe silniki |
|---------|-----------|---------------------|
| `tentaflow-containers/llm/_services/` | Large Language Models | llama-cpp, mlx, vllm, sglang, ollama, tensorrt-llm |
| `tentaflow-containers/stt/_services/` | Speech-to-Text | whisper, parakeet, qwen-asr |
| `tentaflow-containers/tts/_services/` | Text-to-Speech | sherpa-onnx, xtts, voxcpm |
| `tentaflow-containers/image-gen/_services/` | Generowanie obrazów | comfyui, stable-diffusion-cpp |
| `tentaflow-containers/agents/_services/` | Autonomiczne agenty | teams-bot |

Pozostałe katalogi (`vision`, `video-gen`, `music-gen`, `model-3d-gen`, `tools`) istnieją w drzewie, ale dopóki nie dodasz pliku TOML do ich `_services/`, GUI nie pokaże tej sekcji.

### Anatomia pliku TOML

```toml
[engine]
id = "vllm"
category = "llm"
name = "vLLM"
description_pl = "..."
description_en = "..."
homepage = "https://github.com/vllm-project/vllm"
license = "Apache-2.0"
icon = "vllm"
default_port = 8000
api = "openai-compatible"
version = "0.6.3"

[deploy.docker]
context_path = "llm/docker/vllm"
platforms = ["linux", "windows"]

[deploy.native]
platforms = ["linux", "windows"]
runtime = "python-bundle"
bundle_path = "llm/python/vllm"

# Opcjonalnie:
# [deploy.external]
# platforms = ["linux", "macos", "windows"]
# detection_binary = "ollama"
# detection_endpoint = "http://localhost:11434"
# detection_health_path = "/api/tags"

[[model_preset]]
id = "qwen3-5-0-8b"
display_name = "Qwen 3.5 0.8B"
repo = "Qwen/Qwen3.5-0.8B"
recommended = true
```

Pełny opis pól w `tentaflow-containers/_schema/SCHEMA.md`.

### Tryby deploymentu

Manifest ma do trzech sekcji deploy (każda renderuje przycisk w wizardzie):

- **`[deploy.docker]`** — obraz Docker budowany lokalnie z `context_path`. Opcjonalny `download_image` (Pro feature, prebuilt OCI).
- **`[deploy.native]`** — natywne uruchomienie. Pole `runtime` decyduje:
  - `embedded` — wkompilowane w binarkę `tentaflow` przez Cargo `feature_flag` (np. llama.cpp, MLX).
  - `binary` — natywna binarka budowana skryptem `binary_path/build.sh` (np. sherpa-onnx, stable-diffusion-cpp).
  - `python-bundle` — bundle Pythona w `bundle_path` (np. vllm, xtts, comfyui).
- **`[deploy.external]`** — wykrycie zewnętrznego daemona w `PATH` z health-checkiem (np. ollama).

### Native Python Bundles

- Native `python-bundle` używa wspólnego katalogu modeli w `models/`; runner ustawia `HF_HOME`, `HUGGINGFACE_HUB_CACHE`, `TRANSFORMERS_CACHE` i `TORCH_HOME` tak, żeby Docker i native widziały te same pliki modeli.
- Cache runtime bundli można przenieść przez `TENTAFLOW_CACHE_DIR`, co jest przydatne na hostach gdzie `/tmp` jest `tmpfs` albo mało miejsca.
- Runner tworzy wersjonowane template w `<cache>/bundle-templates/<engine>/<template_hash>/venv` i osobne instancje w `<cache>/bundle-instances/<engine>/<instance_name>/`.
- Przy tworzeniu instancji runner próbuje najpierw zrobić hardlink plików z template; zwykła kopia jest tylko fallbackiem. To ogranicza zużycie miejsca dla ciężkich env typu `vllm`.
- Bundla z wrapperem HTTP (`parakeet`, `qwen-asr`, `xtts`, `voxcpm`) muszą trzymać własne `requirements.lock` obok `bundle.toml`, bo upstream repo nie gwarantują `fastapi`/`uvicorn` ani zależności wrappera.
- Native deploy wstrzykuje `PORT` z wizarda/compose do procesu Pythona; `bundle.toml` powinien używać `${PORT:-<domyślny_port>}` zamiast sztywnej wartości.
- `ServiceManifestDeployRequest` dla `runtime=embedded` nie może kończyć się samym rekordem `deployment`: po udanym deployu musi też utworzyć/odświeżyć wpis w tabeli `services`, żeby backend przywracał taki serwis po restarcie i żeby ekran `Services` nie był pusty po natywnym deployu `llama.cpp` / `mlx` / `whisper`.

### Walidacja

Build.rs sprawdza 4 reguły semantyczne przy każdym `cargo build`:

1. `engine.id` pasuje do regex `^[a-z0-9][a-z0-9_-]{0,63}$` (chroni przed path-traversal).
2. Manifest ma przynajmniej jedną sekcję deploy (`docker`, `native` lub `external`).
3. `deploy.native.runtime` spójny z polami: `embedded` ⇒ `feature_flag`; `binary` ⇒ `binary_path`; `python-bundle` ⇒ `bundle_path` (i tylko jedno z trzech).
4. Ścieżki `context_path` / `binary_path` / `bundle_path` istnieją na dysku.

Globalna unikalność `engine.id` jest egzekwowana cross-file.

### API endpoints

| Endpoint | Opis |
|----------|------|
| `GET /api/services/manifest` | Cały manifest jako JSON (lista silników) |
| `GET /api/services/manifest/:engine_id` | Pojedynczy silnik |
| `GET /api/license/info` | Tier licencji (`{tier, allows_pro, allows_enterprise}`) |
| `POST /api/services/deploy` | Deploy silnika (body: `engine_id`, `deploy_method` ∈ `docker`/`native`/`external`, `node_id`, `config`) |
| `GET /api/services/deployed` | Lista uruchomionych deploymentów |

Implementacja: `tentaflow-core/src/api/dashboard/api_services_manifest.rs`.

### Jak dodać nowy silnik

1. Wybierz kategorię (`llm`, `tts`, `stt`, ...) i utwórz `tentaflow-containers/<category>/_services/<engine-id>.toml` zgodnie z `_schema/SCHEMA.md`
2. Dla `[deploy.docker]`: dodaj `<category>/docker/<engine-id>/{Dockerfile, entrypoint.sh, ...}`
3. Dla `[deploy.native]` runtime=`binary`: dodaj `<category>/native/<engine-id>/build.sh`
4. Dla `[deploy.native]` runtime=`python-bundle`: dodaj `<category>/python/<engine-id>/{bundle.toml, server.py}`
5. Dla `[deploy.native]` runtime=`embedded`: tylko TOML manifest + Cargo feature w `tentaflow-core/Cargo.toml`
6. `cargo build` w `tentaflow-core/` — walidacja + auto-generacja Rust + JS rejestru
7. Reload GUI — kafelek silnika pojawi się dynamicznie z manifestu

## Configuration

`config.toml` at project root. Key sections: `[server]`, `[protocols.quic]`, `[mesh]`, `[load_balancing]`, `[monitoring]`. Default ports: HTTPS/QUIC on 8090, Prometheus on 9090.

## Conventions

- Comments in code: English only
- Variable/function names: English
- Commit messages: English, format `[type]: description`
- Rust: `rustfmt` defaults, `snake_case` functions, `PascalCase` types
- JS/HTML/CSS: 2-space indent, `camelCase` JS, `kebab-case` CSS
- C#: 4-space indent, `PascalCase` public, `_camelCase` private fields

## Code quality rules (MANDATORY — apply to every change)

These rules apply to humans AND to every AI agent working on this repo. No exceptions unless the user explicitly overrides a specific rule for a specific task.

### 1. No stubs, placeholders, or TODOs
- Every commit must be production-ready. If you cannot finish a feature in this pass, do not ship a partial implementation that pretends to work.
- Forbidden: `todo!()`, `unimplemented!()`, `// TODO: implement`, empty function bodies that return defaults, mock responses, "we'll wire this up later" scaffolding.
- If a dependency is missing, say so and stop. Do not fake it.

### 2. No backward-compatibility shims, no fallbacks
- When you change a function, change it in place. Do not keep the old version around "just in case".
- No alias exports, no deprecated wrappers, no feature flags for old behavior, no `if let Some(old) = ... else { new_path }` fallback chains.
- Exception: only when the user explicitly asks for compat (rare — assume never).

### 3. No versioned function names
- Forbidden: `process_request_v2`, `do_thing_new`, `calculate_ultrafast`, `handle_event_improved`, `user_check_permission_fixed`.
- If you are improving an existing function, **edit it in place**. The git history is the version record; the code should have one name per concept.
- If the signature change breaks callers, update the callers. That is the work.

### 4. Check for existing functions before writing new ones
- Before adding a new function, search the crate (or the relevant module) for something that already does this. Use Grep/ripgrep on likely names, likely signatures, and likely call sites.
- If a similar function exists and almost fits, extend it (new parameter, new enum variant) rather than forking a parallel one.
- This applies to Rust, JS, CSS, DB helpers — everywhere.

### 5. Delete unused code as you go
- When a refactor removes the last caller of a function, delete the function in the same commit. Do not leave dead code "in case we need it".
- Same for unused imports, unused struct fields, unused CSS classes, unused i18n keys, unused SQL helpers.
- `cargo check` warnings about unused items are bugs, not noise.

### 6. Comments describe WHY, not WHAT
- English only.
- File headers stay: `// ============ File: <name> — <1-sentence purpose> ============`.
- Inline comments only when the code's intent is not obvious from names — e.g. a workaround for a known bug, a non-obvious invariant, a performance trick. Do not narrate what the next line does.
- Forbidden: meta-comments like `// CRITICAL:`, `// OPT-001`, `// Fixed in this PR`, `// Changed from X to Y`, `// OWASP-xxx`. Git blame carries history; comments carry intent.

### 7. No cosmetic edits outside the task
- Do not reorder imports, rewrap lines, fix unrelated whitespace, or rename unrelated symbols while making a feature change. Those belong in a separate formatting commit if at all.

### 8. Always use project web components — never roll your own UI primitive

Project components live under `tentaflow-core/www/js/components/` — currently: `tf-button`, `tf-chip`, `tf-input`, `tf-menu`, `tf-searchbox`, `tf-select`, `tf-table`, `tf-tabs`, `tf-toggle`, `tf-window`.

**Rules:**
- For every UI primitive (button, input, select, toggle, chip, tabs, window/modal, searchbox, menu, table) use the `tf-*` component. Zero `<button>`, `<input>`, `<select>`, hand-rolled `.tabs-bar`, hand-rolled modal overlays in feature modules. The only permitted raw `<input>` is `type="file"` (no tf-file-input exists yet).
- If a `tf-*` component is missing a feature you need (animation, slot, event, variant, prop) — **extend the component**, don't build a one-off. Add the prop to the component's API, update its CSS, bump the demo if one exists.
- If a pattern is repeated in feature code (e.g. an oauth-mode radio card pattern, or a permission matrix cell), consider adding a new `tf-*` component. Add it when the pattern appears in 2+ places OR the feature module exceeds ~30 lines of markup for the same element.
- If a component's existing behavior is broken (no animation, wrong focus ring, missing keyboard handler), fix the component rather than working around it in the feature module.
- Code review rejects any diff that renders a custom tab strip, custom toggle, custom select dropdown, custom modal, etc., when a `tf-*` component exists. "Slight visual difference" is not justification — change the component's CSS variant.

**Why:** one-off UI primitives drift in look, accessibility, animation timing, and keyboard behavior. Users notice inconsistency. Components centralize the fixes.

### Enforcement
- Code review (human or `code-reviewer` agent) rejects any diff violating these rules.
- If an agent reports "I added a stub because X" or "I kept the old function for compat" — that is a reject condition; the work goes back for a real implementation.

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
