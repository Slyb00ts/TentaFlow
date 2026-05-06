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
- **flow_engine/** — DAG workflow executor (plan v4.2). Typed `FlowEnvelope` (payload + artifacts + provenance + ConversationContext + meta + trace), narrow capability dispatchers (`LlmDispatcher`, `EmbeddingsDispatcher`, `TtsDispatcher`, `SttDispatcher`, `MemoryStore`, `PromptStore`, `AuditSink`, `PiiRulesStore`, `TtsCleaningStore`, `ConversationHistoryStore`, `Clock`, `BlobStore`, `MetricsSink`), 13 node adapters in `node_adapters/`, 10 wrapper impls in `dispatchers_impl/`. Two execution paths: `execute_blocking` (full topo, FlowExecutionOutcome) and `execute_streaming` (pre-LLM topo + LLM stream + cancel/disconnect-resilient finalizer). Strict 1-input-edge per node, single trigger, streaming end-shape validated at compile time
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

**Mesh connection lifecycle (iroh)**:
- Transport is iroh QUIC via `IrohMeshManager` (`tentaflow-core/src/mesh/iroh_manager.rs`). Relay URL: `load_relay_url` returns `Option<RelayUrl>`. `None` = iroh's built-in N0 preset (4 production relays `*.relay.n0.iroh-canary.iroh.link`). `Some(url)` = custom override from DB `settings.mesh.iroh_relay_url` or `config.toml`.
- Discovery and auto-connect are intentionally separate concerns now: `PeerDiscovered` always feeds GUI/peer_store, but automatic mesh dialing is only for trusted peers. Discovery/known-peers/topology merge fresh addresses into `trusted_contact:*`, keep the currently working direct path first, and reconnect via hints instead of dialing every newly seen interface.
- On simultaneous dial (A→B and B→A concurrently) iroh produces two distinct QUIC connections. `register_connection` applies a deterministic tie-break: **outgoing wins only if `self_hex < peer_hex`** (lexicographic on endpoint-id hex). Both sides converge on the same physical connection; the loser is closed with reason `"tie-break-loser"`.
- `dial_locks: HashMap<peer_hex, Arc<Mutex<()>>>` — per-peer async mutex. All three `connect_to_peer*` variants acquire the lock before `endpoint.connect`, so at most one dial per peer is in flight. Lock is dropped on `disconnect_peer`.
- Heartbeat: sole producer is the loop in `pipeline.rs` (broadcasts rkyv `HeartbeatMetrics` every `heartbeat_interval_ms`, default 500). The empty `run_heartbeat_loop` stub in `iroh_manager.rs` has been removed.
- Upgrade path: `sanitize_trusted_contacts` runs at startup and strips `settings.trusted_contact:*` entries still pointing at the dead `use.iroh.network` default.

**Dashboard**:
- Frontend `www/` uses vanilla JS + custom elements `tf-*` from `tentaflow-core/www/js/components/`.
- The Addons (WASM) view uses `tf-chip`, `tf-searchbox`, `tf-toggle`, and `tf-button`; layout and styling live in `tentaflow-core/www/css/addons.css`.

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
- Self-hosted iroh relay deployment assets live in `tentaflow-containers/tools/docker/iroh-relay/`; the old top-level `deploy/iroh-relay/` location is no longer used.
- `deploy.docker` supports both single-container deployments via `context_path` and multi-container stack deployments via `compose_path`.
- Manifests may set `engine.resource_kind` to `ai` or `infra`; the catalog renders infrastructure separately so supporting stacks do not appear as AI runtimes.

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
  - `embedded` — wkompilowane w binarkę `tentaflow`. Gating opcjonalnie przez Cargo `feature_flag` (np. llama.cpp, MLX) albo przez `target_os` / stałą zależność (np. `apple-tts`, vision/* przez `tract-onnx`) — wtedy `feature_flag` pomijamy.
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
3. `deploy.native.runtime` spójny z polami: `embedded` MOŻE mieć `feature_flag` (gdy gating przez Cargo feature) ale nie musi (gdy gating przez `target_os` lub stałą zależność, np. `apple-tts`, vision/* przez `tract-onnx`); `embedded` NIE MOŻE mieć `binary_path` ani `bundle_path`. `binary` ⇒ `binary_path`. `python-bundle` ⇒ `bundle_path`. Tylko jedno z trzech.
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

## Profilowanie Nsight Systems

Profilowanie CPU + GPU (NVIDIA Nsight Systems) per-card / per-node sterowane z GUI. Sesja jest uruchamiana lokalnie albo na zaufanym nodzie mesh przez forwarding rkyv; raport jest renderowany w przeglądarce bez zewnętrznego viewera.

### Architektura

- **`tentaflow-core/src/profiling/`** — runner `nsys`, parser SQLite eksportu, builder timeline, storage (FIFO 20 sesji), capability detection (`nsys --version`, cache 5s).
- **`tentaflow-protocol/src/profiling.rs`** — typy rkyv: `NsightScope`, `ProfileReport`, oraz `NsightPayload` z 5+1 parami request/response.
- **`tentaflow-core/src/dispatch/mesh_write_handlers.rs`** — 6 handlerów (`start`, `stop`, `sessions`, `report`, `delete`, `download`), `policy=Admin`.
- **`tentaflow-core/src/dispatch/command_executor.rs`** — wykonanie zdalne; forwarding przez `MeshCommandType::Nsight*` (typed payloady rkyv, tylko trusted peer).
- **`tentaflow-core/src/mesh/peer_store.rs`** — propagacja capability `nsys_available` + `nsys_version` przez `HeartbeatMetrics`.
- **GUI**: `tentaflow-core/www/js/modules/mesh-detail-nsight.js` (modal startu, badge REC z countdown, lista sesji), `tentaflow-core/www/js/modules/profile-report.js` (6 KPI tiles, 7 zakładek `tf-tabs`: Overview / GPU Kernels / CUDA APIs / Memory / CPU Samples / NVTX / Timeline; vanilla SVG line chart per karta dla SM / Memory / Power). Routing: `Router.navigate('profile-report', { nodeId, sessionId })`. Per-card przycisk "Profile" widoczny gdy `gpu.vendor === 'Nvidia'` ORAZ `node.nsys_available`.

### Tryby profilowania

| `NsightScope` | Flagi `nsys profile` |
|---------------|----------------------|
| `Cpu` | `--sample=cpu --trace=osrt --gpu-metrics-device=none` |
| `GpuIndex(i)` | `--sample=none --trace=cuda,cudnn,cublas,nvtx --gpu-metrics-device=<i>` |
| `GpuAll` | `--sample=none --trace=cuda,cudnn,cublas,nvtx --gpu-metrics-device=all` |
| `BothIndex(i)` | `--sample=cpu --trace=cuda,cudnn,cublas,osrt,nvtx --gpu-metrics-device=<i>` |
| `BothAll` | `--sample=cpu --trace=cuda,cudnn,cublas,osrt,nvtx --gpu-metrics-device=all` |

### Wymagania

- `nsys` w `PATH` (CUDA Toolkit z Nsight Systems). Detekcja przez `nsys --version`, capability cache 5s.
- DGX Spark / arm64-sbsa: brak specjalnych ścieżek — wystarczy zainstalowany Nsight Systems dla arm64.
- Sesja działa lokalnie na nodzie posiadającym GPU; mesh forwarding jest tylko transportem żądań.

### Limity

- 1 aktywna sesja per nod (kolejne `start` odrzucane).
- `duration_seconds` ∈ `0..=600` (`0` = manual stop przez `nsight.stop`).
- `label` ≤ 128 znaków, bez znaków kontrolnych.
- Storage FIFO: maks 20 sesji per nod; rotacja sierot starszych niż 1h bez `summary.bin`.

### Storage layout

```
<TENTAFLOW_HOME>/nsight/<node_id>/<session_id>/
├── report.nsys-rep    # surowy raport nsys
└── summary.bin        # rkyv ProfileReport (parsed timeline + KPI)
```

`session_id` walidowane regex `^[a-f0-9]{16,32}$`. Przed `nsys export` runner robi `tokio::fs::symlink_metadata` na ścieżce wyjściowej (anty path-traversal).

### Bezpieczeństwo

- Shell injection: każdy argument `nsys` jest oddzielnym `String`, zero `format!()` shell concat.
- Resource exhaustion: limity (1 sesja/nod, FIFO 20, duration ≤ 600s, label ≤ 128).
- Permission: handlery `policy=Admin`; mesh route akceptuje wyłącznie trusted peerów.

### Audit events

`repository::log_audit` zapisuje:

- `nsight.start` — przy starcie sesji (lokalnej lub zdalnej).
- `nsight.stop` — przy ręcznym lub automatycznym (timeout) zakończeniu.

## Flow engine (plan v4.2 — Etap 1 zakończony)

Single executor stack po stage 1d (zero parallel install). Layout:

```
flow_engine/
├── envelope.rs           # FlowEnvelope, FlowValue, NodeInput, FlowExecutionOutcome,
│                         # ChatMessage, TraceStep, TokenUsage, FinishReason,
│                         # EnvelopeDelta::Llm, LlmStreamChunk, ToolCallDelta
├── types.rs              # FlowDefinition, FlowNode, FlowEdge (DAG types tylko)
├── blob_store.rs         # BlobStore trait + InMemoryBlobStore + FileBlobStore stub
├── cancel_on_drop.rs     # CancelOnDropStream — wpięty w SSE response w routing/streaming.rs
├── cache.rs              # CompiledFlow + FlowCache. CompiledFlow::compile woła
│                         # validation::validate, buduje toposort + adjacency +
│                         # is_streaming detection
├── validation.rs         # 7 strict rules (R1–R7), w tym streaming end-shape (R7)
├── converter.rs          # flow_outcome_to_chat_response, flow_outcome_to_embedding_response
├── executor.rs           # execute_blocking + execute_streaming + finalizer
├── dispatcher.rs         # FlowDispatcher (bootstrap registry + ContextFactory),
│                         # FlowRequestMeta DTO
├── resolver.rs           # resolve_flow(model, service_type) → DbFlow
├── node_adapter.rs       # NodeAdapter + LlmAdapter (typed) + AdapterRegistry +
│                         # ExecutionContext + UsageSink
├── node_adapters/        # 13 adapter implementations
├── dispatchers/          # 10 capability traits + DTO
└── dispatchers_impl/     # 10 wrapperów + QuicClientFinder + slot type aliases
```

### Architektura egzekucji

1. **Trigger seed** — routing buduje `FlowEnvelope` z `ChatCompletionRequest` przez
   `routing::build_initial_envelope_for_user`. Wynik: payload `FlowValue::Text(last_message)`,
   `meta["model"]`, `context.messages` (pełna historia ChatMessage).
2. **Compile** — `CompiledFlow::compile(flow_id, definition, registry)` waliduje 7 reguł,
   robi toposort, wykrywa `is_streaming` (`from_port == "stream"` na którymś edge'u),
   cache'owany w `FlowCache`.
3. **Bootstrap context** — `FlowDispatcher::ContextFactory` klonuje Arc'i wszystkich
   capability dispatcherów per call, dorzuca `BlobStore`, `Clock`, `UsageSink`,
   `CancellationToken`, `deadline`. Surface taki sam dla blocking i streaming.
4. **Topo loop** — `execute_blocking` lub `execute_streaming` walka per `execution_order`:
   - Cancel + deadline check between nodes (klient disconnect / operator timeout).
   - `inputs` budowane z `outputs[from_pos]` (max 1 element przez R4).
   - `adapter.execute(node, &inputs, &ctx)` → `FlowEnvelope` przekazywany dalej.
   - `continue_on_error` z `trigger.config` decyduje czy błąd przerywa flow.
5. **Streaming finalizer** — `execute_streaming` po pre-LLM nodach woła
   `LlmAdapter::prepare_llm_request` (typed accessor), dispatchuje
   `ctx.llm.stream_chat`, spawnuje finalizer z biased `select!` (cancel ↔ adapter_stream),
   buduje `FlowExecutionOutcome` po EOF/cancel/error, persist po `execution_id`.
6. **Disconnect bridge** — `routing/streaming.rs` owija filtered chunk stream w
   `CancelOnDropStream(stream, meta.cancel_token)` przed return; gdy hyper droppuje SSE
   body, Drop puszcza `cancel_token.cancel()`, executor finalizer widzi to przez
   biased select.

### Hard rules (egzekwowane przez validation.rs)

| Reguła | Opis |
|--------|------|
| R1 | każdy edge.from / edge.to wskazuje na istniejący node |
| R2 | każdy node ma adapter w registry |
| R3 | edge.from_port ∈ supported_output_ports producenta; edge.to_port ∈ supported_input_ports konsumenta |
| R4 | trigger ma 0 incoming (źródło flow); każdy non-trigger ma ≤1 incoming |
| R5 | dokładnie jeden trigger node w flow |
| R6 | edge `from_port="true"`/`"false"` tylko z node'a `condition` |
| R7 | streaming end-shape — co najwyżej 1 edge `from_port="stream"`, target musi być `output` z `config.mode="stream"`, LLM nie ma żadnego innego outgoing edge |

### Node adapters (13)

| Node type | Adapter | output_ports | input_ports |
|-----------|---------|--------------|-------------|
| `trigger` | `TriggerNodeAdapter` | `["full"]` | (brak — źródło) |
| `output` | `OutputNodeAdapter` | `["full"]` | `["in"]` |
| `condition` | `ConditionNodeAdapter` | `["true", "false"]` | `["in"]` |
| `pii_filter` | `PiiFilterNodeAdapter` | `["full"]` | `["in"]` |
| `tts_clean` | `TtsCleanNodeAdapter` | `["full"]` | `["in"]` |
| `llm` | `LlmNodeAdapter` (impl `LlmAdapter`) | `["stream", "full"]` | `["in"]` |
| `stt` | `SttNodeAdapter` | `["full"]` | `["in"]` |
| `tts` | `TtsNodeAdapter` | `["full"]` | `["in"]` |
| `embeddings` | `EmbeddingsNodeAdapter` | `["full"]` | `["in"]` |
| `memory` | `MemoryNodeAdapter` (mode `query`/`store`) | `["full"]` | `["in"]` |
| `conversation_history` | `ConversationHistoryNodeAdapter` | `["full"]` | `["in"]` |
| `session_context` | `SessionContextNodeAdapter` | `["full"]` | `["in"]` |
| `speaker_context` | `SpeakerContextNodeAdapter` | `["full"]` | `["in"]` |

### Capability dispatchers + impls

Adaptery widzą wyłącznie wąskie traits (`flow_engine/dispatchers/`); implementacje
(`flow_engine/dispatchers_impl/`) wywołują runtime/registry/DB. Każdy impl trzyma
najwęższe dependency (slot, Arc<DbPool>, registry) — żaden nie holduje
`Arc<ServiceManager>` (D4 invariant).

| Trait | Impl | Wraps |
|-------|------|-------|
| `LlmDispatcher` | `LlmDispatcherImpl` | `ModelRuntimeExecutor::execute_chat` / `stream_chat` (slot pattern) |
| `EmbeddingsDispatcher` | `EmbeddingsDispatcherImpl` | `ModelRuntimeExecutor::execute_embeddings` |
| `TtsDispatcher` | `TtsDispatcherImpl` | `ModelRuntimeExecutor::execute_tts` + `BlobStore` |
| `SttDispatcher` | `SttDispatcherImpl` | `SttRuntime::transcribe` (slot pattern) + `BlobStore` |
| `PromptStore` | `PromptsImpl` | `SharedPromptRegistry::get_content` |
| `MemoryStore` | `MemoryStoreImpl` | rkyv `MemoryPayload` przez `QuicClientFinder` |
| `ConversationHistoryStore` | `ConversationHistoryImpl` | `ConversationCache` |
| `AuditSink` | `AuditSinkImpl` | `repository::log_audit` |
| `PiiRulesStore` | `PiiRulesStoreImpl` | `repository::list_pii_rules_active` |
| `TtsCleaningStore` | `TtsCleaningStoreImpl` | `tts::clean_cache::clean` |
| `Clock` | `SystemClock` | `SystemTime::now` |
| `MetricsSink` | `NoopMetrics` | placeholder |
| `BlobStore` | `InMemoryBlobStore` | bootstrap default; `FileBlobStore` w stage 2 |

### Słowo o bypass'ach

- **Bare passthrough** w `routing/chat.rs` / `routing/streaming.rs` — gdy żaden flow nie
  pasuje (resolver zwraca `None`), request idzie bezpośrednio do `ModelRuntimeExecutor`
  bez flow_engine.
- **Direct executor path** w `services/runtime/executor.rs` (`dispatch_by_flow_id`) —
  używane gdy `CatalogSnapshot` ma `flow_id` przypiętą do publikowanego modelu.
  Buduje `FlowEnvelope` przez `embeddings_request_to_initial_envelope` /
  `build_initial_envelope_for_user`.

### Zmiany w stosunku do legacy

Po stage 1d całkowicie usunięte: `flow_engine/adapters/` (legacy `NodeAdapter` z
`FlowContext` + `serde_json::Value`), `flow_engine/executor_async.rs`
(`FlowExecutorAsync` + `ParsedFlow`), `FlowContext`/`FlowExecutionResult`/`FlowStepLog`
z `types.rs`, `AddonNodeAdapter` z `addon/flow_blocks.rs` (dead code), `routing/
memory_integration.rs`, `memory_analyzer/`, `intent_analyzer/`, hardkodowane prompt
stałe.

Seedowane flows (`db/seed.rs`): `Standardowy pipeline LLM`, `teams-flow`. Test
`seeded_flows_pass_adapter_validation` używa nowego `AdapterRegistry` z
`node_adapter.rs` (`registry.register_llm` + 12× `register`).

Test reference: `cargo test --lib --features dashboard-api flow_engine` (96 testów),
`cargo test --lib --features dashboard-api seeded_flows_pass_adapter_validation`.

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

### 9. No CSV — always JSON for serialized lists

**NEVER** persist or serialize a list-shaped value as CSV (`"a,b,c"`). Use JSON arrays (`["a","b","c"]`) — every layer (DB column, GUI form payload, wire protocol, config file).

**Why:** A real bug we hit: `model_aliases.fallback_targets` was written as CSV by the GUI (`services.js`) and parsed as JSON by the Rust catalog provider via `serde_json::from_str(...).unwrap_or_default()`. Every CSV row silently parsed to an empty list — DB-backed alias fallbacks were invisible to the catalog despite the GUI showing them. CSV gets you ad-hoc parsers per layer, comma collisions in real values, and silent `unwrap_or_default` failures that mask the drift.

**Rules:**
- New list field → JSON array on every layer. `serde_json` in Rust, `JSON.parse` / `JSON.stringify` in JS.
- Code review rejects `.split(',')` and `.join(',')` on list-shaped fields. The ONLY allowed `split(',')` is parsing third-party text formats produced by tools we don't own (`vm_stat`, `iostat`, `/proc/net/dev`). Anything in our own storage / wire / GUI is JSON.
- Existing CSV fields are migration debt: when you touch one, migrate it to JSON in the same commit (writer + reader + DB migration if needed).
- **Migrations don't get to interpret CSV either.** Legacy CSV in our storage is wiped to NULL with a loud warn. Admins reconstruct the data manually. A CSV-tolerant repair path becomes a permanent CSV interpreter the moment someone forgets to remove it.

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
